#[path = "../../src/note_service/content.rs"]
pub mod content;
#[path = "../../src/note_service/editors.rs"]
pub mod editors;
#[path = "../../src/note_service/journal.rs"]
pub mod journal;
#[path = "../../src/note_service/mutation.rs"]
pub mod mutation;
#[path = "../../src/note_service/outbox.rs"]
pub mod outbox;
#[path = "../../src/note_service/repository.rs"]
pub mod repository;
#[path = "../../src/note_service/scope.rs"]
pub mod scope;
#[path = "../../src/note_service/versions.rs"]
pub mod versions;
pub use mutation::{Actor, MutationIntent, MutationResult, NoteService};
pub use repository::{FolderRepository, NoteRepository};
pub use scope::{DomainError, LockedProfile, ProfileRegistry, ProfileScope};
