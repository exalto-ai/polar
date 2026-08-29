//! How clients find the daemon (AD-10).

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Discovery and local sync protocol understood by this daemon build.
///
/// Version 4 adds durable reviewer identities and removes shared external MCP
/// write authority. Legacy sourced updates remain accepted as the unanchored
/// provenance fallback.
pub const PROTOCOL_VERSION: u32 = 4;
pub const IDENTITY_PATH: &str = "/health/identity";
pub const MCP_HEALTH_PATH: &str = "/health/mcp";
const HEALTH_SERVICE: &str = "ai.exalto.thoughtd";
static NEXT_TEMPORARY_ID: AtomicU64 = AtomicU64::new(0);
const MAX_DISCOVERY_BYTES: u64 = 64 * 1024;

/// Exact public response used to confirm that discovery still points at the
/// daemon instance that published it. This carries no authority.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityResponse {
    service: String,
    protocol_version: u32,
    instance_id: String,
}

impl IdentityResponse {
    pub fn current(instance_id: &str) -> Self {
        Self {
            service: HEALTH_SERVICE.into(),
            protocol_version: PROTOCOL_VERSION,
            instance_id: instance_id.into(),
        }
    }
}

/// Exact authenticated response used to confirm that the published bearer is
/// accepted after the public instance check succeeds.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthResponse {
    service: String,
    protocol_version: u32,
    instance_id: String,
    capability: String,
}

