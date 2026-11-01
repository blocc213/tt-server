//! HTTP equivalents of page-owned Tauri resources. IDs never grant cross-session access.
use std::collections::HashMap;
use std::sync::Arc;
use parking_lot::Mutex;
use tokio::sync::Mutex as AsyncMutex;
use tt_ports::byte_reader::ByteReader;
use tt_ports::repositories::chat_commit_repository::ChatSwipeSource;
use crate::error::ServerError;

#[derive(Clone)]
pub struct RequestOwner(pub String);

enum Resource {
    Reader(AsyncMutex<Box<dyn ByteReader>>),
    Swipes(Arc<dyn ChatSwipeSource>),
}

#[derive(Default)]
pub struct BrowserResources {
    entries: Mutex<HashMap<u32, (String, Arc<Resource>)>>,
    commits: Mutex<HashMap<String, String>>,
}
impl BrowserResources {
    fn add(&self, owner: &str, resource: Resource) -> Result<u32, ServerError> {
        let mut entries = self.entries.lock();
        if entries.values().filter(|(key, _)| key == owner).count() >= 128 {
            return Err(ServerError::TooManyRequests("Too many open byte resources; close unused readers".into()));
        }
        let id = loop { let id = rand::random(); if !entries.contains_key(&id) { break id; } };
        entries.insert(id, (owner.into(), Arc::new(resource)));
        Ok(id)
    }
    pub fn reader(&self, owner: &str, reader: Box<dyn ByteReader>) -> Result<u32, ServerError> {
        self.add(owner, Resource::Reader(AsyncMutex::new(reader)))
    }
    pub fn swipes(&self, owner: &str, source: Arc<dyn ChatSwipeSource>) -> Result<u32, ServerError> {
        self.add(owner, Resource::Swipes(source))
    }
    fn get(&self, owner: &str, id: u32) -> Result<Arc<Resource>, ServerError> {
        self.entries.lock().get(&id).filter(|(key, _)| key == owner)
            .map(|(_, resource)| resource.clone()).ok_or_else(|| ServerError::NotFound("Resource expired; reopen it".into()))
    }
    pub fn source(&self, owner: &str, id: u32) -> Result<Arc<dyn ChatSwipeSource>, ServerError> {
        match &*self.get(owner, id)? {
            Resource::Swipes(source) => Ok(source.clone()),
            _ => Err(ServerError::BadRequest("Resource is not a cold swipe source".into())),
        }
    }
    pub async fn read(&self, owner: &str, id: u32) -> Result<Vec<u8>, ServerError> {
        let resource = self.get(owner, id)?;
        let Resource::Reader(reader) = &*resource else { return Err(ServerError::BadRequest("Resource is not a byte reader".into())); };
        let mut bytes = vec![0; 512 * 1024];
        let len = reader.lock().await.read(&mut bytes).await?;
        bytes.truncate(len);
        if len == 0 { self.close(owner, id); }
        Ok(bytes)
    }
    pub fn close(&self, owner: &str, id: u32) {
        let mut entries = self.entries.lock();
        if entries.get(&id).is_some_and(|(key, _)| key == owner) { entries.remove(&id); }
    }
    pub fn close_owner(&self, owner: &str) {
        self.entries.lock().retain(|_, (key, _)| key != owner);
        self.commits.lock().retain(|_, key| key != owner);
    }
    pub fn close_session(&self, session:&str) {
        let prefix=format!("{session}:");
        self.entries.lock().retain(|_,(key,_)|!key.starts_with(&prefix));
        self.commits.lock().retain(|_,key|!key.starts_with(&prefix));
    }
    pub fn own_commit(&self, owner: &str, kind: &str, id: &str) {
        self.commits.lock().insert(format!("{kind}:{id}"), owner.into());
    }
    pub fn check_commit(&self, owner: &str, kind: &str, id: &str) -> Result<(), ServerError> {
        if self.commits.lock().get(&format!("{kind}:{id}")).is_some_and(|key| key == owner) { Ok(()) }
        else { Err(ServerError::NotFound("Commit session not found".into())) }
    }
    pub fn forget_commit(&self, kind: &str, id: &str) { self.commits.lock().remove(&format!("{kind}:{id}")); }
}

/// Browser extension callbacks need a bidirectional channel; do not advertise tools that cannot run.
pub struct NativeExtensionToolsUnavailable;
#[async_trait::async_trait]
impl tt_ports::extension_tools::ExtensionTools for NativeExtensionToolsUnavailable {
    fn list(&self) -> Result<Vec<tt_contracts::extension_tools::ExtensionTool>, tt_domain::errors::DomainError> { Ok(Vec::new()) }
    async fn call(&self, _: tt_contracts::extension_tools::ExtensionToolCall, _: tokio::sync::watch::Receiver<bool>) -> Result<tt_contracts::extension_tools::ExtensionToolReply, tt_domain::errors::DomainError> {
        Err(tt_domain::errors::DomainError::InvalidData("Browser extension tools require a native host; HTTP channel transport is unavailable".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resource_and_commit_ids_do_not_cross_sessions() {
        let resources = BrowserResources::default();
        resources.own_commit("session-a:tab", "settings", "opaque");
        assert!(resources.check_commit("session-a:tab", "settings", "opaque").is_ok());
        assert!(resources.check_commit("session-b:tab", "settings", "opaque").is_err());
        assert!(resources.check_commit("session-a:other-tab", "settings", "opaque").is_err());
        resources.close_owner("session-a:tab");
        assert!(resources.check_commit("session-a:tab", "settings", "opaque").is_err());
    }

    struct BytesReader(&'static [u8]);
    #[async_trait::async_trait]
    impl ByteReader for BytesReader {
        async fn read(&mut self,buffer:&mut [u8])->Result<usize,tt_domain::errors::DomainError>{
            let n=buffer.len().min(self.0.len());buffer[..n].copy_from_slice(&self.0[..n]);self.0=&self.0[n..];Ok(n)
        }
    }
    #[tokio::test]
    async fn readers_are_session_scoped_and_eof_closes_them() {
        let resources=BrowserResources::default();
        let rid=resources.reader("a:tab",Box::new(BytesReader(b"binary\0bytes"))).unwrap();
        assert!(resources.read("b:tab",rid).await.is_err());
        assert!(resources.read("a:other",rid).await.is_err());
        resources.close("b:tab",rid);
        assert_eq!(resources.read("a:tab",rid).await.unwrap(),b"binary\0bytes");
        assert!(resources.read("a:tab",rid).await.unwrap().is_empty());
        assert!(resources.read("a:tab",rid).await.is_err());
        let rid=resources.reader("a:tab",Box::new(BytesReader(b"bytes"))).unwrap();
        resources.close_session("a");
        assert!(resources.read("a:tab",rid).await.is_err());
    }
}
