// Local IPC transport. The authenticated byte stream is handed to the MCP SDK.
use hmac::{Hmac, KeyInit, Mac};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::Sha256;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};
use subtle::ConstantTimeEq;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use uuid::Uuid;
use zeroize::Zeroizing;

pub const MAX_MESSAGE_BYTES: usize = 8_388_608;
pub const MAX_CONNECTIONS: usize = 8;
pub const IPC_VERSION: u32 = 1;
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Local MCP authorization denied")]
    Denied,
    #[error("Invalid or oversized local MCP frame")]
    Frame,
    #[error("Local MCP connection or handshake timed out")]
    Timeout,
    #[error("Local MCP credential vault unavailable")]
    Vault,
    #[error("Another Lexena installation is active; use its matching bridge or transfer the installation in Lexena")]
    Installation,
    #[error("Lexena MCP application/bridge versions or portable package are incompatible; reinstall both executables together")]
    Compatibility,
    #[error("Local MCP I/O failed: {0}")]
    Io(#[from] std::io::Error),
}
pub type Result<T> = std::result::Result<T, Error>;

// Intentionally neither Debug nor Serialize. Never expose this through Tauri.
pub struct Secret(Zeroizing<[u8; 32]>);
impl Secret {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(Zeroizing::new(bytes))
    }
    pub fn generate() -> Result<Self> {
        let mut bytes = [0; 32];
        getrandom::fill(&mut bytes).map_err(|_| Error::Vault)?;
        Ok(Self::from_bytes(bytes))
    }
}

