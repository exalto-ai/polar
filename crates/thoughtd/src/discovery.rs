//! How clients find the daemon (AD-10).

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Discovery and local sync protocol understood by this daemon build.
///
/// Version 2 adds sourced editor updates and separate MCP/editor capabilities.
pub const PROTOCOL_VERSION: u32 = 2;
pub const IDENTITY_PATH: &str = "/health/identity";
pub const MCP_HEALTH_PATH: &str = "/health/mcp";
pub const EDITOR_HEALTH_PATH: &str = "/health/editor";
const HEALTH_SERVICE: &str = "ai.exalto.thoughtd";
static NEXT_TEMPORARY_ID: AtomicU64 = AtomicU64::new(0);

/// Exact unauthenticated response used before sending either capability.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityResponse {
    service: String,
    protocol_version: u32,
}

impl IdentityResponse {
    pub fn current() -> Self {
        Self {
            service: HEALTH_SERVICE.into(),
            protocol_version: PROTOCOL_VERSION,
        }
    }
}

/// Exact authenticated response used to distinguish thoughtd from an unrelated
/// process that later acquired a stale daemon port.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthResponse {
    service: String,
    protocol_version: u32,
    capability: String,
}

impl HealthResponse {
    pub fn mcp() -> Self {
        Self::new("mcp")
    }

    pub fn editor() -> Self {
        Self::new("editor_sync")
    }

    fn new(capability: &str) -> Self {
        Self {
            service: HEALTH_SERVICE.into(),
            protocol_version: PROTOCOL_VERSION,
            capability: capability.into(),
        }
    }
}

/// `THOUGHT_HOME` overrides the location of the store and the discovery file.
/// Without it, a test run would publish itself as *the* daemon and overwrite
/// the real one's port and token.
/// Where the store, the discovery file and the logs live.
pub fn home() -> PathBuf {
    support_dir()
}

fn support_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("THOUGHT_HOME") {
        return PathBuf::from(dir);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    #[cfg(target_os = "macos")]
    return Path::new(&home).join("Library/Application Support/ai.exalto.thought");
    // Everywhere else, the XDG data directory. A macOS-shaped path under a
    // Linux `$HOME` would work and would still be the wrong place to look.
    #[cfg(not(target_os = "macos"))]
    return std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| Path::new(&home).join(".local/share"))
        .join("thought");
}

pub fn default_db_path() -> PathBuf {
    support_dir().join("thought.db")
}

pub fn discovery_path() -> PathBuf {
    support_dir().join("daemon.json")
}

/// 256 bits from the OS. Not a dependency worth taking for this.
///
/// `read_exact`, never `fs::read`: `/dev/urandom` has no EOF, so reading it to
/// the end hangs forever while consuming memory. The daemon did exactly that
/// before it could log a single line.
pub fn random_token() -> io::Result<String> {
    use std::io::Read;
    let mut buf = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut buf)?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

/// What trusted local clients need to reach a running daemon.
///
/// `token` is intentionally limited to MCP. `editor_token` authorizes the editor
/// protocol, whose source labels can create stronger provenance claims. This
/// capability split does not isolate hostile processes running as the same OS
/// user, since such a process may be able to read this private discovery file
/// or the app's own memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Daemon {
    pub url: String,
    pub protocol_version: u32,
    /// Public MCP capability, kept as `token` for existing MCP integrations.
    pub token: String,
    /// Private editor-sync capability. Never hand this to an MCP integration.
    pub editor_token: String,
}

fn health_url(daemon: &Daemon, path: &str) -> Option<String> {
    daemon
        .url
        .strip_suffix("/mcp")
        .map(|base| format!("{base}{path}"))
}

fn probe_identity(daemon: &Daemon) -> bool {
    let Some(url) = health_url(daemon, IDENTITY_PATH) else {
        return false;
    };
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(1)))
        .max_idle_connections(0)
        .build()
        .into();
    let Ok(mut response) = agent.get(&url).header("Accept", "application/json").call() else {
        return false;
    };
    if response.status().as_u16() != 200 {
        return false;
    }
    let Ok(body) = response.body_mut().read_to_string() else {
        return false;
    };
    serde_json::from_str::<IdentityResponse>(&body)
        .is_ok_and(|actual| actual == IdentityResponse::current())
}

fn probe_health(daemon: &Daemon, path: &str, token: &str, expected: &HealthResponse) -> bool {
    if !probe_identity(daemon) {
        return false;
    }
    let Some(url) = health_url(daemon, path) else {
        return false;
    };
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(1)))
        .max_idle_connections(0)
        .build()
        .into();
    let response = agent
        .get(&url)
        .header("Authorization", &format!("Bearer {token}"))
        .header("Accept", "application/json")
        .call();
    let Ok(mut response) = response else {
        return false;
    };
    if response.status().as_u16() != 200 {
        return false;
    }
    let Ok(body) = response.body_mut().read_to_string() else {
        return false;
    };
    serde_json::from_str::<HealthResponse>(&body).is_ok_and(|actual| actual == *expected)
}

