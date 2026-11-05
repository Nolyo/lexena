//! Profile-scoped tools. No network clients, active-profile fallback or raw paths.
use super::{
    connections::{AuthorizationLease, GrantStore, Scope, SessionActor},
    contract::{self, Args},
    error::BusinessError,
};
use crate::note_service::{
    self as domain, Actor, DomainError, FolderRepository, LockedProfile, MutationIntent,
    NoteRepository, NoteService, ProfileScope,
};
use domain::{
    editors::{editors, EditPermit},
    journal::{Revision, RevisionStore},
    mutation::{MutationReceipt, MutationRequest},
    versions::VersionStore,
};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use unicode_normalization::UnicodeNormalization;

pub type PrepareFuture<'a> =
    Pin<Box<dyn Future<Output = Result<EditPermit, DomainError>> + Send + 'a>>;
#[derive(Clone, Debug)]
pub struct Change {
    pub profile_id: String,
    pub entity_id: String,
    pub revision: String,
    pub operation_id: String,
    pub folder: bool,
    pub mutation_type: String,
}
/// Tauri adapter owns renderer communication; the pure adapter exercises the same registry.
pub trait EditorAdapter: Send + Sync + 'static {
    fn prepare<'a>(&'a self, scope: &'a ProfileScope, note_id: &'a str) -> PrepareFuture<'a>;
    fn finish(&self, permit: &EditPermit, committed: bool);
    fn changed(&self, _change: Change) {}
}
pub struct LocalEditors;
impl EditorAdapter for LocalEditors {
    fn prepare<'a>(&'a self, scope: &'a ProfileScope, note_id: &'a str) -> PrepareFuture<'a> {
        Box::pin(async move {
            let (permit, _) = scope.transaction(|p| {
                editors().start_prepare(
                    scope.local_id(),
                    note_id,
                    RevisionStore::note_locked(p, note_id)?,
                )
            })?;
            match editors().ready(&permit) {
                Ok(true) => Ok(permit),
                _ => {
                    editors().release(&permit);
                    Err(DomainError::EditorBusy)
                }
            }
        })
    }
    fn finish(&self, permit: &EditPermit, _committed: bool) {
        editors().release(permit);
    }
}
struct Prepared {
    adapter: Arc<dyn EditorAdapter>,
    permits: Vec<EditPermit>,
    committed: bool,
}
impl Drop for Prepared {
    fn drop(&mut self) {
        for p in &self.permits {
            self.adapter.finish(p, self.committed);
        }
    }
}

