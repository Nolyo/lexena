use super::journal::{files_in, read_json, RETENTION_SECONDS};
use super::repository::NoteData;
#[cfg(test)]
use super::ProfileScope;
use super::{DomainError, LockedProfile};
use serde::{Deserialize, Serialize};
pub const HISTORY_BUDGET: u64 = 268_435_456;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoteVersion {
    pub snapshot_id: String,
    pub note_id: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub revision: super::journal::Revision,
    pub note: NoteData,
}
pub struct VersionStore;
impl VersionStore {
    #[cfg(test)]
    pub fn list(scope: &ProfileScope, note_id: &str) -> Result<Vec<NoteVersion>, DomainError> {
        scope.transaction(|p| Self::list_locked(p, note_id))
    }
    pub fn list_locked(
        p: &LockedProfile<'_>,
        note_id: &str,
    ) -> Result<Vec<NoteVersion>, DomainError> {
        super::journal::Journal::recover_locked(p)?;
        super::scope::validate_id(note_id)?;
        let mut result = Vec::new();
        for path in files_in(p, "note-service/versions")? {
            let v: NoteVersion = read_json(p, &path)?.ok_or(DomainError::RecoveryRequired)?;
            if v.note_id == note_id && v.expires_at >= chrono::Utc::now().timestamp() {
                result.push(v);
            }
        }
        result.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(result)
    }
    pub(crate) fn get_locked(p: &LockedProfile<'_>, id: &str) -> Result<NoteVersion, DomainError> {
        uuid::Uuid::parse_str(id)
            .map_err(|_| DomainError::InvalidInput("Invalid snapshot ID".into()))?;
        let v: NoteVersion = read_json(p, &format!("note-service/versions/{id}.json"))?
            .ok_or_else(|| DomainError::NotFound("snapshot".into()))?;
        if v.expires_at < chrono::Utc::now().timestamp() {
            return Err(DomainError::SnapshotExpired);
        }
        Ok(v)
    }
    pub(crate) fn expire_locked(p: &LockedProfile<'_>) -> Result<(), DomainError> {
        for path in files_in(p, "note-service/versions")? {
            let v: NoteVersion = read_json(p, &path)?.ok_or(DomainError::RecoveryRequired)?;
            if v.expires_at < chrono::Utc::now().timestamp() {
                std::fs::remove_file(p.path(std::path::Path::new(&path))?)?;
            }
        }
        Ok(())
    }
    pub(crate) fn prepare(
        p: &LockedProfile<'_>,
        note: NoteData,
        revision: super::journal::Revision,
    ) -> Result<NoteVersion, DomainError> {
        let now = chrono::Utc::now().timestamp();
        let v = NoteVersion {
            snapshot_id: uuid::Uuid::new_v4().to_string(),
            note_id: note.meta.id.clone(),
            note,
            revision,
            created_at: now,
            expires_at: now + RETENTION_SECONDS,
        };
        Self::expire_locked(p)?;
        let mut size = serde_json::to_vec(&v)?.len() as u64;
        for path in files_in(p, "note-service/versions")? {
            size =
                size.saturating_add(std::fs::metadata(p.path(std::path::Path::new(&path))?)?.len());
        }
        if size > HISTORY_BUDGET {
            return Err(DomainError::HistoryFull);
        }
        Ok(v)
    }
}
