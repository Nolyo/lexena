use tauri::{AppHandle, Manager};

use crate::profiles::{load_manifest, ProfileMeta};

/// List all profiles
#[tauri::command]
pub fn list_profiles(app: AppHandle) -> Result<Vec<ProfileMeta>, String> {
    let manifest = load_manifest(&app).map_err(|e| e.to_string())?;
    Ok(manifest.profiles)
}

/// Get the active profile id
#[tauri::command]
pub fn get_active_profile(app: AppHandle) -> Result<String, String> {
    Ok(crate::profiles::get_active_id(&app))
}

/// Get the settings store path for the active profile (for frontend Store.load)
#[tauri::command]
pub fn get_active_profile_settings_path(app: AppHandle) -> Result<String, String> {
    Ok(crate::profiles::settings_store_path(&app))
}

/// Get the notes-tabs store path for the active profile (for frontend Store.load)
#[tauri::command]
pub fn get_active_profile_notes_tabs_path(app: AppHandle) -> Result<String, String> {
    Ok(crate::profiles::notes_tabs_store_path(&app))
}

/// Get the notes-sidebar store path for the active profile (for frontend Store.load)
#[tauri::command]
pub fn get_active_profile_notes_sidebar_path(app: AppHandle) -> Result<String, String> {
    Ok(crate::profiles::notes_sidebar_store_path(&app))
}

/// Get the sync-meta store path for the active profile (for frontend Store.load)
#[tauri::command]
pub fn get_active_profile_sync_meta_path(app: AppHandle) -> Result<String, String> {
    Ok(crate::profiles::sync_meta_store_path(&app))
}

/// Get the sync-meta store path for an ARBITRARY profile (for frontend Store.load).
/// Used by multi-device import to read/write the cloud_profile_id binding of a
/// profile other than the active one. Validates the profile exists in the
/// manifest so callers can't address a path outside the known profiles.
#[tauri::command]
pub fn get_profile_sync_meta_path(app: AppHandle, id: String) -> Result<String, String> {
    let _scope =
        crate::note_service::ProfileScope::resolve(&app, &id).map_err(|e| e.to_string())?;
    Ok(crate::profiles::profile_store_path(&id, "sync-meta.json"))
}

/// Get the sync-queue store path for the active profile (for frontend Store.load)
#[tauri::command]
pub fn get_active_profile_sync_queue_path(app: AppHandle) -> Result<String, String> {
    Ok(crate::profiles::sync_queue_store_path(&app))
}

/// Get the snippets store path for the active profile (for frontend Store.load)
#[tauri::command]
pub fn get_active_profile_snippets_path(app: AppHandle) -> Result<String, String> {
    Ok(crate::profiles::snippets_store_path(&app))
}

/// Get the dictionary store path for the active profile (for frontend Store.load)
#[tauri::command]
pub fn get_active_profile_dictionary_path(app: AppHandle) -> Result<String, String> {
    Ok(crate::profiles::dictionary_store_path(&app))
}

/// Create a new profile (does NOT switch to it)
#[tauri::command]
pub fn create_profile(app: AppHandle, name: String) -> Result<ProfileMeta, String> {
    let app_data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    crate::note_service::ProfileRegistry::create_in(&app_data, &name).map_err(|e| e.to_string())
}

/// Rename a profile (changes display name only, never renames the directory)
#[tauri::command]
pub fn rename_profile(app: AppHandle, id: String, new_name: String) -> Result<(), String> {
    let app_data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    crate::note_service::ProfileRegistry::rename_in(&app_data, &id, &new_name)
        .map_err(|e| e.to_string())
}

/// Delete a profile (cannot delete the active profile or the last one)
#[tauri::command]
pub fn delete_profile(app: AppHandle, id: String) -> Result<(), String> {
    let app_data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    #[cfg(windows)]
    let mcp = app.try_state::<crate::commands::mcp::LocalMcpState>();
    #[cfg(windows)]
    let _grant_change = mcp
        .as_ref()
        .map(|s| s.profile_changes.lock())
        .transpose()
        .map_err(|_| "Profile permission lock unavailable".to_string())?;
    let result =
        crate::note_service::ProfileRegistry::delete_in(&app_data, &id).map_err(|e| e.to_string());
    #[cfg(windows)]
    if let Some(mcp) = mcp.as_ref() {
        // Directory removal may fail after the logical deletion was persisted.
        if load_manifest(&app).is_ok_and(|m| !m.profiles.iter().any(|p| p.id == id)) {
            mcp.grants.remove_profile(&id).map_err(|e| e.to_string())?;
        }
    }
    result
}

