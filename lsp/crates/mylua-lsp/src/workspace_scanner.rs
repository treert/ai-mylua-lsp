use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tower_lsp_server::ls_types::Uri;
use globset::{Glob, GlobSet, GlobSetBuilder};

use crate::config::{RequireConfig, WorkspaceConfig};

/// Compiled include/exclude glob filters for workspace scanning.
pub struct FileFilter {
    include: GlobSet,
    exclude: GlobSet,
}

impl FileFilter {
    pub fn from_config(config: &WorkspaceConfig) -> Self {
        Self {
            include: build_globset(&config.include),
            exclude: build_globset(&config.exclude),
        }
    }

    /// Returns `true` if the file should be included in the workspace index.
    fn accepts(&self, relative_path: &str) -> bool {
        if !self.include.is_empty() && !self.include.is_match(relative_path) {
            return false;
        }
        !self.exclude.is_match(relative_path)
    }

    /// Returns `true` if a directory should be recursed into.
    /// Skips directories that are themselves matched by an exclude pattern.
    fn should_enter_dir(&self, relative_dir: &str) -> bool {
        !self.exclude.is_match(relative_dir)
    }
}

fn build_globset(patterns: &[String]) -> GlobSet {
    let mut builder = GlobSetBuilder::new();
    for pat in patterns {
        if let Ok(g) = Glob::new(pat) {
            builder.add(g);
        }
    }
    builder.build().unwrap_or_else(|_| GlobSet::empty())
}

/// Scan a directory recursively for .lua files.
/// Returns a map of module_path -> file URI.
pub fn scan_workspace_lua_files(
    roots: &[PathBuf],
    require_config: &RequireConfig,
    workspace_config: &WorkspaceConfig,
) -> HashMap<String, Uri> {
    let mut require_map = HashMap::new();
    let filter = FileFilter::from_config(workspace_config);

    for root in roots {
        if root.is_dir() {
            scan_dir_recursive(root, root, &mut require_map, &require_config.paths, &filter);
        }
    }

    require_map
}

fn scan_dir_recursive(
    base: &Path,
    dir: &Path,
    map: &mut HashMap<String, Uri>,
    path_patterns: &[String],
    filter: &FileFilter,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let relative = path.strip_prefix(base)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");

        if path.is_dir() {
            if !filter.should_enter_dir(&relative) {
                continue;
            }
            scan_dir_recursive(base, &path, map, path_patterns, filter);
        } else if path.extension().map_or(false, |ext| ext == "lua") {
            if !filter.accepts(&relative) {
                continue;
            }
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
pub fn collect_lua_files(roots: &[PathBuf], workspace_config: &WorkspaceConfig) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let filter = FileFilter::from_config(workspace_config);
    for root in roots {
        if root.is_dir() {
            collect_files_recursive(root, root, &mut files, &filter);
        }
    }
    files
}

fn collect_files_recursive(base: &Path, dir: &Path, files: &mut Vec<PathBuf>, filter: &FileFilter) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let relative = path.strip_prefix(base)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");

        if path.is_dir() {
            if !filter.should_enter_dir(&relative) {
                continue;
            }
            collect_files_recursive(base, &path, files, filter);
        } else if path.extension().map_or(false, |ext| ext == "lua") {
            if !filter.accepts(&relative) {
                continue;
            }
            files.push(path);
        }
    }
}

/// Check whether a file path should be included in the workspace index.
pub fn should_index_path(path: &Path, roots: &[PathBuf], filter: &FileFilter) -> bool {
    for root in roots {
        if let Ok(relative) = path.strip_prefix(root) {
            let relative_str = relative.to_string_lossy().replace('\\', "/");
            return filter.accepts(&relative_str);
        }
    }
    true
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
