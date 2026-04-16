use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tower_lsp_server::ls_types::Uri;

use crate::config::RequireConfig;

/// Scan a directory recursively for .lua files.
/// Returns a map of module_path -> file URI.
pub fn scan_workspace_lua_files(
    roots: &[PathBuf],
    require_config: &RequireConfig,
) -> HashMap<String, Uri> {
    let mut require_map = HashMap::new();

    for root in roots {
        if root.is_dir() {
            scan_dir_recursive(root, root, &mut require_map, &require_config.paths);
        }
    }

    require_map
}

fn scan_dir_recursive(
    base: &Path,
    dir: &Path,
    map: &mut HashMap<String, Uri>,
    path_patterns: &[String],
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
            scan_dir_recursive(base, &path, map, path_patterns);
        } else if path.extension().map_or(false, |ext| ext == "lua") {
            if let Some(uri) = path_to_uri(&path) {
                for module_path in file_to_module_paths(base, &path, path_patterns) {
                    map.entry(module_path).or_insert_with(|| uri.clone());
                }
            }
        }
    }
}

/// Convert a file path to all possible Lua module paths based on path patterns.
///
/// For path patterns like `["?.lua", "?/init.lua"]`:
/// - `game/player.lua` matches `?.lua` → module `"game.player"`
/// - `game/init.lua` matches `?/init.lua` → module `"game"`
/// - `game/init.lua` also matches `?.lua` → module `"game.init"`
pub fn file_to_module_paths(base: &Path, file: &Path, patterns: &[String]) -> Vec<String> {
    let relative = match file.strip_prefix(base) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let relative_str = relative.to_string_lossy().replace('\\', "/");

    let mut modules = Vec::new();

    for pattern in patterns {
        let pattern_normalized = pattern.replace('\\', "/");
        if let Some(q_pos) = pattern_normalized.find('?') {
            let prefix = &pattern_normalized[..q_pos];
            let suffix = &pattern_normalized[q_pos + 1..];

            if relative_str.starts_with(prefix) && relative_str.ends_with(suffix) {
                let end = match relative_str.len().checked_sub(suffix.len()) {
                    Some(e) if e >= prefix.len() => e,
                    _ => continue,
                };
                let module_part = &relative_str[prefix.len()..end];
                if !module_part.is_empty() {
                    let module_path = module_part.replace('/', ".");
                    modules.push(module_path);
                }
            }
        }
    }

    let stem = relative.with_extension("");
    let basic_module = stem.to_string_lossy().replace('\\', ".").replace('/', ".");
    if !basic_module.is_empty() && !modules.contains(&basic_module) {
        modules.push(basic_module);
    }

    modules
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

/// Convert a file path to a `file://` URI.
pub fn path_to_uri(path: &Path) -> Option<Uri> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    let normalized = abs.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        format!("file:///{}", normalized).parse().ok()
    } else {
        format!("file://{}", normalized).parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn default_patterns() -> Vec<String> {
        vec!["?.lua".to_string(), "?/init.lua".to_string()]
    }

    #[test]
    fn regular_file_default_patterns() {
        let base = Path::new("/project");
        let file = Path::new("/project/game/player.lua");
        let result = file_to_module_paths(base, file, &default_patterns());
        assert_eq!(result, vec!["game.player"]);
    }

    #[test]
    fn init_lua_produces_short_and_long_module() {
        let base = Path::new("/project");
        let file = Path::new("/project/game/init.lua");
        let result = file_to_module_paths(base, file, &default_patterns());
        assert!(result.contains(&"game".to_string()), "should contain 'game': {:?}", result);
        assert!(result.contains(&"game.init".to_string()), "should contain 'game.init': {:?}", result);
    }

    #[test]
    fn root_init_lua_no_empty_module() {
        let base = Path::new("/project");
        let file = Path::new("/project/init.lua");
        let result = file_to_module_paths(base, file, &default_patterns());
        assert!(!result.contains(&"".to_string()), "should not contain empty string: {:?}", result);
        assert!(result.contains(&"init".to_string()), "should contain 'init': {:?}", result);
    }

    #[test]
    fn custom_lib_prefix_pattern() {
        let patterns = vec!["lib/?.lua".to_string()];
        let base = Path::new("/project");
        let file = Path::new("/project/lib/utils.lua");
        let result = file_to_module_paths(base, file, &patterns);
        assert!(result.contains(&"utils".to_string()), "should contain 'utils': {:?}", result);
        assert!(result.contains(&"lib.utils".to_string()), "fallback should add 'lib.utils': {:?}", result);
    }

    #[test]
    fn empty_patterns_only_fallback() {
        let base = Path::new("/project");
        let file = Path::new("/project/foo/bar.lua");
        let result = file_to_module_paths(base, file, &[]);
        assert_eq!(result, vec!["foo.bar"]);
    }

    #[test]
    fn overlapping_prefix_suffix_no_panic() {
        let patterns = vec!["very/long/prefix/?.lua".to_string()];
        let base = Path::new("/project");
        let file = Path::new("/project/a.lua");
        let result = file_to_module_paths(base, file, &patterns);
        assert!(result.contains(&"a".to_string()), "fallback should still work: {:?}", result);
    }

    #[test]
    fn file_outside_base_returns_empty() {
        let base = Path::new("/project");
        let file = Path::new("/other/foo.lua");
        let result = file_to_module_paths(base, file, &default_patterns());
        assert!(result.is_empty());
    }
}