/// Confirm the MCP capability against this daemon build's exact health reply.
pub fn authenticated_reachable(daemon: &Daemon) -> bool {
    probe_health(
        daemon,
        MCP_HEALTH_PATH,
        &daemon.token,
        &HealthResponse::mcp(),
    )
}

/// Confirm the editor capability on its distinct authenticated health route.
pub fn editor_authenticated_reachable(daemon: &Daemon) -> bool {
    probe_health(
        daemon,
        EDITOR_HEALTH_PATH,
        &daemon.editor_token,
        &HealthResponse::editor(),
    )
}

/// A process-lifetime advisory lock for the SQLite store.
///
/// The lock is acquired before `Workspace::open`, so racing app and stdio
/// launches cannot briefly become concurrent writers while discovery is being
/// published. The lock file remains on disk; closing this handle releases the
/// operating-system lock.
pub struct StoreLock(#[allow(dead_code)] File);

fn store_lock_path(db_path: &Path) -> PathBuf {
    let mut path = OsString::from(db_path.as_os_str());
    path.push(".lock");
    PathBuf::from(path)
}

pub fn try_lock_store(db_path: &Path) -> io::Result<Option<StoreLock>> {
    let path = store_lock_path(db_path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(StoreLock(file))),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Read a published daemon using this build's protocol and capability format.
///
/// Presence of the file is not proof of life: a daemon killed with SIGKILL
/// leaves it behind, and its port may since have been reused by an unrelated
/// process. The authenticated reachability check is the caller's job.
pub fn read() -> Option<Daemon> {
    let body = std::fs::read_to_string(discovery_path()).ok()?;
    parse(&body)
}

fn parse(body: &str) -> Option<Daemon> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    let protocol_version = u32::try_from(json.get("protocol_version")?.as_u64()?).ok()?;
    if protocol_version != PROTOCOL_VERSION {
        return None;
    }
    let token = json.get("token")?.as_str()?.to_string();
    // Do not silently reinterpret a legacy single token as an editor
    // capability. Trusted editor startup must fail clearly until the old
    // daemon is stopped and republishes the split format.
    let editor_token = json.get("editor_token")?.as_str()?.to_string();
    if token == editor_token {
        return None;
    }
    Some(Daemon {
        url: json.get("url")?.as_str()?.to_string(),
        protocol_version,
        token,
        editor_token,
    })
}

/// Publish distinct MCP and editor capabilities, readable only by the user.
pub fn write(port: u16, token: &str, editor_token: &str, db_path: &Path) -> io::Result<PathBuf> {
    if token == editor_token {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "MCP and editor capabilities must be distinct",
        ));
    }
    let path = discovery_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::json!({
        "port": port,
        "protocol_version": PROTOCOL_VERSION,
        // Preserve the established field name for MCP clients and scripts.
        "token": token,
        "editor_token": editor_token,
        "url": format!("http://127.0.0.1:{port}/mcp"),
        "store": db_path.to_string_lossy(),
        // Diagnostic metadata and integration-test cleanup only. Clients do
        // not signal this PID or use it to decide whether to start a daemon.
        "pid": std::process::id(),
    });
    publish(&path, &serde_json::to_vec_pretty(&body)?)?;

    Ok(path)
}

fn temporary_path(path: &Path) -> io::Result<PathBuf> {
    let file_name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "discovery path must have a file name",
        )
    })?;
    let sequence = NEXT_TEMPORARY_ID.fetch_add(1, Ordering::Relaxed);
    let mut temporary_name = OsString::from(".");
    temporary_name.push(file_name);
    temporary_name.push(format!(".{}.{sequence}.tmp", std::process::id()));
    Ok(path.with_file_name(temporary_name))
}

fn publish(path: &Path, body: &[u8]) -> io::Result<()> {
    let temporary = temporary_path(path)?;
    publish_with_temporary(path, &temporary, body)
}

