use crate::note_service::{
    editors::editors,
    journal::RevisionStore,
    mutation::{Actor, MutationIntent, MutationRequest, NoteService},
    outbox::RemoteDecision,
    repository::{FolderMeta, NoteMeta},
    NoteRepository,
};
use crate::note_service::{
    editors::ProfileSession,
    outbox::{LocalOutbox, ResolvedBatch},
    DomainError,
};
use tauri::Emitter;
use tauri::{AppHandle, WebviewWindow};

#[tauri::command]
pub fn seed_local_sync_snapshot(
    app: AppHandle,
    window: WebviewWindow,
    session: ProfileSession,
) -> Result<(), String> {
    super::sync_queue::with_session(&app, &window, &session, LocalOutbox::seed_locked)
}

#[derive(serde::Serialize)]
pub struct ApplyResult {
    applied: bool,
    deferred: bool,
}

/// A captured renderer session is revalidated inside the same profile lock as
/// LWW, pending-intent checks, editor permit validation, and the journal commit.
#[tauri::command]
pub async fn apply_sync_remote_note(
    app: AppHandle,
    window: WebviewWindow,
    session: ProfileSession,
    meta: NoteMeta,
    content: String,
) -> Result<ApplyResult, String> {
    super::sync_queue::with_session(&app, &window, &session, |_| Ok(()))?;
    let scope = crate::note_service::ProfileScope::resolve(&app, &session.profile_id)
        .map_err(|e| e.to_string())?;
    let permit = match super::note_editors::prepare_external_mutation(&app, &scope, &meta.id).await
    {
        Ok(permit) => permit,
        Err(DomainError::EditorBusy) => {
            return Ok(ApplyResult {
                applied: false,
                deferred: true,
            })
        }
        Err(error) => return Err(error.to_string()),
    };
    let result = super::sync_queue::with_session(&app, &window, &session, |p| {
        match LocalOutbox::remote_note_decision_locked(p, &meta, &content)? {
            RemoteDecision::Defer => {
                return Ok(ApplyResult {
                    applied: false,
                    deferred: true,
                })
            }
            RemoteDecision::Skip => {
                return Ok(ApplyResult {
                    applied: false,
                    deferred: false,
                })
            }
            RemoteDecision::Apply => {}
        }
        let operation_id = uuid::Uuid::new_v4().to_string();
        let revision = RevisionStore::note_locked(p, &meta.id)?;
        let receipt = NoteService::mutate_checked_with_precondition_locked(
            p,
            MutationRequest {
                operation_id: operation_id.clone(),
                expected_revision: Some(revision),
                intent: MutationIntent::ImportNote {
                    meta: meta.clone(),
                    content,
                },
            },
            Actor::Remote,
            |_| Ok(()),
            |p| editors().validate_permit_locked(&permit, p),
        )?;
        let _ = app.emit("note-service-changed", serde_json::json!({ "profile_id": session.profile_id, "note_id": meta.id, "revision": receipt.revision, "operation_id": operation_id, "source": "remote", "mutation_type": "import" }));
        Ok(ApplyResult {
            applied: true,
            deferred: false,
        })
    });
    super::note_editors::finish_external_mutation(
        &app,
        &permit,
        result.as_ref().is_ok_and(|r| r.applied),
    );
    result
}

#[tauri::command]
pub fn apply_sync_remote_folder(
    app: AppHandle,
    window: WebviewWindow,
    session: ProfileSession,
    folder: FolderMeta,
) -> Result<ApplyResult, String> {
    super::sync_queue::with_session(&app, &window, &session, |p| {
        // Include unrelated tombstones: replacing an active-only list would
        // silently purge another folder whose deletion is still pending.
        let (decision, all) = LocalOutbox::merge_remote_folder_locked(p, &folder)?;
        match decision {
            RemoteDecision::Defer => {
                return Ok(ApplyResult {
                    applied: false,
                    deferred: true,
                })
            }
            RemoteDecision::Skip => {
                return Ok(ApplyResult {
                    applied: false,
                    deferred: false,
                })
            }
            RemoteDecision::Apply => {}
        }
        let operation_id = uuid::Uuid::new_v4().to_string();
        NoteService::mutate_checked_locked(
            p,
            MutationRequest {
                operation_id: operation_id.clone(),
                expected_revision: None,
                intent: MutationIntent::ImportFolders {
                    folders: all.ok_or(DomainError::RecoveryRequired)?,
                },
            },
            Actor::Remote,
            |_| Ok(()),
        )?;
        let revision = RevisionStore::folder_locked(p, &folder.id)?;
        let _ = app.emit("folder-service-changed", serde_json::json!({ "profile_id": session.profile_id, "folder_id": folder.id, "revision": revision, "operation_id": operation_id, "source": "remote", "mutation_type": "import" }));
        Ok(ApplyResult {
            applied: true,
            deferred: false,
        })
    })
}

