#[allow(unused_imports)] // Transitional public compatibility for existing callers.
pub use crate::note_service::scope::{
    generate_unique_id, name_to_id, profile_exists, validate_profile_name, ProfileMeta,
    ProfilesManifest,
};
use anyhow::{Context, Result};
use chrono::Utc;
use std::fs;
use std::path::PathBuf;
use tauri::{AppHandle, Manager, State};

use crate::state::AppState;

fn get_manifest_path(app: &AppHandle) -> Result<PathBuf> {
    let app_data = app
        .path()
        .app_data_dir()
        .context("Could not resolve app data directory")?;
    Ok(app_data.join("profiles.json"))
}

/// Get the directory for a specific profile
pub fn get_profile_dir(app: &AppHandle, profile_id: &str) -> Result<PathBuf> {
    let app_data = app
        .path()
        .app_data_dir()
        .context("Could not resolve app data directory")?;
    crate::note_service::scope::checked_profile_root_in(&app_data, profile_id).map_err(Into::into)
}

/// Get the directory for the currently active profile
pub fn get_active_profile_dir(app: &AppHandle) -> Result<PathBuf> {
    let id = get_active_id(app);
    get_profile_dir(app, &id)
}

/// Get the active profile id from AppState
pub fn get_active_id(app: &AppHandle) -> String {
    let state: State<AppState> = app.state();
    state
        .active_profile_id
        .lock()
        .map(|g| g.clone())
        .unwrap_or_else(|_| "default".to_string())
}

/// Build a per-profile store path relative to app_data_dir. Pure (testable).
pub fn profile_store_path(id: &str, filename: &str) -> String {
    format!("profiles/{}/{}", id, filename)
}

/// Return the sync-meta store path for the active profile (relative to app_data_dir)
pub fn sync_meta_store_path(app: &AppHandle) -> String {
    profile_store_path(&get_active_id(app), "sync-meta.json")
}

/// Return the sync-queue store path for the active profile (relative to app_data_dir)
pub fn sync_queue_store_path(app: &AppHandle) -> String {
    profile_store_path(&get_active_id(app), "sync-queue.json")
}

/// Return the settings store path for the active profile (relative to app_data_dir)
pub fn settings_store_path(app: &AppHandle) -> String {
    let id = get_active_id(app);
    format!("profiles/{}/settings.json", id)
}

/// Return the notes-tabs store path for the active profile (relative to app_data_dir)
pub fn notes_tabs_store_path(app: &AppHandle) -> String {
    let id = get_active_id(app);
    format!("profiles/{}/notes-tabs.json", id)
}

/// Return the notes-sidebar store path for the active profile (relative to app_data_dir)
pub fn notes_sidebar_store_path(app: &AppHandle) -> String {
    let id = get_active_id(app);
    format!("profiles/{}/notes-sidebar.json", id)
}

/// Return the snippets store path for the active profile (relative to app_data_dir)
pub fn snippets_store_path(app: &AppHandle) -> String {
    profile_store_path(&get_active_id(app), "sync-snippets.json")
}

/// Return the dictionary store path for the active profile (relative to app_data_dir)
pub fn dictionary_store_path(app: &AppHandle) -> String {
    profile_store_path(&get_active_id(app), "sync-dictionary.json")
}

pub fn load_manifest(app: &AppHandle) -> Result<ProfilesManifest> {
    let path = get_manifest_path(app)?;
    crate::note_service::ProfileRegistry::manifest_in(path.parent().expect("manifest parent"))
        .map_err(Into::into)
}