struct Cursor {
    binding: String,
    revision: String,
    offset: usize,
    chain: Option<String>,
    expires: Instant,
}
struct ReadToken {
    profile: String,
    note: String,
    revision: String,
    expires: Instant,
}
#[derive(Default)]
pub struct SessionData {
    cursors: Mutex<HashMap<String, Cursor>>,
    reads: Mutex<HashMap<String, ReadToken>>,
}
impl SessionData {
    #[cfg(test)]
    pub(super) fn expire_for_test(&self) {
        for c in self.cursors.lock().values_mut() {
            c.expires = Instant::now() - Duration::from_secs(1);
        }
        for r in self.reads.lock().values_mut() {
            r.expires = Instant::now() - Duration::from_secs(1);
        }
    }
    fn cursor(
        &self,
        binding: String,
        revision: String,
        offset: usize,
        chain: Option<String>,
    ) -> String {
        let mut map = self.cursors.lock();
        map.retain(|_, c| c.expires > Instant::now());
        if map.len() >= 256 {
            if let Some(key) = map.keys().next().cloned() {
                map.remove(&key);
            }
        }
        let key = uuid::Uuid::new_v4().to_string();
        map.insert(
            key.clone(),
            Cursor {
                binding,
                revision,
                offset,
                chain,
                expires: Instant::now() + Duration::from_secs(600),
            },
        );
        key
    }
    fn take(
        &self,
        key: &str,
        binding: &str,
        revision: &str,
    ) -> Result<(usize, Option<String>), BusinessError> {
        let cursor = self
            .cursors
            .lock()
            .remove(key)
            .ok_or_else(|| BusinessError::new("CURSOR_INVALID"))?;
        if cursor.expires <= Instant::now() {
            return Err(BusinessError::new("CURSOR_EXPIRED"));
        }
        if cursor.binding != binding {
            return Err(BusinessError::new("CURSOR_INVALID"));
        }
        if cursor.revision != revision {
            return Err(BusinessError::new("CURSOR_EXPIRED"));
        }
        Ok((cursor.offset, cursor.chain))
    }
    fn read_token(&self, profile: &str, note: &str, revision: &str) -> String {
        let mut map = self.reads.lock();
        map.retain(|_, c| c.expires > Instant::now());
        if map.len() >= 128 {
            if let Some(key) = map.keys().next().cloned() {
                map.remove(&key);
            }
        }
        let key = uuid::Uuid::new_v4().to_string();
        map.insert(
            key.clone(),
            ReadToken {
                profile: profile.into(),
                note: note.into(),
                revision: revision.into(),
                expires: Instant::now() + Duration::from_secs(600),
            },
        );
        key
    }
    fn validate_read(
        &self,
        key: &str,
        profile: &str,
        note: &str,
        revision: &str,
    ) -> Result<(), DomainError> {
        let map = self.reads.lock();
        let token = map
            .get(key)
            .ok_or_else(|| DomainError::InvalidInput("READ_TOKEN_INVALID".into()))?;
        if token.profile != profile
            || token.note != note
            || token.revision != revision
            || token.expires <= Instant::now()
        {
            return Err(DomainError::InvalidInput("READ_TOKEN_INVALID".into()));
        }
        Ok(())
    }
}
fn binding(name: &str, a: &Args) -> String {
    json!([
        name,
        a.profile_id,
        a.note_id,
        a.folder_id,
        a.favorite,
        a.query,
        a.updated_after,
        a.updated_before,
        a.format,
        a.limit
    ])
    .to_string()
}
fn bounded(s: &str, bytes: usize) -> String {
    let mut end = s.len().min(bytes);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].into()
}
fn normalized(s: &str) -> String {
    s.nfkc().flat_map(char::to_lowercase).collect()
}
fn auth_error() -> BusinessError {
    BusinessError::new("NOT_FOUND")
}
fn visible_note(
    p: &LockedProfile<'_>,
    id: &str,
    lease: &AuthorizationLease<'_>,
    deleted: bool,
) -> Result<domain::repository::NoteData, DomainError> {
    let meta = NoteRepository::metadata_locked(p, id)?;
    if (!deleted && meta.deleted_at.is_some())
        || (meta.local_only && !lease.allows(Scope::NotesLocalOnlyRead))
    {
        return Err(DomainError::NotFound("note".into()));
    }
    Ok(domain::repository::NoteData {
        meta,
        content: NoteRepository::content_locked(p, id)?,
    })
}
fn authorize_note(
    p: &LockedProfile<'_>,
    id: &str,
    lease: &AuthorizationLease<'_>,
) -> Result<(), DomainError> {
    if let Some(private) = RevisionStore::load(p)?.note_privacy.get(id) {
        return if *private && !lease.allows(Scope::NotesLocalOnlyRead) {
            Err(DomainError::NotFound("note".into()))
        } else {
            Ok(())
        };
    }
    match NoteRepository::metadata_locked(p, id) {
        Ok(n) => {
            if n.local_only && !lease.allows(Scope::NotesLocalOnlyRead) {
                return Err(DomainError::NotFound("note".into()));
            }
        }
        Err(DomainError::NotFound(_)) => {
            // Old snapshots never grant public access. Absent legacy privacy defaults private.
            if RevisionStore::load(p)?
                .note_privacy
                .get(id)
                .copied()
                .unwrap_or(true)
                && !lease.allows(Scope::NotesLocalOnlyRead)
            {
                return Err(DomainError::NotFound("note".into()));
            }
        }
        Err(e) => return Err(e),
    };
    Ok(())
}
fn summary(
    p: &LockedProfile<'_>,
    profile_name: &str,
    n: domain::repository::NoteMeta,
    content: &str,
) -> Result<contract::Note, DomainError> {
    Ok(contract::Note {
        profile_id: p.scope().local_id().into(),
        profile_name: profile_name.into(),
        note_id: n.id.clone(),
        title: bounded(&n.title, 1024),
        revision: RevisionStore::note_locked(p, &n.id)?.as_str().into(),
        folder_id: n.folder_id,
        favorite: n.favorite,
        created_at: n.created_at,
        updated_at: n.updated_at,
        excerpt: bounded(&domain::content::plain_text(content), 320),
    })
}
pub fn metadata(
    app_data: &std::path::Path,
    grants: &GrantStore,
    actor: SessionActor,
    name: &str,
) -> Result<Value, BusinessError> {
    let manifest = domain::ProfileRegistry::manifest_in(app_data).map_err(BusinessError::from)?;
    grants
        .with_session_authorization(&actor, |lease| {
            let profiles: Vec<contract::Profile> = manifest
                .profiles
                .iter()
                .filter(|p| lease.allows_profile(&p.id, &p.created_at))
                .map(|p| contract::Profile {
                    profile_id: p.id.clone(),
                    profile_name: p.name.clone(),
                    active: manifest.active == p.id,
                })
                .collect();
            if name == "lexena_list_profiles" {
                return serde_json::to_value(contract::Profiles { profiles }).unwrap();
            }
            let active = profiles
                .iter()
                .find(|p| p.active)
                .map(|p| p.profile_id.clone());
            let permissions = [
                (Scope::NotesRead, "notes.read"),
                (Scope::NotesWrite, "notes.write"),
                (Scope::NotesDelete, "notes.delete"),
                (Scope::NotesReplace, "notes.replace"),
                (Scope::NotesLocalOnlyRead, "notes.local_only.read"),
            ]
            .into_iter()
            .filter(|(s, _)| lease.allows(*s))
            .map(|(_, s)| s.to_string())
            .collect();
            serde_json::to_value(contract::Context {
                profiles,
                active_profile_id: active,
                permissions,
                explicit_profile_required: true,
                server_version: env!("CARGO_PKG_VERSION").into(),
            })
            .unwrap()
        })
        .map_err(|_| auth_error())
}
pub fn resolve(
    app_data: &std::path::Path,
    grants: &GrantStore,
    actor: SessionActor,
    a: &Args,
    required: Scope,
) -> Result<(ProfileScope, String), BusinessError> {
    let manifest = domain::ProfileRegistry::manifest_in(app_data).map_err(|_| auth_error())?;
    let id = a.profile_id.as_deref().expect("validated profile");
    let identity = manifest
        .profiles
        .iter()
        .find(|p| p.id == id)
        .ok_or_else(auth_error)?;
    grants
        .with_authorization(&actor, id, &identity.created_at, &[required], |_| ())
        .map_err(|_| auth_error())?;
    let scope = ProfileScope::resolve_in(app_data, id).map_err(|_| auth_error())?;
    Ok((scope, identity.name.clone()))
}

