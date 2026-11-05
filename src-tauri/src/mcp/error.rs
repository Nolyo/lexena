//! Bounded, content-free business diagnostics. Never expose filesystem/OS errors.
use crate::note_service::DomainError;
use rmcp::{
    model::{CallToolResult, ErrorData},
    schemars::{self, JsonSchema},
};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct BusinessError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub next_action: String,
}
impl BusinessError {
    pub fn new(code: &str) -> Self {
        let (message, retryable, next_action) = match code {
            "NOT_FOUND" => (
                "The requested authorized object was not found.",
                false,
                "Choose an authorized profile and object.",
            ),
            "PERMISSION_DENIED" => (
                "This connection does not allow that operation.",
                false,
                "Change authorization in Lexena settings.",
            ),
            "REVISION_CONFLICT" => (
                "The object changed since it was read.",
                true,
                "Read the current version before a new operation.",
            ),
            "EDITOR_BUSY" => (
                "An editor has unsaved changes or did not respond.",
                true,
                "Save or close the editor, then retry.",
            ),
            "CURSOR_EXPIRED" => (
                "This pagination cursor expired.",
                true,
                "Restart the listing or full HTML read.",
            ),
            "CURSOR_INVALID" => (
                "This cursor does not belong to this request.",
                false,
                "Restart pagination with the same profile and filters.",
            ),
            "READ_TOKEN_REQUIRED" => (
                "A complete current HTML read is required.",
                false,
                "Read every HTML page sequentially and use its final read_token.",
            ),
            "READ_TOKEN_INVALID" => (
                "The complete HTML read is no longer valid.",
                true,
                "Read every page of the current HTML revision again.",
            ),
            "AMBIGUOUS_MATCH" => (
                "The text occurs more than once.",
                false,
                "Choose a unique exact text occurrence.",
            ),
            "UNSUPPORTED_CONTENT" => (
                "The requested content transformation is unsupported.",
                false,
                "Use a supported format without changing unrelated rich content.",
            ),
            "CONTENT_TOO_LARGE" => (
                "The requested content exceeds the mutation limit.",
                false,
                "Reduce the content size.",
            ),
            "IDEMPOTENCY_CONFLICT" => (
                "The operation identifier was already used for different arguments.",
                false,
                "Use the original arguments or a new operation_id for a new operation.",
            ),
            "HISTORY_FULL" => (
                "The private restoration history is full.",
                false,
                "Wait for retained versions to expire.",
            ),
            "SNAPSHOT_EXPIRED" => (
                "The restoration version expired.",
                false,
                "Choose an available version.",
            ),
            "ACTIVE_NOTE_EXISTS" => (
                "An active note already uses this identifier.",
                false,
                "Restore only an absent or deleted note.",
            ),
            "BUSY" => (
                "Four requests are already running on this connection.",
                true,
                "Retry after a running request finishes.",
            ),
            "TIMEOUT" => (
                "The request deadline expired before commit.",
                true,
                "Retry using the same operation_id and arguments.",
            ),
            "MAINTENANCE" => (
                "Lexena is shutting down or updating.",
                true,
                "Reconnect when Lexena is ready.",
            ),
            "RECOVERY_REQUIRED" => (
                "The profile requires storage recovery.",
                true,
                "Allow Lexena to recover the profile before retrying.",
            ),
            _ => (
                "The operation could not be completed.",
                false,
                "Check Lexena and retry with valid arguments.",
            ),
        };
        Self {
            code: code.into(),
            message: message.into(),
            retryable,
            next_action: next_action.into(),
        }
    }
    pub fn result(&self) -> CallToolResult {
        CallToolResult::structured_error(serde_json::to_value(self).expect("error serialization"))
    }
}
impl From<DomainError> for BusinessError {
    fn from(error: DomainError) -> Self {
        let code = match error {
            DomainError::NotFound(_)
            | DomainError::InvalidScope(_)
            | DomainError::ProfileDeleted => "NOT_FOUND",
            DomainError::RevisionConflict => "REVISION_CONFLICT",
            DomainError::OperationConflict => "IDEMPOTENCY_CONFLICT",
            DomainError::EditorBusy | DomainError::EditorSessionExpired => "EDITOR_BUSY",
            DomainError::HistoryFull => "HISTORY_FULL",
            DomainError::SnapshotExpired => "SNAPSHOT_EXPIRED",
            DomainError::ActiveNoteExists => "ACTIVE_NOTE_EXISTS",
            DomainError::RecoveryRequired | DomainError::FormatUnsupported => "RECOVERY_REQUIRED",
            DomainError::InvalidInput(ref s)
                if matches!(
                    s.as_str(),
                    "UNSUPPORTED_CONTENT"
                        | "AMBIGUOUS_MATCH"
                        | "NOT_FOUND"
                        | "CONTENT_TOO_LARGE"
                        | "READ_TOKEN_INVALID"
                        | "READ_TOKEN_REQUIRED"
                ) =>
            {
                s.as_str()
            }
            _ => "INVALID_REQUEST",
        };
        Self::new(code)
    }
}
pub fn invalid_params(code: &str) -> ErrorData {
    ErrorData::invalid_params(
        "Explicit valid tool arguments are required.",
        Some(
            json!({"code":code,"next_action":if code=="PROFILE_REQUIRED" {"Ask which authorized profile to use; pass profile_id explicitly."} else {"Use the tool input schema."}}),
        ),
    )
}
