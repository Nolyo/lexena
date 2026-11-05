#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
use lexena_mcp_ipc::{Error, Result};

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<uuid::Uuid> {
    let values: Vec<String> = args.into_iter().collect();
    if values.len() != 2 || values[0] != "--connection" {
        return Err(Error::Denied);
    }
    uuid::Uuid::parse_str(&values[1]).map_err(|_| Error::Denied)
}

fn main() -> std::process::ExitCode {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => {
            eprintln!("Lexena MCP: could not initialize local transport");
            return std::process::ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(run());
    // Tokio stdin uses a blocking OS read that cannot be cancelled. An ordinary
    // Runtime drop waits forever if the server closes while stdin stays open.
    // Frames already forwarded to stdout have been explicitly flushed; bound
    // shutdown so process exit releases the idle OS read without waiting for it.
    runtime.shutdown_timeout(std::time::Duration::from_millis(200));
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Lexena MCP: {error}. Open Lexena and enable this local connection in Settings → Integrations.");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<()> {
    let connection = parse_args(std::env::args().skip(1))?;
    #[cfg(windows)]
    {
        let descriptor = lexena_mcp_ipc::EndpointDescriptor::read(
            &lexena_mcp_ipc::standard_app_data()?.join("mcp/endpoint.json"),
        )?;
        let executable = std::env::current_exe()?;
        descriptor.validate_bridge_version(env!("CARGO_PKG_VERSION"))?;
        lexena_mcp_ipc::validate_portable_package(&executable, env!("CARGO_PKG_VERSION"))?;
        let mut pipe = lexena_mcp_ipc::windows::connect(&descriptor, &executable).await?;
        let secret = lexena_mcp_ipc::read_secret(descriptor.installation_id, connection)?;
        lexena_mcp_ipc::authenticate_client(
            &mut pipe,
            descriptor.installation_id,
            connection,
            &secret,
        )
        .await?;
        drop(secret);
        let (mut read, mut write) = tokio::io::split(pipe);
        let mut stdin = tokio::io::stdin();
        let mut stdout = tokio::io::stdout();
        tokio::select! {
            result=lexena_mcp_ipc::relay_lines(&mut stdin,&mut write,lexena_mcp_ipc::MAX_MESSAGE_BYTES)=>result,
            result=lexena_mcp_ipc::relay_lines(&mut read,&mut stdout,lexena_mcp_ipc::MAX_MESSAGE_BYTES)=>result,
        }
    }
    #[cfg(not(windows))]
    {
        let _ = connection;
        Err(Error::Installation)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_explicit_public_connection_argument_is_accepted() {
        assert!(super::parse_args([
            "--connection".into(),
            "00000000-0000-0000-0000-000000000001".into()
        ])
        .is_ok());
        assert!(super::parse_args(["--secret".into(), "secret".into()]).is_err());
        assert!(super::parse_args(Vec::<String>::new()).is_err());
        assert!(super::parse_args(["--connection".into(), "../../secret".into()]).is_err());
    }
}

#[cfg(test)]
#[allow(dead_code)]
#[path = "../../../src/mcp/audit.rs"]
pub mod audit;
#[cfg(test)]
#[allow(dead_code)]
#[path = "../../../src/mcp/connections.rs"]
pub mod connections;
#[cfg(test)]
#[allow(dead_code)]
#[path = "../../../src/mcp/permissions.rs"]
pub mod permissions;
#[cfg(test)]
#[allow(dead_code)]
#[path = "../../../src/mcp/runtime.rs"]
pub mod runtime;
#[cfg(test)]
mod mcp {
    pub use crate::connections;
}
