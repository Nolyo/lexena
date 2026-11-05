//! Actual production modules, independently testable without starting Tauri.
#![cfg(windows)]
#[path = "support/local_mcp_modules.rs"]
mod mcp;
#[path = "support/local_mcp_domain.rs"]
mod note_service;