/// First-time migration: if profiles.json doesn't exist, move existing data
/// (settings.json, notes/, transcriptions/, recordings/) into profiles/default/.
pub fn migrate_legacy_to_default(app: &AppHandle) -> Result<()> {
    let _registry = crate::note_service::scope::registry_write();
    let app_data = app
        .path()
        .app_data_dir()
        .context("Could not resolve app data directory")?;

    fs::create_dir_all(&app_data)?;
    let app_data = fs::canonicalize(&app_data)?;
    let manifest_path = crate::note_service::scope::checked_relative(
        &app_data,
        std::path::Path::new("profiles.json"),
    )?;
    if manifest_path.exists() {
        return Ok(()); // Already migrated
    }

    tracing::info!("Profiles migration: moving existing data into profiles/default/");

    let default_dir = crate::note_service::scope::checked_profile_root_in(&app_data, "default")?;
    fs::create_dir_all(&default_dir).context("Failed to create profiles/default directory")?;

    let items = [
        ("settings.json", "settings.json"),
        ("notes", "notes"),
        ("transcriptions", "transcriptions"),
        ("recordings", "recordings"),
    ];

    for (src_name, dst_name) in &items {
        let src = crate::note_service::scope::checked_relative(
            &app_data,
            std::path::Path::new(src_name),
        )?;
        let dst = crate::note_service::scope::checked_relative(
            &default_dir,
            std::path::Path::new(dst_name),
        )?;
        if src.exists() {
            match fs::rename(&src, &dst) {
                Ok(_) => tracing::info!(
                    "Profiles migration: moved {} -> profiles/default/{}",
                    src_name,
                    dst_name
                ),
                Err(e) => tracing::warn!(
                    "Profiles migration: could not move {} ({}), skipping",
                    src_name,
                    e
                ),
            }
        }
    }

    let manifest = ProfilesManifest {
        active: "default".to_string(),
        profiles: vec![ProfileMeta {
            id: "default".to_string(),
            name: "Default".to_string(),
            created_at: Utc::now().to_rfc3339(),
        }],
    };
    let content = serde_json::to_string_pretty(&manifest)?;
    fs::write(&manifest_path, &content).context("Failed to write initial profiles.json")?;

    tracing::info!("Profiles migration completed");
    Ok(())
}

/// Load manifest (creating a default one if absent), then populate AppState.active_profile_id.
pub fn init_active_profile(app: &AppHandle) -> Result<()> {
    let _registry = crate::note_service::scope::registry_write();
    let app_data = app
        .path()
        .app_data_dir()
        .context("Could not resolve app data directory")?;
    fs::create_dir_all(&app_data)?;
    let app_data = fs::canonicalize(&app_data)?;
    let manifest_path = crate::note_service::scope::checked_relative(
        &app_data,
        std::path::Path::new("profiles.json"),
    )?;

    if !manifest_path.exists() {
        // Fresh install with no legacy data
        let default_dir =
            crate::note_service::scope::checked_profile_root_in(&app_data, "default")?;
        fs::create_dir_all(&default_dir).context("Failed to create profiles/default directory")?;

        let manifest = ProfilesManifest {
            active: "default".to_string(),
            profiles: vec![ProfileMeta {
                id: "default".to_string(),
                name: "Default".to_string(),
                created_at: Utc::now().to_rfc3339(),
            }],
        };
        let content = serde_json::to_string_pretty(&manifest)?;
        fs::write(&manifest_path, &content).context("Failed to write initial profiles.json")?;
    }

    let manifest = crate::note_service::scope::read_manifest_unlocked(&app_data)?;

    // Ensure the active profile directory exists
    let profile_dir =
        crate::note_service::scope::checked_profile_root_in(&app_data, &manifest.active)?;
    if !profile_dir.exists() {
        fs::create_dir_all(&profile_dir).with_context(|| {
            format!(
                "Failed to create profile directory: {}",
                profile_dir.display()
            )
        })?;
    }

    let state: State<AppState> = app.state();
    if let Ok(mut guard) = state.active_profile_id.lock() {
        *guard = manifest.active.clone();
    }

    tracing::info!("Active profile: {}", manifest.active);
    Ok(())
}

/// Delete the contaminated legacy root sync stores (sync-queue.json, sync-meta.json).
/// These predate per-profile sync isolation. Snippets/dictionary stores are global
/// and intentionally left in place. Idempotent.
pub fn cleanup_legacy_root_sync_stores_in(app_data: &std::path::Path) -> std::io::Result<()> {
    for name in ["sync-queue.json", "sync-meta.json"] {
        let p = app_data.join(name);
        if p.exists() {
            match fs::remove_file(&p) {
                Ok(_) => tracing::info!("Removed legacy root sync store: {}", name),
                Err(e) => tracing::warn!("Could not remove legacy {} ({}), skipping", name, e),
            }
        }
    }
    Ok(())
}

pub fn cleanup_legacy_root_sync_stores(app: &AppHandle) -> Result<()> {
    let app_data = app
        .path()
        .app_data_dir()
        .context("Could not resolve app data directory")?;
    cleanup_legacy_root_sync_stores_in(&app_data)?;
    Ok(())
}

/// Move the legacy GLOBAL snippets/dictionary stores from app_data root into
/// profiles/default/. Multi-profil sync makes these per-profile (spec 2026-06-25).
/// Idempotent: no-op if root files are absent. Preserves local content.
pub fn migrate_global_snippets_dict_to_default_in(
    app_data: &std::path::Path,
) -> std::io::Result<()> {
    let default_dir = app_data.join("profiles").join("default");
    for name in ["sync-snippets.json", "sync-dictionary.json"] {
        let src = app_data.join(name);
        if src.exists() {
            fs::create_dir_all(&default_dir)?;
            let dst = default_dir.join(name);
            if !dst.exists() {
                match fs::rename(&src, &dst) {
                    Ok(_) => tracing::info!("Moved global {} -> profiles/default/", name),
                    Err(e) => tracing::warn!("Could not move {} ({}), skipping", name, e),
                }
            }
        }
    }
    Ok(())
}