pub fn read(
    scope: &ProfileScope,
    profile_name: &str,
    grants: &GrantStore,
    actor: SessionActor,
    data: &SessionData,
    cache: &super::read_cache::ReadCache,
    name: &str,
    a: &Args,
) -> Result<Value, BusinessError> {
    let mut business = None;
    let value = scope.transaction(|p| {
        let id = p.scope().local_id().to_string();
        let created = p.scope().created_at().to_string();
        let index = if matches!(name, "lexena_list_notes" | "lexena_search_notes") {
            let private = grants
                .with_authorization(&actor, &id, &created, &[Scope::NotesRead], |lease| {
                    lease.allows(Scope::NotesLocalOnlyRead)
                })
                .map_err(|_| DomainError::NotFound("profile".into()))?;
            // Keep profile/recovery/path safety, but let revocation return while
            // building derived data. A fresh retained lease gates all output.
            Some(cache.get_locked(p, private)?)
        } else {
            None
        };
        let result =
            grants.with_authorization(&actor, &id, &created, &[Scope::NotesRead], |lease| {
                let rev = match &index {
                    Some(index) => index.collection.clone(),
                    None => RevisionStore::collection_locked(p)?,
                };
                let key = binding(name, a);
                let limit = a.limit.unwrap_or(20) as usize;
                let result = match name {
                    "lexena_list_folders" => {
                        let all = FolderRepository::list_locked(p)?;
                        let (offset, _) = if let Some(c) = &a.cursor {
                            data.take(c, &key, rev.as_str()).map_err(|e| {
                                business = Some(e);
                                DomainError::InvalidInput("cursor".into())
                            })?
                        } else {
                            (0, None)
                        };
                        let next = (offset + limit < all.len())
                            .then(|| data.cursor(key, rev.as_str().into(), offset + limit, None));
                        let mut items = Vec::new();
                        for f in all.into_iter().skip(offset).take(limit) {
                            items.push(contract::Folder {
                                profile_id: id.clone(),
                                profile_name: profile_name.into(),
                                folder_id: f.id.clone(),
                                name: bounded(&f.name, 1024),
                                icon: f.icon,
                                revision: RevisionStore::folder_locked(p, &f.id)?.as_str().into(),
                                created_at: f.created_at,
                                updated_at: f.updated_at,
                            });
                        }
                        serde_json::to_value(contract::Page {
                            profile_id: id.clone(),
                            profile_name: profile_name.into(),
                            revision: rev.as_str().into(),
                            items,
                            next_cursor: next,
                        })?
                    }
                    "lexena_list_notes" | "lexena_search_notes" => {
                        let query = a.query.as_ref().map(|q| normalized(q));
                        let after = a
                            .updated_after
                            .as_deref()
                            .map(chrono::DateTime::parse_from_rfc3339)
                            .transpose()
                            .map_err(|_| DomainError::InvalidInput("INVALID_DATE".into()))?;
                        let before = a
                            .updated_before
                            .as_deref()
                            .map(chrono::DateTime::parse_from_rfc3339)
                            .transpose()
                            .map_err(|_| DomainError::InvalidInput("INVALID_DATE".into()))?;
                        let mut matches = Vec::new();
                        for entry in &index.as_ref().unwrap().notes {
                            let n = &entry.meta;
                            if n.local_only && !lease.allows(Scope::NotesLocalOnlyRead)
                                || a.folder_id
                                    .as_ref()
                                    .is_some_and(|f| n.folder_id.as_ref() != Some(f))
                                || a.favorite.is_some_and(|f| n.favorite != f)
                            {
                                continue;
                            }
                            if after.is_some() || before.is_some() {
                                let Ok(updated) =
                                    chrono::DateTime::parse_from_rfc3339(&n.updated_at)
                                else {
                                    continue;
                                };
                                if after.as_ref().is_some_and(|bound| updated <= *bound)
                                    || before.as_ref().is_some_and(|bound| updated >= *bound)
                                {
                                    continue;
                                }
                            }
                            if let Some(query) = &query {
                                let found = match &entry.search_text {
                                    Some(text) => text.contains(query),
                                    None => {
                                        normalized(&n.title).contains(query)
                                            || normalized(&domain::content::plain_text(
                                                &NoteRepository::content_locked(p, &n.id)?,
                                            ))
                                            .contains(query)
                                    }
                                };
                                if !found {
                                    continue;
                                }
                            }
                            matches.push(entry);
                        }
                        let (offset, _) = if let Some(c) = &a.cursor {
                            data.take(c, &key, rev.as_str()).map_err(|e| {
                                business = Some(e);
                                DomainError::InvalidInput("cursor".into())
                            })?
                        } else {
                            (0, None)
                        };
                        let next = (offset + limit < matches.len())
                            .then(|| data.cursor(key, rev.as_str().into(), offset + limit, None));
                        let mut items = Vec::new();
                        for entry in matches.into_iter().skip(offset).take(limit) {
                            let n = &entry.meta;
                            items.push(contract::Note {
                                profile_id: id.clone(),
                                profile_name: profile_name.into(),
                                note_id: n.id.clone(),
                                title: bounded(&n.title, 1024),
                                revision: entry.revision.as_str().into(),
                                folder_id: n.folder_id.clone(),
                                favorite: n.favorite,
                                created_at: n.created_at.clone(),
                                updated_at: n.updated_at.clone(),
                                excerpt: entry.excerpt.clone(),
                            });
                        }
                        serde_json::to_value(contract::Page {
                            profile_id: id.clone(),
                            profile_name: profile_name.into(),
                            revision: rev.as_str().into(),
                            items,
                            next_cursor: next,
                        })?
                    }
                    "lexena_get_note" => {
                        let note_id = a.note_id.as_deref().unwrap();
                        let note = visible_note(p, note_id, lease, false)?;
                        let revision = RevisionStore::note_locked(p, note_id)?;
                        let format = a.format.as_deref().unwrap_or("text");
                        let view = domain::content::read_view(&note.content, format)
                            .map_err(|e| DomainError::InvalidInput(e.code().into()))?;
                        let (offset, chain) = if let Some(c) = &a.cursor {
                            data.take(c, &key, revision.as_str()).map_err(|e| {
                                business = Some(e);
                                DomainError::InvalidInput("cursor".into())
                            })?
                        } else {
                            (
                                0,
                                (format == "html").then(|| uuid::Uuid::new_v4().to_string()),
                            )
                        };
                        if offset > view.content.len() || !view.content.is_char_boundary(offset) {
                            return Err(DomainError::InvalidInput("cursor".into()));
                        }
                        let mut end =
                            (offset + a.limit.unwrap_or(32768) as usize).min(view.content.len());
                        while !view.content.is_char_boundary(end) {
                            end -= 1;
                        }
                        // A too-small byte limit must make progress across a UTF-8 scalar.
                        if end == offset && end < view.content.len() {
                            return Err(DomainError::InvalidInput(
                                "limit splits Unicode scalar".into(),
                            ));
                        }
                        let next = (end < view.content.len()).then(|| {
                            data.cursor(key, revision.as_str().into(), end, chain.clone())
                        });
                        let token = (next.is_none() && format == "html" && chain.is_some())
                            .then(|| data.read_token(&id, note_id, revision.as_str()));
                        serde_json::to_value(contract::NoteContent {
                            profile_id: id.clone(),
                            profile_name: profile_name.into(),
                            note_id: note_id.into(),
                            revision: revision.as_str().into(),
                            note: summary(p, profile_name, note.meta, &note.content)?,
                            format: format.into(),
                            content: view.content[offset..end].into(),
                            offset,
                            total_bytes: view.content.len(),
                            lossy: view.lossy,
                            warnings: view.warnings,
                            next_cursor: next,
                            read_token: token,
                        })?
                    }
                    "lexena_list_note_versions" => {
                        let note_id = a.note_id.as_deref().unwrap();
                        authorize_note(p, note_id, lease)?;
                        let revision = RevisionStore::note_locked(p, note_id)?;
                        let versions = VersionStore::list_locked(p, note_id)?;
                        let (offset, _) = if let Some(c) = &a.cursor {
                            data.take(c, &key, rev.as_str()).map_err(|e| {
                                business = Some(e);
                                DomainError::InvalidInput("cursor".into())
                            })?
                        } else {
                            (0, None)
                        };
                        let versions: Vec<_> = versions
                            .into_iter()
                            .filter(|v| {
                                !v.note.meta.local_only || lease.allows(Scope::NotesLocalOnlyRead)
                            })
                            .collect();
                        let next = (offset + limit < versions.len())
                            .then(|| data.cursor(key, rev.as_str().into(), offset + limit, None));
                        let items = versions
                            .into_iter()
                            .skip(offset)
                            .take(limit)
                            .map(|v| contract::Version {
                                profile_id: id.clone(),
                                profile_name: profile_name.into(),
                                note_id: note_id.into(),
                                snapshot_id: v.snapshot_id,
                                revision: v.revision.as_str().into(),
                                created_at: v.created_at,
                                expires_at: v.expires_at,
                            })
                            .collect::<Vec<_>>();
                        serde_json::to_value(contract::Page {
                            profile_id: id.clone(),
                            profile_name: profile_name.into(),
                            revision: revision.as_str().into(),
                            items,
                            next_cursor: next,
                        })?
                    }
                    _ => unreachable!(),
                };
                Ok(result)
            });
        result.map_err(|_| DomainError::NotFound("profile".into()))?
    });
    value.map_err(|e| business.unwrap_or_else(|| e.into()))
}

