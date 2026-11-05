//! Window-bound adapters. All barriers take registry/profile locks before the
//! editor lock, matching external mutation serialization and profile switching.
use crate::note_service::editors::{
    editors, EditPermit, EditorContext, EditorRead, EditorRegistry, PreparedMutation,
    ProfileSession,
};
use crate::note_service::journal::RevisionStore;
use crate::note_service::{Actor, DomainError, MutationIntent, MutationResult, ProfileScope};
use tauri::{AppHandle, Emitter, WebviewWindow};

fn allowed(window: &WebviewWindow) -> Result<(), DomainError> {
    if window.label() == "main" || window.label().starts_with("note-") {
        Ok(())
    } else {
        Err(DomainError::EditorSessionExpired)
    }
}
fn scope(
    app: &AppHandle,
    window: &WebviewWindow,
    context: &EditorContext,
) -> Result<ProfileScope, DomainError> {
    allowed(window)?;
    editors().validate_context(window.label(), context)?;
    ProfileScope::resolve(app, &context.profile_id)
}

#[tauri::command]
pub fn get_profile_session(
    app: AppHandle,
    window: WebviewWindow,
) -> Result<ProfileSession, String> {
    (|| {
        allowed(&window)?;
        if window.label() != "main" {
            return editors()
                .window_session(window.label())
                .ok_or(DomainError::EditorSessionExpired);
        }
        let profile = ProfileScope::active(&app)?;
        profile.transaction(|p| {
            if !p.is_active()? {
                return Err(DomainError::EditorSessionExpired);
            }
            let session = editors().session(p.scope().local_id());
            editors().bind_window(window.label(), session.clone())?;
            Ok(session)
        })
    })()
    .map_err(|e: DomainError| e.to_string())
}

/// Targeted sidebar/wiki-link refresh. This reads metadata only and never
/// participates in editor ownership or loads a note's content.
#[tauri::command]
pub fn read_note_metadata_batch(
    app: AppHandle,
    window: WebviewWindow,
    session: ProfileSession,
    note_ids: Vec<String>,
) -> Result<Vec<crate::notes::NoteMeta>, String> {
    (|| {
        allowed(&window)?;
        ProfileScope::resolve(&app, &session.profile_id)?.transaction(|p| {
            read_metadata_batch_locked(editors(), p, window.label(), &session, note_ids)
        })
    })()
    .map_err(|e: DomainError| e.to_string())
}

