use super::journal::{atomic_json, files_in, read_json};
#[cfg(test)]
use super::ProfileScope;
use super::{DomainError, LockedProfile};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncState {
    DeferredProfileInactive,
    Pending,
    Disabled,
    LocalOnly,
    TooLarge,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboxEntity {
    pub kind: String,
    pub id: String,
    pub revision: super::journal::Revision,
    pub deleted: bool,
    pub sync_state: SyncState,
    /// Privacy transition: delete the cloud copy without deleting the local note.
    #[serde(default)]
    pub cloud_delete: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::note_service::{
        mutation::{Actor, MutationIntent, MutationRequest, NoteService},
        scope::tests::{meta, Fixture, ID},
    };
    #[test]
    fn resolve_uses_latest_state_and_retains_cloud_delete_across_private_edits() {
        let f = Fixture::new();
        let scope = f.scope("work");
        NoteService::mutate(
            &scope,
            MutationIntent::ImportNote {
                meta: meta(ID),
                content: "public old".into(),
            },
            Actor::Ui,
        )
        .unwrap();
        NoteService::mutate(
            &scope,
            MutationIntent::SetLocalOnly {
                id: ID.into(),
                local_only: true,
            },
            Actor::Ui,
        )
        .unwrap();
        NoteService::mutate(
            &scope,
            MutationIntent::UpdateNote {
                id: ID.into(),
                content: "private secret".into(),
                title: "private".into(),
            },
            Actor::Ui,
        )
        .unwrap();
        let batch = scope
            .transaction(|p| LocalOutbox::resolve_locked(p, 0, 100))
            .unwrap();
        assert_eq!(batch.entities.len(), 1);
        assert!(batch.entities[0].cloud_delete);
        assert!(batch.entities[0].note.is_none());
        assert!(!serde_json::to_string(&batch)
            .unwrap()
            .contains("private secret"));
        NoteService::mutate(
            &scope,
            MutationIntent::SetLocalOnly {
                id: ID.into(),
                local_only: false,
            },
            Actor::Ui,
        )
        .unwrap();
        let batch = scope
            .transaction(|p| LocalOutbox::resolve_locked(p, 0, 100))
            .unwrap();
        assert!(!batch.entities[0].cloud_delete);
        assert_eq!(
            batch.entities[0].note.as_ref().unwrap().content,
            "private secret"
        );
        NoteService::mutate(
            &scope,
            MutationIntent::DeleteNote { id: ID.into() },
            Actor::Ui,
        )
        .unwrap();
        let batch = scope
            .transaction(|p| LocalOutbox::resolve_locked(p, 0, 100))
            .unwrap();
        assert!(batch.entities[0].deleted);
        assert!(batch.entities[0].note.is_none());
    }
    #[test]
    fn current_profile_settings_and_utf8_size_determine_truthful_sync_states() {
        let f = Fixture::new();
        let scope = f.scope("work");
        let save = |meta, content| {
            NoteService::mutate_checked(
                &scope,
                MutationRequest {
                    operation_id: uuid::Uuid::new_v4().to_string(),
                    expected_revision: None,
                    intent: MutationIntent::ImportNote { meta, content },
                },
                Actor::Ui,
                |_| Ok(()),
            )
            .unwrap()
            .sync_state
        };
        assert_eq!(save(meta(ID), "hello".into()), SyncState::Disabled);
        std::fs::write(
            scope.root().join("sync-meta.json"),
            r#"{"enabled":true,"cloud_profile_id":"synthetic-binding"}"#,
        )
        .unwrap();
        assert_eq!(
            save(meta(ID), "hello".into()),
            SyncState::DeferredProfileInactive
        );
        crate::note_service::ProfileRegistry::switch_in(&f.0, "work").unwrap();
        assert_eq!(save(meta(ID), "hello".into()), SyncState::Pending);
        assert_eq!(save(meta(ID), "x".repeat(3_145_728)), SyncState::Pending);
        assert_eq!(save(meta(ID), "é".repeat(1_572_865)), SyncState::TooLarge);
        let mut local = meta(ID);
        local.local_only = true;
        assert_eq!(save(local, "hello".into()), SyncState::LocalOnly);
        let entries = LocalOutbox::list_pending(&scope).unwrap();
        assert!(entries
            .windows(2)
            .all(|pair| pair[0].sequence < pair[1].sequence));
        assert!(entries.iter().all(|entry| entry.profile_id == "work"));
    }
    #[test]
    fn remote_import_does_not_reenqueue_and_pending_intent_blocks_remote() {
        let f = Fixture::new();
        let scope = f.scope("work");
        NoteService::mutate(
            &scope,
            MutationIntent::ImportNote {
                meta: meta(ID),
                content: "remote".into(),
            },
            Actor::Remote,
        )
        .unwrap();
        assert!(LocalOutbox::list_pending(&scope).unwrap().is_empty());
        NoteService::mutate(
            &scope,
            MutationIntent::UpdateNote {
                id: ID.into(),
                content: "local".into(),
                title: "local".into(),
            },
            Actor::Ui,
        )
        .unwrap();
        let mut remote = meta(ID);
        remote.updated_at = "2099-01-01T00:00:00Z".into();
        assert_eq!(
            scope
                .transaction(|p| LocalOutbox::remote_note_decision_locked(p, &remote, "new remote"))
                .unwrap(),
            RemoteDecision::Defer
        );
        assert_eq!(
            super::super::NoteRepository::get(&scope, ID)
                .unwrap()
                .content,
            "local"
        );
    }
    #[test]
    fn queue_dlq_and_blocked_marker_protect_after_outbox_import_ack() {
        let f = Fixture::new();
        let scope = f.scope("work");
        for data in [
            serde_json::json!({"queue":[{"operation":{"kind":"note-delete","id":ID}}]}),
            serde_json::json!({"dead-letters":[{"operation":{"kind":"note-upsert","note":{"id":ID}}}]}),
            serde_json::json!({"_mcp_protected_entities":[{"kind":"note","id":ID,"reason":"too_large"}]}),
        ] {
            std::fs::write(
                scope.root().join("sync-queue.json"),
                serde_json::to_vec(&data).unwrap(),
            )
            .unwrap();
            assert!(scope
                .transaction(|p| LocalOutbox::has_intent_locked(p, "note", ID))
                .unwrap());
            assert!(!scope
                .transaction(|p| LocalOutbox::has_intent_locked(p, "folder", ID))
                .unwrap());
        }
        std::fs::write(scope.root().join("sync-queue.json"), "{}").unwrap();
        assert!(!scope
            .transaction(|p| LocalOutbox::has_intent_locked(p, "note", ID))
            .unwrap());
    }
    #[test]
    fn activation_seeds_existing_data_without_timestamp_or_revision_changes() {
        let f = Fixture::new();
        let scope = f.scope("work");
        NoteService::mutate(
            &scope,
            MutationIntent::ImportNote {
                meta: meta(ID),
                content: "remote".into(),
            },
            Actor::Remote,
        )
        .unwrap();
        let before = super::super::NoteRepository::get(&scope, ID).unwrap();
        let revision = super::super::journal::RevisionStore::note(&scope, ID).unwrap();
        scope.transaction(LocalOutbox::seed_locked).unwrap();
        let after = super::super::NoteRepository::get(&scope, ID).unwrap();
        assert_eq!(before.meta.updated_at, after.meta.updated_at);
        assert_eq!(before.content, after.content);
        assert_eq!(
            revision,
            super::super::journal::RevisionStore::note(&scope, ID).unwrap()
        );
        assert_eq!(LocalOutbox::list_pending(&scope).unwrap().len(), 1);
        scope.transaction(LocalOutbox::seed_locked).unwrap();
        assert_eq!(LocalOutbox::list_pending(&scope).unwrap().len(), 1);
        let through = LocalOutbox::list_pending(&scope).unwrap()[0].sequence;
        LocalOutbox::ack(&scope, through).unwrap();
        scope.transaction(LocalOutbox::seed_locked).unwrap();
        assert!(
            LocalOutbox::list_pending(&scope).unwrap().is_empty(),
            "discard/confirmation must not reseed an old intent"
        );
    }
    #[test]
    fn legacy_seed_replaces_stale_upsert_with_current_tombstone() {
        let f = Fixture::new();
        let scope = f.scope("work");
        let mut deleted = meta(ID);
        deleted.deleted_at = Some("2026-11-05T00:00:00Z".into());
        NoteService::mutate(
            &scope,
            MutationIntent::ImportNote {
                meta: deleted,
                content: "old".into(),
            },
            Actor::Remote,
        )
        .unwrap();
        std::fs::write(scope.root().join("sync-queue.json"), serde_json::to_vec(&serde_json::json!({"queue":[{"id":"legacy","operation":{"kind":"note-upsert","note":{"id":ID,"content_html":"stale"}}}]})).unwrap()).unwrap();
        let batch = scope
            .transaction(|p| {
                LocalOutbox::seed_locked(p)?;
                LocalOutbox::resolve_locked(p, 0, 100)
            })
            .unwrap();
        assert_eq!(batch.entities.len(), 1);
        assert!(batch.entities[0].deleted);
        assert!(batch.entities[0].seed);
        assert!(batch.entities[0].note.is_none());
    }
    #[test]
    fn after_confirmation_or_discard_remote_is_reconsidered_with_lww() {
        let f = Fixture::new();
        let scope = f.scope("work");
        let mut current = meta(ID);
        current.updated_at = "2026-11-05T00:00:00Z".into();
        NoteService::mutate(
            &scope,
            MutationIntent::ImportNote {
                meta: current,
                content: "local".into(),
            },
            Actor::Remote,
        )
        .unwrap();
        let local = super::super::NoteRepository::get(&scope, ID).unwrap();
        let mut older = local.meta.clone();
        older.updated_at = "2000-01-01T00:00:00Z".into();
        let mut newer = local.meta.clone();
        newer.updated_at = "2099-01-01T00:00:00Z".into();
        assert_eq!(
            scope
                .transaction(|p| LocalOutbox::remote_note_decision_locked(p, &older, "older"))
                .unwrap(),
            RemoteDecision::Skip
        );
        assert_eq!(
            scope
                .transaction(|p| LocalOutbox::remote_note_decision_locked(p, &newer, "newer"))
                .unwrap(),
            RemoteDecision::Apply
        );
    }
    #[test]
    fn remote_folder_merge_keeps_unrelated_tombstones_and_refuses_stale_resurrection() {
        let f = Fixture::new();
        let scope = f.scope("work");
        let deleted = super::super::repository::FolderMeta {
            id: ID.into(),
            name: "deleted".into(),
            icon: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-11-05T00:00:00Z".into(),
            order: 0,
            deleted_at: Some("2026-11-05T00:00:00Z".into()),
        };
        NoteService::mutate(
            &scope,
            MutationIntent::ImportFolders {
                folders: vec![deleted.clone()],
            },
            Actor::Remote,
        )
        .unwrap();
        let mut old = deleted.clone();
        old.deleted_at = None;
        old.updated_at = "2026-01-01T00:00:00Z".into();
        assert_eq!(
            scope
                .transaction(|p| Ok(LocalOutbox::merge_remote_folder_locked(p, &old)?.0))
                .unwrap(),
            RemoteDecision::Skip
        );
        let mut other = old;
        other.id = "22222222-2222-4222-8222-222222222222".into();
        let (decision, merged) = scope
            .transaction(|p| LocalOutbox::merge_remote_folder_locked(p, &other))
            .unwrap();
        assert_eq!(decision, RemoteDecision::Apply);
        let merged = merged.unwrap();
        assert_eq!(merged.len(), 2);
        assert!(merged.iter().any(|f| f.id == ID && f.deleted_at.is_some()));
    }
    #[test]
    fn pending_parent_folder_also_protects_local_note_from_remote_apply() {
        let f = Fixture::new();
        let scope = f.scope("work");
        let folder_id = "22222222-2222-4222-8222-222222222222";
        let folder = super::super::repository::FolderMeta {
            id: folder_id.into(),
            name: "Folder".into(),
            icon: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-11-05T00:00:00Z".into(),
            order: 0,
            deleted_at: None,
        };
        NoteService::mutate(
            &scope,
            MutationIntent::ImportFolders {
                folders: vec![folder],
            },
            Actor::Remote,
        )
        .unwrap();
        let mut note = meta(ID);
        note.folder_id = Some(folder_id.into());
        note.updated_at = "2026-11-05T00:00:00Z".into();
        NoteService::mutate(
            &scope,
            MutationIntent::ImportNote {
                meta: note.clone(),
                content: "local".into(),
            },
            Actor::Remote,
        )
        .unwrap();
        NoteService::mutate(
            &scope,
            MutationIntent::RenameFolder {
                id: folder_id.into(),
                name: "Changed".into(),
                icon: None,
            },
            Actor::Ui,
        )
        .unwrap();
        note.updated_at = "2099-01-01T00:00:00Z".into();
        assert_eq!(
            scope
                .transaction(|p| LocalOutbox::remote_note_decision_locked(p, &note, "new remote"))
                .unwrap(),
            RemoteDecision::Defer
        );
    }
}
/// Intent only: the importer must resolve the CURRENT entity under the same profile lock.
/// In particular, never replay an old content snapshot over a later tombstone.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboxEntry {
    pub sequence: u64,
    pub profile_id: String,
    pub operation_id: String,
    pub source: super::mutation::Actor,
    pub entities: Vec<OutboxEntity>,
    pub created_at: i64,
}
pub struct LocalOutbox;
#[derive(Debug, PartialEq)]
pub enum RemoteDecision {
    Apply,
    Skip,
    Defer,
}
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedEntity {
    pub kind: String,
    pub id: String,
    pub sequence: u64,
    pub deleted: bool,
    pub cloud_delete: bool,
    pub note: Option<super::repository::NoteData>,
    pub folder: Option<super::repository::FolderMeta>,
    pub seed: bool,
}
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedBatch {
    pub profile_id: String,
    pub through_sequence: u64,
    pub entities: Vec<ResolvedEntity>,
}
impl LocalOutbox {
    pub fn merge_remote_folder_locked(
        p: &mut LockedProfile<'_>,
        folder: &super::repository::FolderMeta,
    ) -> Result<(RemoteDecision, Option<Vec<super::repository::FolderMeta>>), DomainError> {
        if Self::has_intent_locked(p, "folder", &folder.id)? {
            return Ok((RemoteDecision::Defer, None));
        }
        let mut all = super::repository::folders_disk::read_folders(p)?;
        if let Some(local) = all.iter_mut().find(|f| f.id == folder.id) {
            let local_time = chrono::DateTime::parse_from_rfc3339(&local.updated_at)
                .map_err(|_| DomainError::InvalidInput("Invalid local timestamp".into()))?;
            let remote_time = chrono::DateTime::parse_from_rfc3339(&folder.updated_at)
                .map_err(|_| DomainError::InvalidInput("Invalid remote timestamp".into()))?;
            if local_time > remote_time
                || serde_json::to_value(&*local)? == serde_json::to_value(folder)?
            {
                return Ok((RemoteDecision::Skip, None));
            }
            *local = folder.clone();
        } else {
            if folder.deleted_at.is_some() {
                return Ok((RemoteDecision::Skip, None));
            }
            all.push(folder.clone());
        }
        Ok((RemoteDecision::Apply, Some(all)))
    }
    pub fn remote_note_decision_locked(
        p: &mut LockedProfile<'_>,
        meta: &super::repository::NoteMeta,
        content: &str,
    ) -> Result<RemoteDecision, DomainError> {
        if Self::has_intent_locked(p, "note", &meta.id)? {
            return Ok(RemoteDecision::Defer);
        }
        let local = match super::NoteRepository::get_locked(p, &meta.id) {
            Ok(note) => note,
            Err(DomainError::NotFound(_)) => {
                return Ok(if meta.deleted_at.is_some() {
                    RemoteDecision::Skip
                } else {
                    RemoteDecision::Apply
                })
            }
            Err(error) => return Err(error),
        };
        if local.meta.local_only {
            return Ok(RemoteDecision::Skip);
        }
        if let Some(parent) = local.meta.folder_id.as_deref() {
            if Self::has_intent_locked(p, "folder", parent)? {
                return Ok(RemoteDecision::Defer);
            }
        }
        let local_time = chrono::DateTime::parse_from_rfc3339(&local.meta.updated_at)
            .map_err(|_| DomainError::InvalidInput("Invalid local timestamp".into()))?;
        let remote_time = chrono::DateTime::parse_from_rfc3339(&meta.updated_at)
            .map_err(|_| DomainError::InvalidInput("Invalid remote timestamp".into()))?;
        Ok(
            if local_time > remote_time
                || (serde_json::to_value(&local.meta)? == serde_json::to_value(meta)?
                    && local.content == content)
            {
                RemoteDecision::Skip
            } else {
                RemoteDecision::Apply
            },
        )
    }
    /// Activation imports legacy disk state as intent without rewriting content,
    /// timestamps, editor revisions, or an already pending/DLQ intent.
    pub fn seed_locked(p: &mut LockedProfile<'_>) -> Result<(), DomainError> {
        use super::journal::{json_change, Journal, RevisionStore};
        if read_json::<serde_json::Value>(p, "note-service/outbox-seeded.json")?.is_some() {
            return Ok(());
        }
        let pending = Self::list_pending_locked(p)?;
        let disk: serde_json::Value =
            read_json(p, "sync-queue.json")?.unwrap_or(serde_json::json!({}));
        let legacy = |kind: &str, id: &str| {
            !pending
                .iter()
                .any(|entry| entry.entities.iter().any(|e| e.kind == kind && e.id == id))
                && ["queue", "dead-letters"].iter().any(|key| {
                    disk.get(key)
                        .and_then(|v| v.as_array())
                        .is_some_and(|entries| {
                            entries.iter().any(|entry| {
                                if entry
                                    .get("id")
                                    .and_then(|v| v.as_str())
                                    .is_some_and(|s| s.starts_with("outbox:"))
                                {
                                    return false;
                                }
                                entry.get("operation").is_some_and(|op| {
                                    op.get("kind")
                                        .and_then(|v| v.as_str())
                                        .is_some_and(|s| s.starts_with(&format!("{kind}-")))
                                        && (op.get("id").and_then(|v| v.as_str()) == Some(id)
                                            || op
                                                .get(kind)
                                                .and_then(|v| v.get("id"))
                                                .and_then(|v| v.as_str())
                                                == Some(id))
                                })
                            })
                        })
                })
        };
        let mut entities = vec![];
        for folder in super::repository::folders_disk::read_folders(p)? {
            if !Self::has_intent_locked(p, "folder", &folder.id)? || legacy("folder", &folder.id) {
                entities.push(OutboxEntity {
                    kind: "folder".into(),
                    id: folder.id.clone(),
                    revision: RevisionStore::folder_locked(p, &folder.id)?,
                    deleted: folder.deleted_at.is_some(),
                    sync_state: Self::status(p, None)?,
                    cloud_delete: false,
                });
            }
        }
        let mut notes = super::NoteRepository::list_locked(p)?;
        // Legacy queued deletes/upserts may refer to a tombstone omitted by
        // active list APIs. Resolve it too, so an old upsert cannot resurrect it.
        for key in ["queue", "dead-letters"] {
            if let Some(entries) = disk.get(key).and_then(|v| v.as_array()) {
                for entry in entries {
                    if let Some(op) = entry.get("operation") {
                        if op
                            .get("kind")
                            .and_then(|v| v.as_str())
                            .is_some_and(|s| s.starts_with("note-"))
                        {
                            if let Some(id) = op
                                .get("id")
                                .or_else(|| op.get("note").and_then(|n| n.get("id")))
                                .and_then(|v| v.as_str())
                            {
                                if !notes.iter().any(|n| n.id == id) {
                                    match super::NoteRepository::get_locked(p, id) {
                                        Ok(note) => notes.push(note.meta),
                                        Err(DomainError::NotFound(_)) => {}
                                        Err(error) => return Err(error),
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        for meta in notes {
            if !Self::has_intent_locked(p, "note", &meta.id)? || legacy("note", &meta.id) {
                let note = super::NoteRepository::get_locked(p, &meta.id)?;
                entities.push(OutboxEntity {
                    kind: "note".into(),
                    id: meta.id.clone(),
                    revision: RevisionStore::note_locked(p, &meta.id)?,
                    deleted: meta.deleted_at.is_some(),
                    sync_state: Self::status(p, Some(&note))?,
                    cloud_delete: meta.local_only,
                });
            }
        }
        if entities.is_empty() {
            return Journal::commit(
                p,
                vec![json_change(
                    "note-service/outbox-seeded.json".into(),
                    &serde_json::json!({"sequence": null}),
                )?],
            );
        }
        let mut state = RevisionStore::load(p)?;
        state.sequence = state
            .sequence
            .checked_add(1)
            .ok_or(DomainError::RecoveryRequired)?;
        let entry = OutboxEntry {
            sequence: state.sequence,
            profile_id: p.scope().local_id().into(),
            operation_id: uuid::Uuid::new_v4().to_string(),
            source: super::mutation::Actor::Ui,
            entities,
            created_at: chrono::Utc::now().timestamp(),
        };
        Journal::commit(
            p,
            vec![
                json_change("note-service/state.json".into(), &state)?,
                json_change(
                    format!("note-service/outbox/{:020}.json", state.sequence),
                    &entry,
                )?,
                json_change(
                    "note-service/outbox-seeded.json".into(),
                    &serde_json::json!({"sequence": state.sequence}),
                )?,
            ],
        )
    }
    /// Durable import does not confirm cloud delivery. Both unimported intent
    /// and imported queue/DLQ/blocked markers protect a local entity.
    pub fn has_intent_locked(
        p: &mut LockedProfile<'_>,
        kind: &str,
        id: &str,
    ) -> Result<bool, DomainError> {
        if Self::list_pending_locked(p)?.iter().any(|entry| {
            entry
                .entities
                .iter()
                .any(|entity| entity.kind == kind && entity.id == id)
        }) {
            return Ok(true);
        }
        let disk: serde_json::Value =
            read_json(p, "sync-queue.json")?.unwrap_or(serde_json::json!({}));
        for key in ["queue", "dead-letters"] {
            if let Some(entries) = disk.get(key).and_then(|v| v.as_array()) {
                for entry in entries {
                    let Some(op) = entry.get("operation") else {
                        continue;
                    };
                    let Some(operation_kind) = op.get("kind").and_then(|v| v.as_str()) else {
                        continue;
                    };
                    if operation_kind == format!("{kind}-delete")
                        && op.get("id").and_then(|v| v.as_str()) == Some(id)
                    {
                        return Ok(true);
                    }
                    if operation_kind == format!("{kind}-upsert")
                        && op
                            .get(kind)
                            .and_then(|v| v.get("id"))
                            .and_then(|v| v.as_str())
                            == Some(id)
                    {
                        return Ok(true);
                    }
                }
            }
        }
        Ok(disk
            .get("_mcp_protected_entities")
            .and_then(|v| v.as_array())
            .is_some_and(|entries| {
                entries.iter().any(|entry| {
                    entry.get("kind").and_then(|v| v.as_str()) == Some(kind)
                        && entry.get("id").and_then(|v| v.as_str()) == Some(id)
                })
            }))
    }
    /// Resolve intent against the latest committed state, never a captured HTML
    /// payload. Private notes return no content even to the queue importer.
    pub fn resolve_locked(
        p: &mut LockedProfile<'_>,
        after: u64,
        limit: usize,
    ) -> Result<ResolvedBatch, DomainError> {
        let seed_sequence = read_json::<serde_json::Value>(p, "note-service/outbox-seeded.json")?
            .and_then(|v| v.get("sequence").and_then(|s| s.as_u64()));
        let entries: Vec<_> = Self::list_pending_locked(p)?
            .into_iter()
            .filter(|e| e.sequence > after)
            .take(limit.clamp(1, 100))
            .collect();
        let through_sequence = entries.last().map(|e| e.sequence).unwrap_or(after);
        let mut entities = std::collections::BTreeMap::<(String, String), ResolvedEntity>::new();
        for entry in entries {
            for entity in entry.entities {
                let key = (entity.kind.clone(), entity.id.clone());
                let previous_delete = entities.get(&key).is_some_and(|e| e.cloud_delete);
                entities.insert(
                    key,
                    ResolvedEntity {
                        kind: entity.kind,
                        id: entity.id,
                        sequence: entry.sequence,
                        deleted: entity.deleted,
                        cloud_delete: previous_delete || entity.cloud_delete,
                        note: None,
                        folder: None,
                        seed: seed_sequence == Some(entry.sequence),
                    },
                );
            }
        }
        for entity in entities.values_mut() {
            match entity.kind.as_str() {
                "note" => match super::NoteRepository::get_locked(p, &entity.id) {
                    Ok(note) => {
                        entity.deleted = note.meta.deleted_at.is_some();
                        if !note.meta.local_only {
                            entity.cloud_delete = false;
                            if !entity.deleted {
                                entity.note = Some(note);
                            }
                        }
                    }
                    Err(DomainError::NotFound(_)) => {}
                    Err(error) => return Err(error),
                },
                "folder" => match super::FolderRepository::get_locked(p, &entity.id) {
                    Ok(folder) => {
                        entity.deleted = folder.deleted_at.is_some();
                        if !entity.deleted {
                            entity.folder = Some(folder);
                        }
                    }
                    Err(DomainError::NotFound(_)) => {}
                    Err(error) => return Err(error),
                },
                _ => return Err(DomainError::RecoveryRequired),
            }
        }
        Ok(ResolvedBatch {
            profile_id: p.scope().local_id().into(),
            through_sequence,
            entities: entities.into_values().collect(),
        })
    }
    #[cfg(test)]
    pub fn list_pending(scope: &ProfileScope) -> Result<Vec<OutboxEntry>, DomainError> {
        scope.transaction(Self::list_pending_locked)
    }
    pub fn list_pending_locked(p: &mut LockedProfile<'_>) -> Result<Vec<OutboxEntry>, DomainError> {
        super::journal::Journal::recover_locked(p)?;
        let ack: u64 = read_json(p, "note-service/outbox-ack.json")?.unwrap_or(0);
        let mut entries = Vec::new();
        for path in files_in(p, "note-service/outbox")? {
            let entry: OutboxEntry = read_json(p, &path)?.ok_or(DomainError::RecoveryRequired)?;
            if entry.sequence > ack {
                entries.push(entry);
            }
        }
        entries.sort_by_key(|e| e.sequence);
        Ok(entries)
    }
    /// Caller must first durably save the queue AND its import watermark (Task 7).
    #[cfg(test)]
    pub fn ack(scope: &ProfileScope, through: u64) -> Result<(), DomainError> {
        scope.transaction(|p| Self::ack_locked(p, through))
    }
    pub fn ack_locked(p: &mut LockedProfile<'_>, through: u64) -> Result<(), DomainError> {
        let max = super::journal::RevisionStore::load(p)?.sequence;
        if through > max {
            return Err(DomainError::InvalidInput(
                "Outbox acknowledgement exceeds committed sequence".into(),
            ));
        }
        let previous: u64 = read_json(p, "note-service/outbox-ack.json")?.unwrap_or(0);
        atomic_json(p, "note-service/outbox-ack.json", &previous.max(through))?;
        for path in files_in(p, "note-service/outbox")? {
            let entry: OutboxEntry = read_json(p, &path)?.ok_or(DomainError::RecoveryRequired)?;
            if entry.sequence <= previous.max(through) {
                std::fs::remove_file(p.path(std::path::Path::new(&path))?)?;
            }
        }
        Ok(())
    }
    pub(crate) fn status(
        p: &LockedProfile<'_>,
        note: Option<&super::repository::NoteData>,
    ) -> Result<SyncState, DomainError> {
        if let Some(n) = note {
            if n.meta.local_only {
                return Ok(SyncState::LocalOnly);
            }
            if n.meta.deleted_at.is_none() && n.content.len() > 3_145_728 {
                return Ok(SyncState::TooLarge);
            }
        }
        let settings: serde_json::Value =
            read_json(p, "sync-meta.json")?.unwrap_or(serde_json::Value::Null);
        if settings.get("enabled").and_then(|v| v.as_bool()) != Some(true) {
            return Ok(SyncState::Disabled);
        }
        Ok(if p.is_active()? {
            SyncState::Pending
        } else {
            SyncState::DeferredProfileInactive
        })
    }
}
