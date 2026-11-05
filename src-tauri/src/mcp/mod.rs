pub mod audit;
pub mod connections;
pub mod contract;
pub mod error;
#[cfg(windows)]
pub mod lifecycle;
#[cfg(windows)]
pub mod native;
pub mod permissions;
#[cfg(windows)]
pub mod processes;
#[cfg(windows)]
pub mod read_cache;
#[cfg(windows)]
pub mod runtime;
#[cfg(windows)]
pub mod server;
#[cfg(windows)]
pub mod tools;
#[cfg(windows)]
pub use runtime::{LocalMcpRuntime, SessionHandler};
#[cfg(all(test, windows))]
#[path = "tests.rs"]
mod local_mcp;