fn credential(installation: Uuid, connection: Uuid) -> Result<keyring::Entry> {
    keyring::Entry::new(
        "lexena-local-mcp-v1",
        &format!("{installation}/{connection}"),
    )
    .map_err(|_| Error::Vault)
}
// The Windows vault does not promise cross-thread operation ordering. Keep a
// write plus its verification atomic within this process, and serialize reads
// and deletions with that pair. Never retry a failed credential operation.
static VAULT_IO: Mutex<()> = Mutex::new(());
pub fn save_secret(installation: Uuid, connection: Uuid, secret: &Secret) -> Result<()> {
    let _vault = VAULT_IO.lock().map_err(|_| Error::Vault)?;
    let entry = credential(installation, connection)?;
    entry
        .set_secret(secret.0.as_ref())
        .map_err(|_| Error::Vault)?;
    // Detect unavailable or non-persistent vaults instead of accepting a write only.
    let stored = read_secret_unlocked(installation, connection)?;
    if !bool::from(secret.0.as_ref().ct_eq(stored.0.as_ref())) {
        return Err(Error::Vault);
    }
    Ok(())
}
pub fn read_secret(installation: Uuid, connection: Uuid) -> Result<Secret> {
    let _vault = VAULT_IO.lock().map_err(|_| Error::Vault)?;
    read_secret_unlocked(installation, connection)
}
fn read_secret_unlocked(installation: Uuid, connection: Uuid) -> Result<Secret> {
    let bytes = Zeroizing::new(
        credential(installation, connection)?
            .get_secret()
            .map_err(|_| Error::Vault)?,
    );
    let value: [u8; 32] = bytes.as_slice().try_into().map_err(|_| Error::Vault)?;
    Ok(Secret::from_bytes(value))
}
pub fn delete_secret(installation: Uuid, connection: Uuid) -> Result<()> {
    let _vault = VAULT_IO.lock().map_err(|_| Error::Vault)?;
    match credential(installation, connection)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(_) => Err(Error::Vault),
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClientHello {
    version: u32,
    role: String,
    installation_id: Uuid,
    connection_id: Uuid,
    nonce: [u8; 32],
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ServerHello {
    version: u32,
    role: String,
    nonce: [u8; 32],
    proof: [u8; 32],
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClientProof {
    role: String,
    proof: [u8; 32],
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Accepted {
    accepted: bool,
}

async fn write_preface<S: AsyncWrite + Unpin, T: Serialize>(
    stream: &mut S,
    value: &T,
) -> Result<()> {
    let bytes = serde_json::to_vec(value).map_err(|_| Error::Frame)?;
    if bytes.len() > 4096 {
        return Err(Error::Frame);
    }
    stream.write_u32_le(bytes.len() as u32).await?;
    stream.write_all(&bytes).await?;
    stream.flush().await?;
    Ok(())
}
async fn read_preface<S: AsyncRead + Unpin, T: DeserializeOwned>(stream: &mut S) -> Result<T> {
    let size = stream.read_u32_le().await? as usize;
    if size == 0 || size > 4096 {
        return Err(Error::Frame);
    }
    let mut bytes = vec![0; size];
    stream.read_exact(&mut bytes).await?;
    serde_json::from_slice(&bytes).map_err(|_| Error::Frame)
}
fn proof(secret: &Secret, role: &[u8], hello: &ClientHello, server_nonce: &[u8; 32]) -> [u8; 32] {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.0.as_ref()).expect("HMAC accepts 256-bit keys");
    mac.update(b"lexena-mcp-ipc-v1\0");
    mac.update(role);
    mac.update(hello.installation_id.as_bytes());
    mac.update(hello.connection_id.as_bytes());
    mac.update(&hello.nonce);
    mac.update(server_nonce);
    mac.finalize().into_bytes().into()
}
pub struct IpcAuthenticator {
    pub installation_id: Uuid,
    nonces: Mutex<HashSet<(Uuid, [u8; 32])>>,
}
impl IpcAuthenticator {
    pub fn new(installation_id: Uuid) -> Self {
        Self {
            installation_id,
            nonces: Mutex::new(HashSet::new()),
        }
    }
    pub async fn accept<S, F>(&self, stream: &mut S, grant_store: F) -> Result<Uuid>
    where
        S: AsyncRead + AsyncWrite + Unpin,
        F: Fn(Uuid) -> Result<Secret>,
    {
        tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
            let hello: ClientHello = read_preface(stream).await?;
            if hello.version != IPC_VERSION
                || hello.role != "client"
                || hello.installation_id != self.installation_id
            {
                return Err(Error::Denied);
            }
            let secret = grant_store(hello.connection_id)?;
            // Bounded cache fails closed at capacity rather than making old nonces reusable.
            {
                let mut used = self.nonces.lock().map_err(|_| Error::Denied)?;
                if used.len() >= 65_536 || !used.insert((hello.connection_id, hello.nonce)) {
                    return Err(Error::Denied);
                }
            }
            let mut nonce = [0; 32];
            getrandom::fill(&mut nonce).map_err(|_| Error::Denied)?;
            write_preface(
                stream,
                &ServerHello {
                    version: 1,
                    role: "server".into(),
                    nonce,
                    proof: proof(&secret, b"server\0", &hello, &nonce),
                },
            )
            .await?;
            let client: ClientProof = read_preface(stream).await?;
            if client.role != "client"
                || !bool::from(
                    client
                        .proof
                        .ct_eq(&proof(&secret, b"client\0", &hello, &nonce)),
                )
            {
                return Err(Error::Denied);
            }
            // Recheck after handshake so revocation/rotation during authentication wins.
            let current = grant_store(hello.connection_id)?;
            if !bool::from(secret.0.as_ref().ct_eq(current.0.as_ref())) {
                return Err(Error::Denied);
            }
            write_preface(stream, &Accepted { accepted: true }).await?;
            Ok(hello.connection_id)
        })
        .await
        .map_err(|_| Error::Timeout)?
    }
}
pub async fn authenticate_client<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    installation: Uuid,
    connection: Uuid,
    secret: &Secret,
) -> Result<()> {
    tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
        let mut nonce = [0; 32];
        getrandom::fill(&mut nonce).map_err(|_| Error::Denied)?;
        let hello = ClientHello {
            version: 1,
            role: "client".into(),
            installation_id: installation,
            connection_id: connection,
            nonce,
        };
        write_preface(stream, &hello).await?;
        let server: ServerHello = read_preface(stream).await?;
        if server.version != 1
            || server.role != "server"
            || !bool::from(
                server
                    .proof
                    .ct_eq(&proof(secret, b"server\0", &hello, &server.nonce)),
            )
        {
            return Err(Error::Denied);
        }
        write_preface(
            stream,
            &ClientProof {
                role: "client".into(),
                proof: proof(secret, b"client\0", &hello, &server.nonce),
            },
        )
        .await?;
        let ack: Accepted = read_preface(stream).await?;
        if !ack.accepted {
            return Err(Error::Denied);
        }
        Ok(())
    })
    .await
    .map_err(|_| Error::Timeout)?
}

