//! JSON-only services; channel-bound native commands are deliberately absent.
use serde_json::Value;
use crate::{state::SharedState,error::ServerError};
use crate::rpc::{arg,ok,staged,staged_skill_input};
use tt_application::dto::{mcp_dto::*, agent_dto::*};

pub async fn dispatch(state:&SharedState, command:&str,args:Value)->Option<Result<Value,ServerError>> {
    match run(state,command,args).await { Ok(Some(value))=>Some(Ok(value)),Ok(None)=>None,Err(error)=>Some(Err(error)) }
}
async fn run(state:&SharedState, command:&str,args:Value)->Result<Option<Value>,ServerError> {
    let s=&state.services;
    let value=match command {
        "read_frontend_template"=>ok(state.templates().read_frontend_template(&arg::<String>(&args,"name")?)?),
        "read_frontend_extension_template"=>ok(state.templates().read_frontend_extension_template(&arg::<String>(&args,"extension")?,&arg::<String>(&args,"name")?)?),
        "grant_user_endpoint"=>{
            if !arg::<bool>(&args,"confirmed")? {return Err(ServerError::BadRequest("Endpoint trust must be confirmed".into()));}
            let endpoint:String=arg(&args,"endpoint")?;
            if endpoint.len()>8192 || !(endpoint.starts_with("http://")||endpoint.starts_with("https://")) {return Err(ServerError::BadRequest("Invalid endpoint".into()));}
            s.user_endpoint_access_service.grant(endpoint).await;ok(())
        },
        "database_handle"=>ok(s.database_service.execute(arg(&args,"request")?).await?),
        "vector_handle"=>ok(s.vector_service.handle_request(&arg::<String>(&args,"path")?,arg(&args,"request")?).await?),
        "search_searxng"=>{let request=s.searxng_search_service.prepare_request(arg(&args,"dto")?)?;ensure_endpoint(state,Some(request.base_url.to_string())).await?;ok(s.searxng_search_service.search(request).await?)},
        "get_personas"=>ok(s.avatar_service.get_personas().await?),
        "list_sprites"=>ok(s.sprite_service.list(arg(&args,"dto")?).await?),
        "delete_sprite"=>ok(s.sprite_service.delete(arg(&args,"dto")?).await?),
        "upload_sprite"=>{let dto:tt_application::dto::sprite_dto::UploadSpriteDto=arg(&args,"dto")?;staged(state,&dto.file_path)?;ok(s.sprite_service.upload(dto).await?)},
        "upload_sprite_pack"=>{let dto:tt_application::dto::sprite_dto::UploadSpritePackDto=arg(&args,"dto")?;staged(state,&dto.file_path)?;ok(s.sprite_service.upload_pack(dto).await?)},
        "list_chat_backup_catalog"=>ok(s.chat_service.list_chat_backup_catalog().await?),
        "get_character_chat_integrity"=>ok(s.chat_service.get_character_chat_integrity(&arg::<String>(&args,"characterName")?,&arg::<String>(&args,"fileName")?).await?),
        "discover_skill_imports"=>{let input=arg(&args,"input")?;staged_skill_input(state,&input)?;ok(s.skill_service.discover_imports(input).await?)},
        "discard_skill_import_archive"=>{let path=arg::<String>(&args,"path")?;staged(state,&path)?;ok(s.skill_service.discard_import_archive(&path).await?)},
        "stage_file_from_url"=>{
            let upload=state.upload_staging.begin("remote","bin",0).await?;
            match s.content_service.download_external_file(&arg::<String>(&args,"url")?,std::path::Path::new(&upload.file_path)).await {
                Ok(file)=>ok(serde_json::json!({"file_path":upload.file_path,"mime_type":file.content_type})),
                Err(error)=>{let _=state.upload_staging.discard(&upload.file_path).await;return Err(error.into());}
            }
        }
        "read_agent_run_checkpoint"=>ok(s.agent_runtime_service.read_run_checkpoint(arg(&args,"dto")?).await?),
        "resume_agent_run"=>ok(s.agent_runtime_service.resume_run(arg(&args,"dto")?).await?),
        "finish_agent_run_presentation"=>ok(s.agent_runtime_service.finish_run_presentation(arg(&args,"dto")?).await?),
        "read_agent_task_detail"=>ok(s.agent_runtime_service.read_task_detail(arg(&args,"dto")?).await?),
        "list_active_agent_runs"=>ok(s.agent_runtime_service.list_active_runs().await),
        "load_agent_session_profile"=>ok(s.agent_runtime_service.load_session_profile().await?),
        "save_agent_session_profile"=>ok(s.agent_runtime_service.save_session_profile(arg(&args,"dto")?).await?),
        "create_agent_session"=>ok(s.agent_runtime_service.create_session().await?),
        "list_agent_sessions"=>ok(s.agent_runtime_service.list_sessions().await?),
        "rename_agent_session"=>ok(s.agent_runtime_service.rename_session(arg(&args,"dto")?).await?),
        "delete_agent_session"=>ok(s.agent_runtime_service.delete_session(arg(&args,"dto")?).await?),
        "read_agent_session"=>ok(s.agent_runtime_service.read_session(arg(&args,"dto")?).await?),
        "prepare_agent_session_run"=>ok(s.agent_runtime_service.prepare_session_run(arg(&args,"dto")?).await?),
        "start_agent_session_run"=>ok(s.agent_runtime_service.start_session_run(arg(&args,"dto")?).await?),
        "copy_agent_chat_persistent_states"=>{
            let dto:AgentCopyChatPersistentStatesDto=arg(&args,"dto")?;
            use tt_application::services::agent_workspace_lifecycle_service::AgentChatWorkspaceTarget;
            ok(s.chat_service.copy_agent_persistent_states(&AgentChatWorkspaceTarget{chat_ref:dto.source_chat_ref,stable_chat_id:dto.source_stable_chat_id},&AgentChatWorkspaceTarget{chat_ref:dto.target_chat_ref,stable_chat_id:dto.target_stable_chat_id}).await?)
        }
        "get_chat_completions_status"=>{let dto=arg(&args,"dto")?;ensure_endpoint(state,s.chat_completion_service.resolve_status_user_endpoint(&dto)?).await?;ok(s.chat_completion_service.get_status(dto).await?)},
        "generate_chat_completion"=>{let dto=arg(&args,"dto")?;ensure_endpoint(state,s.chat_completion_service.resolve_generate_user_endpoint(&dto)?).await?;ok(s.chat_completion_service.generate_request(&stream_id(&args,"requestId")?,dto).await?)},
        "start_chat_completion_stream"=>{let dto=arg(&args,"dto")?;ensure_endpoint(state,s.chat_completion_service.resolve_generate_user_endpoint(&dto)?).await?;ok(s.chat_completion_service.clone().start_stream(stream_id(&args,"streamId")?,dto).await?)},
        "read_chat_completion_stream"=>ok(s.chat_completion_service.read_stream(&stream_id(&args,"streamId")?,arg(&args,"afterSeq")?).await?),
        "close_chat_completion_stream"=>{s.chat_completion_service.close_stream(&stream_id(&args,"streamId")?).await;ok(())},
        "cancel_chat_completion_generation"=>{s.chat_completion_service.cancel_generation(&stream_id(&args,"requestId")?).await;ok(())},
        "list_mcp_servers"=>ok(s.mcp_service.list_servers().await?),
        "create_mcp_server"=>{let dto:CreateMcpServerDto=arg(&args,"dto")?;ok(s.mcp_service.create_server(dto.display_name,dto.endpoint,dto.headers,dto.protocol_version).await?)},
        "update_mcp_server"=>{let dto:UpdateMcpServerDto=arg(&args,"dto")?;ok(s.mcp_service.update_server(&dto.registration_id,dto.display_name,dto.endpoint,dto.headers,dto.protocol_version).await?)},
        "set_mcp_server_state"=>{let dto:SetMcpServerStateDto=arg(&args,"dto")?;ok(s.mcp_service.set_server_state(&dto.registration_id,dto.state).await?)},
        "remove_mcp_server"=>{let dto:McpRegistrationIdDto=arg(&args,"dto")?;ok(s.mcp_service.remove_server(&dto.registration_id).await?)},
        "discover_mcp_tools"=>{let dto:McpRegistrationIdDto=arg(&args,"dto")?;ok(s.mcp_service.discover_tools(&dto.registration_id).await?)},
        "refresh_mcp_tools"=>{let dto:McpRegistrationIdDto=arg(&args,"dto")?;ok(s.mcp_service.refresh_tools(&dto.registration_id).await?)},
        "set_mcp_tool_permission"=>{let dto:SetMcpToolPermissionDto=arg(&args,"dto")?;ok(s.mcp_service.set_tool_permission(&dto.registration_id,dto.native_name,dto.permission).await?)},
        "set_mcp_tool_description_override"=>{let dto:SetMcpToolDescriptionOverrideDto=arg(&args,"dto")?;ok(s.mcp_service.set_tool_description_override(&dto.registration_id,dto.native_name,dto.override_).await?)},
        "start_mcp_test_call"=>{let dto:McpTestCallIdDto=arg(&args,"dto")?;ok(s.mcp_service.start_call(&dto.call_id).await?)},
        "test_mcp_tool_call"=>{let dto:TestMcpToolCallDto=arg(&args,"dto")?;ok(s.mcp_service.test_call(&dto.call_id,&dto.registration_id,dto.native_name,dto.arguments_json).await?)},
        "cancel_mcp_test_call"=>{let dto:McpTestCallIdDto=arg(&args,"dto")?;ok(s.mcp_service.cancel_call(&dto.call_id).await?)},
        "list_legacy_mcp_tools"=>ok(s.mcp_service.list_legacy_tools_cached().await?),
        "start_legacy_mcp_tool_call"=>{let dto:McpExecutionCallIdDto=arg(&args,"dto")?;ok(s.mcp_service.start_call(&dto.execution_call_id).await?)},
        "call_legacy_mcp_tool"=>{let dto:CallLegacyMcpToolDto=arg(&args,"dto")?;ok(s.mcp_service.call_legacy_tool(&dto.execution_call_id,&dto.tool_id,dto.arguments_json).await?)},
        "cancel_legacy_mcp_tool_call"=>{let dto:McpExecutionCallIdDto=arg(&args,"dto")?;ok(s.mcp_service.cancel_call(&dto.execution_call_id).await?)},
        "subscribe_agent_run_live_projection"|"register_agent_extension_tool"|"set_agent_extension_tool_enabled"|"resolve_agent_extension_tool_call"=>return Err(ServerError::BadRequest("Agent live projection and browser extension tools require a native host; HTTP channel transport is unavailable".into())),
        _=>return Ok(None),
    }?;
    Ok(Some(value))
}
fn stream_id(args:&Value,key:&str)->Result<String,ServerError> {
    let id:String=arg(args,key)?;
    if id.is_empty()||id.len()>128||!id.bytes().all(|c|c.is_ascii_alphanumeric()||c==b'-'||c==b'_'){return Err(ServerError::BadRequest("Invalid completion id".into()));}
    Ok(id)
}

