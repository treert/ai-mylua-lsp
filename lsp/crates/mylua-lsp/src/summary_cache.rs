use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::summary::DocumentSummary;

// Schema version bump history:
// v2 (2025-04): `extract_table_shape` now actually populates
//   `TableShape.fields` (previously the wrapping `field_list` grammar
//   node was skipped, so every cached shape was `{ fields: {},
//   is_closed: true }`). Bumping the schema forces a one-time rebuild
//   so users don't keep seeing false-positive `Unknown field`
//   diagnostics from stale caches.
// v3 (2025-04): CacheMeta records `exe_mtime_ns` so that any
//   `cargo build` / extension upgrade invalidates the cache
//   automatically, without requiring a manual schema bump. Old v2
//   meta.json fails `deny_unknown_fields` deserialization → cache is
//   wiped → one-time rebuild.
// v4 (2025-04): dropped `crate_version` — subsumed by `exe_mtime_ns`
//   (every new release writes a new binary with a new mtime). Kept
//   `deny_unknown_fields` so any lingering v3 meta.json is rejected
//   and wiped.
const SCHEMA_VERSION: u32 = 4;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheMeta {
    schema_version: u32,
    /// mtime (nanoseconds since UNIX epoch) of the currently running
    /// `mylua-lsp` executable. Bumps automatically whenever the binary
    /// is relinked (local `cargo build`) or replaced (extension
    /// upgrade), so both developer and release scenarios invalidate
    /// the cache without manual intervention. Nanosecond precision
    /// avoids same-second collisions in tight rebuild-then-restart
    /// loops. Falls back to `0` if `current_exe()` / metadata is
    /// unavailable, in which case invalidation relies on
    /// `schema_version` and `config_fingerprint`.
    exe_mtime_ns: u64,
    config_fingerprint: u64,
}

#[derive(Serialize, Deserialize)]
struct CacheEntry {
    content_hash: u64,
    summary: DocumentSummary,
}

pub struct SummaryCache {
    cache_dir: PathBuf,
    expected_meta: CacheMeta,
}

impl SummaryCache {
    pub fn new(workspace_root: &Path, config_fingerprint: u64) -> Self {
        Self::new_from_dir(resolve_cache_dir(workspace_root), config_fingerprint)
    }

    pub fn new_from_dir(cache_dir: PathBuf, config_fingerprint: u64) -> Self {
        crate::lsp_log!("[mylua-lsp] summary cache dir: {}", cache_dir.display());
        Self {
            cache_dir,
            expected_meta: build_expected_meta(config_fingerprint),
        }
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    pub fn load_all(&self) -> HashMap<String, DocumentSummary> {
        let mut result = HashMap::new();

        let meta_path = self.cache_dir.join("meta.json");
        let data = match std::fs::read(&meta_path) {
            Ok(d) => d,
            Err(_) => return result,
        };
        match serde_json::from_slice::<CacheMeta>(&data) {
            Ok(meta) => {
                if let Some(reason) = meta_mismatch_reason(&meta, &self.expected_meta) {
                    crate::lsp_log!("[mylua-lsp] cache invalidated: {}", reason);
                    let _ = std::fs::remove_dir_all(&self.cache_dir);
                    return result;
                }
            }
            Err(e) => {
                crate::lsp_log!(
                    "[mylua-lsp] cache invalidated: meta.json is unreadable ({})",
                    e
                );
                let _ = std::fs::remove_dir_all(&self.cache_dir);
                return result;
            }
        }

        let entries_dir = self.cache_dir.join("entries");
        let read_dir = match std::fs::read_dir(&entries_dir) {
            Ok(rd) => rd,
            Err(_) => return result,
        };

        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            if let Ok(data) = std::fs::read(&path) {
                if let Ok(ce) = serde_json::from_slice::<CacheEntry>(&data) {
                    let uri_str = ce.summary.uri.to_string();
                    result.insert(uri_str, ce.summary);
                }
            }
        }

        crate::lsp_log!("[mylua-lsp] loaded {} cached summaries", result.len());
        result
    }