/// Preserve complete standard MCP lines byte-for-byte, bounded before forwarding.
/// EOF in a message is an error: never retry a possibly interrupted mutation.
pub async fn relay_lines<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    reader: &mut R,
    writer: &mut W,
    max: usize,
) -> Result<()> {
    use tokio::io::AsyncBufReadExt;
    let mut reader = tokio::io::BufReader::with_capacity(8192, reader);
    let mut line = Vec::new();
    loop {
        let bytes = reader.fill_buf().await?;
        if bytes.is_empty() {
            if !line.is_empty() {
                return Err(Error::Frame);
            }
            writer.shutdown().await?;
            return Ok(());
        }
        let newline = bytes.iter().position(|b| *b == b'\n');
        let count = newline.map_or(bytes.len(), |pos| pos + 1);
        let payload = count - usize::from(newline.is_some());
        if line.len().saturating_add(payload) > max {
            return Err(Error::Frame);
        }
        line.extend_from_slice(&bytes[..count]);
        reader.consume(count);
        if newline.is_some() {
            std::str::from_utf8(&line).map_err(|_| Error::Frame)?;
            writer.write_all(&line).await?;
            writer.flush().await?;
            line.clear();
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndpointDescriptor {
    pub version: u32,
    pub app_version: String,
    pub installation_id: Uuid,
    pub pipe_name: String,
    pub server_path: PathBuf,
}
impl EndpointDescriptor {
    fn validate_shape(&self) -> Result<()> {
        if self.version != IPC_VERSION
            || self.app_version.is_empty()
            || self.app_version.len() > 128
            || self.pipe_name != format!(r"\\.\pipe\lexena-mcp-{}", self.installation_id)
            || !self.server_path.is_absolute()
        {
            return Err(Error::Installation);
        }
        Ok(())
    }
    pub fn new(installation_id: Uuid, server_path: PathBuf, app_version: &str) -> Self {
        Self {
            version: 1,
            app_version: app_version.to_owned(),
            installation_id,
            pipe_name: format!(r"\\.\pipe\lexena-mcp-{installation_id}"),
            server_path,
        }
    }
    pub fn validate_bridge_version(&self, bridge_version: &str) -> Result<()> {
        self.validate_shape()?;
        if self.app_version != bridge_version {
            return Err(Error::Compatibility);
        }
        Ok(())
    }
    pub fn validate_for_bridge(&self, bridge: &Path) -> Result<()> {
        self.validate_shape()?;
        if self.version != 1
            || self.pipe_name != format!(r"\\.\pipe\lexena-mcp-{}", self.installation_id)
        {
            return Err(Error::Installation);
        }
        let bridge = bridge.canonicalize()?;
        let expected = bridge
            .parent()
            .ok_or(Error::Installation)?
            .join("lexena.exe")
            .canonicalize()?;
        if self.server_path.canonicalize()? != expected {
            return Err(Error::Installation);
        }
        Ok(())
    }
    pub fn read(path: &Path) -> Result<Self> {
        use std::io::Read;
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take(4097)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 4096 {
            return Err(Error::Frame);
        }
        let descriptor: Self = serde_json::from_slice(&bytes).map_err(|_| Error::Frame)?;
        descriptor.validate_shape()?;
        Ok(descriptor)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PortableManifest {
    schema_version: u32,
    app_version: String,
    bridge_version: String,
    ipc_version: u32,
    target: String,
    sha256: PortableHashes,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PortableHashes {
    #[serde(rename = "lexena.exe")]
    app: String,
    #[serde(rename = "lexena-mcp.exe")]
    bridge: String,
}
/// NSIS has no manifest. A present portable manifest must validate completely.
pub fn validate_portable_package(bridge: &Path, bridge_version: &str) -> Result<()> {
    use sha2::Digest;
    use std::io::Read;
    let directory = bridge.parent().ok_or(Error::Compatibility)?;
    let file = match std::fs::File::open(directory.join("mcp-version.json")) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(Error::Compatibility),
    };
    let mut bytes = Vec::new();
    file.take(4097)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Compatibility)?;
    if bytes.len() > 4096 {
        return Err(Error::Compatibility);
    }
    let manifest: PortableManifest =
        serde_json::from_slice(&bytes).map_err(|_| Error::Compatibility)?;
    if manifest.schema_version != 1
        || manifest.ipc_version != IPC_VERSION
        || manifest.target != "x86_64-pc-windows-msvc"
        || manifest.app_version != bridge_version
        || manifest.bridge_version != bridge_version
    {
        return Err(Error::Compatibility);
    }
    for (path, expected) in [
        (directory.join("lexena.exe"), &manifest.sha256.app),
        (bridge.to_path_buf(), &manifest.sha256.bridge),
    ] {
        let mut file = std::fs::File::open(path).map_err(|_| Error::Compatibility)?;
        let mut hash = Sha256::new();
        let mut buffer = [0; 65536];
        loop {
            let read = file.read(&mut buffer).map_err(|_| Error::Compatibility)?;
            if read == 0 {
                break;
            }
            hash.update(&buffer[..read]);
        }
        let actual = hash
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        if &actual != expected {
            return Err(Error::Compatibility);
        }
    }
    Ok(())
}
pub fn write_private_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    use std::io::Write;
    let parent = path.parent().ok_or(Error::Installation)?;
    std::fs::create_dir_all(parent)?;
    protect_path(parent)?;
    let bytes = serde_json::to_vec(value).map_err(|_| Error::Frame)?;
    atomicwrites::AtomicFile::new(path, atomicwrites::AllowOverwrite)
        .write(|f| {
            f.write_all(&bytes)?;
            f.sync_all()
        })
        .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))?;
    protect_path(path)?;
    Ok(())
}
pub fn standard_app_data() -> Result<PathBuf> {
    // Matches Tauri app_data_dir for com.nolyo.lexena on Windows.
    #[cfg(windows)]
    {
        Ok(
            PathBuf::from(std::env::var_os("APPDATA").ok_or(Error::Installation)?)
                .join("com.nolyo.lexena"),
        )
    }
    #[cfg(not(windows))]
    {
        Err(Error::Installation)
    }
}

#[cfg(windows)]
pub mod windows;
#[cfg(windows)]
pub use windows::protect_path;
#[cfg(not(windows))]
pub fn protect_path(_path: &Path) -> Result<()> {
    Err(Error::Installation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[test]
    fn endpoint_requires_exact_application_version_after_update() {
        let mut descriptor = EndpointDescriptor::new(
            Uuid::new_v4(),
            std::env::current_exe().unwrap(),
            "3.2.2-beta.1",
        );
        descriptor.app_version = "3.2.2-beta.1".into();
        assert!(descriptor.validate_bridge_version("3.2.2-beta.1").is_ok());
        assert!(descriptor.validate_bridge_version("3.2.1").is_err());
        let mut json = serde_json::to_value(&descriptor).unwrap();
        json.as_object_mut().unwrap().remove("app_version");
        assert!(serde_json::from_value::<EndpointDescriptor>(json).is_err());
    }
    #[test]
    fn optional_portable_manifest_rejects_mixed_or_tampered_executables() {
        let dir = tempfile::tempdir().unwrap();
        let bridge = dir.path().join("lexena-mcp.exe");
        std::fs::write(&bridge, "bridge").unwrap();
        std::fs::write(dir.path().join("lexena.exe"), "main").unwrap();
        assert!(validate_portable_package(&bridge, "3.2.2-beta.1").is_ok());
        let manifest = serde_json::json!({
            "schema_version":1,"app_version":"3.2.2-beta.1","bridge_version":"3.2.2-beta.1",
            "ipc_version":1,"target":"x86_64-pc-windows-msvc",
            "sha256":{"lexena.exe":"0d6e4079e36703ebd37c00722f5891d28b0e2811dc114b129215123adcce3605",
                "lexena-mcp.exe":"17f29b073143d8cd97b5bbe492bdeffec1c5fee55cc1fe2112c8b9335f8b6121"}
        });
        let path = dir.path().join("mcp-version.json");
        std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert!(validate_portable_package(&bridge, "3.2.2-beta.1").is_ok());
        assert!(validate_portable_package(&bridge, "3.2.1").is_err());
        for (field, value) in [
            ("schema_version", serde_json::json!(2)),
            ("ipc_version", serde_json::json!(2)),
            ("target", serde_json::json!("aarch64-pc-windows-msvc")),
            ("bridge_version", serde_json::json!("3.2.1")),
            ("unexpected", serde_json::json!(true)),
        ] {
            let mut changed = manifest.clone();
            changed[field] = value;
            std::fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
            assert!(validate_portable_package(&bridge, "3.2.2-beta.1").is_err());
        }
        std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        std::fs::write(&bridge, "tampered").unwrap();
        assert!(validate_portable_package(&bridge, "3.2.2-beta.1").is_err());
        std::fs::write(&path, b"{}").unwrap();
        assert!(validate_portable_package(&bridge, "3.2.2-beta.1").is_err());
    }
    #[test]
    fn concurrent_secret_replacements_verify_their_own_write_before_another_writer() {
        let installation = Uuid::new_v4();
        let connection = Uuid::new_v4();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
        let mut threads = Vec::new();
        for index in 1..=4 {
            let barrier = barrier.clone();
            threads.push(std::thread::spawn(move || {
                let mut errors = Vec::new();
                for _ in 0..8 {
                    barrier.wait();
                    if let Err(error) =
                        save_secret(installation, connection, &Secret::from_bytes([index; 32]))
                    {
                        errors.push(error.to_string());
                    }
                }
                errors
            }));
        }
        let errors = threads
            .into_iter()
            .flat_map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>();
        delete_secret(installation, connection).unwrap();
        assert!(
            errors.is_empty(),
            "concurrent vault write/read-back failed: {errors:?}"
        );
    }

    #[tokio::test]
    async fn authenticates_then_preserves_utf8_bytes() {
        let auth = IpcAuthenticator::new(uuid::Uuid::new_v4());
        let id = uuid::Uuid::new_v4();
        let installation = auth.installation_id;
        let (mut client, mut server) = tokio::io::duplex(256);
        let task = tokio::spawn(async move {
            let actor = auth
                .accept(&mut server, |candidate| {
                    if candidate == id {
                        Ok(Secret::from_bytes([7; 32]))
                    } else {
                        Err(Error::Denied)
                    }
                })
                .await
                .unwrap();
            assert_eq!(actor, id);
            let mut payload = [0u8; 9];
            server.read_exact(&mut payload).await.unwrap();
            payload
        });
        authenticate_client(&mut client, installation, id, &Secret::from_bytes([7; 32]))
            .await
            .unwrap();
        client.write_all("été💡".as_bytes()).await.unwrap();
        assert_eq!(&task.await.unwrap(), "été💡".as_bytes());
    }

    #[tokio::test]
    async fn reject_wrong_secret() {
        let auth = IpcAuthenticator::new(uuid::Uuid::new_v4());
        let installation = auth.installation_id;
        let (mut client, mut server) = tokio::io::duplex(4096);
        let task = tokio::spawn(async move {
            auth.accept(&mut server, |_| Ok(Secret::from_bytes([1; 32])))
                .await
        });
        assert!(authenticate_client(
            &mut client,
            installation,
            uuid::Uuid::new_v4(),
            &Secret::from_bytes([2; 32])
        )
        .await
        .is_err());
        drop(client);
        assert!(task.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn reject_replayed_nonce() {
        let auth = IpcAuthenticator::new(uuid::Uuid::new_v4());
        let hello = ClientHello {
            version: 1,
            role: "client".into(),
            installation_id: auth.installation_id,
            connection_id: uuid::Uuid::new_v4(),
            nonce: [4; 32],
        };
        for attempt in 0..2 {
            let (mut client, mut server) = tokio::io::duplex(4096);
            write_preface(&mut client, &hello).await.unwrap();
            let result = tokio::time::timeout(
                std::time::Duration::from_millis(30),
                auth.accept(&mut server, |_| Ok(Secret::from_bytes([1; 32]))),
            )
            .await;
            if attempt == 0 {
                assert!(result.is_err());
            } else {
                assert!(matches!(result, Ok(Err(Error::Denied))));
            }
        }
    }

    #[tokio::test]
    async fn reject_wrong_role_or_version_and_unknown_fields() {
        for json in [
            r#"{"version":2,"role":"client","installation_id":"00000000-0000-0000-0000-000000000000","connection_id":"00000000-0000-0000-0000-000000000000","nonce":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]}"#,
            r#"{"extra":true}"#,
        ] {
            let auth = IpcAuthenticator::new(uuid::Uuid::nil());
            let (mut client, mut server) = tokio::io::duplex(4096);
            client.write_u32_le(json.len() as u32).await.unwrap();
            client.write_all(json.as_bytes()).await.unwrap();
            assert!(auth
                .accept(&mut server, |_| Ok(Secret::from_bytes([1; 32])))
                .await
                .is_err());
        }
    }

    #[tokio::test]
    async fn oversized_preface_rejected_before_allocation() {
        let (mut a, mut b) = tokio::io::duplex(8);
        a.write_u32_le(4097).await.unwrap();
        assert!(read_preface::<_, ClientHello>(&mut b).await.is_err());
    }
    #[tokio::test]
    async fn wrong_client_role_is_denied_even_with_a_valid_secret() {
        let auth = IpcAuthenticator::new(Uuid::new_v4());
        let hello = ClientHello {
            version: 1,
            role: "server".into(),
            installation_id: auth.installation_id,
            connection_id: Uuid::new_v4(),
            nonce: [8; 32],
        };
        let (mut client, mut server) = tokio::io::duplex(4096);
        write_preface(&mut client, &hello).await.unwrap();
        assert!(matches!(
            auth.accept(&mut server, |_| Ok(Secret::from_bytes([1; 32])))
                .await,
            Err(Error::Denied)
        ));
    }

    #[tokio::test]
    async fn relay_enforces_bound_and_eof_without_reformatting() {
        let input = b"{\"v\":\"hello\"}\n{\"x\":1}\n";
        let mut output = Vec::new();
        relay_lines(&mut &input[..], &mut output, 32).await.unwrap();
        assert_eq!(output, input);
        assert!(relay_lines(&mut &b"12345\n"[..], &mut Vec::new(), 4)
            .await
            .is_err());
        assert!(relay_lines(&mut &b"partial"[..], &mut Vec::new(), 32)
            .await
            .is_err());
    }

    #[test]
    fn descriptor_reject_spoofed_server_or_other_installation() {
        let dir = tempfile::tempdir().unwrap();
        let server = dir.path().join("lexena.exe");
        let bridge = dir.path().join("lexena-mcp.exe");
        std::fs::write(&server, []).unwrap();
        std::fs::write(&bridge, []).unwrap();
        let mut descriptor =
            EndpointDescriptor::new(uuid::Uuid::new_v4(), server.canonicalize().unwrap(), "test");
        descriptor.validate_for_bridge(&bridge).unwrap();
        descriptor.server_path = std::env::current_exe().unwrap();
        assert!(descriptor.validate_for_bridge(&bridge).is_err());
    }

    #[tokio::test]
    async fn reject_tampered_proof_and_role_reflection() {
        for reflected in [false, true] {
            let auth = IpcAuthenticator::new(Uuid::new_v4());
            let installation = auth.installation_id;
            let (mut client, mut server) = tokio::io::duplex(4096);
            let task = tokio::spawn(async move {
                auth.accept(&mut server, |_| Ok(Secret::from_bytes([5; 32])))
                    .await
            });
            let hello = ClientHello {
                version: 1,
                role: "client".into(),
                installation_id: installation,
                connection_id: Uuid::new_v4(),
                nonce: [9; 32],
            };
            write_preface(&mut client, &hello).await.unwrap();
            let server: ServerHello = read_preface(&mut client).await.unwrap();
            let mut value = if reflected {
                server.proof
            } else {
                proof(
                    &Secret::from_bytes([5; 32]),
                    b"client\0",
                    &hello,
                    &server.nonce,
                )
            };
            if !reflected {
                value[0] ^= 1;
            }
            write_preface(
                &mut client,
                &ClientProof {
                    role: "client".into(),
                    proof: value,
                },
            )
            .await
            .unwrap();
            assert!(matches!(task.await.unwrap(), Err(Error::Denied)));
        }
    }

    #[tokio::test]
    async fn full_handshake_deadline_and_max_frame_backpressure() {
        let auth = IpcAuthenticator::new(Uuid::new_v4());
        let (_client, mut server) = tokio::io::duplex(8);
        assert!(matches!(
            auth.accept(&mut server, |_| Ok(Secret::from_bytes([1; 32])))
                .await,
            Err(Error::Timeout)
        ));
        let mut exact = vec![b'x'; MAX_MESSAGE_BYTES];
        exact.push(b'\n');
        let mut output = Vec::new();
        relay_lines(&mut &exact[..], &mut output, MAX_MESSAGE_BYTES)
            .await
            .unwrap();
        assert_eq!(output.len(), MAX_MESSAGE_BYTES + 1);
        exact.insert(0, b'x');
        assert!(
            relay_lines(&mut &exact[..], &mut Vec::new(), MAX_MESSAGE_BYTES)
                .await
                .is_err()
        );
        let (mut writer, _blocked_reader) = tokio::io::duplex(4);
        assert!(tokio::time::timeout(
            Duration::from_millis(20),
            relay_lines(&mut &b"12345\n"[..], &mut writer, 32)
        )
        .await
        .is_err());
    }
}
