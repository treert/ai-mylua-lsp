use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

static LOG_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

pub fn init(_workspace_root: &std::path::Path) {
    let log_dir = std::env::temp_dir().join("mylua-lsp");
    let _ = std::fs::create_dir_all(&log_dir);
    let log_path = log_dir.join("mylua-lsp.log");
    let _ = std::fs::write(&log_path, "");
    *LOG_PATH.lock().unwrap() = Some(log_path.clone());
    log(&format!("=== mylua-lsp started, log at {} ===", log_path.display()));
}

pub fn log(msg: &str) {
    eprintln!("{}", msg);
    let guard = LOG_PATH.lock().unwrap();
    if let Some(ref path) = *guard {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "{}", msg);
        }
    }
}

#[macro_export]
macro_rules! lsp_log {
    ($($arg:tt)*) => {
        $crate::logger::log(&format!($($arg)*))
    };
}