#[tauri::command]
pub async fn purge_sync_tombstones(
    app: AppHandle,
    window: WebviewWindow,
    session: ProfileSession,
    note_ids: Vec<String>,
    folder_ids: Vec<String>,
) -> Result<bool, String> {
    let scope = crate::note_service::ProfileScope::resolve(&app, &session.profile_id)
        .map_err(|e| e.to_string())?;
    let mut deferred = false;
    for id in note_ids {
        let candidate = super::sync_queue::with_session(&app, &window, &session, |p| {
            if LocalOutbox::has_intent_locked(p, "note", &id)? {
                return Ok(false);
            }
            match NoteRepository::get_locked(p, &id) {
                Ok(note) => Ok(note.meta.deleted_at.is_some()),
                Err(DomainError::NotFound(_)) => Ok(false),
                Err(error) => Err(error),
            }
        })?;
        if !candidate {
            continue;
        }
        let permit = match super::note_editors::prepare_external_mutation(&app, &scope, &id).await {
            Ok(permit) => permit,
            Err(DomainError::EditorBusy) => {
                deferred = true;
                continue;
            }
            Err(error) => return Err(error.to_string()),
        };
        let result = super::sync_queue::with_session(&app, &window, &session, |p| {
            if LocalOutbox::has_intent_locked(p, "note", &id)? {
                return Ok(false);
            }
            NoteService::mutate_checked_with_precondition_locked(
                p,
                MutationRequest {
                    operation_id: uuid::Uuid::new_v4().to_string(),
                    expected_revision: None,
                    intent: MutationIntent::PurgeNotes {
                        note_ids: vec![id.clone()],
                    },
                },
                Actor::Remote,
                |_| Ok(()),
                |p| editors().validate_permit_locked(&permit, p),
            )?;
            Ok(true)
        });
        super::note_editors::finish_external_mutation(
            &app,
            &permit,
            result.as_ref().is_ok_and(|committed| *committed),
        );
        if !result? {
            deferred = true;
        }
    }
    super::sync_queue::with_session(&app, &window, &session, |p| {
        let mut folders = vec![];
        for id in folder_ids {
            if !LocalOutbox::has_intent_locked(p, "folder", &id)? {
                folders.push(id);
            }
        }
        if !folders.is_empty() {
            NoteService::mutate_locked(
                p,
                MutationIntent::PurgeFolders {
                    folder_ids: folders,
                },
                Actor::Remote,
            )?;
        }
        Ok(())
    })?;
    Ok(deferred)
}

#[tauri::command]
pub fn list_local_mutations(
    app: AppHandle,
    window: WebviewWindow,
    session: ProfileSession,
    after_sequence: u64,
    limit: Option<usize>,
) -> Result<ResolvedBatch, String> {
    super::sync_queue::with_session(&app, &window, &session, |p| {
        LocalOutbox::resolve_locked(p, after_sequence, limit.unwrap_or(100))
    })
}

#[tauri::command]
pub fn ack_local_mutations(
    app: AppHandle,
    window: WebviewWindow,
    session: ProfileSession,
    through_sequence: u64,
) -> Result<(), String> {
    super::sync_queue::with_session(&app, &window, &session, |p| {
        let queue =
            crate::sync_queue::load_snapshot(&p.path(std::path::Path::new("sync-queue.json"))?)
                .map_err(DomainError::Storage)?;
        if queue.imported_outbox_sequence < through_sequence {
            return Err(DomainError::InvalidInput(
                "Outbox checkpoint was not persisted".into(),
            ));
        }
        LocalOutbox::ack_locked(p, through_sequence)
    })
}
