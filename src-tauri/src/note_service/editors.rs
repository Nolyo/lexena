use super::journal::{Revision, RevisionStore};
use super::mutation::{MutationReceipt, MutationRequest};
use super::repository::NoteData;
use super::{Actor, DomainError, LockedProfile, MutationIntent, NoteRepository, NoteService};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::OnceLock,
    time::{Duration, Instant},
};
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct EditorContext {
    pub profile_id: String,
    pub note_id: String,
    pub session_generation: u64,
    pub editor_id: String,
    pub loaded_revision: Revision,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProfileSession {
    pub profile_id: String,
    pub session_generation: u64,
}
#[derive(Clone, Debug, Serialize)]
pub struct EditorRead {
    pub context: EditorContext,
    pub note: NoteData,
}
#[derive(Clone, Debug, Serialize)]
pub struct PrepareEvent {
    pub context: EditorContext,
    pub prepare_id: String,
}
#[derive(Clone, Debug)]
pub struct EditorTarget {
    pub window: String,
    pub event: PrepareEvent,
}
#[derive(Clone)]
struct Entry {
    window: String,
    context: EditorContext,
    dirty: bool,
    alive: bool,
    ack: Option<String>,
}
struct Reservation {
    id: String,
    profile: String,
    note: String,
    revision: Revision,
    deadline: Instant,
    targets: Vec<String>,
}
#[derive(Default)]
struct State {
    generation: u64,
    entries: HashMap<String, Entry>,
    windows: HashMap<String, ProfileSession>,
    pending: HashMap<String, Reservation>,
}
#[derive(Default)]
pub struct EditorRegistry {
    state: Mutex<State>,
}
pub fn editors() -> &'static EditorRegistry {
    static REGISTRY: OnceLock<EditorRegistry> = OnceLock::new();
    REGISTRY.get_or_init(EditorRegistry::default)
}
pub struct EditPermit {
    pub prepare_id: String,
    pub profile_id: String,
    pub note_id: String,
    pub revision: Revision,
}
/// Immutable intent + collection snapshot captured before awaiting renderer acks.
/// Collection CAS prevents folder membership changes from escaping preparation.
pub struct PreparedMutation {
    pub permits: Vec<EditPermit>,
    pub folders_changed: bool,
    session: ProfileSession,
    collection_revision: Revision,
    request: MutationRequest,
}
pub struct EditorSwitchGuard<'a> {
    state: parking_lot::MutexGuard<'a, State>,
}
impl EditorSwitchGuard<'_> {
    pub fn commit(mut self) {
        self.state.generation += 1;
        self.state.entries.clear();
        self.state.windows.clear();
        self.state.pending.clear();
    }
}
impl EditorRegistry {
    fn validate_mutation_session_locked(
        &self,
        p: &LockedProfile<'_>,
        session: &ProfileSession,
    ) -> Result<(), DomainError> {
        self.validate_session(&session.profile_id, session.session_generation)?;
        if p.scope().local_id() != session.profile_id || !p.is_active()? {
            return Err(DomainError::EditorSessionExpired);
        }
        Ok(())
    }
    pub fn prepare_mutation_locked(
        &self,
        p: &LockedProfile<'_>,
        session: ProfileSession,
        intent: MutationIntent,
    ) -> Result<(PreparedMutation, Vec<EditorTarget>), DomainError> {
        self.validate_mutation_session_locked(p, &session)?;
        use MutationIntent::*;
        let note_ids = match &intent {
            UpdateNote { id, .. }
            | AppendNote { id, .. }
            | EditNoteText { id, .. }
            | SetNoteMetadata { id, .. }
            | RestoreNote { id, .. }
            | DeleteNote { id }
            | ToggleFavorite { id }
            | SetLocalOnly { id, .. } => vec![id.clone()],
            ImportNote { meta, .. } => vec![meta.id.clone()],
            MoveNote { note_id, .. } => vec![note_id.clone()],
            ReorderNotes { note_ids, .. } | PurgeNotes { note_ids } => note_ids.clone(),
            DeleteFolder { id } => NoteRepository::list_locked(p)?
                .into_iter()
                .filter(|n| n.folder_id.as_ref() == Some(id))
                .map(|n| n.id)
                .collect(),
            _ => vec![],
        };
        let folders_changed = matches!(
            &intent,
            CreateFolder { .. }
                | RenameFolder { .. }
                | SetFolderMetadata { .. }
                | DeleteFolder { .. }
                | ReorderFolders { .. }
                | ImportFolders { .. }
                | PurgeFolders { .. }
        );
        let collection_revision = RevisionStore::collection_locked(p)?;
        let mut permits = Vec::new();
        let mut targets = Vec::new();
        let unique: std::collections::BTreeSet<_> = note_ids.into_iter().collect();
        for id in unique {
            let result = RevisionStore::note_locked(p, &id)
                .and_then(|revision| self.start_prepare(p.scope().local_id(), &id, revision));
            match result {
                Ok((permit, owners)) => {
                    permits.push(permit);
                    targets.extend(owners);
                }
                Err(error) => {
                    // No prepare event has been emitted yet; release earlier reservations.
                    for permit in &permits {
                        self.release(permit);
                    }
                    return Err(error);
                }
            }
        }
        Ok((
            PreparedMutation {
                permits,
                folders_changed,
                session,
                collection_revision,
                request: MutationRequest {
                    operation_id: uuid::Uuid::new_v4().to_string(),
                    expected_revision: None,
                    intent,
                },
            },
            targets,
        ))
    }
    pub fn commit_prepared_mutation_locked(
        &self,
        p: &mut LockedProfile<'_>,
        prepared: &PreparedMutation,
        actor: Actor,
    ) -> Result<MutationReceipt, DomainError> {
        NoteService::mutate_checked_with_precondition_locked(
            p,
            prepared.request.clone(),
            actor,
            |p| self.validate_mutation_session_locked(p, &prepared.session),
            |p| {
                if RevisionStore::collection_locked(p)? != prepared.collection_revision {
                    return Err(DomainError::RevisionConflict);
                }
                for permit in &prepared.permits {
                    self.validate_permit_locked(permit, p)?;
                }
                Ok(())
            },
        )
    }
    pub fn prepare_profile_switch(&self) -> Result<EditorSwitchGuard<'_>, DomainError> {
        let state = self.state.lock();
        if state.entries.values().any(|e| e.dirty || !e.alive) || !state.pending.is_empty() {
            return Err(DomainError::EditorBusy);
        }
        Ok(EditorSwitchGuard { state })
    }
    pub fn invalidate_sessions(&self) -> Result<(), DomainError> {
        self.prepare_profile_switch()?.commit();
        Ok(())
    }
    /// Only the explicitly confirmed all-data reset may discard dirty editors.
    /// Called under registry lifecycle exclusion after admitted work has drained.
    pub(crate) fn discard_sessions_for_reset(&self) {
        EditorSwitchGuard {
            state: self.state.lock(),
        }
        .commit();
    }
    pub fn session(&self, profile: &str) -> ProfileSession {
        ProfileSession {
            profile_id: profile.into(),
            session_generation: self.state.lock().generation,
        }
    }
    pub fn validate_session(&self, profile: &str, generation: u64) -> Result<(), DomainError> {
        let s = self.state.lock();
        if profile.is_empty() || s.generation != generation {
            return Err(DomainError::EditorSessionExpired);
        }
        Ok(())
    }
    pub fn bind_window(&self, window: &str, session: ProfileSession) -> Result<(), DomainError> {
        let mut s = self.state.lock();
        if s.generation != session.session_generation {
            return Err(DomainError::EditorSessionExpired);
        }
        if let Some(old) = s.windows.get(window) {
            if old.profile_id != session.profile_id
                || old.session_generation != session.session_generation
            {
                return Err(DomainError::EditorSessionExpired);
            }
        }
        s.windows.insert(window.into(), session);
        Ok(())
    }
    pub fn window_session(&self, window: &str) -> Option<ProfileSession> {
        self.state.lock().windows.get(window).cloned()
    }
    fn identity<'a>(
        s: &'a State,
        window: &str,
        c: &EditorContext,
    ) -> Result<&'a Entry, DomainError> {
        if s.generation != c.session_generation {
            return Err(DomainError::EditorSessionExpired);
        }
        let e = s
            .entries
            .get(&c.editor_id)
            .ok_or(DomainError::EditorSessionExpired)?;
        if !e.alive
            || e.window != window
            || e.context.profile_id != c.profile_id
            || e.context.note_id != c.note_id
        {
            return Err(DomainError::EditorSessionExpired);
        }
        Ok(e)
    }
    fn entry<'a>(s: &'a State, window: &str, c: &EditorContext) -> Result<&'a Entry, DomainError> {
        let e = Self::identity(s, window, c)?;
        if e.context.loaded_revision != c.loaded_revision {
            return Err(DomainError::RevisionConflict);
        }
        Ok(e)
    }
    pub fn register(
        &self,
        window: &str,
        profile: &str,
        note: &str,
        editor: &str,
        revision: Revision,
    ) -> Result<EditorContext, DomainError> {
        let mut s = self.state.lock();
        if s.pending
            .values()
            .any(|p| p.profile == profile && p.note == note)
        {
            return Err(DomainError::EditorBusy);
        }
        if s.entries.contains_key(editor) {
            return Err(DomainError::EditorBusy);
        }
        let context = EditorContext {
            profile_id: profile.into(),
            note_id: note.into(),
            session_generation: s.generation,
            editor_id: editor.into(),
            loaded_revision: revision,
        };
        s.entries.insert(
            editor.into(),
            Entry {
                window: window.into(),
                context: context.clone(),
                dirty: false,
                alive: true,
                ack: None,
            },
        );
        Ok(context)
    }
    pub fn validate_context(
        &self,
        window: &str,
        context: &EditorContext,
    ) -> Result<(), DomainError> {
        Self::entry(&self.state.lock(), window, context)?;
        Ok(())
    }
    pub fn begin(&self, window: &str, context: &EditorContext) -> Result<(), DomainError> {
        let mut s = self.state.lock();
        Self::entry(&s, window, context)?;
        // Record the dirty barrier even when preparation won: this invalidates its final guard.
        s.entries.get_mut(&context.editor_id).unwrap().dirty = true;
        if s.pending
            .values()
            .any(|p| p.profile == context.profile_id && p.note == context.note_id)
        {
            return Err(DomainError::EditorBusy);
        }
        Ok(())
    }
    pub fn save_guard(
        &self,
        window: &str,
        context: &EditorContext,
        p: &LockedProfile<'_>,
    ) -> Result<(), DomainError> {
        let s = self.state.lock();
        Self::entry(&s, window, context)?;
        if p.scope().local_id() != context.profile_id {
            return Err(DomainError::EditorSessionExpired);
        }
        if s.pending
            .values()
            .any(|p| p.profile == context.profile_id && p.note == context.note_id)
        {
            return Err(DomainError::EditorBusy);
        }
        if RevisionStore::note_locked(p, &context.note_id)? != context.loaded_revision {
            return Err(DomainError::RevisionConflict);
        }
        Ok(())
    }
    pub fn saved(
        &self,
        window: &str,
        context: &EditorContext,
        revision: Revision,
    ) -> Result<EditorContext, DomainError> {
        let mut s = self.state.lock();
        Self::entry(&s, window, context)?;
        let e = s.entries.get_mut(&context.editor_id).unwrap();
        e.context.loaded_revision = revision;
        e.dirty = false;
        Ok(e.context.clone())
    }
    pub fn reloaded(
        &self,
        window: &str,
        context: &EditorContext,
        revision: Revision,
    ) -> Result<EditorContext, DomainError> {
        let mut s = self.state.lock();
        if Self::entry(&s, window, context)?.dirty {
            return Err(DomainError::EditorBusy);
        }
        let e = s.entries.get_mut(&context.editor_id).unwrap();
        e.context.loaded_revision = revision;
        Ok(e.context.clone())
    }
    pub fn end(&self, window: &str, context: &EditorContext) -> Result<(), DomainError> {
        let mut s = self.state.lock();
        if Self::entry(&s, window, context)?.dirty
            || s.pending
                .values()
                .any(|p| p.targets.contains(&context.editor_id))
        {
            return Err(DomainError::EditorBusy);
        }
        s.entries.remove(&context.editor_id);
        Ok(())
    }
    /// A destroyed WebView cannot prove its last transactions were persisted.
    /// Keep an unavailable entry until process restart; expiry never implies clean.
    pub fn window_destroyed(&self, window: &str) {
        let mut s = self.state.lock();
        for e in s.entries.values_mut().filter(|e| e.window == window) {
            e.alive = false;
        }
        s.windows.remove(window);
    }
    pub fn register_note_locked(
        &self,
        window: &str,
        p: &LockedProfile<'_>,
        note: &str,
        editor: &str,
    ) -> Result<EditorRead, DomainError> {
        let data = NoteRepository::get_locked(p, note)?;
        let revision = RevisionStore::note_locked(p, note)?;
        let context = self.register(window, p.scope().local_id(), note, editor, revision)?;
        Ok(EditorRead {
            context,
            note: data,
        })
    }
    pub fn read_note_locked(
        &self,
        window: &str,
        p: &LockedProfile<'_>,
        context: &EditorContext,
    ) -> Result<EditorRead, DomainError> {
        self.validate_context(window, context)?;
        if p.scope().local_id() != context.profile_id {
            return Err(DomainError::EditorSessionExpired);
        }
        let note = NoteRepository::get_locked(p, &context.note_id)?;
        let revision = RevisionStore::note_locked(p, &context.note_id)?;
        Ok(EditorRead {
            context: self.reloaded(window, context, revision)?,
            note,
        })
    }
    pub fn save_note_locked(
        &self,
        window: &str,
        p: &mut LockedProfile<'_>,
        context: &EditorContext,
        content: String,
        title: String,
        operation_id: String,
    ) -> Result<EditorRead, DomainError> {
        let receipt = NoteService::mutate_checked_with_precondition_locked(
            p,
            MutationRequest {
                operation_id,
                expected_revision: Some(context.loaded_revision.clone()),
                intent: MutationIntent::UpdateNote {
                    id: context.note_id.clone(),
                    content,
                    title,
                },
            },
            Actor::Ui,
            |p| {
                if p.scope().local_id() != context.profile_id {
                    return Err(DomainError::EditorSessionExpired);
                }
                Self::identity(&self.state.lock(), window, context)?;
                Ok(())
            },
            |p| self.save_guard(window, context, p),
        )?;
        // Idempotent retry may carry the pre-save revision; identity stays bound.
        let mut s = self.state.lock();
        Self::identity(&s, window, context)?;
        let e = s.entries.get_mut(&context.editor_id).unwrap();
        if e.context.loaded_revision == context.loaded_revision {
            e.context.loaded_revision = receipt.revision;
            e.dirty = false;
        } else if e.context.loaded_revision != receipt.revision {
            return Err(DomainError::RevisionConflict);
        }
        let context = e.context.clone();
        drop(s);
        let note = NoteRepository::get_locked(p, &context.note_id)?;
        Ok(EditorRead { context, note })
    }
    pub fn start_prepare(
        &self,
        profile: &str,
        note: &str,
        revision: Revision,
    ) -> Result<(EditPermit, Vec<EditorTarget>), DomainError> {
        let mut s = self.state.lock();
        if s.pending
            .values()
            .any(|p| p.profile == profile && p.note == note)
        {
            return Err(DomainError::EditorBusy);
        }
        let entries: Vec<_> = s
            .entries
            .values()
            .filter(|e| e.context.profile_id == profile && e.context.note_id == note)
            .collect();
        if entries
            .iter()
            .any(|e| e.dirty || !e.alive || e.context.loaded_revision != revision)
        {
            return Err(DomainError::EditorBusy);
        }
        let prepare_id = uuid::Uuid::new_v4().to_string();
        let targets: Vec<_> = entries
            .iter()
            .map(|e| EditorTarget {
                window: e.window.clone(),
                event: PrepareEvent {
                    context: e.context.clone(),
                    prepare_id: prepare_id.clone(),
                },
            })
            .collect();
        s.pending.insert(
            prepare_id.clone(),
            Reservation {
                id: prepare_id.clone(),
                profile: profile.into(),
                note: note.into(),
                revision: revision.clone(),
                deadline: Instant::now() + Duration::from_secs(2),
                targets: targets
                    .iter()
                    .map(|t| t.event.context.editor_id.clone())
                    .collect(),
            },
        );
        Ok((
            EditPermit {
                prepare_id,
                profile_id: profile.into(),
                note_id: note.into(),
                revision,
            },
            targets,
        ))
    }
    pub fn ack(
        &self,
        window: &str,
        context: &EditorContext,
        prepare: &str,
        dirty: bool,
    ) -> Result<(), DomainError> {
        let mut s = self.state.lock();
        Self::entry(&s, window, context)?;
        let p = s.pending.get(prepare).ok_or(DomainError::EditorBusy)?;
        if !p.targets.contains(&context.editor_id) || p.deadline <= Instant::now() {
            return Err(DomainError::EditorBusy);
        }
        let e = s.entries.get_mut(&context.editor_id).unwrap();
        if dirty {
            e.dirty = true;
        } else if !e.dirty {
            e.ack = Some(prepare.into());
        }
        Ok(())
    }
    pub fn ready(&self, permit: &EditPermit) -> Result<bool, DomainError> {
        let s = self.state.lock();
        Self::ready_locked(&s, permit)
    }
    fn ready_locked(s: &State, permit: &EditPermit) -> Result<bool, DomainError> {
        let p = s
            .pending
            .get(&permit.prepare_id)
            .ok_or(DomainError::EditorBusy)?;
        if p.deadline <= Instant::now()
            || p.revision != permit.revision
            || p.profile != permit.profile_id
            || p.note != permit.note_id
        {
            return Err(DomainError::EditorBusy);
        }
        let mut ready = true;
        for id in &p.targets {
            let e = s.entries.get(id).ok_or(DomainError::EditorBusy)?;
            if e.dirty || !e.alive {
                return Err(DomainError::EditorBusy);
            }
            if e.ack.as_deref() != Some(&p.id) {
                ready = false;
            }
        }
        Ok(ready)
    }
    pub fn validate_permit_locked(
        &self,
        permit: &EditPermit,
        p: &LockedProfile<'_>,
    ) -> Result<(), DomainError> {
        let s = self.state.lock();
        if p.scope().local_id() != permit.profile_id || !Self::ready_locked(&s, permit)? {
            return Err(DomainError::EditorBusy);
        }
        if RevisionStore::note_locked(p, &permit.note_id)? != permit.revision {
            return Err(DomainError::RevisionConflict);
        }
        Ok(())
    }
    pub fn release(&self, permit: &EditPermit) -> Vec<EditorTarget> {
        let mut s = self.state.lock();
        let Some(p) = s.pending.remove(&permit.prepare_id) else {
            return vec![];
        };
        p.targets
            .iter()
            .filter_map(|id| {
                s.entries.get_mut(id).map(|e| {
                    e.ack = None;
                    EditorTarget {
                        window: e.window.clone(),
                        event: PrepareEvent {
                            context: e.context.clone(),
                            prepare_id: permit.prepare_id.clone(),
                        },
                    }
                })
            })
            .collect()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ui_metadata_commit_reloads_clean_owner_before_next_save() {
        use crate::note_service::scope::tests::{meta, Fixture, ID};
        let f = Fixture::new();
        let scope = crate::note_service::ProfileRegistry::switch_in(&f.0, "work").unwrap();
        let r = EditorRegistry::default();
        NoteService::mutate(
            &scope,
            MutationIntent::ImportNote {
                meta: meta(ID),
                content: "before".into(),
            },
            Actor::Ui,
        )
        .unwrap();
        let before = scope
            .transaction(|p| r.register_note_locked("main", p, ID, "owner"))
            .unwrap();
        let (prepared, targets) = scope
            .transaction(|p| {
                r.prepare_mutation_locked(
                    p,
                    r.session("work"),
                    MutationIntent::ToggleFavorite { id: ID.into() },
                )
            })
            .unwrap();
        assert_eq!(targets.len(), 1);
        r.ack(
            "main",
            &before.context,
            &prepared.permits[0].prepare_id,
            false,
        )
        .unwrap();
        scope
            .transaction(|p| r.commit_prepared_mutation_locked(p, &prepared, Actor::Ui))
            .unwrap();
        for permit in &prepared.permits {
            r.release(permit);
        }
        let after = scope
            .transaction(|p| r.read_note_locked("main", p, &before.context))
            .unwrap();
        assert!(after.note.meta.favorite);
        assert_ne!(
            before.context.loaded_revision,
            after.context.loaded_revision
        );
        r.begin("main", &after.context).unwrap();
        scope
            .transaction(|p| {
                r.save_note_locked(
                    "main",
                    p,
                    &after.context,
                    "new draft".into(),
                    "new draft".into(),
                    uuid::Uuid::new_v4().to_string(),
                )
            })
            .unwrap();
        assert_eq!(
            NoteRepository::get(&scope, ID).unwrap().content,
            "new draft"
        );
    }
    #[test]
    fn dirty_owner_blocks_ui_delete_and_backup_overwrite() {
        use crate::note_service::scope::tests::{meta, Fixture, ID};
        let f = Fixture::new();
        let scope = crate::note_service::ProfileRegistry::switch_in(&f.0, "work").unwrap();
        let r = EditorRegistry::default();
        NoteService::mutate(
            &scope,
            MutationIntent::ImportNote {
                meta: meta(ID),
                content: "saved".into(),
            },
            Actor::Ui,
        )
        .unwrap();
        let read = scope
            .transaction(|p| r.register_note_locked("note-detached", p, ID, "owner"))
            .unwrap();
        r.begin("note-detached", &read.context).unwrap();
        for intent in [
            MutationIntent::DeleteNote { id: ID.into() },
            MutationIntent::ImportNote {
                meta: meta(ID),
                content: "backup".into(),
            },
        ] {
            assert!(matches!(
                scope.transaction(|p| r.prepare_mutation_locked(p, r.session("work"), intent)),
                Err(DomainError::EditorBusy)
            ));
        }
        assert_eq!(NoteRepository::get(&scope, ID).unwrap().content, "saved");
        assert!(NoteRepository::get(&scope, ID)
            .unwrap()
            .meta
            .deleted_at
            .is_none());
    }
    #[test]
    fn folder_delete_prepares_all_members_and_rejects_new_references() {
        use crate::note_service::scope::tests::{meta, Fixture, ID};
        let f = Fixture::new();
        let scope = crate::note_service::ProfileRegistry::switch_in(&f.0, "work").unwrap();
        let r = EditorRegistry::default();
        let folder = NoteService::mutate(
            &scope,
            MutationIntent::CreateFolder {
                name: "folder".into(),
                icon: None,
            },
            Actor::Ui,
        )
        .unwrap()
        .folder()
        .unwrap();
        let mut note = meta(ID);
        note.folder_id = Some(folder.id.clone());
        NoteService::mutate(
            &scope,
            MutationIntent::ImportNote {
                meta: note,
                content: "member".into(),
            },
            Actor::Ui,
        )
        .unwrap();
        let (prepared, _) = scope
            .transaction(|p| {
                r.prepare_mutation_locked(
                    p,
                    r.session("work"),
                    MutationIntent::DeleteFolder {
                        id: folder.id.clone(),
                    },
                )
            })
            .unwrap();
        assert_eq!(prepared.permits.len(), 1);
        NoteService::mutate(
            &scope,
            MutationIntent::CreateNote {
                folder_id: Some(folder.id.clone()),
            },
            Actor::Ui,
        )
        .unwrap();
        assert!(matches!(
            scope.transaction(|p| r.commit_prepared_mutation_locked(p, &prepared, Actor::Ui)),
            Err(DomainError::RevisionConflict)
        ));
        assert_eq!(
            NoteRepository::get(&scope, ID).unwrap().meta.folder_id,
            Some(folder.id)
        );
        for permit in &prepared.permits {
            r.release(permit);
        }
    }
    #[test]
    fn reorder_checks_every_owner_and_releases_earlier_reservations_on_failure() {
        use crate::note_service::scope::tests::{meta, Fixture, ID};
        let f = Fixture::new();
        let scope = crate::note_service::ProfileRegistry::switch_in(&f.0, "work").unwrap();
        let r = EditorRegistry::default();
        let other = "00000000-0000-4000-8000-000000000002";
        for id in [ID, other] {
            NoteService::mutate(
                &scope,
                MutationIntent::ImportNote {
                    meta: meta(id),
                    content: id.into(),
                },
                Actor::Ui,
            )
            .unwrap();
        }
        let read = scope
            .transaction(|p| r.register_note_locked("main", p, other, "owner"))
            .unwrap();
        r.begin("main", &read.context).unwrap();
        assert!(matches!(
            scope.transaction(|p| r.prepare_mutation_locked(
                p,
                r.session("work"),
                MutationIntent::ReorderNotes {
                    folder_id: None,
                    note_ids: vec![ID.into(), other.into()]
                }
            )),
            Err(DomainError::EditorBusy)
        ));
        let (permit, _) = r
            .start_prepare("work", ID, RevisionStore::note(&scope, ID).unwrap())
            .unwrap();
        r.release(&permit);
    }
    #[test]
    fn prepared_ui_write_rejects_expired_profile_generation() {
        use crate::note_service::scope::tests::Fixture;
        let f = Fixture::new();
        let scope = crate::note_service::ProfileRegistry::switch_in(&f.0, "work").unwrap();
        let r = EditorRegistry::default();
        let (prepared, _) = scope
            .transaction(|p| {
                r.prepare_mutation_locked(
                    p,
                    r.session("work"),
                    MutationIntent::CreateNote { folder_id: None },
                )
            })
            .unwrap();
        r.invalidate_sessions().unwrap();
        assert!(matches!(
            scope.transaction(|p| r.commit_prepared_mutation_locked(p, &prepared, Actor::Ui)),
            Err(DomainError::EditorSessionExpired)
        ));
        assert!(NoteRepository::list(&scope).unwrap().is_empty());
    }
    fn revision() -> Revision {
        serde_json::from_str("\"r1\"").unwrap()
    }
    fn registered() -> (EditorRegistry, EditorContext) {
        let r = EditorRegistry::default();
        let c = r
            .register("main", "work", "note", "editor", revision())
            .unwrap();
        (r, c)
    }
    #[test]
    fn dirty_editor_blocks_mcp() {
        let (r, c) = registered();
        r.begin("main", &c).unwrap();
        assert!(matches!(
            r.start_prepare("work", "note", revision()),
            Err(DomainError::EditorBusy)
        ));
    }
    #[test]
    fn clean_editor_pauses_then_reloads() {
        let (r, c) = registered();
        let (p, targets) = r.start_prepare("work", "note", revision()).unwrap();
        assert_eq!(targets.len(), 1);
        assert!(!r.ready(&p).unwrap());
        r.ack("main", &c, &p.prepare_id, false).unwrap();
        assert!(r.ready(&p).unwrap());
        assert!(r.begin("main", &c).is_err());
    }
    #[test]
    fn unresponsive_editor_is_busy() {
        let (r, _) = registered();
        let (p, _) = r.start_prepare("work", "note", revision()).unwrap();
        assert!(!r.ready(&p).unwrap());
    }
    #[test]
    fn detached_editor_obeys_same_protocol() {
        let r = EditorRegistry::default();
        let c = r
            .register("note-1", "work", "note", "editor", revision())
            .unwrap();
        r.begin("note-1", &c).unwrap();
        assert!(r.start_prepare("work", "note", revision()).is_err());
        assert!(r.validate_context("main", &c).is_err());
    }
    #[test]
    fn late_save_after_profile_switch_rejects_even_with_same_note_uuid() {
        let (r, c) = registered();
        r.invalidate_sessions().unwrap();
        r.register("main", "personal", "note", "editor2", revision())
            .unwrap();
        r.invalidate_sessions().unwrap();
        r.register("main", "work", "note", "editor3", revision())
            .unwrap();
        assert!(matches!(
            r.validate_context("main", &c),
            Err(DomainError::EditorSessionExpired)
        ));
    }
    #[test]
    fn dirty_profile_switch_is_refused() {
        let (r, c) = registered();
        r.begin("main", &c).unwrap();
        assert!(r.invalidate_sessions().is_err());
    }
    #[test]
    fn failed_profile_switch_preserves_sessions() {
        let (r, c) = registered();
        drop(r.prepare_profile_switch().unwrap());
        r.validate_context("main", &c).unwrap();
    }
    #[test]
    fn clean_close_during_preparation_is_busy() {
        let (r, c) = registered();
        let (p, _) = r.start_prepare("work", "note", revision()).unwrap();
        assert!(matches!(r.end("main", &c), Err(DomainError::EditorBusy)));
        assert!(!r.ready(&p).unwrap());
    }
    #[test]
    fn late_keystroke_invalidates_acknowledged_permit_without_losing_dirty_state() {
        let (r, c) = registered();
        let (p, _) = r.start_prepare("work", "note", revision()).unwrap();
        r.ack("main", &c, &p.prepare_id, false).unwrap();
        assert!(r.ready(&p).unwrap());
        assert!(matches!(r.begin("main", &c), Err(DomainError::EditorBusy)));
        assert!(matches!(r.ready(&p), Err(DomainError::EditorBusy)));
        r.release(&p);
        assert!(matches!(
            r.start_prepare("work", "note", revision()),
            Err(DomainError::EditorBusy)
        ));
    }
    #[test]
    fn timeout_does_not_assume_an_unseen_editor_is_clean() {
        let (r, _) = registered();
        let (p, _) = r.start_prepare("work", "note", revision()).unwrap();
        r.state
            .lock()
            .pending
            .get_mut(&p.prepare_id)
            .unwrap()
            .deadline = Instant::now();
        assert!(matches!(r.ready(&p), Err(DomainError::EditorBusy)));
        r.release(&p);
        let (next, targets) = r.start_prepare("work", "note", revision()).unwrap();
        assert_eq!(targets.len(), 1);
        assert!(!r.ready(&next).unwrap());
    }
    #[test]
    fn crashed_editor_never_becomes_clean() {
        let (r, c) = registered();
        r.window_destroyed("main");
        assert!(r.validate_context("main", &c).is_err());
        assert!(matches!(
            r.start_prepare("work", "note", revision()),
            Err(DomainError::EditorBusy)
        ));
    }
    #[test]
    fn stale_autosave_cannot_overwrite_mcp() {
        use crate::note_service::{
            scope::tests::{meta, Fixture, ID},
            Actor, MutationIntent, NoteRepository, NoteService,
        };
        let f = Fixture::new();
        let scope = f.scope("work");
        let r = EditorRegistry::default();
        NoteService::mutate(
            &scope,
            MutationIntent::ImportNote {
                meta: meta(ID),
                content: "before".into(),
            },
            Actor::Ui,
        )
        .unwrap();
        let c = scope
            .transaction(|p| r.register_note_locked("main", p, ID, "editor"))
            .unwrap()
            .context;
        NoteService::mutate(
            &scope,
            MutationIntent::UpdateNote {
                id: ID.into(),
                content: "external".into(),
                title: "external".into(),
            },
            Actor::Ui,
        )
        .unwrap();
        assert!(matches!(
            scope.transaction(|p| r.save_note_locked(
                "main",
                p,
                &c,
                "late draft".into(),
                "late".into(),
                uuid::Uuid::new_v4().to_string()
            )),
            Err(DomainError::RevisionConflict)
        ));
        assert_eq!(NoteRepository::get(&scope, ID).unwrap().content, "external");
    }
    #[test]
    fn replayed_save_does_not_clear_a_newer_dirty_barrier() {
        use crate::note_service::{
            scope::tests::{meta, Fixture, ID},
            Actor, MutationIntent, NoteService,
        };
        let f = Fixture::new();
        let scope = f.scope("work");
        let r = EditorRegistry::default();
        NoteService::mutate(
            &scope,
            MutationIntent::ImportNote {
                meta: meta(ID),
                content: "before".into(),
            },
            Actor::Ui,
        )
        .unwrap();
        let context = scope
            .transaction(|p| r.register_note_locked("main", p, ID, "editor"))
            .unwrap()
            .context;
        let operation = uuid::Uuid::new_v4().to_string();
        let saved = scope
            .transaction(|p| {
                r.save_note_locked(
                    "main",
                    p,
                    &context,
                    "first".into(),
                    "first".into(),
                    operation.clone(),
                )
            })
            .unwrap();
        r.begin("main", &saved.context).unwrap();
        scope
            .transaction(|p| {
                r.save_note_locked(
                    "main",
                    p,
                    &context,
                    "first".into(),
                    "first".into(),
                    operation,
                )
            })
            .unwrap();
        assert!(matches!(
            r.start_prepare("work", ID, saved.context.loaded_revision),
            Err(DomainError::EditorBusy)
        ));
    }
}