/// Switch to a different profile.
/// Updates the manifest and AppState in place, re-registers hotkeys,
/// then reloads all WebView windows — works in both dev and production.
#[tauri::command]
pub fn switch_profile(app: AppHandle, id: String) -> Result<(), String> {
    use tauri_plugin_store::StoreBuilder;

    let app_data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    // Capture + switch the registry under the shared lifecycle lock. An existing
    // note transaction retains its captured profile; no temporary UI switch.
    let state: tauri::State<crate::state::AppState> = app.state();
    crate::note_service::ProfileRegistry::switch_with_preflight_in(
        &app_data,
        &id,
        |_| {
            let active = state.active_profile_id.lock().map_err(|_| {
                crate::note_service::DomainError::Storage("Profile state lock unavailable".into())
            })?;
            let editors = crate::note_service::editors::editors().prepare_profile_switch()?;
            Ok((active, editors))
        },
        |_, (mut active, editors)| {
            editors.commit();
            *active = id.clone();
        },
    )
    .map_err(|e| e.to_string())?;
    crate::commands::window::close_all_note_windows(&app);

    // Re-register hotkeys from the new profile's settings
    {
        let settings_path = crate::profiles::settings_store_path(&app);
        if let Ok(store) = StoreBuilder::new(&app, settings_path).build() {
            let new_hotkeys = crate::hotkeys::load_hotkey_config(&store);
            let state: tauri::State<crate::state::AppState> = app.state();
            let current = state.hotkeys.lock().map(|h| h.clone()).unwrap_or_default();
            if let Err(e) = crate::hotkeys::apply_hotkeys(&app, &new_hotkeys) {
                tracing::warn!("Failed to apply hotkeys for new profile: {}", e);
                let _ = crate::hotkeys::apply_hotkeys(&app, &current);
            } else if let Ok(mut guard) = state.hotkeys.lock() {
                *guard = new_hotkeys;
            };
        }
    }

    tracing::info!("Switched to profile: {}", id);

    // Reload all WebView windows — the frontend re-initialises from scratch
    // (SettingsContext re-fetches the store path, notes/transcriptions reload, etc.)
    for (label, window) in app.webview_windows() {
        if let Err(e) = window.eval("window.location.reload()") {
            tracing::warn!("Failed to reload window '{}': {}", label, e);
        }
    }

    Ok(())
}

/// Set a profile's avatar from a PNG data-URL (validates profile + payload)
#[tauri::command]
pub fn set_profile_avatar(app: AppHandle, id: String, data_url: String) -> Result<(), String> {
    let bytes = crate::profiles::decode_avatar_data_url(&data_url)?;
    let scope = crate::note_service::ProfileScope::resolve(&app, &id).map_err(|e| e.to_string())?;
    scope
        .transaction(|profile| {
            profile.path(std::path::Path::new("avatar.png"))?;
            crate::profiles::write_avatar_in(scope.root(), &bytes)
                .map_err(crate::note_service::DomainError::from)
        })
        .map_err(|e| e.to_string())
}

/// Get a profile's avatar as a PNG data-URL, or None if absent
#[tauri::command]
pub fn get_profile_avatar(app: AppHandle, id: String) -> Result<Option<String>, String> {
    let scope = crate::note_service::ProfileScope::resolve(&app, &id).map_err(|e| e.to_string())?;
    scope
        .transaction(|profile| {
            profile.path(std::path::Path::new("avatar.png"))?;
            Ok(crate::profiles::read_avatar_data_url_in(scope.root()))
        })
        .map_err(|e| e.to_string())
}

/// Remove a profile's avatar (no-op if absent)
#[tauri::command]
pub fn clear_profile_avatar(app: AppHandle, id: String) -> Result<(), String> {
    let scope = crate::note_service::ProfileScope::resolve(&app, &id).map_err(|e| e.to_string())?;
    scope
        .transaction(|profile| {
            profile.path(std::path::Path::new("avatar.png"))?;
            crate::profiles::clear_avatar_in(scope.root())
                .map_err(crate::note_service::DomainError::from)
        })
        .map_err(|e| e.to_string())
}