pub(crate) async fn ensure_endpoint(state:&SharedState,endpoint:Option<String>)->Result<(),ServerError>{
    if let Some(endpoint)=endpoint {
        if !state.services.user_endpoint_access_service.is_granted(&endpoint).await {
            return Err(ServerError::EndpointAuthorizationRequired(endpoint));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn frontend_template_commands_load_html_and_reject_path_escape() {
        let root=std::env::temp_dir().join(format!("tt-template-rpc-{}",rand::random::<u64>()));
        let frontend=root.join("frontend");
        tokio::fs::create_dir_all(frontend.join("scripts/templates")).await.unwrap();
        tokio::fs::create_dir_all(frontend.join("scripts/extensions/regex")).await.unwrap();
        tokio::fs::write(frontend.join("scripts/templates/help.html"),"<section>Help</section>").await.unwrap();
        tokio::fs::write(frontend.join("scripts/extensions/regex/dropdown.html"),"<button>Regex</button>").await.unwrap();
        let services=crate::composition::build(&root,std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.."),Default::default(),crate::product::USER_AGENT).await.unwrap();
        let state=std::sync::Arc::new(crate::state::AppState {
            services,auth:crate::auth::Auth::new(None),frontend_dir:frontend.clone(),csrf_token:"test".into(),
            upload_staging:std::sync::Arc::new(crate::upload::UploadStaging::new(&root)),browser_resources:Default::default(),templates:Default::default(),
        });
        assert_eq!(dispatch(&state,"read_frontend_template",serde_json::json!({"name":"help.html"})).await.unwrap().unwrap(),"<section>Help</section>");
        assert_eq!(dispatch(&state,"read_frontend_extension_template",serde_json::json!({"extension":"regex","name":"dropdown"})).await.unwrap().unwrap(),"<button>Regex</button>");
        for name in ["../outside.html","/etc/passwd","..\\outside.html",""] {
            assert!(matches!(dispatch(&state,"read_frontend_template",serde_json::json!({"name":name})).await,Some(Err(ServerError::BadRequest(_)))));
        }
        assert!(matches!(dispatch(&state,"read_frontend_extension_template",serde_json::json!({"extension":"../regex","name":"dropdown"})).await,Some(Err(ServerError::BadRequest(_)))));
        #[cfg(unix)] {
            tokio::fs::write(root.join("outside.html"),"private").await.unwrap();
            std::os::unix::fs::symlink(root.join("outside.html"),frontend.join("scripts/templates/escape.html")).unwrap();
            assert!(dispatch(&state,"read_frontend_template",serde_json::json!({"name":"escape.html"})).await.unwrap().is_err());
        }
        drop(state);tokio::fs::remove_dir_all(root).await.unwrap();
    }
}
