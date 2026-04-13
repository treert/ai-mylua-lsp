use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tower_lsp_server::ls_types::Uri;

/// Scan a directory recursively for .lua files.
/// Returns a map of module_path -> file URI.
pub fn scan_workspace_lua_files(roots: &[PathBuf]) -> HashMap<String, Uri> {
    let mut require_map = HashMap::new();

    for root in roots {
        if root.is_dir() {
            scan_dir_recursive(root, root, &mut require_map);
        }
    }

    require_map
}

fn scan_dir_recursive(
    base: &Path,
    dir: &Path,
    map: &mut HashMap<String, Uri>,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if name.starts_with('.') || name == "node_modules" {
                continue;
            }
            scan_dir_recursive(base, &path, map);
        } else if path.extension().map_or(false, |ext| ext == "lua") {
            if let Some(module_path) = file_to_module_path(base, &path) {
                if let Some(uri) = path_to_uri(&path) {
                    map.insert(module_path, uri);
                }
            }
        }
    }
}

/// Convert a file path to a Lua module path.
/// e.g. base=/project, file=/project/game/player.lua -> "game.player"
///      base=/project, file=/project/init.lua -> "init"
fn file_to_module_path(base: &Path, file: &Path) -> Option<String> {
    let relative = file.strip_prefix(base).ok()?;
    let stem = relative.with_extension("");
    let module = stem
        .to_string_lossy()
        .replace('\\', ".")
        .replace('/', ".");
    Some(module)
}

/// Collect all .lua file paths in the workspace (for batch indexing).
pub fn collect_lua_files(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for root in roots {
        if root.is_dir() {
            collect_files_recursive(root, &mut files);
        }
    }
    files
}

fn collect_files_recursive(dir: &Path, files: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if name.starts_with('.') || name == "node_modules" {
                continue;
            }
            collect_files_recursive(&path, files);
        } else if path.extension().map_or(false, |ext| ext == "lua") {
            files.push(path);
        }
    }
}

/// Convert a file path to a file:// URI.
pub fn path_to_uri(path: &Path) -> Option<Uri> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    let normalized = abs.to_string_lossy().replace('\\', "/");
    format!("file:///{}", normalized).parse().ok()
}