fn request(name: &str, a: &Args) -> Result<MutationRequest, BusinessError> {
    let id = || a.note_id.clone().unwrap();
    let folder = || a.folder_id.clone().unwrap();
    let intent = match name {
        "lexena_create_note" => MutationIntent::CreateNoteWithContent {
            folder_id: a.folder_id.clone(),
            content: a.content.clone().unwrap(),
            format: a.format.clone().unwrap(),
        },
        "lexena_append_to_note" => MutationIntent::AppendNote {
            id: id(),
            content: a.content.clone().unwrap(),
            format: a.format.clone().unwrap(),
        },
        "lexena_edit_note" => MutationIntent::EditNoteText {
            id: id(),
            old: a.old_text.clone().unwrap(),
            new: a.new_text.clone().unwrap(),
        },
        "lexena_set_note_metadata" => MutationIntent::SetNoteMetadata {
            id: id(),
            folder_id: a.folder_present.then(|| a.folder_id.clone()),
            favorite: a.favorite,
        },
        "lexena_replace_note_content" => {
            let html = a.content.clone().unwrap();
            domain::content::validate_html(&html).map_err(|e| BusinessError::new(e.code()))?;
            let title = domain::content::derive_note_title(&html, "en");
            MutationIntent::UpdateNote {
                id: id(),
                content: html,
                title,
            }
        }
        "lexena_delete_note" => MutationIntent::DeleteNote { id: id() },
        "lexena_restore_note" => MutationIntent::RestoreNote {
            id: id(),
            snapshot_id: a.snapshot_id.clone().unwrap(),
        },
        "lexena_create_folder" => MutationIntent::CreateFolder {
            name: a.name.clone().unwrap(),
            icon: a.icon.clone(),
        },
        "lexena_update_folder" => MutationIntent::SetFolderMetadata {
            id: folder(),
            name: a.name.clone(),
            icon: a.icon_present.then(|| a.icon.clone()),
        },
        "lexena_delete_folder" => MutationIntent::DeleteFolder { id: folder() },
        _ => unreachable!(),
    };
    let expected_revision = a
        .expected_revision
        .as_ref()
        .map(|s| serde_json::from_value::<Revision>(json!(s)))
        .transpose()
        .map_err(|_| BusinessError::new("INVALID_REQUEST"))?;
    Ok(MutationRequest {
        operation_id: a.operation_id.clone().unwrap(),
        expected_revision,
        intent,
    })
}
fn privacy(
    p: &LockedProfile<'_>,
    lease: &AuthorizationLease<'_>,
    a: &Args,
    name: &str,
) -> Result<(), DomainError> {
    if let Some(id) = &a.note_id {
        authorize_note(p, id, lease)?;
    }
    if name == "lexena_delete_folder" {
        for n in NoteRepository::list_locked(p)? {
            if n.folder_id == a.folder_id
                && n.local_only
                && !lease.allows(Scope::NotesLocalOnlyRead)
            {
                return Err(DomainError::NotFound("folder".into()));
            }
        }
    }
    Ok(())
}
pub struct MutationWork {
    pub scope: ProfileScope,
    pub profile_name: String,
    pub grants: Arc<GrantStore>,
    pub actor: SessionActor,
    pub data: Arc<SessionData>,
    pub adapter: Arc<dyn EditorAdapter>,
    pub name: String,
    pub args: Args,
    pub cancelled: Arc<AtomicBool>,
}
impl MutationWork {
    fn receipt(&self, receipt: MutationReceipt) -> Value {
        serde_json::to_value(contract::Receipt {
            profile_id: receipt.profile_id,
            profile_name: self.profile_name.clone(),
            entity_id: receipt.entity_id,
            revision: receipt.revision.as_str().into(),
            operation_id: receipt.operation_id,
            sync_state: serde_json::to_value(receipt.sync_state)
                .unwrap()
                .as_str()
                .unwrap()
                .into(),
            snapshot_id: receipt.snapshot_id,
        })
        .unwrap()
    }
    fn lease<T>(
        &self,
        p: &mut LockedProfile<'_>,
        work: impl FnOnce(&mut LockedProfile<'_>, &AuthorizationLease<'_>) -> Result<T, DomainError>,
    ) -> Result<T, DomainError> {
        let id = p.scope().local_id().to_string();
        let created = p.scope().created_at().to_string();
        let required = contract::permission(&self.name).unwrap();
        let scopes = if required == Scope::NotesWrite {
            vec![Scope::NotesRead, required]
        } else {
            vec![required, Scope::NotesRead]
        };
        self.grants
            .with_authorization(&self.actor, &id, &created, &scopes, |lease| work(p, lease))
            .map_err(|_| DomainError::NotFound("profile".into()))?
    }
    /// Preview receipt lookup precedes asynchronous editor preparation; final commit repeats it.
    pub fn replay(&self) -> Result<Option<Value>, BusinessError> {
        let req = request(&self.name, &self.args)?;
        let actor = Actor::Mcp {
            connection_id: self.actor.connection_id().to_string(),
        };
        self.scope
            .transaction(|p| {
                self.lease(p, |p, lease| {
                    NoteService::receipt_checked_locked(p, &req, &actor, |p| {
                        privacy(p, lease, &self.args, &self.name)
                    })
                })
            })
            .map(|r| r.map(|r| self.receipt(r)))
            .map_err(Into::into)
    }
    pub async fn prepare(&self) -> Result<(PreparedMutation, Option<Revision>), BusinessError> {
        let mut prepared = Prepared {
            adapter: self.adapter.clone(),
            permits: Vec::new(),
            committed: false,
        };
        let (ids, collection) = if self.name == "lexena_delete_folder" {
            self.scope
                .transaction(|p| {
                    self.lease(p, |p, lease| {
                        privacy(p, lease, &self.args, &self.name)?;
                        Ok((
                            NoteRepository::list_locked(p)?
                                .into_iter()
                                .filter(|n| n.folder_id == self.args.folder_id)
                                .map(|n| n.id)
                                .collect(),
                            Some(RevisionStore::collection_locked(p)?),
                        ))
                    })
                })
                .map_err(BusinessError::from)?
        } else {
            (self.args.note_id.iter().cloned().collect::<Vec<_>>(), None)
        };
        for id in ids {
            if self.cancelled.load(Ordering::Acquire) {
                return Err(BusinessError::new("TIMEOUT"));
            }
            prepared.permits.push(
                self.adapter
                    .prepare(&self.scope, &id)
                    .await
                    .map_err(BusinessError::from)?,
            );
        }
        Ok((PreparedMutation(prepared), collection))
    }
    pub fn commit(
        &self,
        mut prepared: PreparedMutation,
        collection: Option<Revision>,
    ) -> Result<Value, BusinessError> {
        let req = request(&self.name, &self.args)?;
        let actor = Actor::Mcp {
            connection_id: self.actor.connection_id().to_string(),
        };
        if self.cancelled.load(Ordering::Acquire) {
            return Err(BusinessError::new("TIMEOUT"));
        }
        let receipt = self
            .scope
            .transaction(|p| {
                self.lease(p, |p, lease| {
                    let receipt = NoteService::mutate_checked_with_precondition_locked(
                        p,
                        req,
                        actor,
                        |p| privacy(p, lease, &self.args, &self.name),
                        |p| {
                            if self.cancelled.load(Ordering::Acquire) {
                                return Err(DomainError::InvalidInput("request cancelled".into()));
                            }
                            if collection.as_ref().is_some_and(|r| {
                                RevisionStore::collection_locked(p)
                                    .map(|c| c != *r)
                                    .unwrap_or(true)
                            }) {
                                return Err(DomainError::RevisionConflict);
                            }
                            for permit in &prepared.0.permits {
                                editors().validate_permit_locked(permit, p)?;
                            }
                            if self.name == "lexena_restore_note" {
                                let version = VersionStore::get_locked(
                                    p,
                                    self.args.snapshot_id.as_deref().unwrap(),
                                )?;
                                if version.note_id != self.args.note_id.as_deref().unwrap()
                                    || version.note.meta.local_only
                                        && !lease.allows(Scope::NotesLocalOnlyRead)
                                {
                                    return Err(DomainError::NotFound("snapshot".into()));
                                }
                            }
                            if self.name == "lexena_replace_note_content" {
                                self.data.validate_read(
                                    self.args.read_token.as_deref().unwrap(),
                                    p.scope().local_id(),
                                    self.args.note_id.as_deref().unwrap(),
                                    self.args.expected_revision.as_deref().unwrap(),
                                )?;
                            }
                            Ok(())
                        },
                    )?;
                    // Capture all detachment revisions before releasing the same atomic profile view.
                    let mut changes = Vec::new();
                    if self.name == "lexena_delete_folder" {
                        for permit in &prepared.0.permits {
                            changes.push(Change {
                                profile_id: p.scope().local_id().into(),
                                entity_id: permit.note_id.clone(),
                                revision: RevisionStore::note_locked(p, &permit.note_id)?
                                    .as_str()
                                    .into(),
                                operation_id: receipt.operation_id.clone(),
                                folder: false,
                                mutation_type: "metadata".into(),
                            });
                        }
                    }
                    if let Some(id) = &receipt.entity_id {
                        changes.push(Change {
                            profile_id: p.scope().local_id().into(),
                            entity_id: id.clone(),
                            revision: receipt.revision.as_str().into(),
                            operation_id: receipt.operation_id.clone(),
                            folder: self.name.ends_with("folder"),
                            mutation_type: if self.name.contains("delete") {
                                "delete"
                            } else if self.name.contains("restore") {
                                "restore"
                            } else if self.name.contains("create") {
                                "create"
                            } else {
                                "update"
                            }
                            .into(),
                        });
                    }
                    Ok((receipt, changes))
                })
            })
            .map_err(BusinessError::from)?;
        prepared.0.committed = true;
        for change in receipt.1 {
            self.adapter.changed(change);
        }
        Ok(self.receipt(receipt.0))
    }
}
pub struct PreparedMutation(Prepared);
