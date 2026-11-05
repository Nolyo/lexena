// Allows the same authenticated stdio/EOF tests to exercise the staged release
// binary (including its Windows GUI subsystem), without another production build.
fn bridge_executable() -> std::path::PathBuf {
    std::env::var_os("LEXENA_MCP_TEST_BRIDGE_EXE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_lexena-mcp").into())
}

#[test]
fn closed_lexena_and_invalid_cli_exit_without_stdout() {
    let temporary = tempfile::tempdir().unwrap();
    for args in [
        vec!["--connection", "00000000-0000-0000-0000-000000000001"],
        vec!["--secret", "not-accepted"],
    ] {
        let output = std::process::Command::new(bridge_executable())
            .args(args)
            .env("APPDATA", temporary.path())
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(error.contains("Lexena MCP"));
        assert!(!error.contains("not-accepted"));
    }
}

#[cfg(windows)]
#[test]
fn mixed_update_bridge_exits_cleanly_before_connection() {
    let temporary = tempfile::Builder::new()
        .prefix("Lexena été 研究 ")
        .tempdir()
        .unwrap();
    let server = temporary.path().join("lexena.exe");
    let bridge = temporary.path().join("lexena-mcp.exe");
    std::fs::write(&server, b"synthetic server").unwrap();
    std::fs::copy(bridge_executable(), &bridge).unwrap();
    let app_data = temporary.path().join("com.nolyo.lexena/mcp");
    std::fs::create_dir_all(&app_data).unwrap();
    let descriptor = lexena_mcp_ipc::EndpointDescriptor::new(
        uuid::Uuid::new_v4(),
        server.canonicalize().unwrap(),
        "0.0.0-incompatible",
    );
    std::fs::write(
        app_data.join("endpoint.json"),
        serde_json::to_vec(&descriptor).unwrap(),
    )
    .unwrap();
    let output = std::process::Command::new(&bridge)
        .args(["--connection", "00000000-0000-0000-0000-000000000001"])
        .env("APPDATA", temporary.path())
        .current_dir(std::env::temp_dir())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let diagnostic = String::from_utf8(output.stderr).unwrap();
    assert!(diagnostic.contains("incompatible"));
    assert!(!diagnostic.contains("0.0.0-incompatible"));
    assert!(!diagnostic.contains("synthetic server"));
}

/// Runs only as a child copied to the expected sibling lexena.exe path. The
/// parent test invokes this ignored fixture explicitly and checks its exit.
#[cfg(windows)]
#[test]
#[ignore = "subprocess fixture invoked by authenticated_server_eof_exits_with_stdin_open"]
fn pipe_server_fixture() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        let installation =
            uuid::Uuid::parse_str(&std::env::var("LEXENA_MCP_TEST_INSTALLATION").unwrap()).unwrap();
        let connection =
            uuid::Uuid::parse_str(&std::env::var("LEXENA_MCP_TEST_CONNECTION").unwrap()).unwrap();
        let descriptor = lexena_mcp_ipc::EndpointDescriptor::new(
            installation,
            std::env::current_exe().unwrap().canonicalize().unwrap(),
            env!("CARGO_PKG_VERSION"),
        );
        let mut pipe = lexena_mcp_ipc::windows::create_pipe(&descriptor.pipe_name, true).unwrap();
        lexena_mcp_ipc::write_private_json(
            &lexena_mcp_ipc::standard_app_data()
                .unwrap()
                .join("mcp/endpoint.json"),
            &descriptor,
        )
        .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), pipe.connect())
            .await
            .unwrap()
            .unwrap();
        let auth = lexena_mcp_ipc::IpcAuthenticator::new(installation);
        auth.accept(&mut pipe, |id| {
            if id != connection {
                return Err(lexena_mcp_ipc::Error::Denied);
            }
            lexena_mcp_ipc::read_secret(installation, id)
        })
        .await
        .unwrap();
        let mut reader = tokio::io::BufReader::new(pipe);
        let mut request = String::new();
        reader.read_line(&mut request).await.unwrap();
        assert_eq!(request, "{\"probe\":\"été 💡\"}\n");
        let mut pipe = reader.into_inner();
        pipe.write_all(b"{\"fixture\":\"closed\"}\n").await.unwrap();
        pipe.flush().await.unwrap();
        // Client remains idle with stdin open while the server closes its pipe.
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        drop(pipe);
    });
}

#[cfg(windows)]
#[tokio::test]
async fn authenticated_server_eof_exits_with_stdin_open() {
    use std::{process::Stdio, time::Duration};
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
    let temporary = tempfile::tempdir().unwrap();
    let server_path = temporary.path().join("lexena.exe");
    let bridge_path = temporary.path().join("lexena-mcp.exe");
    std::fs::copy(std::env::current_exe().unwrap(), &server_path).unwrap();
    std::fs::copy(bridge_executable(), &bridge_path).unwrap();
    let installation = uuid::Uuid::new_v4();
    let connection = uuid::Uuid::new_v4();
    struct Cleanup(uuid::Uuid, uuid::Uuid);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = lexena_mcp_ipc::delete_secret(self.0, self.1);
        }
    }
    let _cleanup = Cleanup(installation, connection);
    lexena_mcp_ipc::save_secret(
        installation,
        connection,
        &lexena_mcp_ipc::Secret::generate().unwrap(),
    )
    .unwrap();
    let mut server = tokio::process::Command::new(&server_path)
        .args(["--exact", "pipe_server_fixture", "--ignored", "--nocapture"])
        .env("APPDATA", temporary.path())
        .env("LEXENA_MCP_TEST_INSTALLATION", installation.to_string())
        .env("LEXENA_MCP_TEST_CONNECTION", connection.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let endpoint = temporary.path().join("com.nolyo.lexena/mcp/endpoint.json");
    tokio::time::timeout(Duration::from_secs(10), async {
        while !endpoint.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let mut bridge = tokio::process::Command::new(&bridge_path)
        .args(["--connection", &connection.to_string()])
        .env("APPDATA", temporary.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut input = bridge.stdin.take().unwrap();
    let mut output = tokio::io::BufReader::new(bridge.stdout.take().unwrap());
    input
        .write_all("{\"probe\":\"été 💡\"}\n".as_bytes())
        .await
        .unwrap();
    input.flush().await.unwrap();
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(5), output.read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(line, "{\"fixture\":\"closed\"}\n");
    let server_status = tokio::time::timeout(Duration::from_secs(5), server.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(server_status.success());
    // Hold `input` until both process-exit and stdout-EOF checks are complete.
    let exited = tokio::time::timeout(Duration::from_secs(2), bridge.wait()).await;
    let hung = exited.is_err();
    if hung {
        bridge.kill().await.unwrap();
        bridge.wait().await.unwrap();
    }
    let mut remaining = Vec::new();
    tokio::time::timeout(Duration::from_secs(2), output.read_to_end(&mut remaining))
        .await
        .unwrap()
        .unwrap();
    drop(input);
    assert!(
        !hung,
        "authenticated bridge hung after server EOF while stdin remained open"
    );
    assert!(exited.unwrap().unwrap().success());
    assert!(remaining.is_empty());
}
