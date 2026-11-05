#[path = "../../src/mcp/audit.rs"]
pub mod audit;
#[path = "../../src/mcp/connections.rs"]
pub mod connections;
#[path = "../../src/mcp/contract.rs"]
pub mod contract;
#[path = "../../src/mcp/error.rs"]
pub mod error;
#[cfg(test)]
#[path = "../../src/mcp/tests.rs"]
mod local_mcp;
#[path = "../../src/mcp/permissions.rs"]
pub mod permissions;
#[path = "../../src/mcp/runtime.rs"]
pub mod runtime;
#[path = "../../src/mcp/server.rs"]
pub mod server;
#[path = "../../src/mcp/tools.rs"]
pub mod tools;
#[path = "../../src/mcp/read_cache.rs"]
pub mod read_cache;
