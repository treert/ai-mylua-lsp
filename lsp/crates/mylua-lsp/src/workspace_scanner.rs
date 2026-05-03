use std::path::{Path, PathBuf};
use tower_lsp_server::ls_types::Uri;
use globset::{Glob, GlobSet, GlobSetBuilder};
use jwalk::WalkDir;

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
/// Returns a list of (module_name, Uri) pairs for building the module index.
pub fn scan_workspace_lua_files(
    roots: &[PathBuf],
    _require_config: &RequireConfig,
    workspace_config: &WorkspaceConfig,
) -> Vec<(String, Uri)> {
    let mut modules = Vec::new();
    let filter = FileFilter::from_config(workspace_config);

    for root in roots {
        if root.is_dir() {
            for path in walk_lua_files_jwalk(root, &filter) {
                if let Some(uri) = path_to_uri(&path) {
                    if let Some(module_name) = file_path_to_module_name(&path) {
                        modules.push((module_name, uri));
                    }
                }
            }
        }
    }

    modules
}

/// Scan and collect in a single pass: returns (module_entries, file_list).
/// This avoids walking the entire directory tree twice.
pub fn scan_and_collect_lua_files(
    roots: &[PathBuf],
    _require_config: &RequireConfig,
    workspace_config: &WorkspaceConfig,
) -> (Vec<(String, Uri)>, Vec<PathBuf>) {
    let mut modules = Vec::new();
    let mut files = Vec::new();
    let filter = FileFilter::from_config(workspace_config);

    for root in roots {
        if root.is_dir() {
            for path in walk_lua_files_jwalk(root, &filter) {
                if let Some(uri) = path_to_uri(&path) {
                    if let Some(module_name) = file_path_to_module_name(&path) {
                        modules.push((module_name, uri));
                    }
                }
                files.push(path);
            }
        }
    }

    (modules, files)
}

/// Convert a file path to a normalized module name.
///
/// Rules:
/// 1. Use the full file path (no base directory stripping, except
///    Windows drive letter removal).
/// 2. Replace path separators with `.`
/// 3. Lowercase all letters.
/// 4. Strip `.lua` extension.
/// 5. For `init.lua` files, remove the trailing `.init` segment
///    (the last segment becomes the directory name).
///
/// Examples:
/// - `/project/game/player.lua` → `project.game.player`
/// - `/project/game/init.lua`   → `project.game`
/// - `/project/Game/Player.lua` → `project.game.player`
pub fn file_path_to_module_name(file: &Path) -> Option<String> {
    let path_str = file.to_string_lossy();
    // Normalize separators to `/`
    let normalized = path_str.replace('\\', "/");

    // Strip Windows drive letter (e.g. "C:/" → "/")
    let without_drive = strip_drive_letter(&normalized);

    // Strip leading `/`
    let trimmed = without_drive.trim_start_matches('/');

    // Strip `.lua` extension
    let without_ext = trimmed.strip_suffix(".lua").unwrap_or(trimmed);

    if without_ext.is_empty() {
        return None;
    }

    // Replace `/` with `.` and lowercase
    let module_name = without_ext.replace('/', ".").to_ascii_lowercase();

    // Handle init.lua: strip trailing `.init`
    let module_name = if module_name.ends_with(".init") {
        let stripped = &module_name[..module_name.len() - 5]; // len(".init") == 5
        if stripped.is_empty() {
            // Edge case: the file is just `/init.lua` → module name "init"
            return Some("init".to_string());
        }
        stripped.to_string()
    } else {
        module_name
    };

    Some(module_name)
}

/// Convert a file URI to a normalized module name.
/// Parses the `file:///path` URI format directly.
pub fn uri_to_module_name(uri: &Uri) -> Option<String> {
    let s = uri.to_string();
    let path_str = s.strip_prefix("file:///")?;
    let decoded = crate::util::percent_decode(path_str);
    // On Unix, re-add the leading `/`
    let full_path = if cfg!(not(windows)) {
        format!("/{}", decoded)
    } else {
        decoded
    };
    file_path_to_module_name(std::path::Path::new(&full_path))
}

/// Strip Windows drive letter prefix from a path string.
/// E.g. "C:/foo" → "/foo", "/foo" → "/foo"
fn strip_drive_letter(path: &str) -> &str {
    let bytes = path.as_bytes();
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'/' || bytes[2] == b'\\')
    {
        &path[2..]
    } else {
        path
    }
}

/// Normalize a require module path string using the same rules:
/// - Replace `.` separators (already done in require strings)
/// - Lowercase all letters
/// - If the last segment is "init", remove it
pub fn normalize_require_path(module_path: &str) -> String {
    let lowered = module_path.to_ascii_lowercase();
    // If last segment is "init", strip it
    if let Some(stripped) = lowered.strip_suffix(".init") {
        if stripped.is_empty() {
            // bare "init" → keep as "init" (edge case)
            return lowered;
        }
        return stripped.to_string();
    }
    if lowered == "init" {
        return lowered;
    }
    lowered
}

