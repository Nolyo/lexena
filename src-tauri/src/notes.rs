use crate::commands::note_editors::mutate_ui;
use crate::note_service::editors::ProfileSession;
#[cfg(test)]
use crate::note_service::repository::is_note_active;
pub use crate::note_service::repository::{NoteData, NoteMeta};
use crate::note_service::{
    Actor, MutationIntent, MutationResult, NoteRepository, NoteService, ProfileScope,
};
use anyhow::Result;
use serde::Deserialize;
use tauri::AppHandle;
use tauri::WebviewWindow;
use tauri_plugin_store::StoreBuilder;

/// Pure helper: a note is "active" (visible to the user) when it has not been soft-deleted.

/// Legacy note format stored in settings.json
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyNote {
    id: String,
    title: String,
    content: String,
    created_at: String,
    updated_at: String,
}

/// Get the notes directory for the active profile, creating it if needed

/// Canonical UUID shape (8-4-4-4-12 hex groups). Note ids become window
/// labels (`note-<id>`), which only accept [a-zA-Z0-9\-/:_] — rejecting
/// anything else up front prevents label injection and builder panics.
pub(crate) fn is_valid_note_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

/// Read note metadata from a note directory

/// Strip HTML tags for search purposes

/// Convert plain text to basic HTML paragraphs (for migration)
fn text_to_html(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    text.split('\n')
        .map(|line| {
            if line.trim().is_empty() {
                "<p></p>".to_string()
            } else {
                format!("<p>{}</p>", html_escape(line))
            }
        })
        .collect::<Vec<_>>()
        .join("")
}

/// Escape HTML special characters
fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Migrate notes from settings.json store to file-based storage.
/// Returns the number of migrated notes.
pub fn migrate_notes_from_store(app_handle: &AppHandle) -> Result<u32> {
    let scope = ProfileScope::active(app_handle)?;
    let store = StoreBuilder::new(
        app_handle,
        crate::profiles::profile_store_path(scope.local_id(), "settings.json"),
    )
    .build()?;
    let Some(value) = store.get("notes") else {
        return Ok(0);
    };
    let legacy_notes: Vec<LegacyNote> = match serde_json::from_value(value.clone()) {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!("Failed to parse legacy notes from store: {}", e);
            return Ok(0);
        }
    };
    let migrated = NoteService::transaction(&scope, |profile| {
        let mut migrated = 0;
        for note in &legacy_notes {
            crate::note_service::scope::validate_id(&note.id)?;
            let path = profile.path(&std::path::Path::new("notes").join(&note.id))?;
            if path.exists() {
                migrated += 1;
                continue;
            }
            let meta = NoteMeta {
                id: note.id.clone(),
                title: note.title.clone(),
                created_at: note.created_at.clone(),
                updated_at: note.updated_at.clone(),
                favorite: false,
                folder_id: None,
                order: 0,
                deleted_at: None,
                local_only: false,
            };
            NoteService::mutate_locked(
                profile,
                MutationIntent::ImportNote {
                    meta,
                    content: text_to_html(&note.content),
                },
                Actor::Ui,
            )?;
            migrated += 1;
        }
        Ok(migrated)
    })?;
    store.delete("notes");
    let _ = store.save();
    tracing::info!(
        "Migrated {} notes from settings.json to file-based storage",
        migrated
    );
    Ok(migrated)
}

