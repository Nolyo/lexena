# Windows IPC verification

Automated tests create real named pipes under the running user's token, prove
first-instance exclusion, query the server PID and image, authenticate through
Credential Manager, exercise eight simultaneous sessions, reject the ninth,
close sessions on revocation, and release the listener on shutdown. The bridge
subprocess test checks closed-application diagnostics and clean stdout.

The following acceptance cases require separately provisioned Windows identities
or computers. They have **not** been executed by the unit suite. Do not describe
them as passing based only on ACL construction or a remote-client flag.

## `other_os_user_denied`: another OS user

1. On a disposable Windows test machine, provision two ordinary users A and B.
   Run the signed/package-matched Lexena as A, enable MCP, create a connection,
   and record the public pipe name from A's `mcp/endpoint.json`.
2. In a shell running as B, try to read A's `mcp/endpoint.json` and
   `mcp/connections.json`. Both must return access denied.
3. Independently attempt the known pipe name as B, without needing the file:

   ```powershell
   $pipe = [System.IO.Pipes.NamedPipeClientStream]::new(
     '.', 'lexena-mcp-<installation-uuid>', [System.IO.Pipes.PipeDirection]::InOut)
   $pipe.Connect(2000)
   ```

   Expect `UnauthorizedAccessException`/Win32 access denied. A timeout or missing
   pipe is inconclusive: first prove A can connect to the same running endpoint.
4. As A, verify existing MCP clients still work. No note request from B may
   appear in the activity journal.

## `remote_pipe_client_denied`: remote pipe client

1. Use two Windows machines with controlled SMB connectivity. Prefer the same
   domain user/SID on both machines, so the user-only ACL would otherwise allow
   access. Do not weaken the production ACL or firewall for this test.
2. Run Lexena and establish a successful local connection on the server machine.
3. From the second machine, connect using the known server host and pipe name:

   ```powershell
   $pipe = [System.IO.Pipes.NamedPipeClientStream]::new(
     '<server-host>', 'lexena-mcp-<installation-uuid>',
     [System.IO.Pipes.PipeDirection]::InOut)
   $pipe.Connect(2000)
   ```

   Expect access denied. First verify that the environment can reach a separately
   configured SMB test resource under that identity; network/firewall failures
   alone do not establish `PIPE_REJECT_REMOTE_CLIENTS` behavior.
4. Recheck the local connection. Never accept a successful remote connection,
   even if its HMAC would subsequently fail.

## Installed/portable contention and package behavior

Use a disposable profile: run installed Lexena, then attempt portable Lexena
under the same account. The application single-instance guard or MCP installation
diagnostic must refuse independent activation; the bridge from the other folder
must refuse the installed endpoint. After shutdown, moving an installation must
require explicit local transfer/new client configuration. No pipe scanning,
alternate descriptor, network fallback, or on-demand server startup is allowed.

Package acceptance (tray behavior, updater maintenance, sidecar placement, clean
Windows PC without development tools) is performed against the complete Tauri
bundle, not against these standalone test executables.
