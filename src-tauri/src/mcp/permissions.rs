//! Capability checks are repeated for every invocation, including hidden tools.
use super::connections::{GrantStore, Scope, SessionActor};
use lexena_mcp_ipc::{Error, Result};

/// The caller refreshes this list from the authoritative profiles manifest.
#[derive(Clone)]
pub struct ProfileIdentity {
    pub id: String,
    pub name: String,
    pub created_at: String,
}
#[derive(Clone)]
pub struct AuthorizedProfile {
    pub id: String,
    pub name: String,
    actor: SessionActor,
    scope: Scope,
}
impl AuthorizedProfile {
    pub fn revalidate(&self, store: &GrantStore, profiles: &[ProfileIdentity]) -> Result<()> {
        authorize(store, self.actor, profiles, &self.id, self.scope).map(|_| ())
    }
}
pub fn authorize(
    store: &GrantStore,
    actor: SessionActor,
    profiles: &[ProfileIdentity],
    profile_id: &str,
    scope: Scope,
) -> Result<AuthorizedProfile> {
    // Preview/read-entry helper only. Commits must retain the grant lease inside
    // the already-held profile transaction, rather than caching this result.
    let profile = profiles
        .iter()
        .find(|p| p.id == profile_id)
        .ok_or(Error::Denied)?;
    store.with_authorization(&actor, &profile.id, &profile.created_at, &[scope], |_| {
        AuthorizedProfile {
            id: profile.id.clone(),
            name: profile.name.clone(),
            actor,
            scope,
        }
    })
}
pub fn admin_caller_allowed(
    label: &str,
    scheme: &str,
    host: &str,
    port: Option<u16>,
    development: bool,
) -> bool {
    label == "main"
        && ((scheme == "tauri" && host == "localhost" && port.is_none())
            || (scheme == "http" && host == "tauri.localhost" && port.is_none())
            || (development && scheme == "http" && host == "localhost" && port == Some(1420)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::connections::*;
    fn session(store: &GrantStore, grant: &ConnectionGrant) -> SessionActor {
        SessionActor::authenticated(
            store.snapshot().unwrap().installation_id,
            grant.id,
            grant.generation,
        )
    }

    #[test]
    fn ungranted_profile_hidden_and_new_profile_not_auto_granted() {
        let store = GrantStore::memory();
        let actor = store
            .insert_grant(
                "work AI".into(),
                vec!["work".into()],
                vec![Scope::NotesRead],
            )
            .unwrap();
        let profiles = vec![
            ProfileIdentity {
                id: "work".into(),
                name: "Work".into(),
                created_at: "2026-01-01T00:00:00Z".into(),
            },
            ProfileIdentity {
                id: "personal".into(),
                name: "Secret".into(),
                created_at: "2026-01-01T00:00:00Z".into(),
            },
        ];
        assert_eq!(
            authorize(
                &store,
                session(&store, &actor),
                &profiles,
                "work",
                Scope::NotesRead
            )
            .unwrap()
            .name,
            "Work"
        );
        assert!(authorize(
            &store,
            session(&store, &actor),
            &profiles,
            "personal",
            Scope::NotesRead
        )
        .is_err());
        assert!(authorize(
            &store,
            session(&store, &actor),
            &profiles,
            "missing",
            Scope::NotesRead
        )
        .is_err());
    }

    #[test]
    fn local_only_hidden_without_opt_in_and_hidden_tool_direct_call_denied() {
        let store = GrantStore::memory();
        let actor = store
            .insert_grant("AI".into(), vec!["default".into()], vec![Scope::NotesRead])
            .unwrap();
        assert!(!store.allows(actor.id, "default", Scope::NotesLocalOnlyRead));
        assert!(!store.allows(actor.id, "default", Scope::NotesWrite));
        assert!(store.allows(actor.id, "default", Scope::NotesRead));
    }

    #[test]
    fn revocation_blocks_next_call_and_issued_authorization_revalidation() {
        let store = GrantStore::memory();
        let actor = store
            .insert_grant("AI".into(), vec!["default".into()], vec![Scope::NotesRead])
            .unwrap();
        let profiles = vec![ProfileIdentity {
            id: "default".into(),
            name: "Default".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
        }];
        let authorized = authorize(
            &store,
            session(&store, &actor),
            &profiles,
            "default",
            Scope::NotesRead,
        )
        .unwrap();
        assert!(authorized.revalidate(&store, &profiles).is_ok());
        store.revoke(actor.id).unwrap();
        assert!(authorized.revalidate(&store, &profiles).is_err());
        assert!(authorize(
            &store,
            session(&store, &actor),
            &profiles,
            "default",
            Scope::NotesRead
        )
        .is_err());
    }
    #[test]
    fn only_main_local_window_can_administer_connections() {
        assert!(admin_caller_allowed(
            "main",
            "tauri",
            "localhost",
            None,
            false
        ));
        assert!(admin_caller_allowed(
            "main",
            "http",
            "tauri.localhost",
            None,
            false
        ));
        assert!(!admin_caller_allowed(
            "mini",
            "tauri",
            "localhost",
            None,
            false
        ));
        assert!(!admin_caller_allowed(
            "main",
            "https",
            "evil.example",
            None,
            false
        ));
        assert!(!admin_caller_allowed(
            "main",
            "http",
            "localhost",
            Some(1420),
            false
        ));
        assert!(admin_caller_allowed(
            "main",
            "http",
            "localhost",
            Some(1420),
            true
        ));
    }
    #[test]
    fn recreated_profile_slug_does_not_inherit_grant_after_deletion_crash() {
        let store = GrantStore::memory();
        let grant = store
            .insert_grant("work".into(), vec!["work".into()], vec![Scope::NotesRead])
            .unwrap();
        let original = vec![ProfileIdentity {
            id: "work".into(),
            name: "Work".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
        }];
        assert!(authorize(
            &store,
            session(&store, &grant),
            &original,
            "work",
            Scope::NotesRead
        )
        .is_ok());
        // Crash between manifest deletion and remove_profile: old grant persists.
        let recreated = vec![ProfileIdentity {
            id: "work".into(),
            name: "New work".into(),
            created_at: "2026-11-05T12:00:00Z".into(),
        }];
        assert!(authorize(
            &store,
            session(&store, &grant),
            &recreated,
            "work",
            Scope::NotesRead
        )
        .is_err());
    }
}