impl HealthResponse {
    pub fn mcp(instance_id: &str) -> Self {
        Self {
            service: HEALTH_SERVICE.into(),
            protocol_version: PROTOCOL_VERSION,
            instance_id: instance_id.into(),
            capability: "mcp".into(),
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
/// The private daemon bearer. Reviewer connections receive their own scoped
/// credentials; the bundled window does not need a second copy of this secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Daemon {
    pub url: String,
    pub protocol_version: u32,
    pub instance_id: String,
    /// Private platform bearer used by the bundled window and stdio shim.
    pub token: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PublishedDaemon {
    protocol_version: u32,
    instance_id: String,
    port: u16,
    token: String,
    url: String,
    store: String,
    pid: u32,
}

fn loopback_base(url: &str) -> Option<&str> {
    let base = url.strip_suffix("/mcp")?;
    let port = base.strip_prefix("http://127.0.0.1:")?;
    if port.is_empty() || port.contains('/') || port.parse::<u16>().ok()? == 0 {
        return None;
    }
    Some(base)
}

fn is_random_id(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn local_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(1)))
        .proxy(None)
        .max_redirects(0)
        .max_redirects_will_error(false)
        .http_status_as_error(false)
        .max_idle_connections(0)
        .build()
        .into()
}

fn health_url_for(url: &str, path: &str) -> Option<String> {
    loopback_base(url).map(|base| format!("{base}{path}"))
}

fn health_url(daemon: &Daemon, path: &str) -> Option<String> {
    health_url_for(&daemon.url, path)
}

fn probe_identity(daemon: &Daemon) -> bool {
    let Some(url) = health_url(daemon, IDENTITY_PATH) else {
        return false;
    };
    let Ok(mut response) = local_agent()
        .get(&url)
        .header("Accept", "application/json")
        .call()
    else {
        return false;
    };
    if response.status().as_u16() != 200 {
        return false;
    }
    let Ok(body) = response.body_mut().read_to_string() else {
        return false;
    };
    serde_json::from_str::<IdentityResponse>(&body)
        .is_ok_and(|actual| actual == IdentityResponse::current(&daemon.instance_id))
}

/// Confirm the daemon identity before sending the bearer, then verify it.
pub fn authenticated_reachable(daemon: &Daemon) -> bool {
    if !probe_identity(daemon) {
        return false;
    }
    let Some(url) = health_url(daemon, MCP_HEALTH_PATH) else {
        return false;
    };
    let Ok(mut response) = local_agent()
        .get(&url)
        .header("Authorization", &format!("Bearer {}", daemon.token))
        .header("Accept", "application/json")
        .call()
    else {
        return false;
    };
    if response.status().as_u16() != 200 {
        return false;
    }
    let Ok(body) = response.body_mut().read_to_string() else {
        return false;
    };
    serde_json::from_str::<HealthResponse>(&body)
        .is_ok_and(|actual| actual == HealthResponse::mcp(&daemon.instance_id))
}

fn try_lock_file(path: &Path) -> io::Result<Option<File>> {
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
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Process-lifetime ownership of one `THOUGHT_HOME`.
///
/// The home lock makes discovery publication single-writer even when someone
/// passes a custom store path. The file remains after exit; the operating
/// system lock, not file presence, represents ownership.
pub struct HomeLock(#[allow(dead_code)] File);

pub fn try_lock_home() -> io::Result<Option<HomeLock>> {
    try_lock_file(&support_dir().join("daemon.lock")).map(|lock| lock.map(HomeLock))
}

/// Process-lifetime ownership of the SQLite store.
///
/// This second lock prevents two homes from addressing the same custom store.
/// It is acquired before `Workspace::open`, so competing processes cannot
/// briefly become concurrent CRDT authorities before discovery is published.
pub struct StoreLock(#[allow(dead_code)] File);

fn store_lock_path(db_path: &Path) -> PathBuf {
    let mut path = OsString::from(db_path.as_os_str());
    path.push(".lock");
    PathBuf::from(path)
}

pub fn try_lock_store(db_path: &Path) -> io::Result<Option<StoreLock>> {
    try_lock_file(&store_lock_path(db_path)).map(|lock| lock.map(StoreLock))
}

/// Read the current discovery format.
///
/// A missing or incompatible record returns `None`. Callers distinguish those
/// cases with `discovery_path().exists()` so an older live build is never
/// replaced by a daemon that does not share its lifetime locks.
pub fn read() -> Option<Daemon> {
    let body = read_bounded_discovery(&discovery_path())?;
    parse(body.as_str())
}

fn read_bounded_discovery(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let initial_length = file.metadata().ok()?.len();
    let mut body =
        String::with_capacity(usize::try_from(initial_length.min(MAX_DISCOVERY_BYTES)).ok()?);
    (&mut file)
        .take(MAX_DISCOVERY_BYTES + 1)
        .read_to_string(&mut body)
        .ok()?;
    (body.len() as u64 <= MAX_DISCOVERY_BYTES).then_some(body)
}
fn parse(body: &str) -> Option<Daemon> {
    let published: PublishedDaemon = serde_json::from_str(body).ok()?;
    if published.protocol_version != PROTOCOL_VERSION
        || published.pid == 0
        || !is_random_id(&published.instance_id)
        || !is_random_id(&published.token)
        || loopback_base(&published.url)?
            .strip_prefix("http://127.0.0.1:")?
            .parse::<u16>()
            .ok()?
            != published.port
    {
        return None;
    }
    Some(Daemon {
        url: published.url,
        protocol_version: published.protocol_version,
        instance_id: published.instance_id,
        token: published.token,
    })
}

/// Publish the private platform capability, readable only by the user.
pub fn write(port: u16, token: &str, instance_id: &str, db_path: &Path) -> io::Result<PathBuf> {
    let path = discovery_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = PublishedDaemon {
        protocol_version: PROTOCOL_VERSION,
        instance_id: instance_id.into(),
        port,
        token: token.into(),
        url: format!("http://127.0.0.1:{port}/mcp"),
        store: db_path.to_string_lossy().into_owned(),
        pid: std::process::id(),
    };
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

    const LOCK_CHILD: &str = "THOUGHTD_LOCK_CHILD";
    const LOCK_PATH: &str = "THOUGHTD_LOCK_PATH";

    #[test]
    fn lock_child() {
        let Ok(expectation) = std::env::var(LOCK_CHILD) else {
            return;
        };
        let lock_path = std::env::var(LOCK_PATH).expect("child lock path");
        let lock = super::try_lock_file(Path::new(&lock_path)).expect("child lock attempt");
        match expectation.as_str() {
            "blocked" => assert!(lock.is_none(), "the owning process keeps the lock"),
            "available" => assert!(lock.is_some(), "process exit releases the lock"),
            unexpected => panic!("unexpected child lock expectation: {unexpected}"),
        }
    }

    fn run_lock_child(lock_path: &Path, expectation: &str) {
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "discovery::tests::lock_child", "--nocapture"])
            .env(LOCK_CHILD, expectation)
            .env(LOCK_PATH, lock_path)
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

    fn published(protocol_version: u32, token: &str) -> serde_json::Value {
        serde_json::json!({
            "protocol_version": protocol_version,
            "instance_id": "c".repeat(64),
            "port": 1234,
            "token": token,
            "url": "http://127.0.0.1:1234/mcp",
            "store": "/tmp/thought.db",
            "pid": 1234,
        })
    }

    #[test]
    fn discovery_reads_are_bounded_before_json_parsing() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("daemon.json");
        let maximum = usize::try_from(super::MAX_DISCOVERY_BYTES).unwrap();

        std::fs::write(&path, vec![b' '; maximum]).unwrap();
        assert_eq!(super::read_bounded_discovery(&path).unwrap().len(), maximum);

        std::fs::write(&path, vec![b' '; maximum + 1]).unwrap();
        assert!(super::read_bounded_discovery(&path).is_none());
    }

    #[test]
    fn current_single_token_discovery_is_accepted() {
        let legacy = serde_json::json!({
            "url": "http://127.0.0.1:1234/mcp",
            "protocol_version": super::PROTOCOL_VERSION,
            "token": "mcp-only"
        });
        assert!(super::parse(&legacy.to_string()).is_none());

        let token = "a".repeat(64);
        let current = published(super::PROTOCOL_VERSION, &token);
        assert_eq!(
            super::parse(&current.to_string()),
            Some(super::Daemon {
                url: "http://127.0.0.1:1234/mcp".into(),
                protocol_version: super::PROTOCOL_VERSION,
                instance_id: "c".repeat(64),
                token,
            })
        );
    }

    #[test]
    fn incompatible_protocol_discovery_is_rejected() {
        let future = published(super::PROTOCOL_VERSION + 1, &"a".repeat(64));
        assert!(super::parse(&future.to_string()).is_none());
    }

    #[test]
    fn current_discovery_never_sends_capabilities_to_a_non_loopback_url() {
        let remote = serde_json::json!({
            "url": "http://example.com/mcp",
            "protocol_version": super::PROTOCOL_VERSION,
            "pid": 4321,
            "token": "mcp-only"
        });
        assert!(super::parse(&remote.to_string()).is_none());
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
            "token": "platform-capability"
        });
        let replacement = serde_json::to_vec_pretty(&replacement).unwrap();
        super::publish_with_temporary(&path, &temporary, &replacement).unwrap();

        assert!(!temporary.exists());
        let published = std::fs::read(&path).unwrap();
        assert_eq!(published, replacement);
        let parsed: serde_json::Value = serde_json::from_slice(&published).unwrap();
        assert_eq!(parsed["generation"], "new");
        assert_eq!(parsed["token"], "platform-capability");
    }

    #[test]
    fn unrelated_404_service_receives_no_capability() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for _ in 0..1 {
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
            instance_id: "c".repeat(64),
            token: "a".repeat(64),
        };

        assert!(!super::authenticated_reachable(&daemon));

        let requests = server.join().unwrap();
        for request in requests {
            assert!(request.starts_with("GET /health/identity HTTP/1.1"));
            assert!(!request.to_ascii_lowercase().contains("authorization:"));
            assert!(!request.contains(&"a".repeat(64)));
        }
    }

    #[test]
    fn process_lock_is_exclusive_and_released_on_exit() {
        let directory = tempfile::tempdir().unwrap();
        let lock_path = directory.path().join("daemon.lock");
        let first = super::try_lock_file(&lock_path)
            .unwrap()
            .expect("first process owns the lock");
        run_lock_child(&lock_path, "blocked");
        drop(first);
        run_lock_child(&lock_path, "available");
    }
}
