//! Raw IPC adaptation: JSON control frames and bounded binary data frames.
use axum::body::Body;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};
use tt_ports::repositories::chat_commit_repository::{ChatCommitOperation, ColdSwipeCommitSource};
use crate::{error::ServerError, state::SharedState};
use crate::rpc::{arg, opt_arg};

fn binary(bytes: Vec<u8>) -> Response {
    ([(http::header::CONTENT_TYPE, "application/octet-stream")], Body::from(bytes)).into_response()
}
fn json_response<T: serde::Serialize>(value: T) -> Result<Response, ServerError> { Ok(Json(value).into_response()) }
fn version_response(value: Value, version: tt_ports::repositories::chat_commit_repository::ChatPayloadVersion) -> Result<Response, ServerError> {
    let token = crate::chat::VersionToken::from(version);
    let header = serde_json::to_string(&token)?;
    let mut response = Json(value).into_response();
    response.headers_mut().insert(crate::chat::CHAT_VERSION_HEADER,
        http::HeaderValue::from_str(&header).map_err(|e| ServerError::Internal(e.to_string()))?);
    Ok(response)
}

pub async fn dispatch(state: &SharedState, owner: &str, command: &str, args: &Value) -> Option<Result<Response, ServerError>> {
    match run(state, owner, command, args).await {
        Ok(Some(response)) => Some(Ok(response)),
        Ok(None) => None,
        Err(error) => Some(Err(error)),
    }
}
async fn run(state: &SharedState, owner: &str, command: &str, args: &Value) -> Result<Option<Response>, ServerError> {
    let s = &state.services;
    let resources = &state.browser_resources;
    let response = match command {
        "get_sillytavern_settings" => binary(s.settings_service.get_sillytavern_settings().await?.bytes.to_vec()),
        "get_world_info" => { let dto: tt_application::dto::world_info_dto::GetWorldInfoDto = arg(args, "dto")?; binary(s.world_info_service.get_world_info(&dto.name).await?.bytes) }
        "read_agent_workspace_bytes" => binary(s.agent_runtime_service.read_workspace_bytes(arg(args, "dto")?).await?),
        "read_bytes" => binary(resources.read(owner, arg(args, "rid")?).await?),
        "close_resource" => { resources.close(owner, arg(args, "rid")?); json_response(())? }
        "close_page_resources" => { resources.close_owner(owner); json_response(())? }
        "open_chat_payload_json" => {
            let opened = s.chat_commit_service.open_payload_json_versioned(arg(args, "target")?, opt_arg(args, "allowNotFound")?.unwrap_or(false)).await?;
            match opened { None => json_response(Value::Null)?, Some((reader, version)) => version_response(json!(resources.reader(owner, reader)?), version)? }
        }
        "open_cold_chat" => {
            let opened = s.chat_commit_service.open_swipe_source_versioned(arg(args, "target")?, opt_arg(args, "allowNotFound")?.unwrap_or(false)).await?;
            match opened {
                None => json_response(Value::Null)?,
                Some((source, version)) => {
                    let source_id = resources.swipes(owner, source.clone())?;
                    let reader_id = match resources.reader(owner, source.projection(source_id)) { Ok(id) => id, Err(e) => { resources.close(owner, source_id); return Err(e); } };
                    let token = crate::chat::VersionToken::from(version.clone());
                    version_response(json!({"sourceId":source_id,"readerId":reader_id,"version":token}), version)?
                }
            }
        }
        "open_cold_swipe_record" => {
            let source = resources.source(owner, arg(args, "sourceId")?)?;
            json_response(resources.reader(owner, source.record(arg(args, "record")?).await?)?)?
        }
        "open_chat_backup_download" => json_response(resources.reader(owner, s.chat_service.open_chat_backup_download(&arg::<String>(args, "name")?).await?)?)?,
        "open_extension_store_entry" => {
            let key: String = arg(args, "key")?;
            let kind = arg(args, "kind")?;
            match s.extension_store_service.open_entry(&arg::<String>(args, "namespace")?, opt_arg::<String>(args, "table")?.as_deref(), &key, kind).await? {
                None => json_response(Value::Null)?,
                Some(reader) => json_response(json!({"readerId":resources.reader(owner, reader)?,"mimeType": match kind {
                    tt_contracts::extension_store::EntryKind::Json => "application/json".to_owned(),
                    tt_contracts::extension_store::EntryKind::Blob => mime_guess::from_path(&key).first_or_octet_stream().essence_str().to_owned(),
                }}))?,
            }
        }
        "delete_extension_store_entry" => json_response(s.extension_store_service.delete_entry(&arg::<String>(args,"namespace")?,opt_arg::<String>(args,"table")?.as_deref(),&arg::<String>(args,"key")?,arg(args,"kind")?).await?)?,
        "begin_chat_commit" => {
            let operation: Value = arg(args,"operation")?;
            let force = opt_arg::<bool>(args,"force")?.unwrap_or(false) || operation["force"].as_bool().unwrap_or(false);
            let operation = match operation["kind"].as_str() {
                Some("payload") => ChatCommitOperation::Payload { force, cold_source: operation["coldSourceId"].as_u64().map(|id| {
                    let id = u32::try_from(id).map_err(|_|ServerError::BadRequest("Invalid cold source id".into()))?;
                    Ok::<_,ServerError>(ColdSwipeCommitSource {id,source:resources.source(owner,id)?})
                }).transpose()? },
                Some("metadata") => ChatCommitOperation::Metadata,
                Some("metadataExtension") => ChatCommitOperation::MetadataExtension { namespace: arg(&operation,"namespace")? },
                _ => return Err(ServerError::BadRequest("Unknown chat commit operation".into())),
            };
            let precondition = crate::chat::precondition(force,opt_arg(args,"version")?,opt_arg(args,"isNew")?.unwrap_or(false))?;
            let begin = s.chat_commit_service.begin_with_precondition(arg(args,"target")?, operation, precondition).await?;
            resources.own_commit(owner,"chat",&begin.session_id);
            json_response(begin)?
        }
        "begin_settings_commit" => { let begin=s.settings_service.begin_commit(opt_arg(args,"expectedRevision")?).await?; resources.own_commit(owner,"settings",&begin.session_id); json_response(begin)? }
        "begin_world_info_commit" => { let begin=s.world_info_service.begin_commit().await?; resources.own_commit(owner,"world_info",&begin.session_id); json_response(begin)? }
        "begin_extension_store_commit" => { let begin=s.extension_store_service.begin_commit(&arg::<String>(args,"namespace")?,opt_arg::<String>(args,"table")?.as_deref(),&arg::<String>(args,"key")?,arg(args,"operation")?).await?; resources.own_commit(owner,"extension_store",&begin.session_id); json_response(begin)? }
        "finish_chat_commit" | "finish_settings_commit" | "finish_world_info_commit" | "finish_extension_store_commit" |
        "abort_chat_commit" | "abort_settings_commit" | "abort_world_info_commit" | "abort_extension_store_commit" => {
            let kind = command.strip_prefix("finish_").or_else(||command.strip_prefix("abort_")).unwrap().strip_suffix("_commit").unwrap();
            let id: String = arg(args,"sessionId")?;
            resources.check_commit(owner,kind,&id)?;
            let result = if command.starts_with("finish_") {
                let size=arg(args,"expectedSize")?;
                match kind {
                    "chat" => { let committed=s.chat_commit_service.finish_versioned(&id,size,arg(args,"commitReason")?).await?; json_response(json!({"version":crate::chat::VersionToken::from(committed.version)}))? }
                    "settings" => json_response(s.settings_service.finish_commit(&id,size).await?)?,
                    "world_info" => json_response(s.world_info_service.finish_commit(&id,size).await?)?,
                    "extension_store" => json_response(s.extension_store_service.finish_commit(&id,size).await?)?,
                    _ => unreachable!(),
                }
            } else {
                match kind {"chat"=>s.chat_commit_service.abort(&id).await?,"settings"=>s.settings_service.abort_commit(&id).await?,"world_info"=>s.world_info_service.abort_commit(&id).await?,"extension_store"=>s.extension_store_service.abort_commit(&id).await?,_=>unreachable!()};
                json_response(())?
            };
            resources.forget_commit(kind,&id);
            result
        }
        _ => return Ok(None),
    };
    Ok(Some(response))
}

