//! How clients find the daemon (AD-10).

use std::io;
use std::path::{Path, PathBuf};

/// `POLAR_HOME` overrides the location of the store and the discovery file.
/// Without it, a test run would publish itself as *the* daemon and overwrite
/// the real one's port and token.
fn support_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("POLAR_HOME") {
        return PathBuf::from(dir);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    Path::new(&home).join("Library/Application Support/ai.exalto.polar")
}

pub fn default_db_path() -> PathBuf {
    support_dir().join("polar.db")
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

/// What a client needs to reach a running daemon.
#[derive(Debug, Clone)]
pub struct Daemon {
    pub url: String,
    pub token: String,
    pub pid: u32,
}

/// Read the published daemon, if one has published itself.
///
/// Presence of the file is not proof of life: a daemon killed with SIGKILL
/// leaves it behind, and its port may since have been reused by an unrelated
/// process. The liveness check is the caller's job.
pub fn read() -> Option<Daemon> {
    let body = std::fs::read_to_string(discovery_path()).ok()?;
    let json: serde_json::Value = serde_json::from_str(&body).ok()?;
    Some(Daemon {
        url: json.get("url")?.as_str()?.to_string(),
        token: json.get("token")?.as_str()?.to_string(),
        pid: json.get("pid")?.as_u64()? as u32,
    })
}

/// Publish the port and token, readable only by the user.
pub fn write(port: u16, token: &str, db_path: &Path) -> io::Result<PathBuf> {
    let path = discovery_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::json!({
        "port": port,
        "token": token,
        "url": format!("http://127.0.0.1:{port}/mcp"),
        "store": db_path.to_string_lossy(),
        "pid": std::process::id(),
    });
    std::fs::write(&path, serde_json::to_vec_pretty(&body)?)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // The token is a bearer credential for the user's private writing.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
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
}
