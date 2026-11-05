//! One mutation entry point for UI and imported data. Task 2 adds durable commits here.
use super::repository::{folders_disk, notes_disk};
use super::repository::{FolderMeta, NoteMeta};
use super::{DomainError, LockedProfile, ProfileScope};
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub enum Actor {
    Ui,
    Mcp { connection_id: String },
    Remote,
    Backup,
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum MutationIntent {
    AppendNote {
        id: String,
        content: String,
        format: String,
    },
    EditNoteText {
        id: String,
        old: String,
        new: String,
    },
    CreateNoteWithContent {
        folder_id: Option<String>,
        content: String,
        format: String,
    },
    SetNoteMetadata {
        id: String,
        folder_id: Option<Option<String>>,
        favorite: Option<bool>,
    },
    RestoreNote {
        id: String,
        snapshot_id: String,
    },
    ImportNote {
        meta: NoteMeta,
        content: String,
    },
    CreateNote {
        folder_id: Option<String>,
    },
    UpdateNote {
        id: String,
        content: String,
        title: String,
    },
    DeleteNote {
        id: String,
    },
    PurgeNotes {
        note_ids: Vec<String>,
    },
    ToggleFavorite {
        id: String,
    },
    SetLocalOnly {
        id: String,
        local_only: bool,
    },
    MoveNote {
        note_id: String,
        folder_id: Option<String>,
    },
    ReorderNotes {
        folder_id: Option<String>,
        note_ids: Vec<String>,
    },
    CreateFolder {
        name: String,
        icon: Option<String>,
    },
    RenameFolder {
        id: String,
        name: String,
        icon: Option<String>,
    },
    SetFolderMetadata {
        id: String,
        name: Option<String>,
        icon: Option<Option<String>>,
    },
    DeleteFolder {
        id: String,
    },
    ReorderFolders {
        ids: Vec<String>,
    },
    ImportFolders {
        folders: Vec<FolderMeta>,
    },
    PurgeFolders {
        folder_ids: Vec<String>,
    },
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum MutationResult {
    Note(NoteMeta),
    Folder(FolderMeta),
    Unit,
    Count(u32),
}
impl MutationResult {
    pub fn note(self) -> Result<NoteMeta, DomainError> {
        match self {
            Self::Note(n) => Ok(n),
            _ => Err(DomainError::Storage("Unexpected mutation result".into())),
        }
    }
    pub fn folder(self) -> Result<FolderMeta, DomainError> {
        match self {
            Self::Folder(f) => Ok(f),
            _ => Err(DomainError::Storage("Unexpected mutation result".into())),
        }
    }
    pub fn unit(self) -> Result<(), DomainError> {
        match self {
            Self::Unit => Ok(()),
            _ => Err(DomainError::Storage("Unexpected mutation result".into())),
        }
    }
    pub fn count(self) -> Result<u32, DomainError> {
        match self {
            Self::Count(n) => Ok(n),
            _ => Err(DomainError::Storage("Unexpected mutation result".into())),
        }
    }
}
pub struct NoteService;
#[cfg(test)]
mod durable_tests {
    use super::*;
    use crate::note_service::scope::tests::{meta, Fixture, ID};
    fn setup() -> (Fixture, ProfileScope) {
        let f = Fixture::new();
        let scope = f.scope("work");
        NoteService::mutate(
            &scope,
            MutationIntent::ImportNote {
                meta: meta(ID),
                content: "before".into(),
            },
            Actor::Ui,
        )
        .unwrap();
        (f, scope)
    }
    fn request(scope: &ProfileScope) -> MutationRequest {
        MutationRequest {
            operation_id: uuid::Uuid::new_v4().to_string(),
            expected_revision: Some(super::super::journal::RevisionStore::note(scope, ID).unwrap()),
            intent: MutationIntent::UpdateNote {
                id: ID.into(),
                content: "before after".into(),
                title: "title".into(),
            },
        }
    }
    fn actor() -> Actor {
        Actor::Mcp {
            connection_id: "test-client".into(),
        }
    }
    #[test]
    fn early_receipt_lookup_authorizes_and_bypasses_changed_revision() {
        let (_f, scope) = setup();
        let req = request(&scope);
        assert!(scope
            .transaction(|p| NoteService::receipt_checked_locked(p, &req, &actor(), |_| Ok(())))
            .unwrap()
            .is_none());
        let first = NoteService::mutate_checked(&scope, req.clone(), actor(), |_| Ok(())).unwrap();
        let replay = scope
            .transaction(|p| NoteService::receipt_checked_locked(p, &req, &actor(), |_| Ok(())))
            .unwrap()
            .unwrap();
        assert_eq!(first.revision, replay.revision);
        assert!(scope
            .transaction(
                |p| NoteService::receipt_checked_locked(p, &req, &actor(), |_| Err(
                    DomainError::InvalidInput("denied".into())
                ))
            )
            .is_err());
        let mut conflict = req.clone();
        conflict.intent = MutationIntent::DeleteNote { id: ID.into() };
        assert!(matches!(
            scope.transaction(|p| NoteService::receipt_checked_locked(
                p,
                &conflict,
                &actor(),
                |_| Ok(())
            )),
            Err(DomainError::OperationConflict)
        ));
    }
    fn review_restore_request(scope: &ProfileScope, snapshot_id: String) -> MutationRequest {
        MutationRequest {
            operation_id: uuid::Uuid::new_v4().to_string(),
            expected_revision: Some(super::super::journal::RevisionStore::note(scope, ID).unwrap()),
            intent: MutationIntent::RestoreNote {
                id: ID.into(),
                snapshot_id,
            },
        }
    }
    fn review_public_snapshot_then_private_delete(scope: &ProfileScope, purge: bool) -> String {
        let old_public = NoteService::mutate_checked(scope, request(scope), actor(), |_| Ok(()))
            .unwrap()
            .snapshot_id
            .unwrap();
        NoteService::mutate(
            scope,
            MutationIntent::SetLocalOnly {
                id: ID.into(),
                local_only: true,
            },
            Actor::Ui,
        )
        .unwrap();
        let mut delete = request(scope);
        delete.intent = MutationIntent::DeleteNote { id: ID.into() };
        NoteService::mutate_checked(scope, delete, actor(), |_| Ok(())).unwrap();
        if purge {
            NoteService::mutate(
                scope,
                MutationIntent::PurgeNotes {
                    note_ids: vec![ID.into()],
                },
                Actor::Remote,
            )
            .unwrap();
        }
        old_public
    }
    #[test]
    fn review_restore_old_public_snapshot_preserves_tombstone_privacy() {
        let (_f, scope) = setup();
        let snapshot = review_public_snapshot_then_private_delete(&scope, false);
        let restored = NoteService::mutate_checked(
            &scope,
            review_restore_request(&scope, snapshot),
            actor(),
            |_| Ok(()),
        )
        .unwrap();
        assert!(
            super::super::NoteRepository::get(&scope, ID)
                .unwrap()
                .meta
                .local_only
        );
        assert_eq!(
            restored.sync_state,
            super::super::outbox::SyncState::LocalOnly
        );
    }
    #[test]
    fn review_restore_old_public_snapshot_preserves_privacy_after_purge() {
        let (_f, scope) = setup();
        let snapshot = review_public_snapshot_then_private_delete(&scope, true);
        let restored = NoteService::mutate_checked(
            &scope,
            review_restore_request(&scope, snapshot),
            actor(),
            |_| Ok(()),
        )
        .unwrap();
        assert!(
            super::super::NoteRepository::get(&scope, ID)
                .unwrap()
                .meta
                .local_only
        );
        assert_eq!(
            restored.sync_state,
            super::super::outbox::SyncState::LocalOnly
        );
        let entries = super::super::outbox::LocalOutbox::list_pending(&scope).unwrap();
        assert_eq!(
            entries.last().unwrap().entities[0].sync_state,
            super::super::outbox::SyncState::LocalOnly
        );
    }
    #[test]
    fn review_restore_respects_later_explicit_public_preference_after_purge() {
        let (_f, scope) = setup();
        NoteService::mutate(
            &scope,
            MutationIntent::SetLocalOnly {
                id: ID.into(),
                local_only: true,
            },
            Actor::Ui,
        )
        .unwrap();
        let private_snapshot =
            NoteService::mutate_checked(&scope, request(&scope), actor(), |_| Ok(()))
                .unwrap()
                .snapshot_id
                .unwrap();
        NoteService::mutate(
            &scope,
            MutationIntent::SetLocalOnly {
                id: ID.into(),
                local_only: false,
            },
            Actor::Ui,
        )
        .unwrap();
        let mut delete = request(&scope);
        delete.intent = MutationIntent::DeleteNote { id: ID.into() };
        NoteService::mutate_checked(&scope, delete, actor(), |_| Ok(())).unwrap();
        NoteService::mutate(
            &scope,
            MutationIntent::PurgeNotes {
                note_ids: vec![ID.into()],
            },
            Actor::Remote,
        )
        .unwrap();
        NoteService::mutate_checked(
            &scope,
            review_restore_request(&scope, private_snapshot),
            actor(),
            |_| Ok(()),
        )
        .unwrap();
        assert!(
            !super::super::NoteRepository::get(&scope, ID)
                .unwrap()
                .meta
                .local_only
        );
    }
    #[test]
    fn review_restore_unknown_privacy_after_purge_defaults_to_local_only() {
        let (_f, scope) = setup();
        let snapshot = review_public_snapshot_then_private_delete(&scope, true);
        // Emulate v1 state written before privacy retention was introduced.
        let path = scope.root().join("note-service/state.json");
        let mut state: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        state.as_object_mut().unwrap().remove("note_privacy");
        std::fs::write(path, serde_json::to_vec(&state).unwrap()).unwrap();
        NoteService::mutate_checked(
            &scope,
            review_restore_request(&scope, snapshot),
            actor(),
            |_| Ok(()),
        )
        .unwrap();
        assert!(
            super::super::NoteRepository::get(&scope, ID)
                .unwrap()
                .meta
                .local_only
        );
    }
    #[test]
    fn review_folder_receipt_matches_kind_when_note_has_same_id() {
        use super::super::{journal::RevisionStore, outbox::SyncState};
        let (f, scope) = setup();
        let folder = FolderMeta {
            id: ID.into(),
            name: "same-id folder".into(),
            icon: None,
            created_at: "1".into(),
            updated_at: "1".into(),
            order: 0,
            deleted_at: None,
        };
        NoteService::mutate(
            &scope,
            MutationIntent::ImportFolders {
                folders: vec![folder],
            },
            Actor::Backup,
        )
        .unwrap();
        NoteService::mutate(
            &scope,
            MutationIntent::MoveNote {
                note_id: ID.into(),
                folder_id: Some(ID.into()),
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
        std::fs::write(scope.root().join("sync-meta.json"), r#"{"enabled":true}"#).unwrap();
        super::super::ProfileRegistry::switch_in(&f.0, "work").unwrap();
        let req = MutationRequest {
            operation_id: uuid::Uuid::new_v4().to_string(),
            expected_revision: Some(RevisionStore::folder(&scope, ID).unwrap()),
            intent: MutationIntent::DeleteFolder { id: ID.into() },
        };
        let receipt = NoteService::mutate_checked(&scope, req, actor(), |_| Ok(())).unwrap();
        assert_eq!(receipt.revision, RevisionStore::folder(&scope, ID).unwrap());
        assert_ne!(receipt.revision, RevisionStore::note(&scope, ID).unwrap());
        assert_eq!(receipt.sync_state, SyncState::Pending);
    }
    #[test]
    fn review_operation_uuid_case_has_identical_replay_fingerprint() {
        let (_f, scope) = setup();
        let mut req = request(&scope);
        req.operation_id = "abcdef01-2345-4678-9abc-def012345678".into();
        let first = NoteService::mutate_checked(&scope, req.clone(), actor(), |_| Ok(())).unwrap();
        req.operation_id = req.operation_id.to_uppercase();
        let replay = NoteService::mutate_checked(&scope, req, actor(), |_| Ok(())).unwrap();
        assert_eq!(first.operation_id, replay.operation_id);
        assert_eq!(first.revision, replay.revision);
    }
    struct InterruptedIo {
        at: usize,
        calls: usize,
    }
    #[test]
    fn reset_waits_for_a_real_mutation_at_durable_apply_and_prevents_regrowth() {
        use super::super::{
            editors::EditorRegistry,
            journal::{AtomicIo, DurableIo, FileChange},
            ProfileRegistry,
        };
        use std::sync::{mpsc, Arc};
        use std::time::Duration;
        struct PausedApply {
            entered: Option<mpsc::Sender<()>>,
            release: mpsc::Receiver<()>,
        }
        impl DurableIo for PausedApply {
            fn apply(
                &mut self,
                p: &LockedProfile<'_>,
                change: &FileChange,
            ) -> Result<(), DomainError> {
                AtomicIo.apply(p, change)?;
                if change.relative == "note-service/pending.json" {
                    if let Some(entered) = self.entered.take() {
                        entered.send(()).unwrap();
                        self.release.recv().unwrap();
                    }
                }
                Ok(())
            }
        }
        let (f, scope) = setup();
        let stale_scope = scope.clone();
        let req = request(&scope);
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let writer = std::thread::spawn(move || {
            scope.transaction(|p| {
                NoteService::mutate_with_io(
                    p,
                    req,
                    actor(),
                    |_| Ok(()),
                    |_| Ok(()),
                    &mut PausedApply {
                        entered: Some(entered_tx),
                        release: release_rx,
                    },
                )
            })
        });
        entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(stale_scope
            .root()
            .join("note-service/pending.json")
            .exists());
        let root = f.0.clone();
        let editors = Arc::new(EditorRegistry::default());
        let (reset_tx, reset_rx) = mpsc::channel();
        let resetter = std::thread::spawn(move || {
            let result = ProfileRegistry::reset_app_data_in(&root, &editors);
            reset_tx.send(result).unwrap();
        });
        let early = reset_rx.recv_timeout(Duration::from_millis(100));
        let waited = early.is_err();
        // Always release and join the real writer, including the RED path.
        release_tx.send(()).unwrap();
        writer.join().unwrap().unwrap();
        let result =
            early.unwrap_or_else(|_| reset_rx.recv_timeout(Duration::from_secs(10)).unwrap());
        resetter.join().unwrap();
        result.unwrap();
        assert!(
            waited,
            "reset deleted app-data while a real durable mutation was still applying"
        );
        assert_eq!(
            std::fs::read_dir(&f.0).unwrap().count(),
            0,
            "note/WAL/receipt files regrew after reset"
        );
        assert!(NoteService::mutate(
            &stale_scope,
            MutationIntent::ImportNote {
                meta: super::super::scope::tests::meta(ID),
                content: "queued import".into(),
            },
            Actor::Backup
        )
        .is_err());
        assert_eq!(std::fs::read_dir(&f.0).unwrap().count(), 0);
    }
    impl super::super::journal::DurableIo for InterruptedIo {
        fn apply(
            &mut self,
            p: &LockedProfile<'_>,
            change: &super::super::journal::FileChange,
        ) -> Result<(), DomainError> {
            let current = self.calls;
            self.calls += 1;
            if current == self.at {
                return Err(DomainError::Storage("synthetic interrupted I/O".into()));
            }
            super::super::journal::DurableIo::apply(&mut super::super::journal::AtomicIo, p, change)
        }
    }
    #[test]
    fn replayed_append_returns_same_result_at_every_durable_boundary() {
        use super::super::{journal::Journal, outbox::LocalOutbox, NoteRepository};
        for at in 0..=7 {
            let (_f, scope) = setup();
            let mut req = request(&scope);
            req.intent = MutationIntent::AppendNote {
                id: ID.into(),
                content: "added exactly once".into(),
                format: "text".into(),
            };
            let mut io = InterruptedIo { at, calls: 0 };
            let outcome = scope.transaction(|p| {
                NoteService::mutate_with_io(
                    p,
                    req.clone(),
                    actor(),
                    |_| Ok(()),
                    |_| Ok(()),
                    &mut io,
                )
            });
            assert!(
                outcome.is_err(),
                "boundary {at} must interrupt ({} calls)",
                io.calls
            );
            if at == 0 {
                assert_eq!(NoteRepository::get(&scope, ID).unwrap().content, "before");
            }
            Journal::recover_profile(&scope).unwrap();
            let first =
                NoteService::mutate_checked(&scope, req.clone(), actor(), |_| Ok(())).unwrap();
            let replay = NoteService::mutate_checked(&scope, req, actor(), |_| Ok(())).unwrap();
            assert_eq!(first.revision, replay.revision);
            assert_eq!(
                NoteRepository::get(&scope, ID)
                    .unwrap()
                    .content
                    .matches("added exactly once")
                    .count(),
                1
            );
            assert_eq!(
                super::super::versions::VersionStore::list(&scope, ID)
                    .unwrap()
                    .len(),
                1
            );
            assert_eq!(LocalOutbox::list_pending(&scope).unwrap().len(), 2);
        }
    }
    #[test]
    fn folder_delete_recovers_all_orphans_in_one_intent() {
        use super::super::{
            journal::RevisionStore, outbox::LocalOutbox, FolderRepository, NoteRepository,
        };
        for at in 1..=6 {
            let (_f, scope) = setup();
            let folder = NoteService::mutate(
                &scope,
                MutationIntent::CreateFolder {
                    name: "Folder".into(),
                    icon: None,
                },
                Actor::Ui,
            )
            .unwrap()
            .folder()
            .unwrap();
            let other = "00000000-0000-4000-8000-000000000002";
            for id in [ID, other] {
                let mut m = meta(id);
                m.folder_id = Some(folder.id.clone());
                NoteService::mutate(
                    &scope,
                    MutationIntent::ImportNote {
                        meta: m,
                        content: format!("keep {id}"),
                    },
                    Actor::Ui,
                )
                .unwrap();
            }
            let prior = RevisionStore::note(&scope, ID).unwrap();
            let req = MutationRequest {
                operation_id: uuid::Uuid::new_v4().to_string(),
                expected_revision: Some(RevisionStore::folder(&scope, &folder.id).unwrap()),
                intent: MutationIntent::DeleteFolder {
                    id: folder.id.clone(),
                },
            };
            let mut io = InterruptedIo { at, calls: 0 };
            assert!(scope
                .transaction(|p| NoteService::mutate_with_io(
                    p,
                    req.clone(),
                    actor(),
                    |_| Ok(()),
                    |_| Ok(()),
                    &mut io
                ))
                .is_err());
            // A repository read itself must recover before exposing any state.
            assert!(FolderRepository::get(&scope, &folder.id)
                .unwrap()
                .deleted_at
                .is_some());
            for id in [ID, other] {
                let n = NoteRepository::get(&scope, id).unwrap();
                assert_eq!(n.content, format!("keep {id}"));
                assert!(n.meta.folder_id.is_none());
            }
            assert_ne!(prior, RevisionStore::note(&scope, ID).unwrap());
            let replay = NoteService::mutate_checked(&scope, req, actor(), |_| Ok(())).unwrap();
            let entries = LocalOutbox::list_pending(&scope).unwrap();
            let last = entries.last().unwrap();
            assert_eq!(last.operation_id, replay.operation_id);
            assert_eq!(last.entities.len(), 3);
        }
    }
    #[test]
    fn restore_after_tombstone_purge_preserves_local_only_and_rejects_active_overwrite() {
        use super::super::{journal::RevisionStore, NoteRepository};
        let (_f, scope) = setup();
        NoteService::mutate(
            &scope,
            MutationIntent::SetLocalOnly {
                id: ID.into(),
                local_only: true,
            },
            Actor::Ui,
        )
        .unwrap();
        let mut req = request(&scope);
        req.intent = MutationIntent::DeleteNote { id: ID.into() };
        let deleted = NoteService::mutate_checked(&scope, req, actor(), |_| Ok(())).unwrap();
        NoteService::mutate(
            &scope,
            MutationIntent::PurgeNotes {
                note_ids: vec![ID.into()],
            },
            Actor::Remote,
        )
        .unwrap();
        let mut restore = MutationRequest {
            operation_id: uuid::Uuid::new_v4().to_string(),
            expected_revision: Some(RevisionStore::note(&scope, ID).unwrap()),
            intent: MutationIntent::RestoreNote {
                id: ID.into(),
                snapshot_id: deleted.snapshot_id.unwrap(),
            },
        };
        NoteService::mutate_checked(&scope, restore.clone(), actor(), |_| Ok(())).unwrap();
        let note = NoteRepository::get(&scope, ID).unwrap();
        assert_eq!(note.content, "before");
        assert!(note.meta.local_only);
        assert!(note.meta.deleted_at.is_none());
        restore.operation_id = uuid::Uuid::new_v4().to_string();
        restore.expected_revision = Some(RevisionStore::note(&scope, ID).unwrap());
        assert!(matches!(
            NoteService::mutate_checked(&scope, restore, actor(), |_| Ok(())),
            Err(DomainError::ActiveNoteExists)
        ));
    }
    #[test]
    fn expired_snapshot_is_not_restorable() {
        let (_f, scope) = setup();
        let mut req = request(&scope);
        req.intent = MutationIntent::DeleteNote { id: ID.into() };
        let deleted = NoteService::mutate_checked(&scope, req, actor(), |_| Ok(())).unwrap();
        let snapshot_id = deleted.snapshot_id.unwrap();
        let path = scope
            .root()
            .join(format!("note-service/versions/{snapshot_id}.json"));
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        value["expires_at"] = 0.into();
        std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
        let restore = MutationRequest {
            operation_id: uuid::Uuid::new_v4().to_string(),
            expected_revision: Some(deleted.revision),
            intent: MutationIntent::RestoreNote {
                id: ID.into(),
                snapshot_id,
            },
        };
        assert!(matches!(
            NoteService::mutate_checked(&scope, restore, actor(), |_| Ok(())),
            Err(DomainError::SnapshotExpired)
        ));
    }
    #[test]
    fn same_operation_uuid_is_independent_between_profiles_but_actor_bound() {
        let (f, scope) = setup();
        let other = f.scope("perso");
        NoteService::mutate(
            &other,
            MutationIntent::ImportNote {
                meta: meta(ID),
                content: "personal".into(),
            },
            Actor::Remote,
        )
        .unwrap();
        let req = request(&scope);
        NoteService::mutate_checked(&scope, req.clone(), actor(), |_| Ok(())).unwrap();
        let mut second = req.clone();
        second.expected_revision =
            Some(super::super::journal::RevisionStore::note(&other, ID).unwrap());
        NoteService::mutate_checked(&other, second, actor(), |_| Ok(())).unwrap();
        assert!(matches!(
            NoteService::mutate_checked(
                &scope,
                req,
                Actor::Mcp {
                    connection_id: "different".into()
                },
                |_| Ok(())
            ),
            Err(DomainError::OperationConflict)
        ));
    }
    #[test]
    fn fresh_authorization_and_mutable_editor_precondition_have_separate_replay_order() {
        let (_f, scope) = setup();
        let req = request(&scope);
        let mut checks = 0;
        assert!(
            NoteService::mutate_checked(&scope, req.clone(), actor(), |_| {
                checks += 1;
                if checks == 2 {
                    Err(DomainError::InvalidInput(
                        "revoked during preparation".into(),
                    ))
                } else {
                    Ok(())
                }
            })
            .is_err()
        );
        assert_eq!(checks, 2);
        assert_eq!(
            super::super::NoteRepository::get(&scope, ID)
                .unwrap()
                .content,
            "before"
        );
        NoteService::mutate_checked(&scope, req.clone(), actor(), |_| Ok(())).unwrap();
        scope
            .transaction(|p| {
                NoteService::mutate_checked_with_precondition_locked(
                    p,
                    req,
                    actor(),
                    |_| Ok(()),
                    |_| Err(DomainError::EditorBusy),
                )
            })
            .unwrap();
    }
    #[test]
    fn local_only_transition_records_cloud_delete_without_local_tombstone() {
        use super::super::{outbox::LocalOutbox, NoteRepository};
        let (_f, scope) = setup();
        NoteService::mutate(
            &scope,
            MutationIntent::SetLocalOnly {
                id: ID.into(),
                local_only: true,
            },
            Actor::Ui,
        )
        .unwrap();
        let entries = LocalOutbox::list_pending(&scope).unwrap();
        let entity = &entries.last().unwrap().entities[0];
        assert!(entity.cloud_delete);
        assert!(!entity.deleted);
        assert!(NoteRepository::get(&scope, ID)
            .unwrap()
            .meta
            .deleted_at
            .is_none());
        NoteService::mutate(
            &scope,
            MutationIntent::SetLocalOnly {
                id: ID.into(),
                local_only: false,
            },
            Actor::Ui,
        )
        .unwrap();
        let entries = LocalOutbox::list_pending(&scope).unwrap();
        assert!(!entries.last().unwrap().entities[0].cloud_delete);
        assert!(!NoteRepository::get(&scope, ID).unwrap().meta.local_only);
    }
    #[cfg(windows)]
    #[test]
    fn real_windows_sharing_failure_blocks_reads_until_recovery() {
        use std::os::windows::fs::OpenOptionsExt;
        let (_f, scope) = setup();
        let req = request(&scope);
        let path = scope.root().join(format!("notes/{ID}/note.json"));
        // Windows refuses replacement while this handle denies delete sharing.
        let handle = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(path)
            .unwrap();
        assert!(matches!(
            NoteService::mutate_checked(&scope, req.clone(), actor(), |_| Ok(())),
            Err(DomainError::RecoveryRequired)
        ));
        assert!(matches!(
            super::super::NoteRepository::get(&scope, ID),
            Err(DomainError::RecoveryRequired)
        ));
        drop(handle);
        assert_eq!(
            super::super::NoteRepository::get(&scope, ID)
                .unwrap()
                .content,
            "before after"
        );
        NoteService::mutate_checked(&scope, req, actor(), |_| Ok(())).unwrap();
    }
    #[test]
    fn caught_failed_commit_cannot_serve_partial_state_inside_same_transaction() {
        let (_f, scope) = setup();
        let req = request(&scope);
        scope
            .transaction(|p| {
                let mut io = InterruptedIo { at: 3, calls: 0 };
                assert!(NoteService::mutate_with_io(
                    p,
                    req.clone(),
                    actor(),
                    |_| Ok(()),
                    |_| Ok(()),
                    &mut io
                )
                .is_err());
                // Explicit locked readers must recover too, even if their caller
                // catches an error rather than leaving the outer transaction.
                let note = super::super::NoteRepository::get_locked(p, ID)?;
                assert_eq!(note.content, "before after");
                assert_eq!(note.meta.title, "title");
                Ok(())
            })
            .unwrap();
    }
    #[test]
    fn history_full_rejects_before_mutation() {
        use std::io::Write;
        let (_f, scope) = setup();
        let req = request(&scope);
        let receipt = NoteService::mutate_checked(&scope, req, actor(), |_| Ok(())).unwrap();
        let path = scope.root().join(format!(
            "note-service/versions/{}.json",
            receipt.snapshot_id.unwrap()
        ));
        // Valid JSON padded to the actual production 256 MiB budget. This is a
        // real disk size test, not a magic test-only setting in the MCP API.
        let size = std::fs::metadata(&path).unwrap().len();
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        let chunk = vec![b' '; 1024 * 1024];
        let mut remaining = super::super::versions::HISTORY_BUDGET - size;
        while remaining > 0 {
            let n = remaining.min(chunk.len() as u64) as usize;
            file.write_all(&chunk[..n]).unwrap();
            remaining -= n as u64;
        }
        file.sync_all().unwrap();
        drop(file);
        let mut req = request(&scope);
        req.intent = MutationIntent::DeleteNote { id: ID.into() };
        assert!(matches!(
            NoteService::mutate_checked(&scope, req, actor(), |_| Ok(())),
            Err(DomainError::HistoryFull)
        ));
        let note = super::super::NoteRepository::get(&scope, ID).unwrap();
        assert_eq!(note.content, "before after");
        assert!(note.meta.deleted_at.is_none());
    }
    #[test]
    fn stale_revision_leaves_content_unchanged() {
        let (_f, scope) = setup();
        let req = request(&scope);
        NoteService::mutate(
            &scope,
            MutationIntent::ToggleFavorite { id: ID.into() },
            Actor::Ui,
        )
        .unwrap();
        assert!(matches!(
            NoteService::mutate_checked(&scope, req, actor(), |_| Ok(())),
            Err(DomainError::RevisionConflict)
        ));
        assert_eq!(
            super::super::NoteRepository::get(&scope, ID)
                .unwrap()
                .content,
            "before"
        );
    }
    #[test]
    fn replay_checks_receipt_before_expected_revision() {
        let (_f, scope) = setup();
        let req = request(&scope);
        let first = NoteService::mutate_checked(&scope, req.clone(), actor(), |_| Ok(())).unwrap();
        let second = NoteService::mutate_checked(&scope, req, actor(), |_| Ok(())).unwrap();
        assert_eq!(first.revision, second.revision);
        assert_eq!(
            super::super::versions::VersionStore::list(&scope, ID)
                .unwrap()
                .len(),
            1
        );
    }
    #[test]
    fn same_operation_different_payload_is_rejected() {
        let (_f, scope) = setup();
        let mut req = request(&scope);
        NoteService::mutate_checked(&scope, req.clone(), actor(), |_| Ok(())).unwrap();
        req.intent = MutationIntent::DeleteNote { id: ID.into() };
        assert!(matches!(
            NoteService::mutate_checked(&scope, req, actor(), |_| Ok(())),
            Err(DomainError::OperationConflict)
        ));
    }
    #[test]
    fn revoked_actor_cannot_replay_receipt() {
        let (_f, scope) = setup();
        let req = request(&scope);
        NoteService::mutate_checked(&scope, req.clone(), actor(), |_| Ok(())).unwrap();
        let mut calls = 0;
        assert!(NoteService::mutate_checked(&scope, req, actor(), |_| {
            calls += 1;
            Err(DomainError::InvalidInput("revoked".into()))
        })
        .is_err());
        assert_eq!(calls, 1);
    }
    #[test]
    fn invalid_second_file_never_changes_first_file() {
        let f = Fixture::new();
        let scope = f.scope("work");
        NoteService::mutate(
            &scope,
            MutationIntent::ImportNote {
                meta: meta(ID),
                content: "before".into(),
            },
            Actor::Ui,
        )
        .unwrap();
        let dir = scope.root().join("notes").join(ID);
        std::fs::write(dir.join("note.json"), "invalid json").unwrap();
        assert!(NoteService::mutate(
            &scope,
            MutationIntent::UpdateNote {
                id: ID.into(),
                content: "after".into(),
                title: "after".into()
            },
            Actor::Ui
        )
        .is_err());
        assert_eq!(
            std::fs::read_to_string(dir.join("content.html")).unwrap(),
            "before"
        );
    }
}
impl NoteService {
    /// Journal integration uses this transaction once and calls `mutate_locked`.
    /// Do not re-enter `mutate` or public repositories from the closure.
    pub fn transaction<T>(
        scope: &ProfileScope,
        action: impl FnOnce(&mut LockedProfile<'_>) -> Result<T, DomainError>,
    ) -> Result<T, DomainError> {
        scope.transaction(action)
    }
    #[cfg(test)]
    pub fn mutate(
        scope: &ProfileScope,
        intent: MutationIntent,
        actor: Actor,
    ) -> Result<MutationResult, DomainError> {
        Self::transaction(scope, |p| Self::mutate_locked(p, intent, actor))
    }
    pub fn mutate_locked(
        profile: &mut LockedProfile<'_>,
        intent: MutationIntent,
        actor: Actor,
    ) -> Result<MutationResult, DomainError> {
        let request = MutationRequest {
            operation_id: uuid::Uuid::new_v4().to_string(),
            expected_revision: None,
            intent,
        };
        Ok(Self::mutate_checked_locked(profile, request, actor, |_| Ok(()))?.result)
    }
    #[cfg(test)]
    pub fn mutate_checked(
        scope: &ProfileScope,
        request: MutationRequest,
        actor: Actor,
        mut guard: impl FnMut(&LockedProfile<'_>) -> Result<(), DomainError>,
    ) -> Result<MutationReceipt, DomainError> {
        scope.transaction(|p| Self::mutate_checked_locked(p, request, actor, &mut guard))
    }
    pub fn mutate_checked_locked(
        profile: &mut LockedProfile<'_>,
        request: MutationRequest,
        actor: Actor,
        guard: impl FnMut(&LockedProfile<'_>) -> Result<(), DomainError>,
    ) -> Result<MutationReceipt, DomainError> {
        Self::mutate_with_io(
            profile,
            request,
            actor,
            guard,
            |_| Ok(()),
            &mut super::journal::AtomicIo,
        )
    }
    /// Check replay before asynchronous editor preparation. Callers retain their
    /// current authorization lease and recheck again during the final commit.
    pub fn receipt_checked_locked(
        profile: &LockedProfile<'_>,
        request: &MutationRequest,
        actor: &Actor,
        authorize: impl FnOnce(&LockedProfile<'_>) -> Result<(), DomainError>,
    ) -> Result<Option<MutationReceipt>, DomainError> {
        super::journal::Journal::recover_locked(profile)?;
        authorize(profile)?;
        let operation = uuid::Uuid::parse_str(&request.operation_id)
            .map_err(|_| DomainError::InvalidInput("operation_id must be a UUID".into()))?
            .to_string();
        let canonical = MutationRequest {
            operation_id: operation.clone(),
            ..request.clone()
        };
        let fingerprint = request_fingerprint(&canonical)?;
        let stored: Option<StoredReceipt> =
            super::journal::read_json(profile, &format!("note-service/receipts/{operation}.json"))?;
        match stored.filter(|s| {
            s.created_at + super::journal::RETENTION_SECONDS >= chrono::Utc::now().timestamp()
        }) {
            Some(stored) if stored.actor != *actor || stored.fingerprint != fingerprint => {
                Err(DomainError::OperationConflict)
            }
            Some(stored) => Ok(Some(stored.receipt)),
            None => Ok(None),
        }
    }
    pub fn mutate_checked_with_precondition_locked(
        profile: &mut LockedProfile<'_>,
        request: MutationRequest,
        actor: Actor,
        authorize: impl FnMut(&LockedProfile<'_>) -> Result<(), DomainError>,
        precondition: impl FnMut(&LockedProfile<'_>) -> Result<(), DomainError>,
    ) -> Result<MutationReceipt, DomainError> {
        Self::mutate_with_io(
            profile,
            request,
            actor,
            authorize,
            precondition,
            &mut super::journal::AtomicIo,
        )
    }
    fn mutate_with_io(
        profile: &mut LockedProfile<'_>,
        request: MutationRequest,
        actor: Actor,
        mut guard: impl FnMut(&LockedProfile<'_>) -> Result<(), DomainError>,
        mut precondition: impl FnMut(&LockedProfile<'_>) -> Result<(), DomainError>,
        io: &mut impl super::journal::DurableIo,
    ) -> Result<MutationReceipt, DomainError> {
        use super::journal::{
            json_change, read_json, FileChange, Journal, Revision, RevisionStore,
        };
        Journal::recover_locked(profile)?;
        guard(profile)?;
        let operation = uuid::Uuid::parse_str(&request.operation_id)
            .map_err(|_| DomainError::InvalidInput("operation_id must be a UUID".into()))?
            .to_string();
        // Receipt identity and payload identity use the same UUID spelling.
        let request = MutationRequest {
            operation_id: operation.clone(),
            ..request
        };
        let fingerprint = request_fingerprint(&request)?;
        let receipt_path = format!("note-service/receipts/{operation}.json");
        if let Some(stored) = read_json::<StoredReceipt>(profile, &receipt_path)? {
            if stored.created_at + super::journal::RETENTION_SECONDS
                >= chrono::Utc::now().timestamp()
            {
                if stored.actor != actor || stored.fingerprint != fingerprint {
                    return Err(DomainError::OperationConflict);
                }
                return Ok(stored.receipt);
            }
        }
        if matches!(request.intent, MutationIntent::RestoreNote { .. })
            && request.expected_revision.is_none()
        {
            return Err(DomainError::InvalidInput(
                "Restore requires an explicit absence/tombstone revision".into(),
            ));
        }
        let force_folder_revisions = matches!(request.intent, MutationIntent::ImportFolders { .. });
        let target = request.intent.target();
        let current = match &target {
            Some((kind, id)) if *kind == "note" => RevisionStore::note_locked(profile, id)?,
            Some((_, id)) => RevisionStore::folder_locked(profile, id)?,
            None => RevisionStore::collection_locked(profile)?,
        };
        if let Some(expected) = &request.expected_revision {
            if expected != &current {
                return Err(DomainError::RevisionConflict);
            }
        } else if matches!(actor, Actor::Mcp { .. }) && target.is_some() {
            return Err(DomainError::InvalidInput(
                "expected_revision is required".into(),
            ));
        }
        let snapshot = if matches!(actor, Actor::Mcp { .. }) && request.intent.needs_snapshot() {
            let id = &target
                .as_ref()
                .ok_or_else(|| DomainError::InvalidInput("Missing snapshot target".into()))?
                .1;
            let note = super::NoteRepository::get_locked(profile, id)?;
            if note.meta.deleted_at.is_some() {
                return Err(DomainError::NotFound(id.clone()));
            }
            Some(super::versions::VersionStore::prepare(
                profile,
                note,
                current.clone(),
            )?)
        } else {
            None
        };
        if !profile.staged.borrow().is_empty() {
            return Err(DomainError::Storage("Nested pending mutation".into()));
        }
        let result = match Self::dispatch(profile, request.intent, actor.clone()) {
            Ok(value) => value,
            Err(error) => {
                profile.staged.borrow_mut().clear();
                return Err(error);
            }
        };
        let staged = std::mem::take(&mut *profile.staged.borrow_mut());
        let mut state = RevisionStore::load(profile)?;
        state.collection = Revision::fresh();
        state.sequence = state
            .sequence
            .checked_add(1)
            .ok_or_else(|| DomainError::Storage("Sequence exhausted".into()))?;
        let entities = affected_entities(profile, &staged, &mut state, force_folder_revisions)?;
        let result_entity = match &result {
            MutationResult::Note(n) => Some(("note", n.id.clone())),
            MutationResult::Folder(f) => Some(("folder", f.id.clone())),
            _ => target,
        };
        let entity_id = result_entity.as_ref().map(|(_, id)| id.clone());
        let affected_entity = result_entity
            .as_ref()
            .and_then(|(kind, id)| entities.iter().find(|e| e.kind == *kind && &e.id == id));
        let revision = affected_entity
            .map(|e| e.revision.clone())
            .unwrap_or_else(|| state.collection.clone());
        let sync_state = if matches!(actor, Actor::Remote) {
            super::outbox::SyncState::Disabled
        } else {
            affected_entity
                .map(|e| e.sync_state.clone())
                .unwrap_or(super::outbox::LocalOutbox::status(profile, None)?)
        };
        let receipt = MutationReceipt {
            profile_id: profile.scope().local_id().into(),
            entity_id,
            revision,
            operation_id: operation.clone(),
            sync_state,
            snapshot_id: snapshot.as_ref().map(|s| s.snapshot_id.clone()),
            result,
        };
        let mut files = Vec::new();
        if let Some(snapshot) = snapshot {
            files.push(json_change(
                format!("note-service/versions/{}.json", snapshot.snapshot_id),
                &snapshot,
            )?);
        }
        files.extend(
            staged
                .into_iter()
                .map(|(relative, content)| FileChange { relative, content }),
        );
        files.push(json_change("note-service/state.json".into(), &state)?);
        files.push(json_change(
            receipt_path,
            &StoredReceipt {
                created_at: chrono::Utc::now().timestamp(),
                actor: actor.clone(),
                fingerprint,
                receipt: receipt.clone(),
            },
        )?);
        if !matches!(actor, Actor::Remote) && !entities.is_empty() {
            let entry = super::outbox::OutboxEntry {
                sequence: state.sequence,
                profile_id: profile.scope().local_id().into(),
                operation_id: operation,
                source: actor,
                entities,
                created_at: chrono::Utc::now().timestamp(),
            };
            files.push(json_change(
                format!("note-service/outbox/{:020}.json", state.sequence),
                &entry,
            )?);
        }
        precondition(profile)?;
        guard(profile)?;
        Journal::commit_with(profile, files, io)?;
        Ok(receipt)
    }
    pub(crate) fn commit_maintenance(profile: &mut LockedProfile<'_>) -> Result<(), DomainError> {
        let staged = std::mem::take(&mut *profile.staged.borrow_mut());
        if staged.is_empty() {
            return Ok(());
        }
        let mut state = super::journal::RevisionStore::load(profile)?;
        let entities = affected_entities(profile, &staged, &mut state, false)?;
        state.collection = super::journal::Revision::fresh();
        state.sequence = state
            .sequence
            .checked_add(1)
            .ok_or_else(|| DomainError::Storage("Sequence exhausted".into()))?;
        let operation_id = uuid::Uuid::new_v4().to_string();
        let mut files: Vec<_> = staged
            .into_iter()
            .map(|(relative, content)| super::journal::FileChange { relative, content })
            .collect();
        files.push(super::journal::json_change(
            "note-service/state.json".into(),
            &state,
        )?);
        if !entities.is_empty() {
            let entry = super::outbox::OutboxEntry {
                sequence: state.sequence,
                profile_id: profile.scope().local_id().into(),
                operation_id,
                source: Actor::Ui,
                entities,
                created_at: chrono::Utc::now().timestamp(),
            };
            files.push(super::journal::json_change(
                format!("note-service/outbox/{:020}.json", state.sequence),
                &entry,
            )?);
        }
        super::journal::Journal::commit(profile, files)
    }
    fn dispatch(
        profile: &mut LockedProfile<'_>,
        intent: MutationIntent,
        _actor: Actor,
    ) -> Result<MutationResult, DomainError> {
        use MutationIntent::*;
        match intent {
            AppendNote {
                id,
                content,
                format,
            } => {
                let note = super::NoteRepository::get_locked(profile, &id)?;
                if note.meta.deleted_at.is_some() {
                    return Err(DomainError::NotFound(id));
                }
                let fragment = super::content::render_input(&content, &format)
                    .map_err(|e| DomainError::InvalidInput(e.to_string()))?;
                let html = super::content::append_html(&note.content, &fragment)
                    .map_err(|e| DomainError::InvalidInput(e.to_string()))?;
                let title = super::content::derive_note_title(&html, "en");
                notes_disk::update_note(profile, id, html, title).map(MutationResult::Note)
            }
            EditNoteText { id, old, new } => {
                let note = super::NoteRepository::get_locked(profile, &id)?;
                if note.meta.deleted_at.is_some() {
                    return Err(DomainError::NotFound(id));
                }
                let html = super::content::replace_unique_text(&note.content, &old, &new)
                    .map_err(|e| DomainError::InvalidInput(e.to_string()))?;
                let title = super::content::derive_note_title(&html, "en");
                notes_disk::update_note(profile, id, html, title).map(MutationResult::Note)
            }
            CreateNoteWithContent {
                folder_id,
                content,
                format,
            } => {
                let html = super::content::render_input(&content, &format)
                    .map_err(|e| DomainError::InvalidInput(e.to_string()))?;
                let mut note = notes_disk::create_note(profile, folder_id)?;
                note.title = super::content::derive_note_title(&html, "en");
                notes_disk::import_note_for_backup(profile, note.clone(), html)?;
                Ok(MutationResult::Note(note))
            }
            SetNoteMetadata {
                id,
                folder_id,
                favorite,
            } => {
                let mut note = super::NoteRepository::get_locked(profile, &id)?;
                if note.meta.deleted_at.is_some() {
                    return Err(DomainError::NotFound(id));
                }
                if let Some(folder_id) = folder_id {
                    note.meta.folder_id = folder_id;
                    note.meta.order = 0;
                }
                if let Some(favorite) = favorite {
                    note.meta.favorite = favorite;
                }
                note.meta.updated_at = chrono::Utc::now().to_rfc3339();
                notes_disk::import_note_for_backup(profile, note.meta.clone(), note.content)?;
                Ok(MutationResult::Note(note.meta))
            }
            RestoreNote { id, snapshot_id } => {
                let snapshot = super::versions::VersionStore::get_locked(profile, &snapshot_id)?;
                if snapshot.note_id != id {
                    return Err(DomainError::NotFound("snapshot".into()));
                }
                let path =
                    profile.path(&std::path::Path::new("notes").join(&id).join("note.json"))?;
                let local_only = if path.exists() {
                    let current = super::NoteRepository::get_locked(profile, &id)?;
                    if current.meta.deleted_at.is_none() {
                        return Err(DomainError::ActiveNoteExists);
                    }
                    current.meta.local_only
                } else {
                    // A snapshot is historical content, never permission to
                    // undo a newer privacy preference. Unknown legacy privacy
                    // after purge is conservative until the user changes it.
                    super::journal::RevisionStore::load(profile)?
                        .note_privacy
                        .get(&id)
                        .copied()
                        .unwrap_or(true)
                };
                let mut note = snapshot.note;
                note.meta.local_only = local_only;
                note.meta.deleted_at = None;
                note.meta.updated_at = chrono::Utc::now().to_rfc3339();
                if let Some(folder) = &note.meta.folder_id {
                    if super::FolderRepository::get_locked(profile, folder)
                        .map(|f| f.deleted_at.is_some())
                        .unwrap_or(true)
                    {
                        note.meta.folder_id = None;
                    }
                }
                notes_disk::import_note_for_backup(profile, note.meta.clone(), note.content)?;
                Ok(MutationResult::Note(note.meta))
            }
            ImportNote { meta, content } => {
                notes_disk::import_note_for_backup(profile, meta, content)?;
                Ok(MutationResult::Unit)
            }
            CreateNote { folder_id } => {
                notes_disk::create_note(profile, folder_id).map(MutationResult::Note)
            }
            UpdateNote { id, content, title } => {
                notes_disk::update_note(profile, id, content, title).map(MutationResult::Note)
            }
            DeleteNote { id } => {
                notes_disk::delete_note(profile, id)?;
                Ok(MutationResult::Unit)
            }
            PurgeNotes { note_ids } => {
                notes_disk::purge_soft_deleted_notes_post_pull(profile, note_ids)
                    .map(MutationResult::Count)
            }
            ToggleFavorite { id } => {
                notes_disk::toggle_note_favorite(profile, id).map(MutationResult::Note)
            }
            SetLocalOnly { id, local_only } => {
                notes_disk::set_note_local_only(profile, id, local_only).map(MutationResult::Note)
            }
            MoveNote { note_id, folder_id } => {
                notes_disk::move_note_to_folder(profile, note_id, folder_id)
                    .map(MutationResult::Note)
            }
            ReorderNotes {
                folder_id,
                note_ids,
            } => {
                notes_disk::reorder_notes_in_folder(profile, folder_id, note_ids)?;
                Ok(MutationResult::Unit)
            }
            CreateFolder { name, icon } => {
                folders_disk::create_folder(profile, name, icon).map(MutationResult::Folder)
            }
            RenameFolder { id, name, icon } => {
                folders_disk::rename_folder(profile, id, name, icon).map(MutationResult::Folder)
            }
            SetFolderMetadata { id, name, icon } => {
                let current = super::FolderRepository::get_locked(profile, &id)?;
                if current.deleted_at.is_some() {
                    return Err(DomainError::NotFound(id));
                }
                folders_disk::rename_folder(
                    profile,
                    id,
                    name.unwrap_or(current.name),
                    icon.unwrap_or(current.icon),
                )
                .map(MutationResult::Folder)
            }
            DeleteFolder { id } => {
                folders_disk::delete_folder(profile, id)?;
                Ok(MutationResult::Unit)
            }
            ReorderFolders { ids } => {
                folders_disk::reorder_folders(profile, ids)?;
                Ok(MutationResult::Unit)
            }
            ImportFolders { folders } => {
                folders_disk::import_folders_for_backup(profile, folders)?;
                Ok(MutationResult::Unit)
            }
            PurgeFolders { folder_ids } => {
                folders_disk::purge_soft_deleted_folders_post_pull(profile, folder_ids)
                    .map(MutationResult::Count)
            }
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MutationRequest {
    pub operation_id: String,
    pub expected_revision: Option<super::journal::Revision>,
    pub intent: MutationIntent,
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MutationReceipt {
    pub profile_id: String,
    pub entity_id: Option<String>,
    pub revision: super::journal::Revision,
    pub operation_id: String,
    pub sync_state: super::outbox::SyncState,
    pub snapshot_id: Option<String>,
    pub result: MutationResult,
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct StoredReceipt {
    pub created_at: i64,
    pub actor: Actor,
    pub fingerprint: String,
    pub receipt: MutationReceipt,
}

fn request_fingerprint(request: &MutationRequest) -> Result<String, DomainError> {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    hash.update(serde_json::to_vec(request)?);
    // JSON null alone cannot distinguish an omitted patch from an explicit
    // clear: Option<Option<T>> serializes both None and Some(None) as null.
    // Bind the actual semantic presence without changing the storage wire types.
    match &request.intent {
        MutationIntent::SetNoteMetadata { folder_id, .. } => {
            hash.update(b"\0folder_id_present:");
            hash.update([u8::from(folder_id.is_some())]);
        }
        MutationIntent::SetFolderMetadata { icon, .. } => {
            hash.update(b"\0icon_present:");
            hash.update([u8::from(icon.is_some())]);
        }
        _ => {}
    }
    Ok(format!("{:x}", hash.finalize()))
}

impl MutationIntent {
    fn target(&self) -> Option<(&'static str, String)> {
        use MutationIntent::*;
        match self {
            UpdateNote { id, .. }
            | AppendNote { id, .. }
            | EditNoteText { id, .. }
            | SetNoteMetadata { id, .. }
            | RestoreNote { id, .. }
            | DeleteNote { id }
            | ToggleFavorite { id }
            | SetLocalOnly { id, .. } => Some(("note", id.clone())),
            MoveNote { note_id, .. } => Some(("note", note_id.clone())),
            ImportNote { meta, .. } => Some(("note", meta.id.clone())),
            RenameFolder { id, .. } | SetFolderMetadata { id, .. } | DeleteFolder { id } => {
                Some(("folder", id.clone()))
            }
            _ => None,
        }
    }
    fn needs_snapshot(&self) -> bool {
        matches!(
            self,
            Self::UpdateNote { .. }
                | Self::AppendNote { .. }
                | Self::EditNoteText { .. }
                | Self::DeleteNote { .. }
        )
    }
}
fn affected_entities(
    p: &LockedProfile<'_>,
    staged: &std::collections::BTreeMap<String, Option<String>>,
    state: &mut super::journal::State,
    force_folder_revisions: bool,
) -> Result<Vec<super::outbox::OutboxEntity>, DomainError> {
    use super::journal::Revision;
    use super::outbox::{LocalOutbox, OutboxEntity};
    let mut entities = Vec::new();
    for (path, text) in staged {
        if path == "notes/folders.json" {
            let before = folders_disk::read_folders(p)?;
            let after: Vec<FolderMeta> = serde_json::from_str(text.as_deref().unwrap_or("[]"))?;
            let ids: std::collections::BTreeSet<_> = before
                .iter()
                .chain(after.iter())
                .map(|f| f.id.clone())
                .collect();
            for id in ids {
                let old = before.iter().find(|f| f.id == id);
                let new = after.iter().find(|f| f.id == id);
                if !force_folder_revisions
                    && serde_json::to_value(old)? == serde_json::to_value(new)?
                {
                    continue;
                }
                let revision = Revision::fresh();
                state
                    .entities
                    .insert(format!("folder:{id}"), revision.clone());
                entities.push(OutboxEntity {
                    kind: "folder".into(),
                    id,
                    revision,
                    deleted: new.map(|f| f.deleted_at.is_some()).unwrap_or(true),
                    sync_state: LocalOutbox::status(p, None)?,
                    cloud_delete: false,
                });
            }
        } else if path.ends_with("/note.json") {
            let id = path
                .split('/')
                .nth(1)
                .ok_or_else(|| DomainError::InvalidInput("Invalid plan".into()))?
                .to_string();
            let note = if let Some(text) = text {
                let meta: NoteMeta = serde_json::from_str(text)?;
                let content_path = format!("notes/{id}/content.html");
                let content = match staged.get(&content_path) {
                    Some(text) => text.clone().unwrap_or_default(),
                    None => {
                        let path = p.path(std::path::Path::new(&content_path))?;
                        if path.exists() {
                            p.read_text(path)?
                        } else {
                            String::new()
                        }
                    }
                };
                Some(super::repository::NoteData { meta, content })
            } else {
                None
            };
            let previous_path =
                p.path(&std::path::Path::new("notes").join(&id).join("note.json"))?;
            let previous = if previous_path.exists() {
                Some(serde_json::from_str::<NoteMeta>(
                    &p.read_text(previous_path)?,
                )?)
            } else {
                None
            };
            // Include the latest preference in the same durable state update,
            // retaining/backfilling it when the note's files are purged.
            if let Some(meta) = note.as_ref().map(|n| &n.meta).or(previous.as_ref()) {
                state.note_privacy.insert(id.clone(), meta.local_only);
            }
            let previously_synced = previous.is_some_and(|meta| !meta.local_only);
            let cloud_delete =
                previously_synced && note.as_ref().is_some_and(|n| n.meta.local_only);
            let revision = Revision::fresh();
            state
                .entities
                .insert(format!("note:{id}"), revision.clone());
            entities.push(OutboxEntity {
                kind: "note".into(),
                id,
                revision,
                deleted: note
                    .as_ref()
                    .map(|n| n.meta.deleted_at.is_some())
                    .unwrap_or(true),
                sync_state: LocalOutbox::status(p, note.as_ref())?,
                cloud_delete,
            });
        }
    }
    Ok(entities)
}

#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportSource {
    #[default]
    Remote,
    Backup,
}
impl From<ImportSource> for Actor {
    fn from(source: ImportSource) -> Self {
        match source {
            ImportSource::Remote => Actor::Remote,
            ImportSource::Backup => Actor::Backup,
        }
    }
}
