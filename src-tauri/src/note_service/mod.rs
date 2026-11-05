//! Profile-scoped storage shared by UI, imports and future local integrations.
pub mod content;
pub mod mutation;
pub mod repository;
pub mod scope;

pub use mutation::{Actor, MutationIntent, MutationResult, NoteService};
pub use repository::{FolderRepository, NoteRepository};
pub use scope::{DomainError, LockedProfile, ProfileRegistry, ProfileScope};

use tauri::{AppHandle, Manager};
impl ProfileScope {
    pub fn resolve(app: &AppHandle, profile_id: &str) -> Result<Self, DomainError> {
        let app_data = app
            .path()
            .app_data_dir()
            .map_err(|e| DomainError::Storage(e.to_string()))?;
        Self::resolve_in(&app_data, profile_id)
    }
    /// Transitional UI adapter. Capture once before any I/O; MCP never uses this.
    pub fn active(app: &AppHandle) -> Result<Self, DomainError> {
        let id = crate::profiles::get_active_id(app);
        Self::resolve(app, &id)
    }
}

pub mod editors;
pub mod journal;
pub mod outbox;
pub mod versions;
