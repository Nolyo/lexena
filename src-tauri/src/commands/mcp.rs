//! Local UI administration. Every custom command checks its calling webview.
use crate::mcp::{
    self,
    audit::{AuditEntry, AuditLog},
    connections::{ConnectionGrant, GrantStore, Scope},
    LocalMcpRuntime, SessionHandler,
};
use lexena_mcp_ipc::{Error, Result};
use serde::Serialize;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tauri::{AppHandle, State, WebviewWindow};
use uuid::Uuid;

pub struct LocalMcpState {
    /// Serialize grant validation/creation with profile deletion and grant pruning.
    pub profile_changes: Mutex<()>,
    pub grants: Arc<GrantStore>,
    pub audit: AuditLog,
    pub app_data: PathBuf,
    pub runtime: tokio::sync::Mutex<Option<LocalMcpRuntime>>,
    handler: Mutex<Option<SessionHandler>>,
}
impl LocalMcpState {
    pub fn new(app_data: PathBuf, server_path: &std::path::Path) -> Result<Self> {
        Ok(Self {
            profile_changes: Mutex::new(()),
            grants: GrantStore::load_for_ui(&app_data, server_path)?,
            audit: AuditLog::new(&app_data),
            app_data,
            runtime: tokio::sync::Mutex::new(None),
            handler: Mutex::new(None),
        })
    }
    /// Set only from Rust setup after the real service and recovery gates exist.
    pub fn set_handler(&self, handler: SessionHandler) -> Result<()> {
        *self.handler.lock().map_err(|_| Error::Denied)? = Some(handler);
        Ok(())
    }
    pub async fn start_if_enabled(&self) -> Result<()> {
        if !self.grants.snapshot()?.enabled {
            return Ok(());
        }
        let handler = self
            .handler
            .lock()
            .map_err(|_| Error::Denied)?
            .clone()
            .ok_or(Error::Denied)?;
        let mut runtime = self.runtime.lock().await;
        if runtime.as_ref().is_some_and(|runtime| runtime.is_running()) {
            return Ok(());
        }
        if let Some(previous) = runtime.take() {
            previous.stop().await?;
        }
        *runtime = Some(LocalMcpRuntime::start(
            &self.app_data,
            self.grants.clone(),
            handler,
        )?);
        Ok(())
    }
    pub async fn stop(&self) -> Result<()> {
        if let Some(runtime) = self.runtime.lock().await.take() {
            runtime.stop().await?;
        }
        Ok(())
    }
}
fn ensure_admin(window: &WebviewWindow) -> Result<()> {
    let url = window.url().map_err(|_| Error::Denied)?;
    if !mcp::permissions::admin_caller_allowed(
        window.label(),
        url.scheme(),
        url.host_str().unwrap_or(""),
        url.port(),
        cfg!(debug_assertions),
    ) {
        return Err(Error::Denied);
    }
    Ok(())
}
fn validate_profiles(
    app: &AppHandle,
    ids: &[String],
) -> Result<Vec<mcp::permissions::ProfileIdentity>> {
    let manifest = crate::profiles::load_manifest(app).map_err(|_| Error::Denied)?;
    if ids
        .iter()
        .all(|id| crate::profiles::profile_exists(&manifest, id))
    {
        ids.iter()
            .map(|id| {
                manifest
                    .profiles
                    .iter()
                    .find(|p| &p.id == id)
                    .map(|p| mcp::permissions::ProfileIdentity {
                        id: p.id.clone(),
                        created_at: p.created_at.clone(),
                    })
                    .ok_or(Error::Denied)
            })
            .collect()
    } else {
        Err(Error::Denied)
    }
}
#[derive(Serialize)]
pub struct McpStatus {
    pub initialization_error: Option<String>,
    pub enabled: bool,
    pub runtime_running: bool,
    pub connections: Vec<ConnectionGrant>,
}
#[tauri::command]
pub async fn mcp_get_status(
    window: WebviewWindow,
    state: State<'_, LocalMcpState>,
) -> std::result::Result<McpStatus, String> {
    ensure_admin(&window).map_err(|e| e.to_string())?;
    let config = state.grants.snapshot().map_err(|e| e.to_string())?;
    Ok(McpStatus {
        initialization_error: state.grants.initialization_error(),
        enabled: config.enabled,
        runtime_running: state
            .runtime
            .lock()
            .await
            .as_ref()
            .is_some_and(|r| r.is_running()),
        connections: config.connections,
    })
}