pub async fn raw(state:&SharedState, owner:&str, command:&str, headers:&http::HeaderMap, bytes:&[u8]) -> Result<Response,ServerError> {
    let header=|name:&str| headers.get(name).and_then(|v|v.to_str().ok()).ok_or_else(||ServerError::BadRequest(format!("Missing header `{name}`")));
    let offset=|| header("offset")?.parse::<u64>().map_err(|_|ServerError::BadRequest("Invalid offset".into()));
    if command=="stage_file_chunk" {
        let path=percent_encoding::percent_decode_str(header("file-path")?).decode_utf8().map_err(|_|ServerError::BadRequest("Invalid staged path".into()))?;
        return json_response(state.upload_staging.append(&path,offset()?,bytes).await?);
    }
    let kind=match command {"append_chat_commit_chunk"=>"chat","append_settings_commit_chunk"=>"settings","append_world_info_commit_chunk"=>"world_info","append_extension_store_commit_chunk"=>"extension_store",_=>return Err(ServerError::NotFound(format!("Raw command `{command}` is unavailable")))};
    let id=header("session-id")?;
    state.browser_resources.check_commit(owner,kind,id)?;
    let s=&state.services;
    let next=match kind {"chat"=>s.chat_commit_service.append(id,offset()?,bytes).await?,"settings"=>s.settings_service.append_commit(id,offset()?,bytes).await?,"world_info"=>s.world_info_service.append_commit(id,offset()?,bytes).await?,"extension_store"=>s.extension_store_service.append_commit(id,offset()?,bytes).await?,_=>unreachable!()};
    json_response(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn raw_dispatch_appends_bytes_and_rejects_other_session() {
        let root=std::env::temp_dir().join(format!("tt-raw-{}",rand::random::<u64>()));
        let services=crate::composition::build(&root,std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."),Default::default(),crate::product::USER_AGENT).await.unwrap();
        let state=std::sync::Arc::new(crate::state::AppState{services,auth:crate::auth::Auth::new(None),frontend_dir:root.join("frontend"),csrf_token:"test".into(),upload_staging:std::sync::Arc::new(crate::upload::UploadStaging::new(&root)),browser_resources:Default::default(),templates:Default::default()});
        let begin=state.services.world_info_service.begin_commit().await.unwrap();
        state.browser_resources.own_commit("a:tab","world_info",&begin.session_id);
        let mut headers=http::HeaderMap::new();
        headers.insert("session-id",begin.session_id.parse().unwrap());headers.insert("offset","0".parse().unwrap());
        let bytes=br#"{"name":"Raw","data":{"entries":{}}}"#;
        assert!(raw(&state,"b:tab","append_world_info_commit_chunk",&headers,bytes).await.is_err());
        let response=raw(&state,"a:tab","append_world_info_commit_chunk",&headers,bytes).await.unwrap();
        let body=axum::body::to_bytes(response.into_body(),usize::MAX).await.unwrap();
        assert_eq!(serde_json::from_slice::<u64>(&body).unwrap(),bytes.len() as u64);
        state.services.world_info_service.finish_commit(&begin.session_id,bytes.len() as u64).await.unwrap();
        let response=dispatch(&state,"a:tab","get_world_info",&json!({"dto":{"name":"Raw"}})).await.unwrap().unwrap();
        assert_eq!(response.headers()[http::header::CONTENT_TYPE],"application/octet-stream");
        let body=axum::body::to_bytes(response.into_body(),usize::MAX).await.unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(),json!({"entries":{}}));
        assert!(raw(&state,"a:tab","delete_user_file",&headers,b"anything").await.is_err());
        drop(state);tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
