use crate::note_service::ProfileRegistry;
use std::thread;
use std::time::Duration;
use tauri::{AppHandle, Manager};

pub const RESET_CONFIRMATION_PHRASE: &str = "EFFACER TOUTES MES DONNÉES";

/// Wipe every entry under the application's app_data_dir, then restart the
/// process. The frontend must collect a typed confirmation phrase from the
/// user; the same phrase is verified server-side as defense in depth.
#[tauri::command]
pub async fn reset_app_data(app_handle: AppHandle, confirmation: String) -> Result<(), String> {
    if confirmation != RESET_CONFIRMATION_PHRASE {
        tracing::warn!("reset_app_data refused: confirmation phrase mismatch");
        return Err("Confirmation phrase mismatch".to_string());
    }

    let app_data = app_handle
        .path()
        .app_data_dir()
        .map_err(|e| format!("Could not resolve app data directory: {}", e))?;

    let app_data =
        ProfileRegistry::checked_reset_target_in(&app_data).map_err(|e| e.to_string())?;
    tracing::warn!("Resetting all app data at {}", app_data.display());

    // First close MCP admission, drain already-started disk jobs, and stop the
    // listener. No registry/editor guard may cross this await.
    #[cfg(windows)]
    crate::mcp::lifecycle::stop(&app_handle)
        .await
        .map_err(|e| {
            format!(
                "Reset stopped before deletion: {e}. Restart the application before continuing."
            )
        })?;
    // Keep MCP and native profile work disabled even if deletion fails. Never
    // resume services over a partly erased profile; recovery is a process restart.
    tauri::async_runtime::spawn_blocking(move || {
        ProfileRegistry::reset_app_data_in(&app_data, crate::note_service::editors::editors())
    })
    .await
    .map_err(|e| format!("Reset failed: {e}. Restart the application before continuing."))?
    .map_err(|e| format!("Reset failed: {e}. Restart the application before continuing."))?;

    tracing::info!("App data wiped, scheduling restart");

    // Give the frontend a moment to acknowledge the IPC reply before we
    // tear the process down. Restart from a background thread so the
    // current command can return cleanly first.
    let handle = app_handle.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(200));
        handle.restart();
    });

    Ok(())
}
