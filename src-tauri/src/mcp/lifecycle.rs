use super::server::McpServer;
use crate::commands::mcp::LocalMcpState;
use std::sync::Arc;
use tauri::{AppHandle, Manager};

pub async fn stop(app: &AppHandle) -> Result<(), String> {
    if let Some(server) = app.try_state::<Arc<McpServer>>() {
        server.shutdown().await;
    }
    if let Some(local) = app.try_state::<LocalMcpState>() {
        local.stop().await.map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub async fn prepare_update(app: &AppHandle) -> Result<(), String> {
    stop(app).await?;
    let path = std::env::current_exe()
        .map_err(|_| "MCP_BRIDGE_PATH_UNAVAILABLE")?
        .with_file_name("lexena-mcp.exe");
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let image = path.clone();
        let running =
            tokio::task::spawn_blocking(move || super::processes::bridge_is_running(&image))
                .await
                .map_err(|_| "MCP_BRIDGE_CHECK_FAILED")??;
        if !running {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("MCP_BRIDGE_BUSY: fermez la connexion Lexena dans votre client IA puis relancez la mise à jour.".into());
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

pub async fn resume(app: &AppHandle) {
    if let Some(server) = app.try_state::<Arc<McpServer>>() {
        if server.resume_after_maintenance().is_err() {
            return;
        }
    }
    if let Some(local) = app.try_state::<LocalMcpState>() {
        if let Err(error) = local.start_if_enabled().await {
            tracing::warn!("Local MCP restart failed: {}", error);
        }
    }
}

pub fn quit(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        if let Err(error) = stop(&app).await {
            tracing::warn!("Local MCP shutdown failed: {}", error);
        }
        app.exit(0);
    });
}