#[tauri::command]
pub async fn mcp_reset_installation(
    window: WebviewWindow,
    state: State<'_, LocalMcpState>,
) -> std::result::Result<(), String> {
    ensure_admin(&window).map_err(|e| e.to_string())?;
    state.stop().await.map_err(|e| e.to_string())?;
    state.grants.reset_installation().map_err(|e| e.to_string())
}
#[tauri::command]
pub async fn mcp_set_enabled(
    window: WebviewWindow,
    state: State<'_, LocalMcpState>,
    enabled: bool,
) -> std::result::Result<(), String> {
    ensure_admin(&window).map_err(|e| e.to_string())?;
    state
        .grants
        .set_enabled(enabled)
        .map_err(|e| e.to_string())?;
    if enabled {
        if let Err(error) = state.start_if_enabled().await {
            let _ = state.grants.set_enabled(false);
            let _ = state.stop().await;
            return Err(error.to_string());
        }
    } else {
        state.stop().await.map_err(|e| e.to_string())?;
    }
    Ok(())
}
#[tauri::command]
pub fn mcp_create_connection(
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, LocalMcpState>,
    name: String,
    profile_ids: Vec<String>,
    scopes: Vec<Scope>,
) -> std::result::Result<ConnectionGrant, String> {
    let _profiles = state
        .profile_changes
        .lock()
        .map_err(|_| Error::Denied.to_string())?;
    ensure_admin(&window)
        .and_then(|_| validate_profiles(&app, &profile_ids))
        .and_then(|profiles| state.grants.create(name, profiles, scopes))
        .map_err(|e| e.to_string())
}
#[tauri::command]
pub fn mcp_revoke_connection(
    window: WebviewWindow,
    state: State<'_, LocalMcpState>,
    connection_id: String,
) -> std::result::Result<(), String> {
    ensure_admin(&window)
        .and_then(|_| Uuid::parse_str(&connection_id).map_err(|_| Error::Denied))
        .and_then(|id| state.grants.revoke(id))
        .map_err(|e| e.to_string())
}
#[tauri::command]
pub fn mcp_rotate_connection(
    window: WebviewWindow,
    state: State<'_, LocalMcpState>,
    connection_id: String,
) -> std::result::Result<(), String> {
    ensure_admin(&window)
        .and_then(|_| Uuid::parse_str(&connection_id).map_err(|_| Error::Denied))
        .and_then(|id| state.grants.rotate(id))
        .map_err(|e| e.to_string())
}
#[tauri::command]
pub fn mcp_get_client_config(
    window: WebviewWindow,
    state: State<'_, LocalMcpState>,
    connection_id: String,
) -> std::result::Result<serde_json::Value, String> {
    let config = || -> Result<serde_json::Value> {
        ensure_admin(&window)?;
        let id = Uuid::parse_str(&connection_id).map_err(|_| Error::Denied)?;
        let grant = state.grants.grant(id)?;
        let bridge = state
            .grants
            .snapshot()?
            .server_path
            .parent()
            .ok_or(Error::Installation)?
            .join("lexena-mcp.exe")
            .canonicalize()?;
        Ok(
            serde_json::json!({"mcpServers":{(grant.name):{"command":bridge,"args":["--connection",id.to_string()]}}}),
        )
    };
    config().map_err(|e| e.to_string())
}
#[tauri::command]
pub fn mcp_get_audit(
    window: WebviewWindow,
    state: State<'_, LocalMcpState>,
) -> std::result::Result<Vec<AuditEntry>, String> {
    ensure_admin(&window)
        .and_then(|_| state.audit.list())
        .map_err(|e| e.to_string())
}
#[tauri::command]
pub async fn mcp_test_connection(
    window: WebviewWindow,
    state: State<'_, LocalMcpState>,
    connection_id: String,
) -> std::result::Result<(), String> {
    let test = async {
        ensure_admin(&window)?;
        let id = Uuid::parse_str(&connection_id).map_err(|_| Error::Denied)?;
        let config = state.grants.snapshot()?;
        let descriptor =
            lexena_mcp_ipc::EndpointDescriptor::read(&state.app_data.join("mcp/endpoint.json"))?;
        let bridge = config
            .server_path
            .parent()
            .ok_or(Error::Installation)?
            .join("lexena-mcp.exe");
        let mut pipe = lexena_mcp_ipc::windows::connect(&descriptor, &bridge).await?;
        lexena_mcp_ipc::authenticate_client(
            &mut pipe,
            config.installation_id,
            id,
            &state.grants.secret(id)?,
        )
        .await
    };
    test.await.map_err(|e| e.to_string())
}
