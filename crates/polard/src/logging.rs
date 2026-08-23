//! Where the daemon says what happened.
//!
//! `polard` is a background process with no window. When something goes wrong
//! for a real user there is nothing to look at unless it wrote something down,
//! and until now it wrote three lines at startup and then went silent for the
//! rest of its life.
//!
//! Logs go to a daily-rotated file under `POLAR_HOME` and, when someone is
//! watching, to stderr as well. `POLAR_LOG` overrides the level, e.g.
//! `POLAR_LOG=polar_mcp=debug,polard=trace`.

use std::path::Path;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, fmt};

/// Kept alive for the process's lifetime — dropping it stops the writer thread
/// and silently discards anything still buffered.
pub struct LogGuard(#[allow(dead_code)] tracing_appender::non_blocking::WorkerGuard);

pub fn init(home: &Path) -> LogGuard {
    let _ = std::fs::create_dir_all(home);

    // Daily rotation with a bounded history: a daemon that runs for months
    // should not quietly fill a disk with its own diary.
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("polard")
        .filename_suffix("log")
        .max_log_files(7)
        .build(home)
        .expect("log directory is writable");

    let (file, guard) = tracing_appender::non_blocking(appender);

    let filter = || {
        EnvFilter::try_from_env("POLAR_LOG")
            .unwrap_or_else(|_| EnvFilter::new("info,polard=info,polar_mcp=info"))
    };

    tracing_subscriber::registry()
        .with(
            fmt::layer()
                .with_writer(file)
                .with_ansi(false)
                .with_target(true)
                .with_filter(filter()),
        )
        // stderr as well, for `tauri dev` and for running it by hand. The file
        // is the one that survives.
        .with(
            fmt::layer()
                .with_writer(std::io::stderr)
                .with_target(false)
                .with_filter(filter()),
        )
        .init();

    LogGuard(guard)
}
