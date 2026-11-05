//! Only the main renderer owns queue snapshots; its captured profile session is
//! validated while the profile lifecycle lock is retained.
use crate::note_service::{
    editors::{editors, ProfileSession},
    DomainError, LockedProfile, ProfileScope,
};
use crate::sync_queue::{self, QueueSnapshot};
use tauri::{AppHandle, WebviewWindow};

pub(crate) fn with_session<T>(
    app: &AppHandle,
    window: &WebviewWindow,
    session: &ProfileSession,
    work: impl FnOnce(&mut LockedProfile<'_>) -> Result<T, DomainError>,
) -> Result<T, String> {
    let url = window
        .url()
        .map_err(|_| "EDITOR_SESSION_EXPIRED".to_string())?;
    let local =
        (url.scheme() == "tauri" && url.host_str() == Some("localhost") && url.port().is_none())
            || (url.scheme() == "http"
                && url.host_str() == Some("tauri.localhost")
                && url.port().is_none())
            || (cfg!(debug_assertions)
                && url.scheme() == "http"
                && url.host_str() == Some("localhost")
                && url.port() == Some(1420));
    if window.label() != "main" || !local {
        return Err("EDITOR_SESSION_EXPIRED".into());
    }
    ProfileScope::resolve(app, &session.profile_id)
        .and_then(|scope| {
            scope.transaction(|p| {
                editors().validate_session(&session.profile_id, session.session_generation)?;
                if !p.is_active()? {
                    return Err(DomainError::EditorSessionExpired);
                }
                work(p)
            })
        })
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn load_sync_queue(
    app: AppHandle,
    window: WebviewWindow,
    session: ProfileSession,
) -> Result<QueueSnapshot, String> {
    with_session(&app, &window, &session, |p| {
        sync_queue::load_snapshot(&p.path(std::path::Path::new("sync-queue.json"))?)
            .map_err(DomainError::Storage)
    })
}

#[tauri::command]
pub fn save_sync_queue(
    app: AppHandle,
    window: WebviewWindow,
    session: ProfileSession,
    snapshot: QueueSnapshot,
    expected_revision: String,
) -> Result<QueueSnapshot, String> {
    with_session(&app, &window, &session, |p| {
        sync_queue::save_snapshot(
            &p.path(std::path::Path::new("sync-queue.json"))?,
            snapshot,
            &expected_revision,
        )
        .map_err(DomainError::Storage)
    })
}