pub fn migrate_global_snippets_dict_to_default(app: &AppHandle) -> Result<()> {
    let app_data = app
        .path()
        .app_data_dir()
        .context("Could not resolve app data directory")?;
    migrate_global_snippets_dict_to_default_in(&app_data)?;
    Ok(())
}

// --- Profile avatar (local photo) ---
//
// The avatar is `profiles/<id>/avatar.png` (256×256, produced by the frontend
// canvas). File presence is the source of truth — no ProfileMeta field.
// IPC carries PNG data-URLs both ways.

pub const AVATAR_FILENAME: &str = "avatar.png";
pub const AVATAR_DATA_URL_PREFIX: &str = "data:image/png;base64,";
/// Decoded payload cap. A 256×256 PNG is ~30-80 KB; 1 MB is a generous guard
/// against arbitrary renderer payloads landing on disk.
pub const AVATAR_MAX_BYTES: usize = 1024 * 1024;
pub const PNG_MAGIC: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

/// Decode and validate an avatar data-URL. Pure (testable).
pub fn decode_avatar_data_url(data_url: &str) -> Result<Vec<u8>, String> {
    use base64::Engine as _;

    let b64 = data_url
        .strip_prefix(AVATAR_DATA_URL_PREFIX)
        .ok_or_else(|| "Avatar must be a PNG data-URL.".to_string())?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| format!("Invalid base64 avatar payload: {}", e))?;
    if bytes.len() > AVATAR_MAX_BYTES {
        return Err("Avatar image is too large (max 1 MB).".to_string());
    }
    if bytes.len() < PNG_MAGIC.len() || bytes[..PNG_MAGIC.len()] != PNG_MAGIC {
        return Err("Avatar payload is not a PNG image.".to_string());
    }
    Ok(bytes)
}

pub fn write_avatar_in(profile_dir: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    fs::create_dir_all(profile_dir)
        .map_err(|e| format!("Failed to create profile directory: {}", e))?;
    fs::write(profile_dir.join(AVATAR_FILENAME), bytes)
        .map_err(|e| format!("Failed to write avatar: {}", e))
}