#[tauri::command]
pub async fn list_notes(app_handle: AppHandle) -> Result<Vec<NoteMeta>, String> {
    let scope = ProfileScope::active(&app_handle).map_err(|e| e.to_string())?;
    NoteRepository::list(&scope).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn read_note(app_handle: AppHandle, id: String) -> Result<NoteData, String> {
    let scope = ProfileScope::active(&app_handle).map_err(|e| e.to_string())?;
    NoteRepository::get(&scope, &id).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn create_note(
    app_handle: AppHandle,
    window: WebviewWindow,
    session: Option<ProfileSession>,
    folder_id: Option<String>,
) -> Result<NoteMeta, String> {
    mutate_ui(
        &app_handle,
        &window,
        session,
        MutationIntent::CreateNote { folder_id },
        Actor::Ui,
    )
    .await
    .and_then(MutationResult::note)
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn update_note(
    app_handle: AppHandle,
    window: WebviewWindow,
    session: Option<ProfileSession>,
    id: String,
    content: String,
    title: String,
) -> Result<NoteMeta, String> {
    mutate_ui(
        &app_handle,
        &window,
        session,
        MutationIntent::UpdateNote { id, content, title },
        Actor::Ui,
    )
    .await
    .and_then(MutationResult::note)
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_note(
    app_handle: AppHandle,
    window: WebviewWindow,
    session: Option<ProfileSession>,
    id: String,
) -> Result<(), String> {
    mutate_ui(
        &app_handle,
        &window,
        session,
        MutationIntent::DeleteNote { id },
        Actor::Ui,
    )
    .await
    .and_then(MutationResult::unit)
    .map_err(|e| e.to_string())
}

/// Hard-delete soft-deleted notes after server confirmed they were purged.
/// Called from the frontend sync engine after a successful pull that returns
/// the list of note IDs whose tombstones were >30j and dropped server-side.
/// Only deletes notes that ARE soft-deleted locally — never removes an active note.
#[tauri::command]
pub async fn purge_soft_deleted_notes_post_pull(
    app_handle: AppHandle,
    window: WebviewWindow,
    session: Option<ProfileSession>,
    note_ids: Vec<String>,
) -> Result<u32, String> {
    mutate_ui(
        &app_handle,
        &window,
        session,
        MutationIntent::PurgeNotes { note_ids },
        Actor::Remote,
    )
    .await
    .and_then(MutationResult::count)
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn toggle_note_favorite(
    app_handle: AppHandle,
    window: WebviewWindow,
    session: Option<ProfileSession>,
    id: String,
) -> Result<NoteMeta, String> {
    mutate_ui(
        &app_handle,
        &window,
        session,
        MutationIntent::ToggleFavorite { id },
        Actor::Ui,
    )
    .await
    .and_then(MutationResult::note)
    .map_err(|e| e.to_string())
}

/// Set whether a note is "local only" (never synced to the cloud).
/// The cloud reconciliation (tombstone / re-upsert) is handled by the
/// frontend notes-store.
///
/// `updated_at` is bumped ONLY when clearing the flag (re-sync): the server
/// stamps a fresh `updated_at` on the tombstone created by the earlier
/// synced → local toggle, so without the bump a pull landing before the
/// re-upsert flushes would win LWW and soft-delete the local copy on this
/// very device. Setting the flag needs no bump — the pull guard in
/// `applyRemoteNote` already shields local-only notes from their cloud rows.
#[tauri::command]
pub async fn set_note_local_only(
    app_handle: AppHandle,
    window: WebviewWindow,
    session: Option<ProfileSession>,
    id: String,
    local_only: bool,
) -> Result<NoteMeta, String> {
    mutate_ui(
        &app_handle,
        &window,
        session,
        MutationIntent::SetLocalOnly { id, local_only },
        Actor::Ui,
    )
    .await
    .and_then(MutationResult::note)
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn move_note_to_folder(
    app_handle: AppHandle,
    window: WebviewWindow,
    session: Option<ProfileSession>,
    note_id: String,
    folder_id: Option<String>,
) -> Result<NoteMeta, String> {
    mutate_ui(
        &app_handle,
        &window,
        session,
        MutationIntent::MoveNote { note_id, folder_id },
        Actor::Ui,
    )
    .await
    .and_then(MutationResult::note)
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn reorder_notes_in_folder(
    app_handle: AppHandle,
    window: WebviewWindow,
    session: Option<ProfileSession>,
    folder_id: Option<String>,
    note_ids: Vec<String>,
) -> Result<(), String> {
    mutate_ui(
        &app_handle,
        &window,
        session,
        MutationIntent::ReorderNotes {
            folder_id,
            note_ids,
        },
        Actor::Ui,
    )
    .await
    .and_then(MutationResult::unit)
    .map_err(|e| e.to_string())
}

/// Reset folder_id to None for all notes currently in the given folder.
/// Used when a folder is deleted — notes become orphans (root) instead of being lost.

/// Return all notes whose HTML content references `note_id` via a note-link node.
/// The pattern looked up is literal: `data-note-id="{note_id}"`.
#[tauri::command]
pub async fn get_backlinks(
    app_handle: AppHandle,
    note_id: String,
) -> Result<Vec<NoteMeta>, String> {
    let scope = ProfileScope::active(&app_handle).map_err(|e| e.to_string())?;
    NoteRepository::backlinks(&scope, note_id).map_err(|e| e.to_string())
}

/// Imports a note for backup restore. The note's id, meta, and content_html
/// are preserved exactly — overwriting any existing note with the same id.
/// If the meta has `deleted_at: Some(...)`, the note is restored as soft-deleted
/// (preserves the tombstone semantic).
#[tauri::command]
pub async fn import_note_for_backup(
    app_handle: AppHandle,
    window: WebviewWindow,
    session: Option<ProfileSession>,
    meta: NoteMeta,
    content: String,
    source: Option<crate::note_service::mutation::ImportSource>,
) -> Result<(), String> {
    mutate_ui(
        &app_handle,
        &window,
        session,
        MutationIntent::ImportNote { meta, content },
        source.unwrap_or_default().into(),
    )
    .await
    .and_then(MutationResult::unit)
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn search_notes(app_handle: AppHandle, query: String) -> Result<Vec<NoteMeta>, String> {
    let scope = ProfileScope::active(&app_handle).map_err(|e| e.to_string())?;
    NoteRepository::search(&scope, query).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_meta(deleted_at: Option<String>) -> NoteMeta {
        NoteMeta {
            id: "test-id".to_string(),
            title: "Test".to_string(),
            created_at: "2026-05-19T10:00:00Z".to_string(),
            updated_at: "2026-05-19T10:00:00Z".to_string(),
            favorite: false,
            folder_id: None,
            order: 0,
            deleted_at,
            local_only: false,
        }
    }

    #[test]
    fn is_note_active_returns_true_when_deleted_at_is_none() {
        let meta = make_meta(None);
        assert!(is_note_active(&meta));
    }

    #[test]
    fn is_note_active_returns_false_when_deleted_at_is_some() {
        let meta = make_meta(Some("2026-05-19T11:00:00Z".to_string()));
        assert!(!is_note_active(&meta));
    }

    #[test]
    fn note_meta_roundtrips_with_deleted_at() {
        let meta = make_meta(Some("2026-05-19T11:00:00Z".to_string()));
        let json = serde_json::to_string(&meta).expect("serialize");
        // camelCase rename → JSON key is `deletedAt`
        assert!(
            json.contains("\"deletedAt\":\"2026-05-19T11:00:00Z\""),
            "expected deletedAt in JSON, got: {}",
            json
        );

        let decoded: NoteMeta = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(decoded.deleted_at, meta.deleted_at);
        assert_eq!(decoded.id, meta.id);
        assert_eq!(decoded.title, meta.title);
        assert_eq!(decoded.created_at, meta.created_at);
        assert_eq!(decoded.updated_at, meta.updated_at);
        assert_eq!(decoded.favorite, meta.favorite);
        assert_eq!(decoded.folder_id, meta.folder_id);
        assert_eq!(decoded.order, meta.order);
    }

    #[test]
    fn note_meta_skips_deleted_at_when_none() {
        let meta = make_meta(None);
        let json = serde_json::to_string(&meta).expect("serialize");
        assert!(
            !json.contains("deletedAt"),
            "expected deletedAt to be omitted, got: {}",
            json
        );
    }

    #[test]
    fn purge_post_pull_predicate_only_removes_soft_deleted() {
        // Mirror the predicate from purge_soft_deleted_notes_post_pull:
        // a note is purged iff its id is in the pulled list AND it is locally soft-deleted.
        let active = make_meta(None);
        let tombstoned = make_meta(Some("2026-05-19T11:00:00Z".to_string()));
        let pulled_ids = vec!["test-id".to_string()];
        let is_pulled = |m: &NoteMeta| pulled_ids.contains(&m.id);
        assert!(
            !(is_pulled(&active) && active.deleted_at.is_some()),
            "active note should NOT be purged even if its id is in the pulled list"
        );
        assert!(
            is_pulled(&tombstoned) && tombstoned.deleted_at.is_some(),
            "soft-deleted note with matching id SHOULD be purged"
        );
    }

    #[test]
    fn import_note_payload_preserves_tombstone_via_serde() {
        // Simulates what import_note_for_backup does on disk: serialize the meta
        // exactly as received. We assert the tombstone roundtrips so a restored
        // note keeps its soft-deleted state.
        let meta = make_meta(Some("2026-05-19T11:00:00Z".to_string()));
        let payload = serde_json::to_string_pretty(&meta).expect("serialize");
        let restored: NoteMeta = serde_json::from_str(&payload).expect("deserialize");
        assert_eq!(restored.deleted_at, meta.deleted_at);
        assert_eq!(restored.id, meta.id);
        assert!(!is_note_active(&restored), "tombstone must survive restore");
    }

    #[test]
    fn import_note_payload_preserves_active_state_via_serde() {
        let meta = make_meta(None);
        let payload = serde_json::to_string_pretty(&meta).expect("serialize");
        let restored: NoteMeta = serde_json::from_str(&payload).expect("deserialize");
        assert!(is_note_active(&restored));
        assert_eq!(restored.id, meta.id);
    }

    #[test]
    fn note_meta_deserializes_without_deleted_at_key() {
        // Backward compat: legacy note.json files on disk don't have the deletedAt key.
        let legacy_json = r#"{
            "id": "abc",
            "title": "Legacy Note",
            "createdAt": "2026-05-19T10:00:00Z",
            "updatedAt": "2026-05-19T10:00:00Z",
            "favorite": false,
            "order": 0
        }"#;
        let meta: NoteMeta = serde_json::from_str(legacy_json).expect("deserialize legacy");
        assert_eq!(meta.deleted_at, None);
        assert!(is_note_active(&meta));
    }

    #[test]
    fn note_meta_roundtrips_local_only_true() {
        let mut meta = make_meta(None);
        meta.local_only = true;
        let json = serde_json::to_string(&meta).expect("serialize");
        // camelCase rename → JSON key is `localOnly`
        assert!(
            json.contains("\"localOnly\":true"),
            "expected localOnly in JSON, got: {}",
            json
        );
        let decoded: NoteMeta = serde_json::from_str(&json).expect("deserialize");
        assert!(decoded.local_only);
    }

    #[test]
    fn note_meta_defaults_local_only_false_on_legacy_json() {
        // Backward compat: note.json files written before PR3 have no localOnly key.
        let legacy_json = r#"{
            "id": "abc",
            "title": "Legacy Note",
            "createdAt": "2026-05-19T10:00:00Z",
            "updatedAt": "2026-05-19T10:00:00Z",
            "favorite": false,
            "order": 0
        }"#;
        let meta: NoteMeta = serde_json::from_str(legacy_json).expect("deserialize legacy");
        assert!(!meta.local_only);
    }

    #[test]
    fn import_note_payload_preserves_local_only_via_serde() {
        // import_note_for_backup serializes the meta exactly as received —
        // a restored local-only note must stay local-only.
        let mut meta = make_meta(None);
        meta.local_only = true;
        let payload = serde_json::to_string_pretty(&meta).expect("serialize");
        let restored: NoteMeta = serde_json::from_str(&payload).expect("deserialize");
        assert!(restored.local_only);
        assert_eq!(restored.id, meta.id);
    }
}

#[cfg(test)]
mod note_id_tests {
    use super::is_valid_note_id;

    #[test]
    fn accepts_canonical_uuid() {
        assert!(is_valid_note_id("0f8fad5b-d9cb-469f-a165-70867728950e"));
    }

    #[test]
    fn rejects_empty_and_traversal() {
        assert!(!is_valid_note_id(""));
        assert!(!is_valid_note_id("../../etc/passwd"));
    }

    #[test]
    fn rejects_wrong_shape() {
        // no dashes
        assert!(!is_valid_note_id("0f8fad5bd9cb469fa16570867728950e0000"));
        // non-hex char at the end
        assert!(!is_valid_note_id("0f8fad5b-d9cb-469f-a165-70867728950g"));
        // dash at wrong index
        assert!(!is_valid_note_id("0f8fad5bd-9cb-469f-a165-70867728950e"));
    }
}
