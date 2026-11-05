//! Connection metadata lives at app-data/mcp, outside profiles and synced settings.
use lexena_mcp_ipc::{Error, Result, Secret};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio::sync::watch;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Scope {
    #[serde(rename = "notes.read")]
    NotesRead,
    #[serde(rename = "notes.write")]
    NotesWrite,
    #[serde(rename = "notes.delete")]
    NotesDelete,
    #[serde(rename = "notes.replace")]
    NotesReplace,
    #[serde(rename = "notes.local_only.read")]
    NotesLocalOnlyRead,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionGrant {
    pub id: Uuid,
    pub name: String,
    pub profile_ids: Vec<String>,
    /// Bind the public slug to the manifest creation identity. A recreated slug
    /// must never inherit a previous profile's grant after a deletion crash.
    #[serde(default)]
    pub profile_created_at: BTreeMap<String, String>,
    pub scopes: Vec<Scope>,
    pub revoked_at: Option<String>,
    pub last_activity_at: Option<String>,
    #[serde(default)]
    pub generation: u64,
    /// Durable deny state: failed/interrupted rotations require an explicit retry.
    #[serde(default)]
    pub rotating: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionConfig {
    pub version: u32,
    pub installation_id: Uuid,
    pub server_path: PathBuf,
    pub enabled: bool,
    pub connections: Vec<ConnectionGrant>,
}
/// Immutable identity produced after mutual IPC authentication, never client JSON.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionActor {
    installation_id: Uuid,
    connection_id: Uuid,
    generation: u64,
}
impl SessionActor {
    pub(super) fn authenticated(
        installation_id: Uuid,
        connection_id: Uuid,
        generation: u64,
    ) -> Self {
        Self {
            installation_id,
            connection_id,
            generation,
        }
    }
    pub fn installation_id(&self) -> Uuid {
        self.installation_id
    }
    pub fn connection_id(&self) -> Uuid {
        self.connection_id
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
}
pub struct AuthorizationLease<'a> {
    grant: &'a ConnectionGrant,
    _not_send: std::marker::PhantomData<std::rc::Rc<()>>,
}
impl AuthorizationLease<'_> {
    pub fn allows(&self, scope: Scope) -> bool {
        self.grant.scopes.contains(&scope)
    }
    pub fn allows_profile(&self, profile_id: &str, created_at: &str) -> bool {
        self.grant.profile_ids.iter().any(|id| id == profile_id)
            && self
                .grant
                .profile_created_at
                .get(profile_id)
                .is_some_and(|created| created == created_at)
    }
}
pub struct GrantStore {
    path: Option<PathBuf>,
    config: Mutex<ConnectionConfig>,
    changes: watch::Sender<u64>,
    initialization_error: Mutex<Option<String>>,
    rotations: Mutex<()>,
}
impl GrantStore {
    /// Hold permission state stable for a synchronous metadata/read/commit
    /// closure. Refresh registry information BEFORE entering this function.
    /// Never re-lock GrantStore or await from `work`.
    pub fn with_session_authorization<R>(
        &self,
        actor: &SessionActor,
        work: impl FnOnce(&AuthorizationLease<'_>) -> R,
    ) -> Result<R> {
        self.ensure_ready()?;
        let config = self.config.lock().map_err(|_| Error::Denied)?;
        if !config.enabled || config.installation_id != actor.installation_id() {
            return Err(Error::Denied);
        }
        let grant = config
            .connections
            .iter()
            .find(|grant| {
                grant.id == actor.connection_id()
                    && grant.generation == actor.generation()
                    && grant.revoked_at.is_none()
                    && !grant.rotating
            })
            .ok_or(Error::Denied)?;
        // `grant` borrows the guard: it remains locked until work has returned.
        Ok(work(&AuthorizationLease {
            grant,
            _not_send: std::marker::PhantomData,
        }))
    }
    /// Lock order: first the caller's profile transaction, THEN this grant lease.
    /// `created_at` must come from that transaction's validated ProfileScope.
    /// Run only synchronous checked operations (e.g. mutate_checked_locked) here,
    /// after async editor preparation. The lease never escapes or crosses await.
    pub fn with_authorization<R>(
        &self,
        actor: &SessionActor,
        profile_id: &str,
        created_at: &str,
        scopes: &[Scope],
        work: impl FnOnce(&AuthorizationLease<'_>) -> R,
    ) -> Result<R> {
        self.with_session_authorization(actor, |lease| {
            if scopes.is_empty()
                || !lease.allows_profile(profile_id, created_at)
                || !scopes.iter().all(|scope| lease.allows(*scope))
            {
                return Err(Error::Denied);
            }
            Ok(work(lease))
        })?
    }
    pub fn load(app_data: &Path, server_path: &Path) -> Result<Arc<Self>> {
        let path = app_data.join("mcp/connections.json");
        let server_path = server_path.canonicalize()?;
        let config = if path.exists() {
            let bytes = std::fs::read(&path)?;
            if bytes.len() > 1_048_576 {
                return Err(Error::Frame);
            }
            let config: ConnectionConfig =
                serde_json::from_slice(&bytes).map_err(|_| Error::Frame)?;
            if config.version != 1 || config.server_path != server_path {
                return Err(Error::Installation);
            }
            config
        } else {
            ConnectionConfig {
                version: 1,
                installation_id: Uuid::new_v4(),
                server_path,
                enabled: false,
                connections: vec![],
            }
        };
        let (changes, _) = watch::channel(0);
        let store = Arc::new(Self {
            path: Some(path),
            config: Mutex::new(config),
            changes,
            initialization_error: Mutex::new(None),
            rotations: Mutex::new(()),
        });
        store.persist(&store.snapshot()?)?;
        Ok(store)
    }
    /// Keep status and explicit reconnect available when copied/moved metadata
    /// cannot be adopted. No existing file is modified on this fallback path.
    pub fn load_for_ui(app_data: &Path, server_path: &Path) -> Result<Arc<Self>> {
        match Self::load(app_data, server_path) {
            Ok(store) => Ok(store),
            Err(error) => {
                let (changes, _) = watch::channel(0);
                Ok(Arc::new(Self {
                    path: Some(app_data.join("mcp/connections.json")),
                    config: Mutex::new(ConnectionConfig {
                        version: 1,
                        installation_id: Uuid::new_v4(),
                        server_path: server_path.canonicalize()?,
                        enabled: false,
                        connections: vec![],
                    }),
                    changes,
                    initialization_error: Mutex::new(Some(error.to_string())),
                    rotations: Mutex::new(()),
                }))
            }
        }
    }
    pub fn initialization_error(&self) -> Option<String> {
        self.initialization_error
            .lock()
            .ok()
            .and_then(|error| error.clone())
    }
    fn ensure_ready(&self) -> Result<()> {
        if self
            .initialization_error
            .lock()
            .map_err(|_| Error::Denied)?
            .is_some()
        {
            return Err(Error::Installation);
        }
        Ok(())
    }
    /// Explicit local UI action: preserve profiles, replace installation identity,
    /// revoke all old grants, and require fresh client configuration.
    pub fn reset_installation(&self) -> Result<()> {
        let path = self.path.as_ref().ok_or(Error::Installation)?;
        let endpoint = path.with_file_name("endpoint.json");
        if let Ok(descriptor) = lexena_mcp_ipc::EndpointDescriptor::read(&endpoint) {
            #[cfg(windows)]
            if lexena_mcp_ipc::windows::is_pipe_active(&descriptor.pipe_name)? {
                return Err(Error::Installation);
            }
        }
        let previous = std::fs::read(path)
            .ok()
            .filter(|bytes| bytes.len() <= 1_048_576)
            .and_then(|bytes| serde_json::from_slice::<ConnectionConfig>(&bytes).ok());
        let mut config = self.config.lock().map_err(|_| Error::Denied)?;
        let replacement = ConnectionConfig {
            version: 1,
            installation_id: Uuid::new_v4(),
            server_path: config.server_path.clone(),
            enabled: false,
            connections: vec![],
        };
        lexena_mcp_ipc::write_private_json(path, &replacement)?;
        *config = replacement;
        *self
            .initialization_error
            .lock()
            .map_err(|_| Error::Denied)? = None;
        self.changes
            .send_modify(|epoch| *epoch = epoch.wrapping_add(1));
        let _ = std::fs::remove_file(endpoint);
        if let Some(previous) = previous {
            for grant in previous.connections {
                let _ = lexena_mcp_ipc::delete_secret(previous.installation_id, grant.id);
            }
        }
        Ok(())
    }
    #[cfg(test)]
    pub fn memory() -> Self {
        let (changes, _) = watch::channel(0);
        Self {
            path: None,
            config: Mutex::new(ConnectionConfig {
                version: 1,
                installation_id: Uuid::new_v4(),
                server_path: std::env::current_exe().expect("test executable path"),
                enabled: true,
                connections: vec![],
            }),
            changes,
            initialization_error: Mutex::new(None),
            rotations: Mutex::new(()),
        }
    }
    fn persist(&self, config: &ConnectionConfig) -> Result<()> {
        self.ensure_ready()?;
        if let Some(path) = &self.path {
            lexena_mcp_ipc::write_private_json(path, config)?;
        }
        Ok(())
    }
    fn update<T>(&self, change: impl FnOnce(&mut ConnectionConfig) -> Result<T>) -> Result<T> {
        let mut current = self.config.lock().map_err(|_| Error::Denied)?;
        let mut next = current.clone();
        let result = change(&mut next)?;
        self.persist(&next)?;
        *current = next;
        self.changes
            .send_modify(|epoch| *epoch = epoch.wrapping_add(1));
        Ok(result)
    }
    pub fn snapshot(&self) -> Result<ConnectionConfig> {
        Ok(self.config.lock().map_err(|_| Error::Denied)?.clone())
    }
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }
    pub fn set_enabled(&self, enabled: bool) -> Result<()> {
        self.ensure_ready()?;
        if enabled {
            let config = self.snapshot()?;
            // A real set/read/delete probe is mandatory even if no connection exists.
            let probe = Uuid::new_v4();
            let secret = Secret::generate()?;
            lexena_mcp_ipc::save_secret(config.installation_id, probe, &secret)?;
            lexena_mcp_ipc::delete_secret(config.installation_id, probe)?;
        }
        self.update(|config| {
            config.enabled = enabled;
            Ok(())
        })
    }
    #[cfg(test)]
    pub(crate) fn insert_grant(
        &self,
        name: String,
        profile_ids: Vec<String>,
        scopes: Vec<Scope>,
    ) -> Result<ConnectionGrant> {
        validate_grant(&name, &profile_ids, &scopes)?;
        let profile_created_at = profile_ids
            .iter()
            .map(|id| (id.clone(), "2026-01-01T00:00:00Z".to_string()))
            .collect();
        self.update(|config| {
            if config.connections.len() >= 256 {
                return Err(Error::Denied);
            }
            let grant = ConnectionGrant {
                id: Uuid::new_v4(),
                name,
                profile_ids,
                profile_created_at,
                scopes,
                revoked_at: None,
                last_activity_at: None,
                generation: 0,
                rotating: false,
            };
            config.connections.push(grant.clone());
            Ok(grant)
        })
    }
    pub fn create(
        &self,
        name: String,
        profiles: Vec<super::permissions::ProfileIdentity>,
        scopes: Vec<Scope>,
    ) -> Result<ConnectionGrant> {
        self.ensure_ready()?;
        let profile_ids = profiles.iter().map(|p| p.id.clone()).collect::<Vec<_>>();
        if profiles
            .iter()
            .any(|p| p.created_at.is_empty() || p.created_at.len() > 128)
        {
            return Err(Error::Denied);
        }
        let profile_created_at = profiles.into_iter().map(|p| (p.id, p.created_at)).collect();
        validate_grant(&name, &profile_ids, &scopes)?;
        let installation = self.snapshot()?.installation_id;
        let secret = Secret::generate()?;
        // Persist secret before publishing its ID. Roll back if metadata fails.
        let id = Uuid::new_v4();
        lexena_mcp_ipc::save_secret(installation, id, &secret)?;
        let result = self.update(|config| {
            if config.connections.len() >= 256 {
                return Err(Error::Denied);
            }
            let grant = ConnectionGrant {
                id,
                name,
                profile_ids,
                profile_created_at,
                scopes,
                revoked_at: None,
                last_activity_at: None,
                generation: 0,
                rotating: false,
            };
            config.connections.push(grant.clone());
            Ok(grant)
        });
        if result.is_err() {
            let _ = lexena_mcp_ipc::delete_secret(installation, id);
        }
        result
    }
    pub fn revoke(&self, id: Uuid) -> Result<()> {
        // Metadata authorization is authoritative even if the vault is unavailable.
        self.update(|config| {
            let grant = config
                .connections
                .iter_mut()
                .find(|g| g.id == id)
                .ok_or(Error::Denied)?;
            grant.revoked_at = Some(chrono::Utc::now().to_rfc3339());
            grant.generation += 1;
            Ok(())
        })?;
        let installation = self.snapshot()?.installation_id;
        if self.path.is_some() {
            lexena_mcp_ipc::delete_secret(installation, id)
        } else {
            Ok(())
        }
    }
    pub fn rotate(&self, id: Uuid) -> Result<()> {
        self.rotate_with_secret_writer(id, lexena_mcp_ipc::save_secret)
    }
    fn rotate_with_secret_writer(
        &self,
        id: Uuid,
        write_secret: impl FnOnce(Uuid, Uuid, &Secret) -> Result<()>,
    ) -> Result<()> {
        // Serialize credential replacement. Authentication remains nonblocking:
        // it reads the durable rotating flag and fails closed throughout.
        let _rotation = self.rotations.lock().map_err(|_| Error::Denied)?;
        let (installation, generation) = self.update(|config| {
            let grant = config
                .connections
                .iter_mut()
                .find(|g| g.id == id && g.revoked_at.is_none())
                .ok_or(Error::Denied)?;
            grant.rotating = true;
            grant.generation = grant.generation.checked_add(1).ok_or(Error::Denied)?;
            Ok((config.installation_id, grant.generation))
        })?;
        // Any failure, process termination or final persistence failure leaves
        // the on-disk grant rotating. Never reactivate the previous credential.
        write_secret(installation, id, &Secret::generate()?)?;
        self.update(|config| {
            if config.installation_id != installation {
                return Err(Error::Denied);
            }
            let grant = config
                .connections
                .iter_mut()
                .find(|g| {
                    g.id == id && g.revoked_at.is_none() && g.rotating && g.generation == generation
                })
                .ok_or(Error::Denied)?;
            grant.generation = grant.generation.checked_add(1).ok_or(Error::Denied)?;
            grant.rotating = false;
            Ok(())
        })
    }
    pub fn grant(&self, id: Uuid) -> Result<ConnectionGrant> {
        self.ensure_ready()?;
        let config = self.config.lock().map_err(|_| Error::Denied)?;
        if !config.enabled {
            return Err(Error::Denied);
        }
        config
            .connections
            .iter()
            .find(|g| g.id == id && g.revoked_at.is_none() && !g.rotating)
            .cloned()
            .ok_or(Error::Denied)
    }
    pub fn secret(&self, id: Uuid) -> Result<Secret> {
        self.grant(id)?;
        lexena_mcp_ipc::read_secret(self.snapshot()?.installation_id, id)
    }
    /// Test fixture predicate. Production checks retain with_authorization.
    #[cfg(test)]
    pub fn allows(&self, id: Uuid, profile: &str, scope: Scope) -> bool {
        self.grant(id).is_ok_and(|grant| {
            grant.profile_ids.iter().any(|id| id == profile) && grant.scopes.contains(&scope)
        })
    }
    pub fn remove_profile(&self, profile: &str) -> Result<()> {
        self.update(|config| {
            for grant in &mut config.connections {
                grant.profile_ids.retain(|id| id != profile);
                grant.profile_created_at.remove(profile);
                grant.generation += 1;
            }
            Ok(())
        })
    }
    pub fn record_activity(&self, id: Uuid) -> Result<()> {
        self.update(|config| {
            let grant = config
                .connections
                .iter_mut()
                .find(|g| g.id == id)
                .ok_or(Error::Denied)?;
            grant.last_activity_at = Some(chrono::Utc::now().to_rfc3339());
            Ok(())
        })
    }
}
fn validate_grant(name: &str, profiles: &[String], scopes: &[Scope]) -> Result<()> {
    if name.trim().is_empty()
        || name.len() > 128
        || profiles.is_empty()
        || profiles.len() > 128
        || scopes.is_empty()
        || profiles.iter().any(|id| id.is_empty() || id.len() > 128)
        || profiles.iter().collect::<HashSet<_>>().len() != profiles.len()
        || scopes.iter().collect::<HashSet<_>>().len() != scopes.len()
    {
        return Err(Error::Denied);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn commit_lease_rejects_wrong_installation_profile_lifetime_and_scopes_before_callback() {
        let store = GrantStore::memory();
        let grant = store
            .insert_grant(
                "lease".into(),
                vec!["default".into()],
                vec![Scope::NotesWrite],
            )
            .unwrap();
        let installation = store.snapshot().unwrap().installation_id;
        let actor = SessionActor::authenticated(installation, grant.id, grant.generation);
        let wrong_install = SessionActor::authenticated(Uuid::new_v4(), grant.id, grant.generation);
        let mut callbacks = 0;
        for (session, profile, created, scopes) in [
            (&actor, "default", "different", vec![Scope::NotesWrite]),
            (
                &actor,
                "other",
                "2026-01-01T00:00:00Z",
                vec![Scope::NotesWrite],
            ),
            (
                &actor,
                "default",
                "2026-01-01T00:00:00Z",
                vec![Scope::NotesRead],
            ),
            (&actor, "default", "2026-01-01T00:00:00Z", vec![]),
            (
                &wrong_install,
                "default",
                "2026-01-01T00:00:00Z",
                vec![Scope::NotesWrite],
            ),
        ] {
            assert!(store
                .with_authorization(session, profile, created, &scopes, |_| {
                    callbacks += 1;
                })
                .is_err());
        }
        assert_eq!(callbacks, 0);
    }
    #[test]
    fn stale_session_generation_cannot_authorize_after_rotation_without_runtime_cancellation() {
        let store = GrantStore::memory();
        let grant = store
            .insert_grant(
                "lease".into(),
                vec!["default".into()],
                vec![Scope::NotesWrite],
            )
            .unwrap();
        let installation = store.snapshot().unwrap().installation_id;
        let actor = SessionActor::authenticated(installation, grant.id, grant.generation);
        let calls = std::sync::atomic::AtomicUsize::new(0);
        store
            .with_authorization(
                &actor,
                "default",
                "2026-01-01T00:00:00Z",
                &[Scope::NotesWrite],
                |lease| {
                    assert!(lease.allows(Scope::NotesWrite));
                    assert!(!lease.allows(Scope::NotesLocalOnlyRead));
                    calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                },
            )
            .unwrap();
        store.rotate(grant.id).unwrap();
        let stale = store.with_authorization(
            &actor,
            "default",
            "2026-01-01T00:00:00Z",
            &[Scope::NotesWrite],
            |_| {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            },
        );
        let _ = lexena_mcp_ipc::delete_secret(installation, grant.id);
        assert!(
            stale.is_err(),
            "old authenticated session generation authorized after rotation"
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
    #[test]
    fn revoke_and_rotate_serialize_with_the_held_synchronous_commit_lease() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            mpsc,
        };
        for rotate in [false, true] {
            let store = Arc::new(GrantStore::memory());
            let grant = store
                .insert_grant(
                    "lease".into(),
                    vec!["default".into()],
                    vec![Scope::NotesWrite],
                )
                .unwrap();
            let installation = store.snapshot().unwrap().installation_id;
            let actor = SessionActor::authenticated(installation, grant.id, grant.generation);
            let committed = Arc::new(AtomicUsize::new(0));
            let (entered, seen) = mpsc::channel();
            let (resume, paused) = mpsc::channel();
            let commit_store = store.clone();
            let commit_count = committed.clone();
            let commit = std::thread::spawn(move || {
                commit_store.with_authorization(
                    &actor,
                    "default",
                    "2026-01-01T00:00:00Z",
                    &[Scope::NotesWrite],
                    |lease| {
                        assert!(lease.allows(Scope::NotesWrite));
                        entered.send(()).unwrap();
                        paused
                            .recv_timeout(std::time::Duration::from_secs(5))
                            .unwrap();
                        commit_count.fetch_add(1, Ordering::SeqCst);
                    },
                )
            });
            seen.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            let held = store.config.try_lock().is_err();
            let (started, observed) = mpsc::channel();
            let (finished, result) = mpsc::channel();
            let transition_store = store.clone();
            let transition_count = committed.clone();
            let transition = std::thread::spawn(move || {
                started.send(()).unwrap();
                let changed = if rotate {
                    transition_store.rotate(grant.id)
                } else {
                    transition_store.revoke(grant.id)
                };
                finished
                    .send((changed, transition_count.load(Ordering::SeqCst)))
                    .unwrap();
            });
            observed
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            let returned_early = result.recv_timeout(std::time::Duration::from_millis(50));
            resume.send(()).unwrap();
            commit.join().unwrap().unwrap();
            transition.join().unwrap();
            let returned_before_commit = returned_early.is_ok();
            let (changed, commits) = returned_early.unwrap_or_else(|_| {
                result
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap()
            });
            let _ = lexena_mcp_ipc::delete_secret(installation, grant.id);
            changed.unwrap();
            assert!(held, "grant state was released before synchronous commit");
            assert!(
                !returned_before_commit,
                "authorization change returned while commit lease was held"
            );
            assert_eq!(commits, 1);
            let mut started_after = false;
            assert!(store
                .with_authorization(
                    &actor,
                    "default",
                    "2026-01-01T00:00:00Z",
                    &[Scope::NotesWrite],
                    |_| {
                        started_after = true;
                    }
                )
                .is_err());
            assert!(!started_after);
        }
    }
    #[test]
    fn rotation_failure_stays_blocked_after_reload_until_explicit_fresh_rotation() {
        for replaced in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let exe = std::env::current_exe().unwrap();
            let store = GrantStore::load(dir.path(), &exe).unwrap();
            store
                .update(|config| {
                    config.enabled = true;
                    Ok(())
                })
                .unwrap();
            let grant = store
                .insert_grant(
                    "rotation".into(),
                    vec!["default".into()],
                    vec![Scope::NotesRead],
                )
                .unwrap();
            let installation = store.snapshot().unwrap().installation_id;
            let failed = store.rotate_with_secret_writer(grant.id, |installation, id, secret| {
                if replaced {
                    lexena_mcp_ipc::save_secret(installation, id, secret)?;
                }
                Err(Error::Vault)
            });
            let reopened = GrantStore::load(dir.path(), &exe).unwrap();
            let remained_blocked = reopened.grant(grant.id).is_err();
            reopened.rotate(grant.id).unwrap();
            let recovered = reopened.grant(grant.id).is_ok();
            lexena_mcp_ipc::delete_secret(installation, grant.id).unwrap();
            assert!(failed.is_err());
            assert!(
                remained_blocked,
                "failed rotation grant reactivated on startup"
            );
            assert!(recovered);
        }
    }
    #[tokio::test]
    async fn rotation_blocks_handshake_between_publication_and_vault_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let exe = std::env::current_exe().unwrap();
        let store = GrantStore::load(dir.path(), &exe).unwrap();
        store
            .update(|config| {
                config.enabled = true;
                Ok(())
            })
            .unwrap();
        let grant = store
            .insert_grant(
                "rotation".into(),
                vec!["default".into()],
                vec![Scope::NotesRead],
            )
            .unwrap();
        let installation = store.snapshot().unwrap().installation_id;
        let old_secret = Secret::generate().unwrap();
        lexena_mcp_ipc::save_secret(installation, grant.id, &old_secret).unwrap();
        struct Cleanup(Uuid, Uuid);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = lexena_mcp_ipc::delete_secret(self.0, self.1);
            }
        }
        let _cleanup = Cleanup(installation, grant.id);
        let (published, seen) = std::sync::mpsc::channel();
        let (resume, paused) = std::sync::mpsc::channel();
        let rotation_store = store.clone();
        let id = grant.id;
        let worker = std::thread::spawn(move || {
            rotation_store.rotate_with_secret_writer(id, |installation, id, secret| {
                published.send(()).unwrap();
                paused
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .map_err(|_| Error::Timeout)?;
                lexena_mcp_ipc::save_secret(installation, id, secret)
            })
        });
        seen.recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let reopened = GrantStore::load(dir.path(), &exe).unwrap();
        let blocked_after_restart = reopened.grant(id).is_err();
        let auth = lexena_mcp_ipc::IpcAuthenticator::new(installation);
        let (mut client, mut server) = tokio::io::duplex(4096);
        let (accepted, connected) = tokio::join!(
            async {
                let result = auth.accept(&mut server, |id| store.secret(id)).await;
                drop(server);
                result
            },
            lexena_mcp_ipc::authenticate_client(&mut client, installation, id, &old_secret)
        );
        let blocked_in_transition = accepted.is_err() && connected.is_err();
        resume.send(()).unwrap();
        worker.join().unwrap().unwrap();
        assert!(
            blocked_in_transition,
            "old credential authenticated during rotation"
        );
        assert!(
            blocked_after_restart,
            "interrupted rotation silently reactivated persisted grant"
        );
        assert!(store.grant(id).is_ok());
        let auth = lexena_mcp_ipc::IpcAuthenticator::new(installation);
        let (mut old_client, mut old_server) = tokio::io::duplex(4096);
        let (old_accepted, old_connected) = tokio::join!(
            async {
                let result = auth.accept(&mut old_server, |id| store.secret(id)).await;
                drop(old_server);
                result
            },
            async {
                let result = lexena_mcp_ipc::authenticate_client(
                    &mut old_client,
                    installation,
                    id,
                    &old_secret,
                )
                .await;
                drop(old_client);
                result
            }
        );
        assert!(
            old_accepted.is_err() && old_connected.is_err(),
            "previous secret survived completed rotation"
        );
        let fresh = lexena_mcp_ipc::read_secret(installation, id).unwrap();
        let auth = lexena_mcp_ipc::IpcAuthenticator::new(installation);
        let (mut client, mut server) = tokio::io::duplex(4096);
        let (accepted, connected) = tokio::join!(
            auth.accept(&mut server, |id| store.secret(id)),
            lexena_mcp_ipc::authenticate_client(&mut client, installation, id, &fresh)
        );
        assert!(
            accepted.is_ok() && connected.is_ok(),
            "fresh handshake: server={accepted:?}, client={connected:?}"
        );
    }
    #[test]
    fn metadata_reloads_and_another_installation_is_not_silently_adopted() {
        let dir = tempfile::tempdir().unwrap();
        let exe = std::env::current_exe().unwrap();
        let store = GrantStore::load(dir.path(), &exe).unwrap();
        assert!(!store.snapshot().unwrap().enabled);
        let grant = store
            .insert_grant("work".into(), vec!["work".into()], vec![Scope::NotesRead])
            .unwrap();
        let installation = store.snapshot().unwrap().installation_id;
        drop(store);
        let restored = GrantStore::load(dir.path(), &exe).unwrap();
        assert_eq!(restored.snapshot().unwrap().installation_id, installation);
        assert_eq!(restored.snapshot().unwrap().connections[0].id, grant.id);
        let other = dir.path().join("other.exe");
        std::fs::write(&other, []).unwrap();
        assert!(matches!(
            GrantStore::load(dir.path(), &other),
            Err(Error::Installation)
        ));
    }
    #[test]
    fn moved_installation_stays_blocked_until_explicit_reset_and_drops_old_grants() {
        let dir = tempfile::tempdir().unwrap();
        let exe = std::env::current_exe().unwrap();
        let store = GrantStore::load(dir.path(), &exe).unwrap();
        let old = store.snapshot().unwrap().installation_id;
        store
            .insert_grant("old".into(), vec!["work".into()], vec![Scope::NotesRead])
            .unwrap();
        drop(store);
        let moved = dir.path().join("lexena.exe");
        std::fs::write(&moved, []).unwrap();
        let blocked = GrantStore::load_for_ui(dir.path(), &moved).unwrap();
        assert!(blocked.initialization_error().is_some());
        assert!(blocked
            .insert_grant("no".into(), vec!["work".into()], vec![Scope::NotesRead])
            .is_err());
        assert!(
            GrantStore::load(dir.path(), &exe)
                .unwrap()
                .snapshot()
                .unwrap()
                .connections
                .len()
                == 1
        );
        blocked.reset_installation().unwrap();
        let reset = GrantStore::load(dir.path(), &moved)
            .unwrap()
            .snapshot()
            .unwrap();
        assert_ne!(reset.installation_id, old);
        assert!(reset.connections.is_empty());
        assert!(!reset.enabled);
        assert!(blocked.initialization_error().is_none());
    }
    #[tokio::test]
    async fn explicit_reset_refuses_a_still_active_installation() {
        let dir = tempfile::tempdir().unwrap();
        let store = GrantStore::load(dir.path(), &std::env::current_exe().unwrap()).unwrap();
        let config = store.snapshot().unwrap();
        let descriptor = lexena_mcp_ipc::EndpointDescriptor::new(
            config.installation_id,
            config.server_path,
            env!("CARGO_PKG_VERSION"),
        );
        let _pipe = lexena_mcp_ipc::windows::create_pipe(&descriptor.pipe_name, true).unwrap();
        lexena_mcp_ipc::write_private_json(&dir.path().join("mcp/endpoint.json"), &descriptor)
            .unwrap();
        assert!(matches!(
            store.reset_installation(),
            Err(Error::Installation)
        ));
        assert_eq!(
            store.snapshot().unwrap().installation_id,
            descriptor.installation_id
        );
    }
}
