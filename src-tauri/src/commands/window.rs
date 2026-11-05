use tauri::{AppHandle, LogicalSize, Manager, Size};
use tauri_plugin_store::StoreBuilder;

use crate::window::{
    capture_mini_window_state, hide_mini_window, position_mini_window, DEFAULT_MINI_HEIGHT,
    DEFAULT_MINI_WIDTH,
};

/// Exit the application completely
#[tauri::command]
pub fn exit_app(app_handle: AppHandle) {
    #[cfg(windows)]
    crate::mcp::lifecycle::quit(app_handle);
    #[cfg(not(windows))]
    app_handle.exit(0);
}

/// Explicitly close/hide the mini window from frontend.
/// The size/position chosen by the user is preserved across hide/show cycles.
#[tauri::command]
pub fn close_mini_window(app_handle: AppHandle) {
    hide_mini_window(&app_handle);
    tracing::debug!("Mini window hidden");
}

/// Persist the current mini-window geometry to the settings store.
/// The Moved/Resized listener already updates the in-memory cache on every
/// event; this command flushes the store to disk explicitly.
#[tauri::command]
pub fn save_mini_window_geometry(app_handle: AppHandle) -> Result<(), String> {
    let mini_window = app_handle
        .get_webview_window("mini")
        .ok_or_else(|| "Mini window not found".to_string())?;

    let settings_path = crate::profiles::settings_store_path(&app_handle);
    let store = StoreBuilder::new(&app_handle, settings_path)
        .build()
        .map_err(|e| format!("Failed to load settings store: {}", e))?;

    capture_mini_window_state(&mini_window, &store);
    store
        .save()
        .map_err(|e| format!("Failed to save settings store: {}", e))?;

    Ok(())
}

/// Reset the mini-window to its default size and re-center it above the
/// taskbar (using the monitor work area).
#[tauri::command]
pub fn recenter_mini_window(app_handle: AppHandle) -> Result<(), String> {
    let mini_window = app_handle
        .get_webview_window("mini")
        .ok_or_else(|| "Mini window not found".to_string())?;

    mini_window
        .set_size(Size::Logical(LogicalSize {
            width: DEFAULT_MINI_WIDTH,
            height: DEFAULT_MINI_HEIGHT,
        }))
        .map_err(|e| format!("Failed to resize mini window: {}", e))?;

    position_mini_window(&app_handle, &mini_window);

    let settings_path = crate::profiles::settings_store_path(&app_handle);
    let store = StoreBuilder::new(&app_handle, settings_path)
        .build()
        .map_err(|e| format!("Failed to load settings store: {}", e))?;

    capture_mini_window_state(&mini_window, &store);
    store
        .save()
        .map_err(|e| format!("Failed to save settings store: {}", e))?;

    tracing::info!("Mini window recentered");
    Ok(())
}

/// Open (or focus) the detached window for a note. `at_cursor: true` places
/// the window at the OS cursor position (drag-out drop).
/// MUST stay `async`: on Windows, creating a webview from a synchronous
/// command deadlocks the main thread (WebView2 known issue — see
/// `WebviewWindowBuilder::new` docs), freezing all further IPC.
#[tauri::command]
pub async fn open_note_window(
    app_handle: AppHandle,
    note_id: String,
    at_cursor: Option<bool>,
) -> Result<(), String> {
    if !crate::notes::is_valid_note_id(&note_id) {
        return Err("Invalid note id".to_string());
    }
    let profile =
        crate::note_service::ProfileScope::active(&app_handle).map_err(|e| e.to_string())?;
    profile
        .transaction(|p| {
            if !p.is_active()? {
                return Err(crate::note_service::DomainError::EditorSessionExpired);
            }
            crate::note_service::NoteRepository::get_locked(p, &note_id)?;
            let registry = crate::note_service::editors::editors();
            registry.bind_window(
                &crate::window::note_window_label(&note_id),
                registry.session(p.scope().local_id()),
            )
        })
        .map_err(|e| e.to_string())?;
    crate::window::open_note_window(&app_handle, &note_id, at_cursor.unwrap_or(false))?;
    tracing::info!("Note window opened for {}", note_id);
    Ok(())
}

/// Close the detached window for a note (delete flow, explicit reattach).
#[tauri::command]
pub fn close_note_window(app_handle: AppHandle, note_id: String) -> Result<(), String> {
    if !crate::notes::is_valid_note_id(&note_id) {
        return Err("Invalid note id".to_string());
    }
    let label = crate::window::note_window_label(&note_id);
    if let Some(window) = app_handle.get_webview_window(&label) {
        window
            .close()
            .map_err(|e| format!("Failed to close note window: {e}"))?;
    }
    Ok(())
}

/// Show + focus the main window (explicit reattach, wiki-link opened from a
/// detached window).
#[tauri::command]
pub fn show_main_window(app_handle: AppHandle) {
    if let Some(main) = app_handle.get_webview_window("main") {
        let _ = main.show();
        let _ = main.unminimize();
        let _ = main.set_focus();
    }
}

/// Close every detached note window (profile switch).
pub(crate) fn close_all_note_windows(app_handle: &AppHandle) {
    for (label, window) in app_handle.webview_windows() {
        if label.starts_with("note-") {
            // Profile switch already committed a clean-editor barrier and
            // invalidated old sessions; do not ask an expired renderer to save.
            let _ = window.destroy();
        }
    }
}
