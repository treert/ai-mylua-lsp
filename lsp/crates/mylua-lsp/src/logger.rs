use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

static LOG_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);
static ENABLED: Mutex<bool> = Mutex::new(true);

pub fn init(workspace_root: &std::path::Path, enable_file_log: bool) {
    *ENABLED.lock().unwrap() = enable_file_log;
    if !enable_file_log {
        return;
    }
    let vscode_dir = workspace_root.join(".vscode");
    let _ = std::fs::create_dir_all(&vscode_dir);
    let log_path = vscode_dir.join("mylua-lsp.log");
    let _ = std::fs::write(&log_path, "");
    *LOG_PATH.lock().unwrap() = Some(log_path.clone());
    log(&format!("=== mylua-lsp started, log at {} ===", log_path.display()));
}

pub fn log(msg: &str) {
    eprintln!("{}", msg);
    if !*ENABLED.lock().unwrap() {
        return;
    }
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