fn read_metadata_batch_locked(
    registry: &EditorRegistry,
    profile: &crate::note_service::LockedProfile<'_>,
    window: &str,
    session: &ProfileSession,
    note_ids: Vec<String>,
) -> Result<Vec<crate::notes::NoteMeta>, DomainError> {
    if note_ids.len() > 100 {
        return Err(DomainError::InvalidInput(
            "At most 100 note IDs per metadata batch".into(),
        ));
    }
    registry.validate_session(&session.profile_id, session.session_generation)?;
    if profile.scope().local_id() != session.profile_id {
        return Err(DomainError::EditorSessionExpired);
    }
    if window == "main" {
        if !profile.is_active()? {
            return Err(DomainError::EditorSessionExpired);
        }
    } else {
        let bound = registry
            .window_session(window)
            .ok_or(DomainError::EditorSessionExpired)?;
        if !window.starts_with("note-")
            || bound.profile_id != session.profile_id
            || bound.session_generation != session.session_generation
        {
            return Err(DomainError::EditorSessionExpired);
        }
    }
    let mut result = Vec::new();
    for id in note_ids
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>()
    {
        match crate::note_service::NoteRepository::metadata_locked(profile, &id) {
            Ok(meta) if meta.deleted_at.is_none() => result.push(meta),
            Ok(_) | Err(DomainError::NotFound(_)) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(result)
}

#[tauri::command]
pub fn register_note_editor(
    app: AppHandle,
    window: WebviewWindow,
    note_id: String,
    editor_id: String,
    session: ProfileSession,
) -> Result<EditorRead, String> {
    (|| {
        allowed(&window)?;
        let profile = ProfileScope::resolve(&app, &session.profile_id)?;
        profile.transaction(|p| {
            editors().validate_session(&session.profile_id, session.session_generation)?;
            if window.label() == "main" {
                if !p.is_active()? {
                    return Err(DomainError::EditorSessionExpired);
                }
            } else if window.label() != crate::window::note_window_label(&note_id) {
                return Err(DomainError::EditorSessionExpired);
            }
            editors().bind_window(window.label(), session)?;
            editors().register_note_locked(window.label(), p, &note_id, &editor_id)
        })
    })()
    .map_err(|e: DomainError| e.to_string())
}

#[tauri::command]
pub fn read_editor_note(
    app: AppHandle,
    window: WebviewWindow,
    context: EditorContext,
) -> Result<EditorRead, String> {
    (|| {
        scope(&app, &window, &context)?
            .transaction(|p| editors().read_note_locked(window.label(), p, &context))
    })()
    .map_err(|e| e.to_string())
}
#[tauri::command]
pub fn begin_note_edit(
    app: AppHandle,
    window: WebviewWindow,
    context: EditorContext,
) -> Result<(), String> {
    (|| scope(&app, &window, &context)?.transaction(|_| editors().begin(window.label(), &context)))(
    )
    .map_err(|e| e.to_string())
}
#[tauri::command]
pub fn save_editor_note(
    app: AppHandle,
    window: WebviewWindow,
    context: EditorContext,
    content: String,
    title: String,
    operation_id: String,
) -> Result<EditorRead, String> {
    // save_note_locked authorizes stable identity before receipt replay; mutable
    // dirty/revision checks run only for a genuinely new transaction.
    let event_operation_id = operation_id.clone();
    let result = (|| {
        allowed(&window)?;
        ProfileScope::resolve(&app, &context.profile_id)?.transaction(|p| {
            editors().save_note_locked(window.label(), p, &context, content, title, operation_id)
        })
    })()
    .map_err(|e: DomainError| e.to_string())?;
    let _ = app.emit("note-service-changed",serde_json::json!({"profile_id":result.context.profile_id,"note_id":result.context.note_id,"revision":result.context.loaded_revision,"operation_id":event_operation_id,"source":"ui","mutation_type":"update"}));
    Ok(result)
}
#[tauri::command]
pub fn ack_editor_prepare(
    app: AppHandle,
    window: WebviewWindow,
    context: EditorContext,
    prepare_id: String,
    dirty: bool,
) -> Result<(), String> {
    (|| {
        scope(&app, &window, &context)?
            .transaction(|_| editors().ack(window.label(), &context, &prepare_id, dirty))
    })()
    .map_err(|e| e.to_string())
}
#[tauri::command]
pub fn end_note_edit(
    app: AppHandle,
    window: WebviewWindow,
    context: EditorContext,
) -> Result<(), String> {
    (|| scope(&app, &window, &context)?.transaction(|_| editors().end(window.label(), &context)))()
        .map_err(|e| e.to_string())
}

pub async fn prepare_external_mutation(
    app: &AppHandle,
    scope: &ProfileScope,
    note_id: &str,
) -> Result<EditPermit, DomainError> {
    let (permit, targets) = scope.transaction(|p| {
        editors().start_prepare(
            scope.local_id(),
            note_id,
            RevisionStore::note_locked(p, note_id)?,
        )
    })?;
    // Cancellation of the async wait resumes renderers and releases only the
    // reservation. It never resets an editor's dirty/unavailable state.
    struct Waiting<'a> {
        app: &'a AppHandle,
        permit: Option<EditPermit>,
    }
    impl Drop for Waiting<'_> {
        fn drop(&mut self) {
            if let Some(permit) = self.permit.take() {
                finish_external_mutation(self.app, &permit, false);
            }
        }
    }
    let mut waiting = Waiting {
        app,
        permit: Some(permit),
    };
    for target in targets {
        if app
            .emit_to(target.window.as_str(), "note-editor-prepare", &target.event)
            .is_err()
        {
            return Err(DomainError::EditorBusy);
        }
    }
    loop {
        match editors().ready(waiting.permit.as_ref().unwrap()) {
            Ok(true) => return Ok(waiting.permit.take().unwrap()),
            Ok(false) => tokio::time::sleep(std::time::Duration::from_millis(10)).await,
            Err(error) => return Err(error),
        }
    }
}
pub fn finish_external_mutation(app: &AppHandle, permit: &EditPermit, committed: bool) {
    for target in editors().release(permit) {
        let _ = app.emit_to(target.window.as_str(),"note-editor-resume",serde_json::json!({"context":target.event.context,"prepare_id":target.event.prepare_id,"committed":committed}));
    }
}