fn publish_with_temporary(path: &Path, temporary: &Path, body: &[u8]) -> io::Result<()> {
    match std::fs::remove_file(temporary) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);

        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }

        let mut file = options.open(temporary)?;
        file.write_all(body)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);

        std::fs::rename(temporary, path)
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }

    result
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::path::Path;

    const STORE_LOCK_CHILD: &str = "THOUGHTD_STORE_LOCK_CHILD";
    const STORE_LOCK_DB_PATH: &str = "THOUGHTD_STORE_LOCK_DB_PATH";

    #[test]
    fn store_lock_child() {
        let Ok(expectation) = std::env::var(STORE_LOCK_CHILD) else {
            return;
        };
        let db_path = std::env::var(STORE_LOCK_DB_PATH).expect("child store path");
        let lock = super::try_lock_store(Path::new(&db_path)).expect("child lock attempt");
        match expectation.as_str() {
            "blocked" => assert!(lock.is_none(), "the owning process keeps the lock"),
            "available" => assert!(lock.is_some(), "process exit releases the lock"),
            unexpected => panic!("unexpected child lock expectation: {unexpected}"),
        }
    }

    fn run_store_lock_child(db_path: &Path, expectation: &str) {
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "discovery::tests::store_lock_child",
                "--nocapture",
            ])
            .env(STORE_LOCK_CHILD, expectation)
            .env(STORE_LOCK_DB_PATH, db_path)
            .output()
            .unwrap();
        assert!(
            child.status.success(),
            "child lock check failed: {}",
            String::from_utf8_lossy(&child.stderr),
        );
    }
    #[test]
    fn token_is_256_bits_of_hex_and_returns_promptly() {
        let token = super::random_token().expect("urandom is readable");
        assert_eq!(token.len(), 64, "32 bytes, hex-encoded");
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(
            token,
            super::random_token().unwrap(),
            "must not be constant"
        );
    }

    #[test]
    fn legacy_single_token_discovery_is_not_an_editor_capability() {
        let legacy = serde_json::json!({
            "url": "http://127.0.0.1:1234/mcp",
            "protocol_version": super::PROTOCOL_VERSION,
            "token": "mcp-only"
        });
        assert!(super::parse(&legacy.to_string()).is_none());

        let current = serde_json::json!({
            "url": "http://127.0.0.1:1234/mcp",
            "protocol_version": super::PROTOCOL_VERSION,
            "token": "mcp-only",
            "editor_token": "editor-only"
        });
        assert_eq!(
            super::parse(&current.to_string()),
            Some(super::Daemon {
                url: "http://127.0.0.1:1234/mcp".into(),
                protocol_version: super::PROTOCOL_VERSION,
                token: "mcp-only".into(),
                editor_token: "editor-only".into(),
            })
        );
    }

    #[test]
    fn incompatible_protocol_discovery_is_rejected() {
        let future = serde_json::json!({
            "url": "http://127.0.0.1:1234/mcp",
            "protocol_version": super::PROTOCOL_VERSION + 1,
            "token": "mcp-only",
            "editor_token": "editor-only"
        });
        assert!(super::parse(&future.to_string()).is_none());
    }

    #[test]
    fn identical_capabilities_are_rejected() {
        let shared = serde_json::json!({
            "url": "http://127.0.0.1:1234/mcp",
            "protocol_version": super::PROTOCOL_VERSION,
            "token": "shared",
            "editor_token": "shared"
        });
        assert!(super::parse(&shared.to_string()).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn published_discovery_has_private_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("daemon.json");
        super::publish(&path, br#"{"token":"private"}"#).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn publication_replaces_stale_files_with_complete_valid_json() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("daemon.json");
        let temporary = directory.path().join(".daemon.json.stale-test.tmp");
        std::fs::write(&path, r#"{"generation":"old"}"#).unwrap();
        std::fs::write(&temporary, "partial secret").unwrap();

        let replacement = serde_json::json!({
            "generation": "new",
            "token": "mcp-capability",
            "editor_token": "editor-capability"
        });
        let replacement = serde_json::to_vec_pretty(&replacement).unwrap();
        super::publish_with_temporary(&path, &temporary, &replacement).unwrap();

        assert!(!temporary.exists());
        let published = std::fs::read(&path).unwrap();
        assert_eq!(published, replacement);
        let parsed: serde_json::Value = serde_json::from_slice(&published).unwrap();
        assert_eq!(parsed["generation"], "new");
        assert_eq!(parsed["token"], "mcp-capability");
        assert_eq!(parsed["editor_token"], "editor-capability");
    }

    #[test]
    fn unrelated_404_service_receives_no_capability() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                let mut chunk = [0_u8; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let read = stream.read(&mut chunk).unwrap();
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..read]);
                }
                requests.push(String::from_utf8(request).unwrap());
                stream
                    .write_all(
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .unwrap();
            }
            requests
        });
        let daemon = super::Daemon {
            url: format!("http://{address}/mcp"),
            protocol_version: super::PROTOCOL_VERSION,
            token: "mcp-only".into(),
            editor_token: "editor-only".into(),
        };

        assert!(!super::authenticated_reachable(&daemon));
        assert!(!super::editor_authenticated_reachable(&daemon));

        let requests = server.join().unwrap();
        for request in requests {
            assert!(request.starts_with("GET /health/identity HTTP/1.1"));
            assert!(!request.to_ascii_lowercase().contains("authorization:"));
            assert!(!request.contains("mcp-only"));
            assert!(!request.contains("editor-only"));
        }
    }

    #[test]
    fn only_one_process_can_lock_a_store() {
        let directory = tempfile::tempdir().unwrap();
        let db_path = directory.path().join("thought.db");
        let first = super::try_lock_store(&db_path)
            .unwrap()
            .expect("first process owns the store");
        run_store_lock_child(&db_path, "blocked");
        drop(first);
        run_store_lock_child(&db_path, "available");
    }
}
