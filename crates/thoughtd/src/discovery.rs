//! How clients find the daemon (AD-10).

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::process::Command;
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
    /// Process that published this endpoint. Editor-capability callers bind
    /// this PID to the listening socket and expected sidecar executable before
    /// sending the bearer.
    pub pid: u32,
    /// Public MCP capability, kept as `token` for existing MCP integrations.
    pub token: String,
    /// Private editor-sync capability. Never hand this to an MCP integration.
    pub editor_token: String,
}

/// Non-secret process metadata used to assess a stale discovery record.
/// It is deliberately insufficient on its own to authorize or terminate a
/// process. Cleanup also requires the record to remain byte-identical, the
/// published PID to be conclusively absent, and the store locks to be free.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PublishedDaemon {
    url: String,
    protocol_version: u32,
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

fn health_url_for(url: &str, path: &str) -> Option<String> {
    loopback_base(url).map(|base| format!("{base}{path}"))
}

fn health_url(daemon: &Daemon, path: &str) -> Option<String> {
    health_url_for(&daemon.url, path)
}

fn local_probe_agent() -> ureq::Agent {
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

fn probe_identity_version(url: &str, protocol_version: u32) -> bool {
    let Some(url) = health_url_for(url, IDENTITY_PATH) else {
        return false;
    };
    let agent = local_probe_agent();
    let Ok(mut response) = agent.get(&url).header("Accept", "application/json").call() else {
        return false;
    };
    if response.status().as_u16() != 200 {
        return false;
    }
    let Ok(body) = response.body_mut().read_to_string() else {
        return false;
    };
    serde_json::from_str::<IdentityResponse>(&body).is_ok_and(|actual| {
        actual.service == HEALTH_SERVICE && actual.protocol_version == protocol_version
    })
}

fn probe_identity(daemon: &Daemon) -> bool {
    probe_identity_version(&daemon.url, PROTOCOL_VERSION)
}

fn probe_capability_health(
    daemon: &Daemon,
    path: &str,
    token: &str,
    expected: &HealthResponse,
) -> bool {
    let Some(url) = health_url(daemon, path) else {
        return false;
    };
    let agent = local_probe_agent();
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

fn expected_daemon_reachable(daemon: &Daemon, expected_executable: &Path) -> bool {
    probe_identity(daemon)
        && process_owns_published_listener(daemon.pid, &daemon.url)
        && process_is_expected_daemon(daemon.pid, expected_executable)
}

/// Confirm the MCP capability only after the published PID owns the listener
/// and executes the exact expected sidecar. The bearer is never sent to an
/// identity-only or port-reused loopback service.
pub fn mcp_authenticated_reachable(daemon: &Daemon, expected_executable: &Path) -> bool {
    expected_daemon_reachable(daemon, expected_executable)
        && probe_capability_health(
            daemon,
            MCP_HEALTH_PATH,
            &daemon.token,
            &HealthResponse::mcp(),
        )
}

/// Confirm the editor capability only after the PID in discovery is proven to
/// own the loopback listener and execute the exact expected sidecar. Public
/// identity is checked first, but the editor bearer is not sent until both OS
/// checks pass.
pub fn editor_authenticated_reachable(daemon: &Daemon, expected_executable: &Path) -> bool {
    expected_daemon_reachable(daemon, expected_executable)
        && probe_capability_health(
            daemon,
            EDITOR_HEALTH_PATH,
            &daemon.editor_token,
            &HealthResponse::editor(),
        )
}

#[cfg(target_os = "macos")]
fn process_executable(pid: u32) -> Option<PathBuf> {
    use std::ffi::{CStr, c_int, c_void};

    unsafe extern "C" {
        fn proc_pidpath(pid: c_int, buffer: *mut c_void, buffer_size: u32) -> c_int;
    }

    let mut buffer = vec![0_i8; 4096];
    let length = unsafe {
        proc_pidpath(
            c_int::try_from(pid).ok()?,
            buffer.as_mut_ptr().cast(),
            u32::try_from(buffer.len()).ok()?,
        )
    };
    if length <= 0 {
        return None;
    }
    let path = unsafe { CStr::from_ptr(buffer.as_ptr()) };
    Some(PathBuf::from(path.to_string_lossy().into_owned()))
}

#[cfg(target_os = "linux")]
fn process_executable(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/exe")).ok()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn process_executable(_: u32) -> Option<PathBuf> {
    None
}

fn process_is_expected_daemon(pid: u32, expected: &Path) -> bool {
    process_executable(pid)
        .and_then(|path| path.canonicalize().ok())
        .zip(expected.canonicalize().ok())
        .is_some_and(|(actual, expected)| actual == expected)
}

/// Treat a PID as gone only when process inspection and signal zero both say
/// it no longer exists. Permission failures and unsupported inspection remain
/// ambiguous and therefore fail closed.
#[cfg(unix)]
fn process_is_definitively_absent(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    if process_executable(pid.cast_unsigned()).is_some() {
        return false;
    }
    (unsafe { libc::kill(pid, 0) }) == -1
        && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

#[cfg(not(unix))]
fn process_is_definitively_absent(_: u32) -> bool {
    false
}

fn published_loopback_port(url: &str) -> Option<u16> {
    loopback_base(url)?
        .strip_prefix("http://127.0.0.1:")?
        .parse()
        .ok()
}

#[cfg(target_os = "macos")]
fn process_owns_published_listener(pid: u32, url: &str) -> bool {
    let Some(port) = published_loopback_port(url) else {
        return false;
    };
    let output = Command::new("/usr/sbin/lsof")
        .args([
            "-nP".to_string(),
            "-a".to_string(),
            "-p".to_string(),
            pid.to_string(),
            format!("-iTCP@127.0.0.1:{port}"),
            "-sTCP:LISTEN".to_string(),
            "-Fp".to_string(),
        ])
        .output();
    output.is_ok_and(|output| {
        output.status.success()
            && String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|line| line == format!("p{pid}"))
    })
}

#[cfg(target_os = "linux")]
fn process_owns_published_listener(pid: u32, url: &str) -> bool {
    let Some(port) = published_loopback_port(url) else {
        return false;
    };
    let expected_address = format!("0100007F:{port:04X}");
    let Ok(tcp) = std::fs::read_to_string("/proc/net/tcp") else {
        return false;
    };
    let listening_inodes = tcp
        .lines()
        .skip(1)
        .filter_map(|line| {
            let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
            if fields.get(1).copied() == Some(expected_address.as_str())
                && fields.get(3).copied() == Some("0A")
            {
                fields.get(9).copied()
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    if listening_inodes.is_empty() {
        return false;
    }
    let Ok(descriptors) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
        return false;
    };
    descriptors.filter_map(Result::ok).any(|descriptor| {
        std::fs::read_link(descriptor.path())
            .ok()
            .and_then(|target| target.to_str().map(str::to_string))
            .is_some_and(|target| {
                listening_inodes
                    .iter()
                    .any(|inode| target == format!("socket:[{inode}]"))
            })
    })
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn process_owns_published_listener(_: u32, _: &str) -> bool {
    false
}

/// A process-lifetime advisory lock for the SQLite store.
///
/// The lock is acquired before `Workspace::open`, so racing app and stdio
/// launches cannot briefly become concurrent writers while discovery is being
/// published. The lock file remains on disk; closing this handle releases the
/// operating-system lock.
pub struct DiscoveryLock(#[allow(dead_code)] File);

fn discovery_lock_path(path: &Path) -> PathBuf {
    let mut lock = OsString::from(path.as_os_str());
    lock.push(".lock");
    PathBuf::from(lock)
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

fn try_lock_discovery_at(path: &Path) -> io::Result<Option<DiscoveryLock>> {
    try_lock_file(&discovery_lock_path(path)).map(|lock| lock.map(DiscoveryLock))
}

pub fn try_lock_discovery() -> io::Result<Option<DiscoveryLock>> {
    try_lock_discovery_at(&discovery_path())
}

pub struct StoreLock(#[allow(dead_code)] File);

fn store_lock_path(db_path: &Path) -> PathBuf {
    let mut path = OsString::from(db_path.as_os_str());
    path.push(".lock");
    PathBuf::from(path)
}

pub fn try_lock_store(db_path: &Path) -> io::Result<Option<StoreLock>> {
    let path = store_lock_path(db_path);
    try_lock_file(&path).map(|lock| lock.map(StoreLock))
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

/// Remove only a supported discovery row whose publisher is conclusively
/// dead. Both the home-wide discovery lock and default-store lock are held
/// through the final byte and PID rechecks.
pub fn remove_definitively_stale_discovery() -> io::Result<bool> {
    remove_definitively_stale_discovery_at(&discovery_path(), &default_db_path())
}

fn remove_definitively_stale_discovery_at(path: &Path, db_path: &Path) -> io::Result<bool> {
    remove_definitively_stale_discovery_at_with_process_check(
        path,
        db_path,
        process_is_definitively_absent,
    )
}

fn remove_definitively_stale_discovery_at_with_process_check(
    path: &Path,
    db_path: &Path,
    process_is_absent: fn(u32) -> bool,
) -> io::Result<bool> {
    let Ok(original) = std::fs::read_to_string(path) else {
        return Ok(false);
    };
    let Some(published) = parse_published(&original) else {
        return Ok(false);
    };
    remove_stale_candidate_if_unchanged(path, db_path, &original, &published, process_is_absent)
}

fn remove_stale_candidate_if_unchanged(
    path: &Path,
    db_path: &Path,
    original: &str,
    published: &PublishedDaemon,
    process_is_absent: fn(u32) -> bool,
) -> io::Result<bool> {
    if !(3..=PROTOCOL_VERSION).contains(&published.protocol_version)
        || !process_is_absent(published.pid)
    {
        return Ok(false);
    }
    let Some(_discovery_lock) = try_lock_discovery_at(path)? else {
        return Ok(false);
    };
    let Some(_store_lock) = try_lock_store(db_path)? else {
        return Ok(false);
    };
    let Ok(current) = std::fs::read_to_string(path) else {
        return Ok(false);
    };
    if current.as_bytes() != original.as_bytes()
        || parse_published(&current).as_ref() != Some(published)
        || !process_is_absent(published.pid)
    {
        return Ok(false);
    }
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error),
    }
}

fn parse_published(body: &str) -> Option<PublishedDaemon> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    let protocol_version = u32::try_from(json.get("protocol_version")?.as_u64()?).ok()?;
    let pid = u32::try_from(json.get("pid")?.as_u64()?).ok()?;
    let url = json.get("url")?.as_str()?.to_string();
    if protocol_version == 0 || pid == 0 || loopback_base(&url).is_none() {
        return None;
    }
    Some(PublishedDaemon {
        url,
        protocol_version,
        pid,
    })
}

fn parse(body: &str) -> Option<Daemon> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    let protocol_version = u32::try_from(json.get("protocol_version")?.as_u64()?).ok()?;
    if protocol_version != PROTOCOL_VERSION {
        return None;
    }
    let url = json.get("url")?.as_str()?.to_string();
    loopback_base(&url)?;
    let pid = u32::try_from(json.get("pid")?.as_u64()?).ok()?;
    if pid == 0 {
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
        url,
        protocol_version,
        pid,
        token,
        editor_token,
    })
}

/// Publish distinct MCP and editor capabilities, readable only by the user.
pub fn write(
    _discovery_lock: &DiscoveryLock,
    port: u16,
    token: &str,
    editor_token: &str,
    db_path: &Path,
) -> io::Result<PathBuf> {
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
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, MutexGuard};

    const PROXY_PROBE_CHILD: &str = "THOUGHTD_DISCOVERY_PROXY_CHILD";
    const PROXY_PROBE_TARGET: &str = "THOUGHTD_DISCOVERY_PROXY_TARGET";
    // A spawned child can briefly inherit an advisory-lock file descriptor
    // before exec closes it. Serialize tests that own a lock or spawn a child
    // so lock-release assertions exercise the intended parent lifetime.
    static LOCK_AND_SUBPROCESS_TEST_MUTEX: Mutex<()> = Mutex::new(());

    fn serialize_lock_and_subprocess_test() -> MutexGuard<'static, ()> {
        LOCK_AND_SUBPROCESS_TEST_MUTEX
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[cfg(unix)]
    fn always_absent(_: u32) -> bool {
        true
    }

    #[cfg(unix)]
    fn published_body(protocol_version: u32, pid: u32) -> String {
        serde_json::json!({
            "url": "http://127.0.0.1:1/mcp",
            "protocol_version": protocol_version,
            "pid": pid,
            "token": "old-mcp",
            "editor_token": "old-editor"
        })
        .to_string()
    }

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
            "pid": 4321,
            "token": "mcp-only",
            "editor_token": "editor-only"
        });
        assert_eq!(
            super::parse(&current.to_string()),
            Some(super::Daemon {
                url: "http://127.0.0.1:1234/mcp".into(),
                protocol_version: super::PROTOCOL_VERSION,
                pid: 4321,
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
            "pid": 4321,
            "token": "mcp-only",
            "editor_token": "editor-only"
        });
        assert!(super::parse(&future.to_string()).is_none());
    }

    #[test]
    fn prior_protocol_metadata_is_available_only_for_loopback_process_validation() {
        let prior = serde_json::json!({
            "url": "http://127.0.0.1:1234/mcp",
            "protocol_version": super::PROTOCOL_VERSION - 1,
            "pid": 4321,
            "token": "old-mcp",
            "editor_token": "old-editor"
        });
        assert_eq!(
            super::parse_published(&prior.to_string()),
            Some(super::PublishedDaemon {
                url: "http://127.0.0.1:1234/mcp".into(),
                protocol_version: super::PROTOCOL_VERSION - 1,
                pid: 4321,
            })
        );
        assert!(super::parse(&prior.to_string()).is_none());

        let remote = serde_json::json!({
            "url": "https://example.com/mcp",
            "protocol_version": super::PROTOCOL_VERSION - 1,
            "pid": 4321
        });
        assert!(super::parse_published(&remote.to_string()).is_none());
    }

    #[test]
    fn current_discovery_never_sends_capabilities_to_a_non_loopback_url() {
        let remote = serde_json::json!({
            "url": "http://example.com/mcp",
            "protocol_version": super::PROTOCOL_VERSION,
            "pid": 4321,
            "token": "mcp-only",
            "editor_token": "editor-only"
        });
        assert!(super::parse(&remote.to_string()).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn conclusively_dead_supported_discovery_is_removed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("daemon.json");
        let db_path = directory.path().join("thought.db");
        let pid = 4321;
        for protocol_version in [3, super::PROTOCOL_VERSION] {
            std::fs::write(&path, published_body(protocol_version, pid)).unwrap();
            assert!(
                super::remove_definitively_stale_discovery_at_with_process_check(
                    &path,
                    &db_path,
                    always_absent,
                )
                .unwrap()
            );
            assert!(!path.exists());
        }
    }

    #[cfg(unix)]
    #[test]
    fn stale_cleanup_refuses_live_pid_busy_store_and_changed_bytes() {
        let _subprocess_guard = serialize_lock_and_subprocess_test();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("daemon.json");
        let db_path = directory.path().join("thought.db");
        assert!(!super::process_is_definitively_absent(std::process::id()));
        let live = published_body(super::PROTOCOL_VERSION - 1, std::process::id());
        std::fs::write(&path, &live).unwrap();
        assert!(!super::remove_definitively_stale_discovery_at(&path, &db_path).unwrap());

        let dead = published_body(super::PROTOCOL_VERSION - 1, 4321);
        std::fs::write(&path, &dead).unwrap();
        let store_lock = super::try_lock_store(&db_path).unwrap().unwrap();
        assert!(
            !super::remove_definitively_stale_discovery_at_with_process_check(
                &path,
                &db_path,
                always_absent,
            )
            .unwrap()
        );
        drop(store_lock);

        let published = super::parse_published(&dead).unwrap();
        std::fs::write(&path, format!("{dead}\n")).unwrap();
        assert!(
            !super::remove_stale_candidate_if_unchanged(
                &path,
                &db_path,
                &dead,
                &published,
                always_absent,
            )
            .unwrap()
        );
        assert!(path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn stale_cleanup_refuses_legacy_future_and_active_home_lock() {
        let _subprocess_guard = serialize_lock_and_subprocess_test();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("daemon.json");
        let db_path = directory.path().join("thought.db");
        let pid = 4321;
        for protocol_version in [2, super::PROTOCOL_VERSION + 1] {
            let body = published_body(protocol_version, pid);
            std::fs::write(&path, &body).unwrap();
            assert!(
                !super::remove_definitively_stale_discovery_at_with_process_check(
                    &path,
                    &db_path,
                    always_absent,
                )
                .unwrap()
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), body);
        }

        let body = published_body(super::PROTOCOL_VERSION, pid);
        std::fs::write(&path, &body).unwrap();
        let publisher = super::try_lock_discovery_at(&path).unwrap().unwrap();
        assert!(
            !super::remove_definitively_stale_discovery_at_with_process_check(
                &path,
                &db_path,
                always_absent,
            )
            .unwrap()
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), body);
        drop(publisher);
        assert!(
            super::remove_definitively_stale_discovery_at_with_process_check(
                &path,
                &db_path,
                always_absent,
            )
            .unwrap()
        );
    }

    #[test]
    fn local_probe_proxy_child() {
        if std::env::var(PROXY_PROBE_CHILD).as_deref() != Ok("1") {
            return;
        }
        let target = std::env::var(PROXY_PROBE_TARGET).unwrap();
        assert!(super::probe_identity_version(
            &target,
            super::PROTOCOL_VERSION,
        ));
    }

    #[test]
    fn local_probe_ignores_proxy_environment() {
        let _subprocess_guard = serialize_lock_and_subprocess_test();
        let target = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        target.set_nonblocking(true).unwrap();
        let target_address = target.local_addr().unwrap();
        let proxy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        proxy.set_nonblocking(true).unwrap();
        let proxy_address = proxy.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));

        let target_stop = stop.clone();
        let target_server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            while !target_stop.load(Ordering::Acquire) {
                match target.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        let mut request = Vec::new();
                        let mut chunk = [0_u8; 1024];
                        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                            let read = stream.read(&mut chunk).unwrap();
                            if read == 0 {
                                break;
                            }
                            request.extend_from_slice(&chunk[..read]);
                        }
                        requests.push(request);
                        let body =
                            serde_json::to_string(&super::IdentityResponse::current()).unwrap();
                        write!(
                            stream,
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body,
                        )
                        .unwrap();
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(error) => panic!("unexpected target listener error: {error}"),
                }
            }
            requests
        });

        let proxy_stop = stop.clone();
        let proxy_server = std::thread::spawn(move || {
            let mut requests = Vec::new();
            while !proxy_stop.load(Ordering::Acquire) {
                match proxy.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        let mut request = Vec::new();
                        stream.read_to_end(&mut request).unwrap();
                        requests.push(request);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(error) => panic!("unexpected proxy listener error: {error}"),
                }
            }
            requests
        });

        let proxy_url = format!("http://{proxy_address}");
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "discovery::tests::local_probe_proxy_child",
                "--nocapture",
            ])
            .env(PROXY_PROBE_CHILD, "1")
            .env(PROXY_PROBE_TARGET, format!("http://{target_address}/mcp"))
            .env("HTTP_PROXY", &proxy_url)
            .env("HTTPS_PROXY", &proxy_url)
            .env("ALL_PROXY", &proxy_url)
            .env("http_proxy", &proxy_url)
            .env("https_proxy", &proxy_url)
            .env("all_proxy", &proxy_url)
            .env("NO_PROXY", "")
            .env("no_proxy", "")
            .output()
            .unwrap();
        stop.store(true, Ordering::Release);
        let target_requests = target_server.join().unwrap();
        let proxy_requests = proxy_server.join().unwrap();

        assert!(
            child.status.success(),
            "child probe failed: {}",
            String::from_utf8_lossy(&child.stderr),
        );
        assert_eq!(target_requests.len(), 1);
        assert!(proxy_requests.is_empty());
    }

    #[test]
    fn identical_capabilities_are_rejected() {
        let shared = serde_json::json!({
            "url": "http://127.0.0.1:1234/mcp",
            "protocol_version": super::PROTOCOL_VERSION,
            "pid": 4321,
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
            pid: std::process::id(),
            token: "mcp-only".into(),
            editor_token: "editor-only".into(),
        };

        assert!(!super::mcp_authenticated_reachable(
            &daemon,
            &std::env::current_exe().unwrap()
        ));
        assert!(!super::editor_authenticated_reachable(
            &daemon,
            &std::env::current_exe().unwrap()
        ));

        let requests = server.join().unwrap();
        for request in requests {
            assert!(request.starts_with("GET /health/identity HTTP/1.1"));
            assert!(!request.to_ascii_lowercase().contains("authorization:"));
            assert!(!request.contains("mcp-only"));
            assert!(!request.contains("editor-only"));
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn capability_bearers_are_withheld_when_the_listener_executable_does_not_match() {
        let _subprocess_guard = serialize_lock_and_subprocess_test();
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
                let body = serde_json::to_string(&super::IdentityResponse::current()).unwrap();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .unwrap();
                requests.push(String::from_utf8(request).unwrap());
            }
            requests
        });
        let daemon = super::Daemon {
            url: format!("http://{address}/mcp"),
            protocol_version: super::PROTOCOL_VERSION,
            pid: std::process::id(),
            token: "mcp-only".into(),
            editor_token: "editor-secret-sentinel".into(),
        };
        let directory = tempfile::tempdir().unwrap();
        let wrong_executable = directory.path().join("thoughtd");
        std::fs::write(&wrong_executable, b"not this process").unwrap();

        assert!(!super::mcp_authenticated_reachable(
            &daemon,
            &wrong_executable
        ));
        assert!(!super::editor_authenticated_reachable(
            &daemon,
            &wrong_executable
        ));
        for request in server.join().unwrap() {
            assert!(request.starts_with("GET /health/identity HTTP/1.1"));
            assert!(!request.to_ascii_lowercase().contains("authorization:"));
            assert!(!request.contains("mcp-only"));
            assert!(!request.contains("editor-secret-sentinel"));
        }
    }

    #[test]
    fn only_one_process_can_lock_a_store() {
        let _subprocess_guard = serialize_lock_and_subprocess_test();
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
