use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteMeta {
    pub id: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub favorite: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_id: Option<String>,
    #[serde(default)]
    pub order: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<String>,
    #[serde(default)]
    pub local_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteData {
    pub meta: NoteMeta,
    pub content: String,
}

pub(crate) fn is_note_active(meta: &NoteMeta) -> bool {
    meta.deleted_at.is_none()
}

pub(crate) fn strip_html_tags(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => result.push(ch),
            _ => {}
        }
    }
    result
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderMeta {
    pub id: String,
    pub name: String,
    /// Emoji icon shown instead of the default folder glyph. `None` = default
    /// glyph. Synced through the cloud row (same LWW timestamp as the name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    pub created_at: String,
    /// Last-write-wins timestamp for sync. Migrated from `created_at` on first
    /// read when missing (see [`migrate_folder_updated_at_if_needed`]).
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub order: i32,
    /// Soft-delete tombstone. `None` = active folder; `Some(rfc3339)` = deleted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<String>,
}

pub(crate) fn is_folder_active(folder: &FolderMeta) -> bool {
    folder.deleted_at.is_none()
}

pub(crate) fn normalize_icon(icon: Option<String>) -> Option<String> {
    icon.and_then(|s| {
        let trimmed = s.trim();
        // Mirror the sync-push cap (max 32 UTF-16 units): an over-long icon
        // would 400 the whole push batch, so it must never be persisted.
        if trimmed.is_empty() || trimmed.encode_utf16().count() > 32 {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

pub(crate) fn migrate_folder_orders_if_needed(folders: &mut Vec<FolderMeta>) -> bool {
    if folders.len() < 2 {
        return false;
    }
    if !folders.iter().all(|f| f.order == 0) {
        return false;
    }
    folders.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    for (idx, folder) in folders.iter_mut().enumerate() {
        folder.order = idx as i32;
    }
    true
}

pub(crate) fn migrate_folder_updated_at_if_needed(folders: &mut Vec<FolderMeta>) -> bool {
    let mut migrated = false;
    for folder in folders.iter_mut() {
        if folder.updated_at.is_empty() {
            folder.updated_at = folder.created_at.clone();
            migrated = true;
        }
    }
    migrated
}

use super::scope::validate_id;
use super::{DomainError, LockedProfile, ProfileScope};
use std::{
    fs,
    path::{Path, PathBuf},
};
pub struct NoteRepository;
pub struct FolderRepository;
impl NoteRepository {
    pub fn metadata_locked(profile: &LockedProfile<'_>, id: &str) -> Result<NoteMeta, DomainError> {
        super::journal::Journal::recover_locked(profile)?;
        let directory = note_path(profile, id)?;
        if !profile
            .path(&Path::new("notes").join(id).join("note.json"))?
            .exists()
        {
            return Err(DomainError::NotFound(id.into()));
        }
        notes_disk::read_note_meta(profile, &directory)
    }
    pub fn content_locked(profile: &LockedProfile<'_>, id: &str) -> Result<String, DomainError> {
        super::journal::Journal::recover_locked(profile)?;
        let directory = note_path(profile, id)?;
        let content = safe(profile, directory.join("content.html"))?;
        if content.exists() {
            profile.read_text(content)
        } else {
            Ok(String::new())
        }
    }
    pub fn list(scope: &ProfileScope) -> Result<Vec<NoteMeta>, DomainError> {
        scope.transaction(|p| Self::list_locked(p))
    }
    pub fn get(scope: &ProfileScope, id: &str) -> Result<NoteData, DomainError> {
        scope.transaction(|p| Self::get_locked(p, id))
    }
    pub fn search(scope: &ProfileScope, query: String) -> Result<Vec<NoteMeta>, DomainError> {
        scope.transaction(|p| notes_disk::search_notes(p, query))
    }
    pub fn backlinks(scope: &ProfileScope, id: String) -> Result<Vec<NoteMeta>, DomainError> {
        scope.transaction(|p| {
            validate_id(&id)?;
            notes_disk::get_backlinks(p, id)
        })
    }
    pub fn list_locked(profile: &LockedProfile<'_>) -> Result<Vec<NoteMeta>, DomainError> {
        super::journal::Journal::recover_locked(profile)?;
        notes_disk::list_notes(profile)
    }
    pub fn get_locked(profile: &LockedProfile<'_>, id: &str) -> Result<NoteData, DomainError> {
        super::journal::Journal::recover_locked(profile)?;
        notes_disk::read_note(profile, id.into())
    }
}
impl FolderRepository {
    pub fn list(scope: &ProfileScope) -> Result<Vec<FolderMeta>, DomainError> {
        scope.transaction(|p| Self::list_locked(p))
    }
    pub fn get(scope: &ProfileScope, id: &str) -> Result<FolderMeta, DomainError> {
        scope.transaction(|p| Self::get_locked(p, id))
    }
    pub fn list_locked(profile: &LockedProfile<'_>) -> Result<Vec<FolderMeta>, DomainError> {
        super::journal::Journal::recover_locked(profile)?;
        folders_disk::list_folders(profile)
    }
    pub fn get_locked(profile: &LockedProfile<'_>, id: &str) -> Result<FolderMeta, DomainError> {
        super::journal::Journal::recover_locked(profile)?;
        validate_id(id)?;
        folders_disk::read_folders(profile)?
            .into_iter()
            .find(|f| f.id == id)
            .ok_or_else(|| DomainError::NotFound(id.into()))
    }
}
fn safe(profile: &LockedProfile<'_>, path: PathBuf) -> Result<PathBuf, DomainError> {
    let relative = path
        .strip_prefix(profile.scope().root())
        .map_err(|_| DomainError::InvalidScope(path.display().to_string()))?;
    profile.path(relative)
}
fn get_notes_dir(profile: &LockedProfile<'_>) -> Result<PathBuf, DomainError> {
    let path = profile.path(Path::new("notes"))?;
    safe(profile, path)
}
fn note_path(profile: &LockedProfile<'_>, id: &str) -> Result<PathBuf, DomainError> {
    validate_id(id)?;
    get_notes_dir(profile)?;
    profile.path(&Path::new("notes").join(id))
}
fn require_folder(profile: &LockedProfile<'_>, id: Option<&str>) -> Result<(), DomainError> {
    if let Some(id) = id {
        let folder = FolderRepository::get_locked(profile, id)?;
        if folder.deleted_at.is_some() {
            return Err(DomainError::NotFound(id.into()));
        }
    }
    Ok(())
}
pub(crate) mod notes_disk {
    use super::*;
    pub(super) fn read_note_meta(
        profile: &LockedProfile<'_>,
        note_dir: &PathBuf,
    ) -> Result<NoteMeta, DomainError> {
        let meta: NoteMeta =
            serde_json::from_str(&profile.read_text(safe(profile, note_dir.join("note.json"))?)?)?;
        validate_id(&meta.id)?;
        if note_dir.file_name().and_then(|n| n.to_str()) != Some(meta.id.as_str()) {
            return Err(DomainError::InvalidInput(
                "Note ID does not match directory".into(),
            ));
        }
        Ok(meta)
    }
    pub(crate) fn list_notes(profile: &LockedProfile<'_>) -> Result<Vec<NoteMeta>, DomainError> {
        let notes_dir = get_notes_dir(profile)?;

        let mut notes: Vec<NoteMeta> = Vec::new();

        if !notes_dir.exists() {
            return Ok(Vec::new());
        }
        let entries = fs::read_dir(&notes_dir)?;
        for entry in entries.flatten() {
            let path = safe(profile, entry.path())?;
            if !path.is_dir() {
                continue;
            }
            if let Ok(meta) = read_note_meta(profile, &path) {
                if is_note_active(&meta) {
                    notes.push(meta);
                }
            }
        }

        // Sort by updated_at descending
        notes.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

        Ok(notes)
    }

    pub(crate) fn read_note(
        profile: &LockedProfile<'_>,
        id: String,
    ) -> Result<NoteData, DomainError> {
        let note_dir = note_path(profile, &id)?;

        if !note_dir.exists() || !safe(profile, note_dir.join("note.json"))?.exists() {
            return Err(DomainError::NotFound(format!("Note not found: {}", id)));
        }

        let meta = read_note_meta(profile, &note_dir)?;

        let content_path = safe(profile, note_dir.join("content.html"))?;
        let content = if content_path.exists() {
            profile.read_text(&content_path)?
        } else {
            String::new()
        };

        Ok(NoteData { meta, content })
    }

    pub(crate) fn create_note(
        profile: &LockedProfile<'_>,
        folder_id: Option<String>,
    ) -> Result<NoteMeta, DomainError> {
        require_folder(profile, folder_id.as_deref())?;

        let id = uuid::Uuid::new_v4().to_string();
        let note_dir = note_path(profile, &id)?;

        let now = chrono::Utc::now().to_rfc3339();
        let meta = NoteMeta {
            id,
            title: "Untitled Note".to_string(),
            created_at: now.clone(),
            updated_at: now,
            favorite: false,
            folder_id,
            order: 0,
            deleted_at: None,
            local_only: false,
        };

        let meta_json = serde_json::to_string_pretty(&meta)?;
        profile.stage_text(safe(profile, note_dir.join("note.json"))?, meta_json)?;
        profile.stage_text(safe(profile, note_dir.join("content.html"))?, "")?;

        Ok(meta)
    }

    pub(crate) fn update_note(
        profile: &LockedProfile<'_>,
        id: String,
        content: String,
        title: String,
    ) -> Result<NoteMeta, DomainError> {
        let note_dir = note_path(profile, &id)?;

        if !note_dir.exists() {
            return Err(DomainError::NotFound(format!("Note not found: {}", id)));
        }

        // Write content
        profile.stage_text(safe(profile, note_dir.join("content.html"))?, &content)?;

        // Update metadata
        let mut meta = read_note_meta(profile, &note_dir)?;
        meta.title = title;
        meta.updated_at = chrono::Utc::now().to_rfc3339();

        let meta_json = serde_json::to_string_pretty(&meta)?;
        profile.stage_text(safe(profile, note_dir.join("note.json"))?, meta_json)?;

        Ok(meta)
    }

    pub(crate) fn delete_note(profile: &LockedProfile<'_>, id: String) -> Result<(), DomainError> {
        let note_dir = note_path(profile, &id)?;

        if !note_dir.exists() {
            return Ok(()); // idempotent
        }

        let mut meta = read_note_meta(profile, &note_dir)?;
        let now = chrono::Utc::now().to_rfc3339();
        meta.deleted_at = Some(now.clone());
        meta.updated_at = now;

        let meta_json = serde_json::to_string_pretty(&meta)?;
        profile.stage_text(safe(profile, note_dir.join("note.json"))?, meta_json)?;

        Ok(())
    }

    pub(crate) fn purge_soft_deleted_notes_post_pull(
        profile: &LockedProfile<'_>,
        note_ids: Vec<String>,
    ) -> Result<u32, DomainError> {
        for id in &note_ids {
            validate_id(id)?;
        }
        let mut purged = 0u32;
        for note_id in note_ids {
            let note_dir = note_path(profile, &note_id)?;
            if !note_dir.exists() {
                continue;
            }
            let meta = match read_note_meta(profile, &note_dir) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.deleted_at.is_some() {
                profile.stage_purge_note(&note_dir)?;
                purged += 1;
            }
        }
        Ok(purged)
    }

    pub(crate) fn toggle_note_favorite(
        profile: &LockedProfile<'_>,
        id: String,
    ) -> Result<NoteMeta, DomainError> {
        let note_dir = note_path(profile, &id)?;

        if !note_dir.exists() {
            return Err(DomainError::NotFound(format!("Note not found: {}", id)));
        }

        let mut meta = read_note_meta(profile, &note_dir)?;
        meta.favorite = !meta.favorite;

        let meta_json = serde_json::to_string_pretty(&meta)?;
        profile.stage_text(safe(profile, note_dir.join("note.json"))?, meta_json)?;

        Ok(meta)
    }

    pub(crate) fn set_note_local_only(
        profile: &LockedProfile<'_>,
        id: String,
        local_only: bool,
    ) -> Result<NoteMeta, DomainError> {
        let note_dir = note_path(profile, &id)?;

        if !note_dir.exists() {
            return Err(DomainError::NotFound(format!("Note not found: {}", id)));
        }

        let mut meta = read_note_meta(profile, &note_dir)?;
        meta.local_only = local_only;
        if !local_only {
            meta.updated_at = chrono::Utc::now().to_rfc3339();
        }

        let meta_json = serde_json::to_string_pretty(&meta)?;
        profile.stage_text(safe(profile, note_dir.join("note.json"))?, meta_json)?;

        Ok(meta)
    }

    pub(crate) fn move_note_to_folder(
        profile: &LockedProfile<'_>,
        note_id: String,
        folder_id: Option<String>,
    ) -> Result<NoteMeta, DomainError> {
        require_folder(profile, folder_id.as_deref())?;
        let note_dir = note_path(profile, &note_id)?;

        if !note_dir.exists() {
            return Err(DomainError::NotFound(format!(
                "Note not found: {}",
                note_id
            )));
        }

        let mut meta = read_note_meta(profile, &note_dir)?;
        meta.folder_id = folder_id;
        meta.updated_at = chrono::Utc::now().to_rfc3339();
        // Reset order so the moved note sits at the top of its new folder until
        // the user reorders that folder explicitly.
        meta.order = 0;

        let meta_json = serde_json::to_string_pretty(&meta)?;
        profile.stage_text(safe(profile, note_dir.join("note.json"))?, meta_json)?;

        Ok(meta)
    }

    pub(crate) fn reorder_notes_in_folder(
        profile: &LockedProfile<'_>,
        folder_id: Option<String>,
        note_ids: Vec<String>,
    ) -> Result<(), DomainError> {
        require_folder(profile, folder_id.as_deref())?;
        for id in &note_ids {
            validate_id(id)?;
        }

        for (index, note_id) in note_ids.iter().enumerate() {
            let note_dir = note_path(profile, note_id)?;
            if !note_dir.exists() {
                continue;
            }
            let mut meta = match read_note_meta(profile, &note_dir) {
                Ok(m) => m,
                Err(_) => continue,
            };
            // Defensive: only update if the note is actually in the declared folder.
            if meta.folder_id != folder_id {
                continue;
            }
            meta.order = index as i32;
            let meta_json = serde_json::to_string_pretty(&meta)?;
            profile.stage_text(safe(profile, note_dir.join("note.json"))?, meta_json)?;
        }

        Ok(())
    }

    pub(crate) fn orphan_notes_in_folder(
        profile: &LockedProfile<'_>,
        folder_id: &str,
    ) -> Result<(), DomainError> {
        let notes_dir = get_notes_dir(profile)?;

        if !notes_dir.exists() {
            return Ok(());
        }
        let entries = fs::read_dir(&notes_dir)?;
        for entry in entries.flatten() {
            let path = safe(profile, entry.path())?;
            if !path.is_dir() {
                continue;
            }

            let mut meta = match read_note_meta(profile, &path) {
                Ok(m) => m,
                Err(_) => continue,
            };

            // Soft-deleted notes keep their folder_id intact (tombstone preservation).
            if !is_note_active(&meta) {
                continue;
            }

            if meta.folder_id.as_deref() == Some(folder_id) {
                meta.folder_id = None;
                let meta_json = serde_json::to_string_pretty(&meta)?;
                profile.stage_text(safe(profile, path.join("note.json"))?, meta_json)?;
            }
        }

        Ok(())
    }

    pub(crate) fn get_backlinks(
        profile: &LockedProfile<'_>,
        note_id: String,
    ) -> Result<Vec<NoteMeta>, DomainError> {
        let notes_dir = get_notes_dir(profile)?;
        let needle = format!("data-note-id=\"{}\"", note_id);

        let mut results: Vec<NoteMeta> = Vec::new();

        if !notes_dir.exists() {
            return Ok(Vec::new());
        }
        let entries = fs::read_dir(&notes_dir)?;
        for entry in entries.flatten() {
            let path = safe(profile, entry.path())?;
            if !path.is_dir() {
                continue;
            }

            // Skip the note itself — self-references don't count as backlinks.
            if path.file_name().and_then(|n| n.to_str()) == Some(note_id.as_str()) {
                continue;
            }

            let content_path = safe(profile, path.join("content.html"))?;
            let html = match profile.read_text(&content_path) {
                Ok(h) => h,
                Err(_) => continue,
            };

            if html.contains(&needle) {
                if let Ok(meta) = read_note_meta(profile, &path) {
                    if is_note_active(&meta) {
                        results.push(meta);
                    }
                }
            }
        }

        results.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

        Ok(results)
    }

    pub(crate) fn import_note_for_backup(
        profile: &LockedProfile<'_>,
        meta: NoteMeta,
        content: String,
    ) -> Result<(), DomainError> {
        if meta.deleted_at.is_none() {
            require_folder(profile, meta.folder_id.as_deref())?;
        }
        let note_dir = note_path(profile, &meta.id)?;

        let meta_json = serde_json::to_string_pretty(&meta)?;
        profile.stage_text(safe(profile, note_dir.join("note.json"))?, meta_json)?;
        profile.stage_text(safe(profile, note_dir.join("content.html"))?, content)?;
        Ok(())
    }

    pub(crate) fn search_notes(
        profile: &LockedProfile<'_>,
        query: String,
    ) -> Result<Vec<NoteMeta>, DomainError> {
        let notes_dir = get_notes_dir(profile)?;
        let lower_query = query.to_lowercase();

        let mut results: Vec<NoteMeta> = Vec::new();

        if !notes_dir.exists() {
            return Ok(Vec::new());
        }
        let entries = fs::read_dir(&notes_dir)?;
        for entry in entries.flatten() {
            let path = safe(profile, entry.path())?;
            if !path.is_dir() {
                continue;
            }

            if let Ok(meta) = read_note_meta(profile, &path) {
                if !is_note_active(&meta) {
                    continue;
                }

                let title_match = meta.title.to_lowercase().contains(&lower_query);

                let content_match = if !title_match {
                    let content_path = safe(profile, path.join("content.html"))?;
                    if let Ok(html) = profile.read_text(&content_path) {
                        let text = strip_html_tags(&html);
                        text.to_lowercase().contains(&lower_query)
                    } else {
                        false
                    }
                } else {
                    false
                };

                if title_match || content_match {
                    results.push(meta);
                }
            }
        }

        results.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

        Ok(results)
    }
}

pub(crate) mod folders_disk {
    use super::*;
    fn get_folders_file(profile: &LockedProfile<'_>) -> Result<PathBuf, DomainError> {
        get_notes_dir(profile)?;
        profile.path(Path::new("notes/folders.json"))
    }
    pub(crate) fn read_folders(
        profile: &LockedProfile<'_>,
    ) -> Result<Vec<FolderMeta>, DomainError> {
        let path = get_folders_file(profile)?;
        if !path.exists() {
            return Ok(Vec::new());
        }
        let content = profile.read_text(path)?;
        if content.trim().is_empty() {
            return Ok(Vec::new());
        }
        let folders: Vec<FolderMeta> = serde_json::from_str(&content)?;
        for folder in &folders {
            validate_id(&folder.id)?;
        }
        Ok(folders)
    }
    fn write_folders(
        profile: &LockedProfile<'_>,
        folders: &[FolderMeta],
    ) -> Result<(), DomainError> {
        profile.stage_text(
            get_folders_file(profile)?,
            serde_json::to_string_pretty(folders)?,
        )?;
        Ok(())
    }
    pub(crate) fn list_folders(
        profile: &LockedProfile<'_>,
    ) -> Result<Vec<FolderMeta>, DomainError> {
        let mut folders = read_folders(profile)?;
        let orders_migrated = migrate_folder_orders_if_needed(&mut folders);
        let updated_at_migrated = migrate_folder_updated_at_if_needed(&mut folders);
        if orders_migrated || updated_at_migrated {
            write_folders(profile, &folders)?;
        }
        folders.retain(is_folder_active);
        folders.sort_by(|a, b| {
            a.order
                .cmp(&b.order)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        Ok(folders)
    }

    pub(crate) fn create_folder(
        profile: &LockedProfile<'_>,
        name: String,
        icon: Option<String>,
    ) -> Result<FolderMeta, DomainError> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(DomainError::InvalidInput(
                "Folder name cannot be empty".into(),
            ));
        }

        let mut folders = read_folders(profile)?;
        migrate_folder_orders_if_needed(&mut folders);

        let next_order = folders.iter().map(|f| f.order).max().unwrap_or(-1) + 1;

        let now = chrono::Utc::now().to_rfc3339();
        let meta = FolderMeta {
            id: uuid::Uuid::new_v4().to_string(),
            name: trimmed.to_string(),
            icon: normalize_icon(icon),
            created_at: now.clone(),
            updated_at: now,
            order: next_order,
            deleted_at: None,
        };

        folders.push(meta.clone());
        write_folders(profile, &folders)?;

        Ok(meta)
    }

    pub(crate) fn reorder_folders(
        profile: &LockedProfile<'_>,
        ids: Vec<String>,
    ) -> Result<(), DomainError> {
        for id in &ids {
            validate_id(id)?;
        }
        let mut folders = read_folders(profile)?;

        let id_to_index: std::collections::HashMap<String, usize> =
            ids.into_iter().enumerate().map(|(i, id)| (id, i)).collect();

        let now = chrono::Utc::now().to_rfc3339();

        // Folders not in the input list keep a stable position after the reordered ones.
        let mut max_ordered = id_to_index.len() as i32;
        for folder in folders.iter_mut() {
            if let Some(&idx) = id_to_index.get(&folder.id) {
                folder.order = idx as i32;
                // Bulk reorder is a coupled mutation: bump every folder explicitly
                // listed in the input so LWW sync picks up the new ordering.
                folder.updated_at = now.clone();
            } else {
                folder.order = max_ordered;
                max_ordered += 1;
            }
        }

        write_folders(profile, &folders)?;
        Ok(())
    }

    pub(crate) fn rename_folder(
        profile: &LockedProfile<'_>,
        id: String,
        name: String,
        icon: Option<String>,
    ) -> Result<FolderMeta, DomainError> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(DomainError::InvalidInput(
                "Folder name cannot be empty".into(),
            ));
        }

        let mut folders = read_folders(profile)?;

        let folder = folders
            .iter_mut()
            .find(|f| f.id == id)
            .ok_or_else(|| DomainError::NotFound(format!("Folder not found: {}", id)))?;
        folder.name = trimmed.to_string();
        folder.icon = normalize_icon(icon);
        folder.updated_at = chrono::Utc::now().to_rfc3339();
        let updated = folder.clone();

        write_folders(profile, &folders)?;

        Ok(updated)
    }

    pub(crate) fn delete_folder(
        profile: &LockedProfile<'_>,
        id: String,
    ) -> Result<(), DomainError> {
        let mut folders = read_folders(profile)?;
        let now = chrono::Utc::now().to_rfc3339();
        let mut found = false;
        for folder in folders.iter_mut() {
            if folder.id == id {
                folder.deleted_at = Some(now.clone());
                folder.updated_at = now.clone();
                found = true;
                break;
            }
        }
        if !found {
            return Ok(());
        }
        write_folders(profile, &folders)?;

        // Existing semantics: notes in this folder become orphans (folder_id reset to None).
        notes_disk::orphan_notes_in_folder(profile, &id)?;

        Ok(())
    }

    pub(crate) fn import_folders_for_backup(
        profile: &LockedProfile<'_>,
        folders: Vec<FolderMeta>,
    ) -> Result<(), DomainError> {
        for folder in &folders {
            validate_id(&folder.id)?;
        }
        write_folders(profile, &folders)?;
        Ok(())
    }

    pub(crate) fn purge_soft_deleted_folders_post_pull(
        profile: &LockedProfile<'_>,
        folder_ids: Vec<String>,
    ) -> Result<u32, DomainError> {
        for id in &folder_ids {
            validate_id(id)?;
        }
        if folder_ids.is_empty() {
            return Ok(0);
        }
        let mut folders = read_folders(profile)?;
        let before = folders.len();
        folders.retain(|f| !(folder_ids.contains(&f.id) && f.deleted_at.is_some()));
        let purged = (before - folders.len()) as u32;
        if purged > 0 {
            write_folders(profile, &folders)?;
        }
        Ok(purged)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::note_service::{
        scope::tests::{meta, Fixture, ID},
        Actor, MutationIntent, NoteService,
    };
    #[test]
    fn same_note_ids_are_isolated_between_profiles() {
        let f = Fixture::new();
        let perso = f.scope("perso");
        let work = f.scope("work");
        for (scope, content) in [(&perso, "private"), (&work, "business")] {
            NoteService::mutate(
                scope,
                MutationIntent::ImportNote {
                    meta: meta(ID),
                    content: content.into(),
                },
                Actor::Ui,
            )
            .unwrap();
        }
        assert_eq!(NoteRepository::get(&perso, ID).unwrap().content, "private");
        assert_eq!(NoteRepository::get(&work, ID).unwrap().content, "business");
    }
    #[test]
    fn folder_must_belong_to_note_profile() {
        let f = Fixture::new();
        let perso = f.scope("perso");
        let work = f.scope("work");
        let result = NoteService::mutate(
            &perso,
            MutationIntent::CreateFolder {
                name: "Private".into(),
                icon: None,
            },
            Actor::Ui,
        )
        .unwrap();
        let crate::note_service::MutationResult::Folder(folder) = result else {
            panic!("expected folder")
        };
        assert!(NoteService::mutate(
            &work,
            MutationIntent::CreateNote {
                folder_id: Some(folder.id)
            },
            Actor::Ui
        )
        .is_err());
        assert!(NoteRepository::list(&work).unwrap().is_empty());
    }
    #[test]
    fn disk_mutations_preserve_folder_icon_migration_and_note_lifecycle() {
        let f = Fixture::new();
        let scope = f.scope("work");
        let folders = vec![
            FolderMeta {
                id: "legacy-z".into(),
                name: "Zulu".into(),
                icon: Some("🧪".into()),
                created_at: "2026-01-01".into(),
                updated_at: String::new(),
                order: 0,
                deleted_at: None,
            },
            FolderMeta {
                id: "legacy-a".into(),
                name: "Alpha".into(),
                icon: None,
                created_at: "2026-01-02".into(),
                updated_at: String::new(),
                order: 0,
                deleted_at: None,
            },
        ];
        NoteService::mutate(&scope, MutationIntent::ImportFolders { folders }, Actor::Ui).unwrap();
        let migrated = FolderRepository::list(&scope).unwrap();
        assert_eq!(
            migrated.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(),
            vec!["legacy-a", "legacy-z"]
        );
        assert_eq!(migrated[0].updated_at, "2026-01-02");
        assert_eq!(
            FolderRepository::get(&scope, "legacy-z")
                .unwrap()
                .icon
                .as_deref(),
            Some("🧪")
        );
        let folder = NoteService::mutate(
            &scope,
            MutationIntent::CreateFolder {
                name: "  Projects  ".into(),
                icon: Some(" 💼 ".into()),
            },
            Actor::Ui,
        )
        .unwrap()
        .folder()
        .unwrap();
        assert_eq!(folder.name, "Projects");
        assert_eq!(folder.icon.as_deref(), Some("💼"));
        let note = NoteService::mutate(
            &scope,
            MutationIntent::CreateNote {
                folder_id: Some(folder.id.clone()),
            },
            Actor::Ui,
        )
        .unwrap()
        .note()
        .unwrap();
        NoteService::mutate(
            &scope,
            MutationIntent::UpdateNote {
                id: note.id.clone(),
                content: "<p>Hello</p>".into(),
                title: "Project".into(),
            },
            Actor::Ui,
        )
        .unwrap();
        NoteService::mutate(
            &scope,
            MutationIntent::ToggleFavorite {
                id: note.id.clone(),
            },
            Actor::Ui,
        )
        .unwrap();
        NoteService::mutate(
            &scope,
            MutationIntent::SetLocalOnly {
                id: note.id.clone(),
                local_only: true,
            },
            Actor::Ui,
        )
        .unwrap();
        NoteService::mutate(
            &scope,
            MutationIntent::ReorderNotes {
                folder_id: Some(folder.id.clone()),
                note_ids: vec![note.id.clone()],
            },
            Actor::Ui,
        )
        .unwrap();
        let loaded = NoteRepository::get(&scope, &note.id).unwrap();
        assert_eq!(loaded.content, "<p>Hello</p>");
        assert!(loaded.meta.favorite);
        assert!(loaded.meta.local_only);
        assert_eq!(
            NoteRepository::search(&scope, "hello".into())
                .unwrap()
                .len(),
            1
        );
        let mut tombstone = meta(ID);
        tombstone.folder_id = Some(folder.id.clone());
        tombstone.deleted_at = Some("deleted".into());
        NoteService::mutate(
            &scope,
            MutationIntent::ImportNote {
                meta: tombstone,
                content: "old".into(),
            },
            Actor::Ui,
        )
        .unwrap();
        NoteService::mutate(
            &scope,
            MutationIntent::DeleteFolder {
                id: folder.id.clone(),
            },
            Actor::Ui,
        )
        .unwrap();
        assert!(NoteRepository::get(&scope, &note.id)
            .unwrap()
            .meta
            .folder_id
            .is_none());
        assert_eq!(
            NoteRepository::get(&scope, ID)
                .unwrap()
                .meta
                .folder_id
                .as_deref(),
            Some(folder.id.as_str())
        );
        assert_eq!(
            NoteService::mutate(
                &scope,
                MutationIntent::PurgeFolders {
                    folder_ids: vec![folder.id]
                },
                Actor::Ui
            )
            .unwrap()
            .count()
            .unwrap(),
            1
        );
        NoteService::mutate(
            &scope,
            MutationIntent::DeleteNote {
                id: note.id.clone(),
            },
            Actor::Ui,
        )
        .unwrap();
        assert!(NoteRepository::list(&scope).unwrap().is_empty());
        assert_eq!(
            NoteService::mutate(
                &scope,
                MutationIntent::PurgeNotes {
                    note_ids: vec![note.id.clone()]
                },
                Actor::Ui
            )
            .unwrap()
            .count()
            .unwrap(),
            1
        );
        assert!(NoteRepository::get(&scope, &note.id).is_err());
        assert_eq!(NoteRepository::get(&scope, ID).unwrap().content, "old");
    }
    #[test]
    fn active_import_rejects_foreign_folder_but_tombstone_preserves_history() {
        let f = Fixture::new();
        let scope = f.scope("work");
        let mut note = meta(ID);
        note.folder_id = Some("missing-folder".into());
        assert!(NoteService::mutate(
            &scope,
            MutationIntent::ImportNote {
                meta: note.clone(),
                content: "no".into()
            },
            Actor::Ui
        )
        .is_err());
        assert!(!scope.root().join("notes").join(ID).exists());
        note.deleted_at = Some("deleted".into());
        NoteService::mutate(
            &scope,
            MutationIntent::ImportNote {
                meta: note,
                content: "historical".into(),
            },
            Actor::Ui,
        )
        .unwrap();
        assert_eq!(
            NoteRepository::get(&scope, ID)
                .unwrap()
                .meta
                .folder_id
                .as_deref(),
            Some("missing-folder")
        );
    }
}
