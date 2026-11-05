use crate::commands::note_editors::mutate_ui;
use crate::note_service::editors::ProfileSession;
pub use crate::note_service::repository::FolderMeta;
#[cfg(test)]
use crate::note_service::repository::{
    is_folder_active, migrate_folder_updated_at_if_needed, normalize_icon,
};
use crate::note_service::{
    Actor, FolderRepository, MutationIntent, MutationResult, ProfileScope,
};
use tauri::AppHandle;
use tauri::WebviewWindow;

/// Pure helper: a folder is "active" (visible to the user) when it has not been
/// soft-deleted. Mirrors `notes::is_note_active`.

/// Pure helper: trims the icon, maps empty/whitespace-only to `None`, and
/// rejects anything longer than 32 UTF-16 units (the sync-push Edge cap —
/// a longer icon would 400 the whole push batch).

/// If all folders have order == 0 (i.e. field was defaulted on deserialize),
/// assign orders from the current alphabetical sort. Returns true if migrated.

/// Backfill `updated_at` from `created_at` for legacy folders persisted before
/// the field existed. Returns true if at least one folder was migrated.

#[tauri::command]
pub async fn list_folders(app_handle: AppHandle) -> Result<Vec<FolderMeta>, String> {
    let scope = ProfileScope::active(&app_handle).map_err(|e| e.to_string())?;
    FolderRepository::list(&scope).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn create_folder(
    app_handle: AppHandle,
    window: WebviewWindow,
    session: Option<ProfileSession>,
    name: String,
    icon: Option<String>,
) -> Result<FolderMeta, String> {
    mutate_ui(
        &app_handle,
        &window,
        session,
        MutationIntent::CreateFolder { name, icon },
        Actor::Ui,
    )
    .await
    .and_then(MutationResult::folder)
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn reorder_folders(
    app_handle: AppHandle,
    window: WebviewWindow,
    session: Option<ProfileSession>,
    ids: Vec<String>,
) -> Result<(), String> {
    mutate_ui(
        &app_handle,
        &window,
        session,
        MutationIntent::ReorderFolders { ids },
        Actor::Ui,
    )
    .await
    .and_then(MutationResult::unit)
    .map_err(|e| e.to_string())
}

/// Rename a folder and set its emoji icon in one atomic write.
/// `icon` is the FULL desired state: `None` clears any existing emoji (the
/// FolderNameDialog is the single mutation point and always sends the
/// complete name+icon pair). `updated_at` is bumped so LWW ships both.
#[tauri::command]
pub async fn rename_folder(
    app_handle: AppHandle,
    window: WebviewWindow,
    session: Option<ProfileSession>,
    id: String,
    name: String,
    icon: Option<String>,
) -> Result<FolderMeta, String> {
    mutate_ui(
        &app_handle,
        &window,
        session,
        MutationIntent::RenameFolder { id, name, icon },
        Actor::Ui,
    )
    .await
    .and_then(MutationResult::folder)
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_folder(
    app_handle: AppHandle,
    window: WebviewWindow,
    session: Option<ProfileSession>,
    id: String,
) -> Result<(), String> {
    mutate_ui(
        &app_handle,
        &window,
        session,
        MutationIntent::DeleteFolder { id },
        Actor::Ui,
    )
    .await
    .and_then(MutationResult::unit)
    .map_err(|e| e.to_string())
}

/// Imports folders for backup restore. Replaces the local folders.json entirely
/// with the provided list (preserves ids + updated_at + deleted_at).
#[tauri::command]
pub async fn import_folders_for_backup(
    app_handle: AppHandle,
    window: WebviewWindow,
    session: Option<ProfileSession>,
    folders: Vec<FolderMeta>,
    source: Option<crate::note_service::mutation::ImportSource>,
) -> Result<(), String> {
    mutate_ui(
        &app_handle,
        &window,
        session,
        MutationIntent::ImportFolders { folders },
        source.unwrap_or_default().into(),
    )
    .await
    .and_then(MutationResult::unit)
    .map_err(|e| e.to_string())
}

/// Hard-delete soft-deleted folders after server confirmed they were purged.
/// Mirror of `notes::purge_soft_deleted_notes_post_pull`. Only removes folders
/// that are locally soft-deleted — an active folder is never removed.
#[tauri::command]
pub async fn purge_soft_deleted_folders_post_pull(
    app_handle: AppHandle,
    window: WebviewWindow,
    session: Option<ProfileSession>,
    folder_ids: Vec<String>,
) -> Result<u32, String> {
    mutate_ui(
        &app_handle,
        &window,
        session,
        MutationIntent::PurgeFolders { folder_ids },
        Actor::Remote,
    )
    .await
    .and_then(MutationResult::count)
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_folder(updated_at: &str, deleted_at: Option<String>) -> FolderMeta {
        FolderMeta {
            id: "folder-id".to_string(),
            name: "Test".to_string(),
            icon: None,
            created_at: "2026-05-19T10:00:00Z".to_string(),
            updated_at: updated_at.to_string(),
            order: 0,
            deleted_at,
        }
    }

    #[test]
    fn folder_meta_roundtrips_with_updated_at_and_deleted_at() {
        let folder = make_folder(
            "2026-05-19T11:00:00Z",
            Some("2026-05-19T12:00:00Z".to_string()),
        );
        let json = serde_json::to_string(&folder).expect("serialize");
        // camelCase rename → JSON keys are `updatedAt` / `deletedAt`.
        assert!(
            json.contains("\"updatedAt\":\"2026-05-19T11:00:00Z\""),
            "expected updatedAt in JSON, got: {}",
            json
        );
        assert!(
            json.contains("\"deletedAt\":\"2026-05-19T12:00:00Z\""),
            "expected deletedAt in JSON, got: {}",
            json
        );

        let decoded: FolderMeta = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded.id, folder.id);
        assert_eq!(decoded.name, folder.name);
        assert_eq!(decoded.created_at, folder.created_at);
        assert_eq!(decoded.updated_at, folder.updated_at);
        assert_eq!(decoded.order, folder.order);
        assert_eq!(decoded.deleted_at, folder.deleted_at);
    }

    #[test]
    fn folder_meta_deserializes_without_updated_at_or_deleted_at() {
        // Backward compat: legacy folders.json on disk lacks both keys.
        let legacy_json = r#"{
            "id": "x",
            "name": "n",
            "createdAt": "2026-01-01T00:00:00Z",
            "order": 0
        }"#;
        let meta: FolderMeta = serde_json::from_str(legacy_json).expect("deserialize legacy");
        assert_eq!(meta.updated_at, "");
        assert_eq!(meta.deleted_at, None);
        assert!(is_folder_active(&meta));
    }

    #[test]
    fn folder_meta_skips_deleted_at_when_none() {
        let folder = make_folder("2026-05-19T11:00:00Z", None);
        let json = serde_json::to_string(&folder).expect("serialize");
        assert!(
            !json.contains("deletedAt"),
            "expected deletedAt to be omitted, got: {}",
            json
        );
    }

    #[test]
    fn migrate_folder_updated_at_sets_from_created_at_when_empty() {
        let mut folders = vec![
            make_folder("", None),
            make_folder("2026-05-19T11:00:00Z", None),
        ];
        let migrated = migrate_folder_updated_at_if_needed(&mut folders);
        assert!(migrated, "expected migration to report true");
        // First folder backfilled from created_at.
        assert_eq!(folders[0].updated_at, "2026-05-19T10:00:00Z");
        // Second folder untouched.
        assert_eq!(folders[1].updated_at, "2026-05-19T11:00:00Z");
    }

    #[test]
    fn migrate_folder_updated_at_noop_when_all_populated() {
        let mut folders = vec![
            make_folder("2026-05-19T11:00:00Z", None),
            make_folder("2026-05-19T12:00:00Z", None),
        ];
        let migrated = migrate_folder_updated_at_if_needed(&mut folders);
        assert!(!migrated, "expected no migration");
        assert_eq!(folders[0].updated_at, "2026-05-19T11:00:00Z");
        assert_eq!(folders[1].updated_at, "2026-05-19T12:00:00Z");
    }

    #[test]
    fn is_folder_active_returns_true_when_deleted_at_is_none() {
        let folder = make_folder("2026-05-19T11:00:00Z", None);
        assert!(is_folder_active(&folder));
    }

    #[test]
    fn is_folder_active_returns_false_when_deleted_at_is_some() {
        let folder = make_folder(
            "2026-05-19T11:00:00Z",
            Some("2026-05-19T12:00:00Z".to_string()),
        );
        assert!(!is_folder_active(&folder));
    }

    #[test]
    fn import_folders_payload_roundtrips_preserves_all_fields() {
        // Simulates what import_folders_for_backup writes on disk: serialize the
        // full list exactly as received. We assert every field roundtrips —
        // including tombstones — so a restored folder set matches the backup.
        let folders = vec![
            FolderMeta {
                id: "active-1".to_string(),
                name: "Work".to_string(),
                icon: Some("💼".to_string()),
                created_at: "2026-05-19T10:00:00Z".to_string(),
                updated_at: "2026-05-19T11:00:00Z".to_string(),
                order: 0,
                deleted_at: None,
            },
            FolderMeta {
                id: "tombstoned-1".to_string(),
                name: "Old".to_string(),
                icon: None,
                created_at: "2026-05-19T09:00:00Z".to_string(),
                updated_at: "2026-05-19T12:00:00Z".to_string(),
                order: 1,
                deleted_at: Some("2026-05-19T12:00:00Z".to_string()),
            },
        ];

        let payload = serde_json::to_string_pretty(&folders).expect("serialize");
        let restored: Vec<FolderMeta> = serde_json::from_str(&payload).expect("deserialize");

        assert_eq!(restored.len(), 2);
        assert_eq!(restored[0].id, "active-1");
        assert_eq!(restored[0].name, "Work");
        assert_eq!(restored[0].created_at, "2026-05-19T10:00:00Z");
        assert_eq!(restored[0].updated_at, "2026-05-19T11:00:00Z");
        assert_eq!(restored[0].order, 0);
        assert_eq!(restored[0].deleted_at, None);
        assert_eq!(restored[0].icon, Some("💼".to_string()));
        assert!(is_folder_active(&restored[0]));

        assert_eq!(restored[1].id, "tombstoned-1");
        assert_eq!(restored[1].name, "Old");
        assert_eq!(
            restored[1].deleted_at,
            Some("2026-05-19T12:00:00Z".to_string())
        );
        assert_eq!(restored[1].icon, None);
        assert!(
            !is_folder_active(&restored[1]),
            "tombstone must survive restore"
        );
    }

    #[test]
    fn purge_post_pull_retain_only_drops_tombstoned_in_target_set() {
        // Mirror the retain() predicate from purge_soft_deleted_folders_post_pull.
        let mut folders = vec![
            FolderMeta {
                id: "active-1".to_string(),
                name: "A".to_string(),
                created_at: "2026-05-19T10:00:00Z".to_string(),
                updated_at: "2026-05-19T10:00:00Z".to_string(),
                order: 0,
                deleted_at: None,
                icon: None,
            },
            FolderMeta {
                id: "tombstoned-1".to_string(),
                name: "B".to_string(),
                created_at: "2026-05-19T10:00:00Z".to_string(),
                updated_at: "2026-05-19T11:00:00Z".to_string(),
                order: 1,
                deleted_at: Some("2026-05-19T11:00:00Z".to_string()),
                icon: None,
            },
            FolderMeta {
                id: "tombstoned-not-in-target".to_string(),
                name: "C".to_string(),
                created_at: "2026-05-19T10:00:00Z".to_string(),
                updated_at: "2026-05-19T11:00:00Z".to_string(),
                order: 2,
                deleted_at: Some("2026-05-19T11:00:00Z".to_string()),
                icon: None,
            },
        ];
        let target_ids = vec!["active-1".to_string(), "tombstoned-1".to_string()];
        folders.retain(|f| !(target_ids.contains(&f.id) && f.deleted_at.is_some()));
        assert_eq!(folders.len(), 2, "only 'tombstoned-1' should be dropped");
        assert_eq!(folders[0].id, "active-1");
        assert_eq!(folders[1].id, "tombstoned-not-in-target");
    }

    #[test]
    fn folder_meta_roundtrips_icon() {
        let mut folder = make_folder("2026-05-19T11:00:00Z", None);
        folder.icon = Some("📁".to_string());
        let json = serde_json::to_string(&folder).expect("serialize");
        assert!(
            json.contains("\"icon\":\"📁\""),
            "expected icon in JSON, got: {}",
            json
        );
        let decoded: FolderMeta = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded.icon, Some("📁".to_string()));
    }

    #[test]
    fn folder_meta_deserializes_without_icon() {
        // Backward compat: folders.json written before the icon field existed.
        let legacy_json = r#"{
            "id": "x",
            "name": "n",
            "createdAt": "2026-01-01T00:00:00Z",
            "updatedAt": "2026-01-01T00:00:00Z",
            "order": 0
        }"#;
        let meta: FolderMeta = serde_json::from_str(legacy_json).expect("deserialize legacy");
        assert_eq!(meta.icon, None);
    }

    #[test]
    fn folder_meta_skips_icon_when_none() {
        let folder = make_folder("2026-05-19T11:00:00Z", None);
        let json = serde_json::to_string(&folder).expect("serialize");
        assert!(
            !json.contains("\"icon\""),
            "expected icon to be omitted when None, got: {}",
            json
        );
    }

    #[test]
    fn normalize_icon_trims_and_maps_empty_to_none() {
        assert_eq!(normalize_icon(None), None);
        assert_eq!(normalize_icon(Some("".to_string())), None);
        assert_eq!(normalize_icon(Some("   ".to_string())), None);
        assert_eq!(
            normalize_icon(Some(" 📁 ".to_string())),
            Some("📁".to_string())
        );
    }

    #[test]
    fn normalize_icon_rejects_over_32_utf16_units() {
        // Zalgo-style cluster: 'e' + 40 combining acute accents = 41 UTF-16 units.
        let zalgo = format!("e{}", "\u{0301}".repeat(40));
        assert_eq!(normalize_icon(Some(zalgo)), None);
        // A long-but-legit ZWJ emoji stays (family emoji = 11 UTF-16 units).
        assert_eq!(
            normalize_icon(Some("👨\u{200D}👩\u{200D}👧\u{200D}👦".to_string())),
            Some("👨\u{200D}👩\u{200D}👧\u{200D}👦".to_string())
        );
    }
}