    pub fn save_all(&self, summaries: &HashMap<tower_lsp_server::ls_types::Uri, DocumentSummary>) {
        let entries_dir = self.cache_dir.join("entries");
        if let Err(e) = std::fs::create_dir_all(&entries_dir) {
            crate::lsp_log!("[mylua-lsp] cache: failed to create dir {:?}: {}", entries_dir, e);
            return;
        }

        // Auto-write a `.gitignore` inside the cache dir when missing
        // so users don't accidentally commit tens of MB of derived
        // JSON. Contract:
        // - Written only if `.gitignore` does not already exist, so a
        //   user who adds custom rules is preserved *for the lifetime
        //   of the current cache*.
        // - After a `load_all` invalidation the whole dir (including
        //   `.gitignore`) is `remove_dir_all`'d and the next
        //   `save_all` here regenerates the default file. This is
        //   acceptable: users wanting persistent custom ignore rules
        //   should put them in the parent workspace `.gitignore`
        //   instead, not inside a regenerated cache dir.
        // - `!.gitignore` keeps this marker itself trackable so
        //   projects that want to commit the gitignore rule
        //   (sharing the ignore policy across collaborators) can
        //   `git add .cache-mylua-lsp/.gitignore` without `-f`.
        let gitignore_path = self.cache_dir.join(".gitignore");
        if !gitignore_path.exists() {
            let _ = std::fs::write(
                &gitignore_path,
                "# Auto-generated by mylua-lsp. Safe to delete; will be regenerated on next index.\n*\n!.gitignore\n",
            );
        }

        let meta_path = self.cache_dir.join("meta.json");
        match serde_json::to_vec(&self.expected_meta) {
            Ok(data) => {
                if let Err(e) = std::fs::write(&meta_path, data) {
                    crate::lsp_log!("[mylua-lsp] cache: failed to write meta: {}", e);
                }
            }
            Err(e) => crate::lsp_log!("[mylua-lsp] cache: failed to serialize meta: {}", e),
        }

        let mut write_errors = 0u32;
        for (uri, summary) in summaries {
            let entry = CacheEntry {
                content_hash: summary.content_hash,
                summary: summary.clone(),
            };
            let filename = uri_to_cache_filename(&uri.to_string());
            let path = entries_dir.join(format!("{}.json", filename));
            match serde_json::to_vec(&entry) {
                Ok(data) => {
                    if std::fs::write(&path, data).is_err() {
                        write_errors += 1;
                    }
                }
                Err(_) => write_errors += 1,
            }
        }
        if write_errors > 0 {
            crate::lsp_log!("[mylua-lsp] cache: {} entries failed to write", write_errors);
        }
    }

}

/// Cache lives inside the workspace at
/// `<root>/.vscode/.cache-mylua-lsp/`, keeping all mylua-lsp
/// generated state (`mylua-lsp.log` + cache) under a single editor-
/// controlled directory.
///
/// Rationale:
/// - No orphan accumulation in the user cache dir when projects are
///   moved/renamed/deleted (the old `~/.cache/mylua-lsp/<hash>/`
///   layout would leak directories forever).
/// - `.vscode/` is already universally understood as "editor/tool
///   state" and is almost always gitignored or editor-managed, so
///   committing the cache by accident is very unlikely.
/// - Two-layer self-index protection:
///     1. Default `workspace.exclude` glob `**/.*` already covers
///        the entire `.vscode/` subtree (including our cache).
///     2. A hard-coded built-in exclude in `workspace_scanner` that
///        matches the exact `.vscode/.cache-mylua-lsp` path and
///        fires even when the user fully overrides the exclude
///        list (e.g. sets `workspace.exclude = []`).
/// - A `.gitignore` is auto-written on first `save_all` so even if
///   a user's project-level `.gitignore` does not ignore `.vscode/`,
///   the cache JSON is still protected.
fn resolve_cache_dir(workspace_root: &Path) -> PathBuf {
    workspace_root.join(".vscode").join(".cache-mylua-lsp")
}

fn uri_to_cache_filename(uri: &str) -> String {
    format!("{:016x}", crate::util::hash_bytes(uri.as_bytes()))
}

fn build_expected_meta(config_fingerprint: u64) -> CacheMeta {
    CacheMeta {
        schema_version: SCHEMA_VERSION,
        exe_mtime_ns: current_exe_mtime_ns().unwrap_or(0),
        config_fingerprint,
    }
}

