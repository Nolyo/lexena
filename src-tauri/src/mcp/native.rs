//! Native events are refresh hints; durability and sync come from the journal.
use super::tools::{Change, EditorAdapter, PrepareFuture};
use crate::note_service::{editors::EditPermit, ProfileScope};
use tauri::{AppHandle, Emitter};
pub struct TauriEditors {
    app: AppHandle,
}
impl TauriEditors {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}
impl EditorAdapter for TauriEditors {
    fn prepare<'a>(&'a self, scope: &'a ProfileScope, note_id: &'a str) -> PrepareFuture<'a> {
        Box::pin(crate::commands::note_editors::prepare_external_mutation(
            &self.app, scope, note_id,
        ))
    }
    fn finish(&self, permit: &EditPermit, committed: bool) {
        crate::commands::note_editors::finish_external_mutation(&self.app, permit, committed);
    }
    fn changed(&self, change: Change) {
        if change.folder {
            let _=self.app.emit("folder-service-changed",serde_json::json!({"profile_id":change.profile_id,"folder_id":change.entity_id,"revision":change.revision,"operation_id":change.operation_id,"source":"mcp","mutation_type":change.mutation_type}));
        } else {
            let _=self.app.emit("note-service-changed",serde_json::json!({"profile_id":change.profile_id,"note_id":change.entity_id,"revision":change.revision,"operation_id":change.operation_id,"source":"mcp","mutation_type":change.mutation_type}));
        }
    }
}
