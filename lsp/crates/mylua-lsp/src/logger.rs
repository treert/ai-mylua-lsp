use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// Fast gate for `lsp_log!` consumers. Reading this `AtomicBool` is a
/// single relaxed load, whereas grabbing the writer mutex is not
/// free. The macro checks it *before* `format!`-ing the message so a
/// future regression that adds a hot-path `lsp_log!("… {:?}", big)`
/// pays zero cost when `mylua.debug.fileLog` is disabled — no
/// allocation, no lock, no syscall.
static ENABLED: AtomicBool = AtomicBool::new(true);

/// Writer holder. Initialized once by `init`; `log()` reuses the same
/// `BufWriter<File>` instead of reopening the log file per line. The
/// previous design (open/append/close per message) serialized every
/// log call through the kernel's inode lock — catastrophic under the
/// rayon-parallel cold-start indexer, where 8+ worker threads could
/// contend millions of times on even routine debug output.
static WRITER: Mutex<Option<BufWriter<File>>> = Mutex::new(None);

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

pub fn init(workspace_root: &Path, enable_file_log: bool) {
    ENABLED.store(enable_file_log, Ordering::Relaxed);
    if !enable_file_log {
        if let Ok(mut w) = WRITER.lock() {
            *w = None;
        }
        return;
    }
    let vscode_dir = workspace_root.join(".vscode");
    let _ = std::fs::create_dir_all(&vscode_dir);
    let log_path = vscode_dir.join("mylua-lsp.log");
    // Truncate on each session start so the log reflects only the
    // current run — matches the previous behavior.
    let new_writer = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&log_path)
        .ok()
        .map(BufWriter::new);

    if let Ok(mut w) = WRITER.lock() {
        *w = new_writer;
    }
    log(&format!(
        "=== mylua-lsp started, log at {} ===",
        log_path.display()
    ));

    // Print executable path and its last-modified time so we can
    // quickly verify the correct binary is running and up-to-date.
    match std::env::current_exe() {
        Ok(exe) => {
            let mtime_str = std::fs::metadata(&exe)
                .and_then(|m| m.modified())
                .map(|t| {
                    // Format as seconds since UNIX epoch — unambiguous and
                    // timezone-independent. A full human-readable local
                    // timestamp would require the `chrono` crate which we
                    // don't want to pull in just for one log line.
                    match t.duration_since(std::time::UNIX_EPOCH) {
                        Ok(d) => {
                            // Rough UTC breakdown without pulling in chrono.
                            let secs = d.as_secs();
                            let (y, m, day, h, min, s) = epoch_to_utc(secs);
                            format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC", y, m, day, h, min, s)
                        }
                        Err(_) => "unknown".to_string(),
                    }
                })
                .unwrap_or_else(|_| "unknown".to_string());
            log(&format!(
                "[mylua-lsp] executable: {} (modified: {})",
                exe.display(),
                mtime_str
            ));
        }
        Err(e) => {
            log(&format!("[mylua-lsp] executable: <unknown> ({})", e));
        }
    }
}

/// Convert seconds since UNIX epoch to (year, month, day, hour, minute, second) in UTC.
/// Minimal implementation — no leap-second handling, good enough for log display.
fn epoch_to_utc(epoch_secs: u64) -> (u64, u64, u64, u64, u64, u64) {
    let s = epoch_secs % 60;
    let total_min = epoch_secs / 60;
    let min = total_min % 60;
    let total_hours = total_min / 60;
    let h = total_hours % 24;
    let mut days = total_hours / 24;

    // Walk years from 1970
    let mut year = 1970u64;
    loop {
        let days_in_year = if is_leap(year) { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }

    let leap = is_leap(year);
    let month_days: [u64; 12] = [
        31,
        if leap { 29 } else { 28 },
        31, 30, 31, 30, 31, 31, 30, 31, 30, 31,
    ];
    let mut month = 0u64;
    for &md in &month_days {
        if days < md {
            break;
        }
        days -= md;
        month += 1;
    }

    (year, month + 1, days + 1, h, min, s)
}

fn is_leap(y: u64) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

pub fn log(msg: &str) {
    if !enabled() {
        return;
    }
    eprintln!("{}", msg);
    // `unwrap_or_else(|p| p.into_inner())` recovers from a poisoned
    // mutex — the logger has no invariants to maintain, so falling
    // back to the tainted state and continuing is strictly better
    // than permanently swallowing every subsequent log line, which
    // would hide exactly the kind of diagnostic we most want after
    // a panic.
    let mut guard = WRITER.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(ref mut w) = *guard {
        // Ignore write errors: a transient I/O failure shouldn't
        // crash the LSP or wedge the caller. Flush each line so
        // `tail -f` style debugging keeps its live feel; the
        // cost is comparable to a `LineWriter`.
        let _ = writeln!(w, "{}", msg);
        let _ = w.flush();
    }
}

#[macro_export]
macro_rules! lsp_log {
    ($($arg:tt)*) => {
        if $crate::logger::enabled() {
            $crate::logger::log(&format!($($arg)*))
        }
    };
}