/// Returns `Some(human_readable_reason)` when the on-disk meta is
/// stale relative to the currently running binary, else `None`.
fn meta_mismatch_reason(on_disk: &CacheMeta, expected: &CacheMeta) -> Option<String> {
    if on_disk.schema_version != expected.schema_version {
        return Some(format!(
            "schema_version {} != {}",
            on_disk.schema_version, expected.schema_version
        ));
    }
    if on_disk.exe_mtime_ns != expected.exe_mtime_ns {
        return Some(format!(
            "exe_mtime_ns {} != {}",
            on_disk.exe_mtime_ns, expected.exe_mtime_ns
        ));
    }
    if on_disk.config_fingerprint != expected.config_fingerprint {
        return Some(format!(
            "config_fingerprint {:016x} != {:016x}",
            on_disk.config_fingerprint, expected.config_fingerprint
        ));
    }
    None
}

/// mtime (in nanoseconds since UNIX epoch) of the currently running
/// `mylua-lsp` executable. Returns `None` if `current_exe()` fails or
/// the file system does not expose a usable mtime — callers treat that
/// as `0` so invalidation silently falls back to the other fields.
///
/// Nanosecond precision (rather than seconds) avoids the edge case
/// where a very fast `cargo build` + restart lands within the same
/// second as the previous save_all, which would produce identical
/// `u64` values and fail to invalidate. `u64` nanoseconds saturates
/// around year 2554, well beyond any realistic use.
fn current_exe_mtime_ns() -> Option<u64> {
    let exe = std::env::current_exe().ok()?;
    let meta = std::fs::metadata(&exe).ok()?;
    let mtime = meta.modified().ok()?;
    let dur = mtime.duration_since(std::time::UNIX_EPOCH).ok()?;
    u64::try_from(dur.as_nanos()).ok()
}

pub fn compute_config_fingerprint(config: &crate::config::LspConfig) -> u64 {
    let mut combined = String::new();
    for path in &config.require.paths {
        combined.push_str(path);
        combined.push('\0');
    }
    let mut aliases: Vec<_> = config.require.aliases.iter().collect();
    aliases.sort_by_key(|(k, _)| (*k).clone());
    for (k, v) in aliases {
        combined.push_str(k);
        combined.push('=');
        combined.push_str(v);
        combined.push('\0');
    }
    crate::util::hash_bytes(combined.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Each test gets its own subdirectory under the system temp dir
    /// so tests can run in parallel without fighting over files. The
    /// directory is cleaned up when `TempRoot` drops.
    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(tag: &str) -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let pid = std::process::id();
            let path = std::env::temp_dir()
                .join(format!("mylua-lsp-test-{}-{}-{}-{}", tag, pid, nanos, n));
            std::fs::create_dir_all(&path).expect("create temp root");
            TempRoot(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn resolve_cache_dir_is_workspace_local_under_vscode() {
        let ws = Path::new("/some/workspace");
        assert_eq!(
            resolve_cache_dir(ws),
            PathBuf::from("/some/workspace/.vscode/.cache-mylua-lsp")
        );
    }

    #[test]
    fn save_all_auto_writes_gitignore() {
        let root = TempRoot::new("gitignore-default");
        let cache = SummaryCache::new(root.path(), 42);

        cache.save_all(&HashMap::new());

        let gitignore = root
            .path()
            .join(".vscode")
            .join(".cache-mylua-lsp")
            .join(".gitignore");
        let content = std::fs::read_to_string(&gitignore).expect("gitignore exists");
        assert!(
            content.contains("*"),
            "default gitignore must ignore all cache contents, got: {:?}",
            content
        );
        assert!(
            content.contains("!.gitignore"),
            "default gitignore must re-include itself so projects can track the ignore rule, got: {:?}",
            content
        );
    }

    #[test]
    fn save_all_preserves_custom_gitignore() {
        let root = TempRoot::new("gitignore-custom");
        let cache_dir = root.path().join(".vscode").join(".cache-mylua-lsp");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let gitignore = cache_dir.join(".gitignore");
        let custom = "# user customized\n*.foo\n";
        std::fs::write(&gitignore, custom).unwrap();

        let cache = SummaryCache::new(root.path(), 42);
        cache.save_all(&HashMap::new());

        let content = std::fs::read_to_string(&gitignore).expect("gitignore exists");
        assert_eq!(
            content, custom,
            "existing .gitignore must not be overwritten by save_all"
        );
    }
}