/// Legacy UI/backup adapters join exactly the same ownership protocol as MCP.
/// Capture once, reserve every affected owner, await without profile locks, then
/// validate collection/session/permits in the durable commit transaction.
pub async fn mutate_ui(
    app: &AppHandle,
    window: &WebviewWindow,
    session: Option<ProfileSession>,
    intent: MutationIntent,
    actor: Actor,
) -> Result<MutationResult, DomainError> {
    allowed(window)?;
    let session = match session {
        Some(session) => session,
        None if window.label() != "main" => editors()
            .window_session(window.label())
            .ok_or(DomainError::EditorSessionExpired)?,
        None => {
            let scope = ProfileScope::active(app)?;
            scope.transaction(|p| {
                if !p.is_active()? {
                    return Err(DomainError::EditorSessionExpired);
                }
                Ok(editors().session(p.scope().local_id()))
            })?
        }
    };
    if window.label() != "main" {
        let bound = editors()
            .window_session(window.label())
            .ok_or(DomainError::EditorSessionExpired)?;
        if bound.profile_id != session.profile_id
            || bound.session_generation != session.session_generation
        {
            return Err(DomainError::EditorSessionExpired);
        }
    }
    let scope = ProfileScope::resolve(app, &session.profile_id)?;
    let (prepared, targets) =
        scope.transaction(|p| editors().prepare_mutation_locked(p, session, intent))?;
    struct Waiting<'a> {
        app: &'a AppHandle,
        prepared: &'a PreparedMutation,
        committed: bool,
    }
    impl Drop for Waiting<'_> {
        fn drop(&mut self) {
            for permit in &self.prepared.permits {
                finish_external_mutation(self.app, permit, self.committed);
            }
        }
    }
    let mut waiting = Waiting {
        app,
        prepared: &prepared,
        committed: false,
    };
    for target in targets {
        app.emit_to(target.window.as_str(), "note-editor-prepare", &target.event)
            .map_err(|_| DomainError::EditorBusy)?;
    }
    loop {
        let mut ready = true;
        for permit in &prepared.permits {
            ready &= editors().ready(permit)?;
        }
        if ready {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let source = match &actor {
        Actor::Backup => "backup",
        Actor::Remote => "remote",
        _ => "ui",
    };
    let (receipt, changed_notes) = scope.transaction(|p| {
        let receipt = editors().commit_prepared_mutation_locked(p, &prepared, actor)?;
        let mut ids: std::collections::BTreeSet<_> = prepared
            .permits
            .iter()
            .map(|permit| permit.note_id.clone())
            .collect();
        if let MutationResult::Note(note) = &receipt.result {
            ids.insert(note.id.clone());
        }
        let changed = ids
            .into_iter()
            .map(|id| Ok((id.clone(), RevisionStore::note_locked(p, &id)?)))
            .collect::<Result<Vec<_>, DomainError>>()?;
        Ok((receipt, changed))
    })?;
    waiting.committed = true;
    drop(waiting); // Release before dispatching refreshes; no callbacks under profile lock.
    for (id, revision) in changed_notes {
        let _ = app.emit("note-service-changed", serde_json::json!({"profile_id":receipt.profile_id,"note_id":id,"revision":revision,"operation_id":receipt.operation_id,"source":source,"mutation_type":"update"}));
    }
    if prepared.folders_changed {
        let _ = app.emit("folder-service-changed", serde_json::json!({"profile_id":receipt.profile_id,"operation_id":receipt.operation_id,"revision":receipt.revision,"source":source}));
    }
    Ok(receipt.result)
}

#[cfg(test)]
mod metadata_batch_tests {
    use super::*;
    use crate::note_service::scope::tests::{meta, Fixture, ID};
    use std::fs;

    #[test]
    fn targeted_metadata_batch_deduplicates_and_never_reads_content() {
        let fixture = Fixture::new();
        let scope = fixture.scope("perso");
        let registry = EditorRegistry::default();
        let session = registry.session("perso");
        let directory = scope.root().join("notes").join(ID);
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("note.json"),
            serde_json::to_vec(&meta(ID)).unwrap(),
        )
        .unwrap();
        // Invalid UTF-8 would make even an accidental content read fail.
        fs::write(directory.join("content.html"), [0xff, 0xfe]).unwrap();
        let values = scope
            .transaction(|p| {
                read_metadata_batch_locked(
                    &registry,
                    p,
                    "main",
                    &session,
                    vec![ID.into(), ID.into(), "missing".into()],
                )
            })
            .unwrap();
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].id, ID);
        let mut deleted = meta(ID);
        deleted.deleted_at = Some("deleted".into());
        fs::write(
            directory.join("note.json"),
            serde_json::to_vec(&deleted).unwrap(),
        )
        .unwrap();
        assert!(scope
            .transaction(|p| read_metadata_batch_locked(
                &registry,
                p,
                "main",
                &session,
                vec![ID.into()]
            ))
            .unwrap()
            .is_empty());
        fs::write(directory.join("note.json"), b"{corrupt").unwrap();
        assert!(matches!(
            scope.transaction(|p| read_metadata_batch_locked(
                &registry,
                p,
                "main",
                &session,
                vec![ID.into()]
            )),
            Err(DomainError::Storage(_))
        ));
    }

    #[test]
    fn metadata_batch_requires_active_main_or_exact_native_detached_session() {
        let fixture = Fixture::new();
        let work = fixture.scope("work");
        let registry = EditorRegistry::default();
        let session = registry.session("work");
        assert!(matches!(
            work.transaction(|p| read_metadata_batch_locked(
                &registry,
                p,
                "main",
                &session,
                vec![]
            )),
            Err(DomainError::EditorSessionExpired)
        ));
        assert!(matches!(
            work.transaction(|p| read_metadata_batch_locked(
                &registry,
                p,
                "note-test",
                &session,
                vec![]
            )),
            Err(DomainError::EditorSessionExpired)
        ));
        registry.bind_window("note-test", session.clone()).unwrap();
        assert!(work
            .transaction(|p| read_metadata_batch_locked(
                &registry,
                p,
                "note-test",
                &session,
                vec![]
            ))
            .is_ok());
        let foreign = registry.session("perso");
        assert!(matches!(
            work.transaction(|p| read_metadata_batch_locked(
                &registry,
                p,
                "note-test",
                &foreign,
                vec![]
            )),
            Err(DomainError::EditorSessionExpired)
        ));
        assert!(matches!(
            work.transaction(|p| read_metadata_batch_locked(
                &registry,
                p,
                "note-test",
                &session,
                vec![ID.into(); 101]
            )),
            Err(DomainError::InvalidInput(_))
        ));
        // Switch away and back: same profile ID still has an expired generation.
        registry.invalidate_sessions().unwrap();
        registry
            .bind_window("note-test", registry.session("work"))
            .unwrap();
        assert!(matches!(
            work.transaction(|p| read_metadata_batch_locked(
                &registry,
                p,
                "note-test",
                &session,
                vec![]
            )),
            Err(DomainError::EditorSessionExpired)
        ));
    }
}