/// Extract the last segment from a dot-separated module name.
/// E.g. "game.player" → "player", "player" → "player"
pub fn module_last_segment(module_name: &str) -> &str {
    match module_name.rfind('.') {
        Some(pos) => &module_name[pos + 1..],
        None => module_name,
    }
}

/// Collect all .lua file paths in the workspace (for batch indexing).
pub fn collect_lua_files(roots: &[PathBuf], workspace_config: &WorkspaceConfig) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let filter = FileFilter::from_config(workspace_config);
    for root in roots {
        if root.is_dir() {
            files.extend(walk_lua_files_jwalk(root, &filter));
        }
    }
    files
}

/// Walk a directory tree using `jwalk` for parallel I/O, returning all
/// `.lua` files accepted by `filter`. Directory pruning happens inside
/// `process_read_dir` so excluded subtrees (e.g. `node_modules`,
/// `.git`) never trigger further `readdir` syscalls.
fn walk_lua_files_jwalk(base: &Path, filter: &FileFilter) -> Vec<PathBuf> {
    let base_owned = base.to_path_buf();
    let filter_exclude = filter.exclude.clone();

    let walker = WalkDir::new(base)
        .skip_hidden(false) // we handle hidden dirs via our own exclude globs
        .process_read_dir(move |_depth, _path, _state, children| {
            children.retain(|entry_result| {
                let Ok(entry) = entry_result.as_ref() else {
                    return false;
                };
                if entry.file_type.is_dir() {
                    // Compute relative path for directory filtering
                    let relative = entry
                        .path()
                        .strip_prefix(&base_owned)
                        .unwrap_or(&entry.path())
                        .to_string_lossy()
                        .replace('\\', "/");
                    // Apply user exclude globs.
                    !filter_exclude.is_match(&relative)
                } else {
                    // Keep all non-directory entries; file-level filtering
                    // is done after the walk to avoid duplicating the
                    // include+exclude logic inside the closure.
                    true
                }
            });
        });

    let mut files = Vec::new();
    for entry in walker {
        let Ok(entry) = entry else { continue };
        if entry.file_type.is_dir() {
            continue;
        }
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "lua") {
            let relative = path
                .strip_prefix(base)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            if filter.accepts(&relative) {
                files.push(path);
            }
        }
    }
    files
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
/// `scan_and_collect_lua_files` / `collect_lua_files`.
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
        // On Windows, canonicalize() returns a `\\?\` verbatim
        // prefix and an uppercase drive letter. Normalize both so
        // that downstream `path_to_uri` and `starts_with` checks
        // against workspace roots (which use the client's original
        // casing) behave consistently.
        #[cfg(windows)]
        let canonical = normalize_windows_path(canonical);
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
///
/// Delegates to `Uri::from_file_path` which handles Windows drive-letter
/// percent-encoding (`C:` → `C%3A`) and special characters correctly.
/// For non-absolute paths, resolves against `current_dir` first.
///
/// On Windows, strips the `\\?\` verbatim prefix that
/// `std::fs::canonicalize` produces, since `Uri::from_file_path`
/// would percent-encode the `?` and produce a malformed URI.
/// Additionally normalizes the drive letter to lowercase so that
/// URIs are consistent regardless of whether the path came from
/// `canonicalize()` (uppercase) or the LSP client (lowercase).
pub fn path_to_uri(path: &Path) -> Option<Uri> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    #[cfg(windows)]
    let abs = normalize_windows_path(abs);
    let uri = Uri::from_file_path(&abs)?;
    // `ls-types`' `Uri::from_file_path` calls `capitalize_drive_letter`
    // internally, forcing the drive letter to uppercase (e.g. `F%3A`).
    // VS Code sends URIs with a lowercase drive letter (`f%3A`), so we
    // must post-process the URI string to lowercase the drive letter,
    // otherwise the same file ends up with two distinct URIs and
    // `global_shard` accumulates duplicate candidates.
    #[cfg(windows)]
    let uri = lowercase_drive_in_uri(uri);
    Some(uri)
}

