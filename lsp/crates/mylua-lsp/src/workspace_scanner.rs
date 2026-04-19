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

/// Built-in relative path that always holds our own summary cache.
/// Hard-coded as an unconditional exclude below so that users who
/// fully override `workspace.exclude` in settings don't accidentally
/// pay the cost of walking tens of thousands of `<hash>.json` files
/// every cold start. Writes happen via `SummaryCache::save_all`, so
/// the path is effectively a contract of this crate.
///
/// The separate `.vscode/` component is normally already excluded
/// by the default `**/.*` glob, but we hard-code the full path here
/// as a belt-and-suspenders guarantee.
const BUILTIN_CACHE_DIR: &str = ".vscode/.cache-mylua-lsp";

fn is_builtin_cache_path(relative_path: &str) -> bool {
    // `relative_path` is forward-slash normalized by the scanner
    // (see `scan_dir_recursive`), so matching on `/` is
    // cross-platform safe.
    relative_path == BUILTIN_CACHE_DIR
        || relative_path.starts_with(&format!("{}/", BUILTIN_CACHE_DIR))
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
        if is_builtin_cache_path(relative_path) {
            return false;
        }
        if !self.include.is_empty() && !self.include.is_match(relative_path) {
            return false;
        }
        !self.exclude.is_match(relative_path)
    }

    /// Returns `true` if a directory should be recursed into.
    /// Skips directories that are themselves matched by an exclude pattern.
    fn should_enter_dir(&self, relative_dir: &str) -> bool {
        if is_builtin_cache_path(relative_dir) {
            return false;
        }
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

/// Resolve user-configured `workspace.library` strings to absolute,
/// canonicalized directory paths.
///
/// Semantics:
/// - Absolute paths are used as-is.
/// - Strings prefixed with `~/` are expanded against `$HOME`
///   (Unix) / `%USERPROFILE%` (Windows).
/// - Relative paths are resolved against the **first** workspace
///   root. With no workspace roots (headless LSP scenarios),
///   relative entries are dropped since we have nothing to anchor
///   them against.
/// - Empty / whitespace-only entries are silently ignored.
/// - Non-existent paths are silently dropped — callers already log
///   what they end up indexing, and a log here would double up on
///   every startup for the common "user configured library that
///   doesn't exist on this machine" case.
/// - Entries resolving to the same canonical path are de-duplicated
///   in input order.
///
/// Returned paths are suitable to pass as additional roots to
/// `scan_workspace_lua_files` / `collect_lua_files`.
pub fn resolve_library_roots(
    library: &[String],
    workspace_roots: &[PathBuf],
) -> Vec<PathBuf> {
    let first_root = workspace_roots.first();
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let mut out: Vec<PathBuf> = Vec::new();

    for entry in library {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            continue;
        }

        let expanded: PathBuf = if let Some(stripped) = trimmed.strip_prefix("~/") {
            match home_dir() {
                Some(home) => home.join(stripped),
                None => continue,
            }
        } else {
            PathBuf::from(trimmed)
        };

        let absolute: PathBuf = if expanded.is_absolute() {
            expanded
        } else if let Some(root) = first_root {
            root.join(&expanded)
        } else {
            continue;
        };

        // Fail-closed on canonicalization: a broken symlink or a
        // path whose containing directory is unreadable would
        // otherwise leave a non-canonical form in `seen`, allowing
        // two entries that resolve to the same physical location
        // to slip through as separate roots and get scanned twice.
        let canonical = match absolute.canonicalize() {
            Ok(c) => c,
            Err(_) => continue,
        };
        if !canonical.is_dir() {
            continue;
        }
        if seen.insert(canonical.clone()) {
            out.push(canonical);
        }
    }

    out
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
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

    #[test]
    fn library_resolver_drops_empty_and_missing_entries() {
        let roots = vec![PathBuf::from("/no/such/project")];
        let out = resolve_library_roots(
            &[
                "".to_string(),
                "   ".to_string(),
                "/definitely/does/not/exist/xyzzy123".to_string(),
            ],
            &roots,
        );
        assert!(out.is_empty(), "no valid entries should resolve: {:?}", out);
    }

    /// Unique per-process, per-test name to avoid collisions when
    /// `cargo test` runs tests in parallel (`-j N`) or when a
    /// previous failed run left residue behind. `process::id()`
    /// alone is insufficient because two parallel tests in the same
    /// process share it.
    fn unique_temp_dir(label: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "mylua_libtest_{}_{}_{}",
            std::process::id(),
            seq,
            label
        ))
    }

    #[test]
    fn library_resolver_deduplicates_same_canonical_path() {
        let p = unique_temp_dir("dedupe");
        std::fs::create_dir_all(&p).unwrap();

        let canonical = p.canonicalize().unwrap();
        let out = resolve_library_roots(
            &[
                canonical.to_string_lossy().to_string(),
                canonical.to_string_lossy().to_string(),
            ],
            &[],
        );
        assert_eq!(out.len(), 1, "duplicate absolute paths should collapse");

        let _ = std::fs::remove_dir_all(&p);
    }

    #[test]
    fn library_resolver_expands_relative_against_first_workspace_root() {
        let root = unique_temp_dir("relroot");
        let lib = root.join("stubs");
        std::fs::create_dir_all(&lib).unwrap();

        let out = resolve_library_roots(&["stubs".to_string()], &[root.clone()]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].canonicalize().unwrap(), lib.canonicalize().unwrap());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn library_resolver_skips_relative_when_no_workspace_root() {
        // Without a workspace root to anchor against, a bare relative
        // path has no sensible canonical form — drop it rather than
        // guess against CWD (tests and headless LSP scenarios are not
        // intended to resolve user-relative library paths).
        let out = resolve_library_roots(&["stubs".to_string()], &[]);
        assert!(out.is_empty());
    }

    #[test]
    fn builtin_cache_dir_always_excluded_even_when_user_overrides_all_excludes() {
        // Simulate a user who fully replaced the default exclude list
        // with something narrow (or empty), removing both `**/.*` and
        // `**/.cache*`. The built-in guard must still refuse to walk
        // into our own cache directory, which now lives at
        // `.vscode/.cache-mylua-lsp/`.
        let cfg = WorkspaceConfig {
            include: vec!["**/*.lua".to_string()],
            exclude: vec![],
            index_mode: crate::config::IndexMode::Merged,
            library: Vec::new(),
        };
        let filter = FileFilter::from_config(&cfg);

        assert!(
            !filter.should_enter_dir(".vscode/.cache-mylua-lsp"),
            "built-in cache dir must be skipped"
        );
        assert!(
            !filter.should_enter_dir(".vscode/.cache-mylua-lsp/entries"),
            "subdir inside built-in cache dir must be skipped"
        );
        assert!(
            !filter.accepts(".vscode/.cache-mylua-lsp/entries/abc.lua"),
            "file inside built-in cache dir must be rejected even if it happens to be .lua"
        );
        // A sibling name that merely *starts* with `.cache-mylua-lsp`
        // but is not the same path (e.g. `.vscode/.cache-mylua-lsp-backup`)
        // is not our cache dir and should still pass the built-in
        // check (the user's exclude list would normally handle it).
        assert!(
            filter.should_enter_dir(".vscode/.cache-mylua-lsp-backup"),
            "unrelated dir whose name only shares a prefix must not be skipped by the built-in check"
        );
        // Top-level `.cache-mylua-lsp/` (old layout before this
        // change) is *not* covered by the built-in check anymore —
        // users still have `**/.cache*` default glob to handle
        // leftover directories during upgrade.
        assert!(
            filter.should_enter_dir(".cache-mylua-lsp"),
            "top-level .cache-mylua-lsp is no longer the builtin path; left to user excludes"
        );
    }
}