pub fn read_avatar_data_url_in(profile_dir: &std::path::Path) -> Option<String> {
    use base64::Engine as _;

    let bytes = fs::read(profile_dir.join(AVATAR_FILENAME)).ok()?;
    Some(format!(
        "{}{}",
        AVATAR_DATA_URL_PREFIX,
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

pub fn clear_avatar_in(profile_dir: &std::path::Path) -> Result<(), String> {
    let path = profile_dir.join(AVATAR_FILENAME);
    if path.exists() {
        fs::remove_file(&path).map_err(|e| format!("Failed to remove avatar: {}", e))?;
    }
    Ok(())
}

// --- ID / name helpers ---

/// Generate a unique id for a new profile, avoiding collisions with existing ids
/// and reserved directory names (models, logs).

#[cfg(test)]
mod tests {
    use super::cleanup_legacy_root_sync_stores_in;
    use super::profile_store_path;
    use std::fs;

    #[test]
    fn profile_store_path_handles_snippets_and_dictionary() {
        assert_eq!(
            profile_store_path("perso", "sync-snippets.json"),
            "profiles/perso/sync-snippets.json"
        );
        assert_eq!(
            profile_store_path("perso", "sync-dictionary.json"),
            "profiles/perso/sync-dictionary.json"
        );
    }

    #[test]
    fn migrate_moves_root_snippets_dict_into_default() {
        let dir = std::env::temp_dir().join(format!("lexena_snipmig_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("profiles").join("default")).unwrap();
        fs::write(dir.join("sync-snippets.json"), "{\"snippets\":[]}").unwrap();
        fs::write(dir.join("sync-dictionary.json"), "{\"words\":[]}").unwrap();

        super::migrate_global_snippets_dict_to_default_in(&dir).unwrap();

        assert!(
            !dir.join("sync-snippets.json").exists(),
            "root snippets moved"
        );
        assert!(
            dir.join("profiles/default/sync-snippets.json").exists(),
            "snippets in default"
        );
        assert!(
            !dir.join("sync-dictionary.json").exists(),
            "root dict moved"
        );
        assert!(
            dir.join("profiles/default/sync-dictionary.json").exists(),
            "dict in default"
        );

        // Idempotent
        super::migrate_global_snippets_dict_to_default_in(&dir).unwrap();
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn profile_store_path_joins_id_and_filename() {
        assert_eq!(
            profile_store_path("work", "sync-queue.json"),
            "profiles/work/sync-queue.json"
        );
        assert_eq!(
            profile_store_path("default", "sync-meta.json"),
            "profiles/default/sync-meta.json"
        );
    }

    #[test]
    fn cleanup_removes_only_legacy_queue_and_meta() {
        let dir = std::env::temp_dir().join(format!("lexena_cleanup_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for f in [
            "sync-queue.json",
            "sync-meta.json",
            "sync-snippets.json",
            "sync-dictionary.json",
        ] {
            fs::write(dir.join(f), "{}").unwrap();
        }

        cleanup_legacy_root_sync_stores_in(&dir).unwrap();

        assert!(
            !dir.join("sync-queue.json").exists(),
            "queue should be deleted"
        );
        assert!(
            !dir.join("sync-meta.json").exists(),
            "meta should be deleted"
        );
        assert!(
            dir.join("sync-snippets.json").exists(),
            "snippets must survive"
        );
        assert!(
            dir.join("sync-dictionary.json").exists(),
            "dictionary must survive"
        );

        // Idempotent: second run with files already gone must not error.
        cleanup_legacy_root_sync_stores_in(&dir).unwrap();

        let _ = fs::remove_dir_all(&dir);
    }

    use super::{
        clear_avatar_in, decode_avatar_data_url, profile_exists, read_avatar_data_url_in,
        write_avatar_in, ProfileMeta, ProfilesManifest, AVATAR_DATA_URL_PREFIX, PNG_MAGIC,
    };

    /// Minimal valid payload: PNG magic followed by arbitrary bytes.
    fn fake_png() -> Vec<u8> {
        let mut v = PNG_MAGIC.to_vec();
        v.extend_from_slice(b"not-a-real-png-but-magic-is-enough");
        v
    }

    fn to_data_url(bytes: &[u8]) -> String {
        use base64::Engine as _;
        format!(
            "{}{}",
            AVATAR_DATA_URL_PREFIX,
            base64::engine::general_purpose::STANDARD.encode(bytes)
        )
    }

    #[test]
    fn decode_avatar_data_url_roundtrips_png_bytes() {
        let bytes = fake_png();
        let decoded = decode_avatar_data_url(&to_data_url(&bytes)).unwrap();
        assert_eq!(decoded, bytes);
    }

    #[test]
    fn decode_avatar_rejects_wrong_prefix() {
        let err = decode_avatar_data_url("data:image/jpeg;base64,AAAA").unwrap_err();
        assert!(err.contains("PNG data-URL"), "unexpected error: {err}");
    }

    #[test]
    fn decode_avatar_rejects_invalid_base64() {
        let url = format!("{}%%%not-base64%%%", AVATAR_DATA_URL_PREFIX);
        assert!(decode_avatar_data_url(&url).is_err());
    }

    #[test]
    fn decode_avatar_rejects_non_png_payload() {
        assert!(decode_avatar_data_url(&to_data_url(b"hello world")).is_err());
    }

    #[test]
    fn decode_avatar_rejects_oversized_payload() {
        let mut big = PNG_MAGIC.to_vec();
        big.resize(super::AVATAR_MAX_BYTES + 1, 0u8);
        let err = decode_avatar_data_url(&to_data_url(&big)).unwrap_err();
        assert!(err.contains("too large"), "unexpected error: {err}");
    }

    #[test]
    fn avatar_write_read_clear_roundtrip() {
        let dir = std::env::temp_dir().join(format!("lexena_avatar_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);

        // Missing file -> None
        assert!(read_avatar_data_url_in(&dir).is_none());

        let bytes = fake_png();
        write_avatar_in(&dir, &bytes).unwrap();
        let url = read_avatar_data_url_in(&dir).expect("avatar should exist");
        assert!(url.starts_with(AVATAR_DATA_URL_PREFIX));
        assert_eq!(decode_avatar_data_url(&url).unwrap(), bytes);

        clear_avatar_in(&dir).unwrap();
        assert!(read_avatar_data_url_in(&dir).is_none());
        // Idempotent
        clear_avatar_in(&dir).unwrap();

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn profile_exists_checks_manifest_ids() {
        let manifest = ProfilesManifest {
            active: "default".to_string(),
            profiles: vec![ProfileMeta {
                id: "default".to_string(),
                name: "Default".to_string(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
            }],
        };
        assert!(profile_exists(&manifest, "default"));
        assert!(!profile_exists(&manifest, "ghost"));
    }
}
