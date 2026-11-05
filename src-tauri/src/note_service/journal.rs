//! Durable redo journal. Only typed, confined targets can be committed or replayed.
use super::{DomainError, LockedProfile, ProfileScope};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, io::Write, path::Path};

pub const FORMAT: u32 = 1;
const MAX_JOURNAL_BYTES: u64 = 536_870_912;
pub const RETENTION_SECONDS: i64 = 30 * 24 * 60 * 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Revision(String);
impl Revision {
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub(crate) fn fresh() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct State {
    pub format: u32,
    pub collection: Revision,
    pub entities: BTreeMap<String, Revision>,
    /// Latest local privacy preference survives tombstone/content purge.
    /// Absent entries in legacy state mean unknown, not publicly syncable.
    #[serde(default)]
    pub note_privacy: BTreeMap<String, bool>,
    pub sequence: u64,
}
impl Default for State {
    fn default() -> Self {
        Self {
            format: FORMAT,
            collection: Revision::fresh(),
            entities: BTreeMap::new(),
            note_privacy: BTreeMap::new(),
            sequence: 0,
        }
    }
}
pub struct RevisionStore;
impl RevisionStore {
    /// One checked state snapshot for bounded read indexes. Allocate missing
    /// legacy note revisions in a single durable metadata transaction.
    pub fn note_snapshot_locked(
        p: &LockedProfile<'_>,
        ensure_ids: &[String],
    ) -> Result<(Revision, BTreeMap<String, Revision>), DomainError> {
        for id in ensure_ids {
            super::scope::validate_id(id)?;
        }
        let mut state = Self::load(p)?;
        let mut changed = false;
        for id in ensure_ids {
            let key = format!("note:{id}");
            if !state.entities.contains_key(&key) {
                state.entities.insert(key, Revision::fresh());
                changed = true;
            }
        }
        if changed || !p.path(Path::new("note-service/state.json"))?.exists() {
            Journal::commit(
                p,
                vec![json_change("note-service/state.json".into(), &state)?],
            )?;
        }
        Ok((
            state.collection,
            state
                .entities
                .into_iter()
                .filter_map(|(key, revision)| {
                    key.strip_prefix("note:")
                        .map(|id| (id.to_string(), revision))
                })
                .collect(),
        ))
    }
    pub(crate) fn load(p: &LockedProfile<'_>) -> Result<State, DomainError> {
        Journal::recover_locked(p)?;
        let state: State = read_json(p, "note-service/state.json")?.unwrap_or_default();
        if state.format != FORMAT {
            return Err(DomainError::FormatUnsupported);
        }
        Ok(state)
    }
    #[cfg(test)]
    pub fn note(scope: &ProfileScope, id: &str) -> Result<Revision, DomainError> {
        scope.transaction(|p| Self::note_locked(p, id))
    }
    #[cfg(test)]
    pub fn folder(scope: &ProfileScope, id: &str) -> Result<Revision, DomainError> {
        scope.transaction(|p| Self::folder_locked(p, id))
    }
    #[cfg(test)]
    pub fn collection(scope: &ProfileScope) -> Result<Revision, DomainError> {
        scope.transaction(|p| Self::collection_locked(p))
    }
    pub fn note_locked(p: &LockedProfile<'_>, id: &str) -> Result<Revision, DomainError> {
        Self::entity(p, "note", id)
    }
    pub fn folder_locked(p: &LockedProfile<'_>, id: &str) -> Result<Revision, DomainError> {
        Self::entity(p, "folder", id)
    }
    pub fn collection_locked(p: &LockedProfile<'_>) -> Result<Revision, DomainError> {
        let state = Self::load(p)?;
        if !p.path(Path::new("note-service/state.json"))?.exists() {
            Journal::commit(
                p,
                vec![json_change("note-service/state.json".into(), &state)?],
            )?;
        }
        Ok(state.collection)
    }
    fn entity(p: &LockedProfile<'_>, kind: &str, id: &str) -> Result<Revision, DomainError> {
        super::scope::validate_id(id)?;
        let mut state = Self::load(p)?;
        let key = format!("{kind}:{id}");
        if let Some(revision) = state.entities.get(&key) {
            return Ok(revision.clone());
        }
        let revision = Revision::fresh();
        state.entities.insert(key, revision.clone());
        Journal::commit(
            p,
            vec![json_change("note-service/state.json".into(), &state)?],
        )?;
        Ok(revision)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct FileChange {
    pub relative: String,
    pub content: Option<String>,
}
#[derive(Debug, Serialize, Deserialize)]
struct Intent {
    format: u32,
    profile_id: String,
    files: Vec<FileChange>,
}

/// Injectable file boundary, used by crash tests and by the production atomic writer.
pub(crate) trait DurableIo {
    fn apply(&mut self, p: &LockedProfile<'_>, change: &FileChange) -> Result<(), DomainError>;
}
pub(crate) struct AtomicIo;
impl DurableIo for AtomicIo {
    fn apply(&mut self, p: &LockedProfile<'_>, change: &FileChange) -> Result<(), DomainError> {
        validate_target(&change.relative)?;
        if let Some(content) = &change.content {
            atomic_text(p, &change.relative, content)
        } else {
            let path = p.path(Path::new(&change.relative))?;
            match fs::remove_file(path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e.into()),
            }
        }
    }
}
pub struct Journal;
impl Journal {
    pub fn recover_profile(scope: &ProfileScope) -> Result<(), DomainError> {
        scope.transaction(|_| Ok(()))
    }
    pub(crate) fn recover_locked(p: &LockedProfile<'_>) -> Result<(), DomainError> {
        let format: Option<u32> = read_json(p, "note-service/format.json")?;
        if format.is_some_and(|f| f != FORMAT) {
            return Err(DomainError::FormatUnsupported);
        }
        let path = p.path(Path::new("note-service/pending.json"))?;
        if !path.exists() {
            return Ok(());
        }
        if fs::metadata(&path)?.len() > MAX_JOURNAL_BYTES {
            return Err(DomainError::RecoveryRequired);
        }
        let intent: Intent = serde_json::from_str(&fs::read_to_string(path)?)
            .map_err(|_| DomainError::RecoveryRequired)?;
        if intent.format != FORMAT {
            return Err(DomainError::FormatUnsupported);
        }
        if intent.profile_id != p.scope().local_id() {
            return Err(DomainError::RecoveryRequired);
        }
        for file in &intent.files {
            validate_target(&file.relative).map_err(|_| DomainError::RecoveryRequired)?;
        }
        if intent.files.is_empty() {
            return Ok(());
        }
        Self::apply(p, &intent, &mut AtomicIo).map_err(|_| DomainError::RecoveryRequired)
    }
    pub(crate) fn commit(p: &LockedProfile<'_>, files: Vec<FileChange>) -> Result<(), DomainError> {
        Self::commit_with(p, files, &mut AtomicIo)
    }
    pub(crate) fn commit_with(
        p: &LockedProfile<'_>,
        files: Vec<FileChange>,
        io: &mut impl DurableIo,
    ) -> Result<(), DomainError> {
        for file in &files {
            validate_target(&file.relative)?;
            p.path(Path::new(&file.relative))?;
        }
        let intent = Intent {
            format: FORMAT,
            profile_id: p.scope().local_id().into(),
            files,
        };
        let json = serde_json::to_string(&intent)?;
        if json.len() as u64 > MAX_JOURNAL_BYTES {
            return Err(DomainError::InvalidInput(
                "Transaction exceeds journal budget".into(),
            ));
        }
        atomic_json(p, "note-service/format.json", &FORMAT)?;
        io.apply(
            p,
            &FileChange {
                relative: "note-service/pending.json".into(),
                content: Some(json),
            },
        )?;
        Self::apply(p, &intent, io).map_err(|_| DomainError::RecoveryRequired)
    }
    fn apply(
        p: &LockedProfile<'_>,
        intent: &Intent,
        io: &mut impl DurableIo,
    ) -> Result<(), DomainError> {
        for change in &intent.files {
            io.apply(p, change)?;
        }
        // A durable empty marker, rather than an unsynced unlink, closes the WAL.
        // Power loss must not resurrect a prior intent after a later queue ack.
        io.apply(
            p,
            &FileChange {
                relative: "note-service/pending.json".into(),
                content: Some(serde_json::to_string(&Intent {
                    format: FORMAT,
                    profile_id: p.scope().local_id().into(),
                    files: Vec::new(),
                })?),
            },
        )
    }
    /// Receipts older than retention may expire; unacknowledged outbox entries are never compacted here.
    pub fn compact(scope: &ProfileScope) -> Result<(), DomainError> {
        scope.transaction(|p| {
            for path in files_in(p, "note-service/receipts")? {
                let receipt: super::mutation::StoredReceipt =
                    read_json(p, &path)?.ok_or(DomainError::RecoveryRequired)?;
                if receipt.created_at + RETENTION_SECONDS < chrono::Utc::now().timestamp() {
                    fs::remove_file(p.path(Path::new(&path))?)?;
                }
            }
            super::versions::VersionStore::expire_locked(p)
        })
    }
}
fn validate_target(relative: &str) -> Result<(), DomainError> {
    let c: Vec<_> = relative.split('/').collect();
    match c.as_slice() {
        ["notes", "folders.json"] => Ok(()),
        ["notes", id, "note.json" | "content.html"] => super::scope::validate_id(id),
        ["note-service", "state.json" | "pending.json" | "outbox-ack.json" | "outbox-seeded.json"] => {
            Ok(())
        }
        ["note-service", "receipts" | "versions", filename] => {
            let id = filename
                .strip_suffix(".json")
                .ok_or_else(|| DomainError::InvalidInput("Invalid journal target".into()))?;
            uuid::Uuid::parse_str(id)
                .map_err(|_| DomainError::InvalidInput("Invalid journal UUID".into()))?;
            Ok(())
        }
        ["note-service", "outbox", filename]
            if filename
                .strip_suffix(".json")
                .is_some_and(|s| s.len() == 20 && s.bytes().all(|b| b.is_ascii_digit())) =>
        {
            Ok(())
        }
        _ => Err(DomainError::InvalidInput("Invalid journal target".into())),
    }
}
pub(crate) fn atomic_text(
    p: &LockedProfile<'_>,
    relative: &str,
    text: &str,
) -> Result<(), DomainError> {
    let path = p.path(Path::new(relative))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        if relative.starts_with("note-service/") {
            let root = p.path(Path::new("note-service"))?;
            protect_private_directory(&root)?;
            protect_private_directory(parent)?;
        }
    }
    let path = p.path(Path::new(relative))?;
    atomicwrites::AtomicFile::new(&path, atomicwrites::AllowOverwrite)
        .write(|file| {
            file.write_all(text.as_bytes())?;
            file.sync_all()
        })
        .map_err(|e| DomainError::Storage(format!("Atomic write failed: {e}")))
}
fn protect_private_directory(path: &Path) -> Result<(), DomainError> {
    #[cfg(windows)]
    lexena_mcp_ipc::protect_path(path)
        .map_err(|e| DomainError::Storage(format!("Private storage ACL failed: {e}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
pub(crate) fn atomic_json(
    p: &LockedProfile<'_>,
    relative: &str,
    data: &impl Serialize,
) -> Result<(), DomainError> {
    atomic_text(p, relative, &serde_json::to_string(data)?)
}
pub(crate) fn json_change(
    relative: String,
    data: &impl Serialize,
) -> Result<FileChange, DomainError> {
    Ok(FileChange {
        relative,
        content: Some(serde_json::to_string(data)?),
    })
}
pub(crate) fn read_json<T: serde::de::DeserializeOwned>(
    p: &LockedProfile<'_>,
    relative: &str,
) -> Result<Option<T>, DomainError> {
    let path = p.path(Path::new(relative))?;
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(serde_json::from_str(&text)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
pub(crate) fn files_in(p: &LockedProfile<'_>, relative: &str) -> Result<Vec<String>, DomainError> {
    let dir = p.path(Path::new(relative))?;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut result = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| DomainError::RecoveryRequired)?;
        let path = format!("{relative}/{name}");
        p.path(Path::new(&path))?;
        if name.ends_with(".json") {
            result.push(path);
        }
    }
    result.sort();
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::note_service::{
        scope::tests::{meta, Fixture, ID},
        Actor, MutationIntent, NoteRepository, NoteService,
    };
    #[test]
    fn unsupported_format_and_untrusted_journal_paths_fail_closed() {
        let f = Fixture::new();
        let scope = f.scope("work");
        fs::create_dir_all(scope.root().join("note-service")).unwrap();
        fs::write(scope.root().join("note-service/format.json"), "999").unwrap();
        assert!(matches!(
            NoteRepository::list(&scope),
            Err(DomainError::FormatUnsupported)
        ));
        fs::write(scope.root().join("note-service/format.json"), "1").unwrap();
        let intent = serde_json::json!({"format":1,"profile_id":"work","files":[{"relative":"../escape","content":"bad"}]});
        fs::write(
            scope.root().join("note-service/pending.json"),
            serde_json::to_vec(&intent).unwrap(),
        )
        .unwrap();
        assert!(matches!(
            NoteRepository::list(&scope),
            Err(DomainError::RecoveryRequired)
        ));
        assert!(!scope.root().parent().unwrap().join("escape").exists());
    }
    #[test]
    fn remote_import_invalidates_revision_without_outbox_loop_and_compaction_keeps_intents() {
        let f = Fixture::new();
        let scope = f.scope("work");
        let initial = RevisionStore::note(&scope, ID).unwrap();
        let collection = RevisionStore::collection(&scope).unwrap();
        NoteService::mutate(
            &scope,
            MutationIntent::ImportNote {
                meta: meta(ID),
                content: "remote".into(),
            },
            Actor::Remote,
        )
        .unwrap();
        assert_ne!(initial, RevisionStore::note(&scope, ID).unwrap());
        assert_ne!(collection, RevisionStore::collection(&scope).unwrap());
        assert!(super::super::outbox::LocalOutbox::list_pending(&scope)
            .unwrap()
            .is_empty());
        NoteService::mutate(
            &scope,
            MutationIntent::ToggleFavorite { id: ID.into() },
            Actor::Ui,
        )
        .unwrap();
        let entries = super::super::outbox::LocalOutbox::list_pending(&scope).unwrap();
        assert_eq!(entries.len(), 1);
        let paths = scope
            .transaction(|p| files_in(p, "note-service/receipts"))
            .unwrap();
        Journal::compact(&scope).unwrap();
        assert_eq!(
            scope
                .transaction(|p| files_in(p, "note-service/receipts"))
                .unwrap()
                .len(),
            paths.len()
        );
        assert_eq!(
            super::super::outbox::LocalOutbox::list_pending(&scope)
                .unwrap()
                .len(),
            1
        );
        // Expiring receipts never drops the separate unacknowledged intent.
        for path in paths {
            let path = scope.root().join(path);
            let mut r: super::super::mutation::StoredReceipt =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            r.created_at = 0;
            fs::write(path, serde_json::to_vec(&r).unwrap()).unwrap();
        }
        Journal::compact(&scope).unwrap();
        assert!(scope
            .transaction(|p| files_in(p, "note-service/receipts"))
            .unwrap()
            .is_empty());
        assert_eq!(
            super::super::outbox::LocalOutbox::list_pending(&scope)
                .unwrap()
                .len(),
            1
        );
        let through = entries.last().unwrap().sequence;
        assert!(super::super::outbox::LocalOutbox::ack(&scope, through + 1).is_err());
        super::super::outbox::LocalOutbox::ack(&scope, through).unwrap();
        super::super::outbox::LocalOutbox::ack(&scope, through).unwrap();
        assert!(super::super::outbox::LocalOutbox::list_pending(&scope)
            .unwrap()
            .is_empty());
    }
}