/// Normalize a Windows path for consistent URI generation:
/// 1. Strip the `\\?\` (verbatim) prefix that `std::fs::canonicalize`
///    adds — the prefix confuses URI builders (`\\?\C:\foo` would
///    become `file:////%3F/C%3A/foo` instead of `file:///c%3A/foo`).
/// 2. Lowercase the drive letter so that URIs are identical regardless
///    of whether the path came from `canonicalize()` (uppercase `F:`)
///    or the LSP client / VS Code (lowercase `f:`). Without this,
///    `did_open` on a library file creates a second URI that doesn't
///    match the one from the cold-start scan, causing duplicate
///    `global_shard` candidates.
#[cfg(windows)]
pub fn normalize_windows_path(path: PathBuf) -> PathBuf {
    use std::path::{Component, Prefix};
    match path.components().next() {
        Some(Component::Prefix(p)) => {
            let (drive_char, is_verbatim) = match p.kind() {
                Prefix::VerbatimDisk(disk) => (disk as char, true),
                Prefix::Disk(disk) => (disk as char, false),
                _ => return path,
            };
            let lower = drive_char.to_ascii_lowercase();
            // Skip normalization when the drive letter is already
            // lowercase and there is no verbatim prefix to strip.
            if lower == drive_char && !is_verbatim {
                return path;
            }
            let drive = format!("{}:", lower);
            let rest: PathBuf = path.components().skip(1).collect();
            PathBuf::from(drive).join(rest)
        }
        _ => path,
    }
}

/// Post-process a URI produced by `Uri::from_file_path` to lowercase
/// the Windows drive letter. `ls-types`' `from_file_path` calls
/// `capitalize_drive_letter` internally, producing `file:///F%3A/…`,
/// but VS Code consistently uses lowercase (`file:///f%3A/…`). The
/// mismatch causes the same physical file to appear under two distinct
/// URI keys in `global_shard` / `summaries`.
///
/// The function rewrites the URI string in-place when it matches the
/// pattern `file:///X%3A/…` (single ASCII letter followed by `%3A`).
#[cfg(windows)]
fn lowercase_drive_in_uri(uri: Uri) -> Uri {
    let s = uri.as_str();
    // `file:///F%3A/…`
    //  0       8
    //  file:///F%3A/…
    //          ^--- drive letter at index 8
    //           ^^^--- "%3A" at index 9..12
    if s.len() >= 13
        && s.starts_with("file:///")
        && s.as_bytes()[9..12].eq_ignore_ascii_case(b"%3A")
    {
        let drive = s.as_bytes()[8];
        if drive.is_ascii_alphabetic() {
            let lower = drive.to_ascii_lowercase();
            if lower != drive {
                let mut owned = s.to_owned();
                // SAFETY: replacing one ASCII byte with another ASCII byte
                // preserves UTF-8 validity.
                unsafe { owned.as_bytes_mut()[8] = lower; }
                if let Ok(new_uri) = owned.parse::<Uri>() {
                    return new_uri;
                }
            }
        }
    }
    uri
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    // ── file_path_to_module_name tests ─────────────────────────────

    #[test]
    fn regular_file_module_name() {
        let file = Path::new("/project/game/player.lua");
        let result = file_path_to_module_name(file);
        assert_eq!(result, Some("project.game.player".to_string()));
    }

    #[test]
    fn init_lua_strips_init_segment() {
        let file = Path::new("/project/game/init.lua");
        let result = file_path_to_module_name(file);
        assert_eq!(result, Some("project.game".to_string()));
    }

    #[test]
    fn root_init_lua_returns_init() {
        let file = Path::new("/init.lua");
        let result = file_path_to_module_name(file);
        assert_eq!(result, Some("init".to_string()));
    }

    #[test]
    fn module_name_is_lowercased() {
        let file = Path::new("/Project/Game/Player.lua");
        let result = file_path_to_module_name(file);
        assert_eq!(result, Some("project.game.player".to_string()));
    }

    #[test]
    fn no_base_stripping_full_path_preserved() {
        // The full path (minus drive letter and .lua) becomes the module name
        let file = Path::new("/workspace/src/utils/math.lua");
        let result = file_path_to_module_name(file);
        assert_eq!(result, Some("workspace.src.utils.math".to_string()));
    }

    #[test]
    fn nested_init_lua() {
        let file = Path::new("/project/game/systems/init.lua");
        let result = file_path_to_module_name(file);
        assert_eq!(result, Some("project.game.systems".to_string()));
    }

    // ── normalize_require_path tests ───────────────────────────────

    #[test]
    fn normalize_strips_trailing_init() {
        assert_eq!(normalize_require_path("game.init"), "game");
    }

    #[test]
    fn normalize_bare_init_stays() {
        assert_eq!(normalize_require_path("init"), "init");
    }

    #[test]
    fn normalize_lowercases() {
        assert_eq!(normalize_require_path("Game.Player"), "game.player");
    }

    #[test]
    fn normalize_no_init_suffix() {
        assert_eq!(normalize_require_path("game.player"), "game.player");
    }

    // ── module_last_segment tests ──────────────────────────────────

    #[test]
    fn last_segment_dotted() {
        assert_eq!(module_last_segment("game.player"), "player");
    }

    #[test]
    fn last_segment_single() {
        assert_eq!(module_last_segment("player"), "player");
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

}
