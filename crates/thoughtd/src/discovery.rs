//! How clients find the daemon (AD-10).

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Discovery and local sync protocol understood by this daemon build.
///
/// Version 2 binds current-source responses to normalized visible wording.
pub const PROTOCOL_VERSION: u32 = 2;
pub const IDENTITY_PATH: &str = "/health/identity";
pub const MCP_HEALTH_PATH: &str = "/health/mcp";
const HEALTH_SERVICE: &str = "ai.exalto.thoughtd";

/// Exact public response used to confirm that a discovery record still points
/// at the daemon instance that published it. This carries no authority.
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
/// accepted for MCP access after the public instance check succeeds.
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

/// What a trusted local client needs to reach a running daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Daemon {
    pub url: String,
    pub protocol_version: u32,
    pub instance_id: String,
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

fn health_url(daemon: &Daemon, path: &str) -> Option<String> {
    loopback_base(&daemon.url).map(|base| format!("{base}{path}"))
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

/// Confirm the public daemon instance before sending its bearer, then verify
/// that the bearer authorizes the expected MCP health endpoint.
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
    let body = std::fs::read_to_string(discovery_path()).ok()?;
    let published: PublishedDaemon = serde_json::from_str(&body).ok()?;
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

/// Publish the live instance atomically and owner-readably.
///
/// The daemon holds the home lock while replacing this file, so there is one
/// publisher. A random, create-new temporary path ensures bearer bytes never
/// pass through a pre-existing broad-permission file.
pub fn write(port: u16, token: &str, instance_id: &str, db_path: &Path) -> io::Result<PathBuf> {
    let path = discovery_path();
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "discovery has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let body = serde_json::to_vec_pretty(&PublishedDaemon {
        protocol_version: PROTOCOL_VERSION,
        instance_id: instance_id.into(),
        port,
        token: token.into(),
        url: format!("http://127.0.0.1:{port}/mcp"),
        store: db_path.to_string_lossy().into_owned(),
        pid: std::process::id(),
    })?;
    let temporary = parent.join(format!(".daemon-{instance_id}.tmp"));

    let result = (|| {
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(&body)?;
        file.sync_all()?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))?;
        }

        std::fs::rename(&temporary, &path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result?;
    Ok(path)
}

#[cfg(test)]
mod tests {
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
