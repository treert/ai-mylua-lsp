use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::summary::DocumentSummary;

const SCHEMA_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct CacheMeta {
    schema_version: u32,
    config_fingerprint: u64,
}

#[derive(Serialize, Deserialize)]
struct CacheEntry {
    content_hash: u64,
    summary: DocumentSummary,
}

pub struct SummaryCache {
    cache_dir: PathBuf,
    config_fingerprint: u64,
}

impl SummaryCache {
    pub fn new(workspace_root: &Path, config_fingerprint: u64) -> Option<Self> {
        let cache_dir = resolve_cache_dir(workspace_root)?;
        eprintln!("[mylua-lsp] summary cache dir: {}", cache_dir.display());
        Some(Self {
            cache_dir,
            config_fingerprint,
        })
    }

    pub fn new_from_dir(cache_dir: PathBuf, config_fingerprint: u64) -> Self {
        eprintln!("[mylua-lsp] summary cache dir: {}", cache_dir.display());
        Self {
            cache_dir,
            config_fingerprint,
        }
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    pub fn load_all(&self) -> HashMap<String, DocumentSummary> {
        let mut result = HashMap::new();

        let meta_path = self.cache_dir.join("meta.json");
        if let Ok(data) = std::fs::read(&meta_path) {
            if let Ok(meta) = serde_json::from_slice::<CacheMeta>(&data) {
                if meta.schema_version != SCHEMA_VERSION
                    || meta.config_fingerprint != self.config_fingerprint
                {
                    eprintln!(
                        "[mylua-lsp] cache invalidated (schema={} vs {}, config={} vs {})",
                        meta.schema_version, SCHEMA_VERSION,
                        meta.config_fingerprint, self.config_fingerprint,
                    );
                    let _ = std::fs::remove_dir_all(&self.cache_dir);
                    return result;
                }
            } else {
                let _ = std::fs::remove_dir_all(&self.cache_dir);
                return result;
            }
        } else {
            return result;
        }

        let entries_dir = self.cache_dir.join("entries");
        let read_dir = match std::fs::read_dir(&entries_dir) {
            Ok(rd) => rd,
            Err(_) => return result,
        };

        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.extension().map_or(true, |e| e != "json") {
                continue;
            }
            if let Ok(data) = std::fs::read(&path) {
                if let Ok(ce) = serde_json::from_slice::<CacheEntry>(&data) {
                    let uri_str = ce.summary.uri.to_string();
                    result.insert(uri_str, ce.summary);
                }
            }
        }

        eprintln!("[mylua-lsp] loaded {} cached summaries", result.len());
        result
    }

    pub fn save_all(&self, summaries: &HashMap<tower_lsp_server::ls_types::Uri, DocumentSummary>) {
        let entries_dir = self.cache_dir.join("entries");
        if let Err(e) = std::fs::create_dir_all(&entries_dir) {
            eprintln!("[mylua-lsp] cache: failed to create dir {:?}: {}", entries_dir, e);
            return;
        }

        let meta = CacheMeta {
            schema_version: SCHEMA_VERSION,
            config_fingerprint: self.config_fingerprint,
        };
        let meta_path = self.cache_dir.join("meta.json");
        match serde_json::to_vec(&meta) {
            Ok(data) => {
                if let Err(e) = std::fs::write(&meta_path, data) {
                    eprintln!("[mylua-lsp] cache: failed to write meta: {}", e);
                }
            }
            Err(e) => eprintln!("[mylua-lsp] cache: failed to serialize meta: {}", e),
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
                    if let Err(_) = std::fs::write(&path, data) {
                        write_errors += 1;
                    }
                }
                Err(_) => write_errors += 1,
            }
        }
        if write_errors > 0 {
            eprintln!("[mylua-lsp] cache: {} entries failed to write", write_errors);
        }
    }

}

fn resolve_cache_dir(workspace_root: &Path) -> Option<PathBuf> {
    let base = dirs_cache_base()?;
    let workspace_hash = crate::util::hash_bytes(
        workspace_root
            .to_string_lossy()
            .to_lowercase()
            .as_bytes(),
    );
    Some(base.join("mylua-lsp").join(format!("{:016x}", workspace_hash)))
}

fn dirs_cache_base() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var("LOCALAPPDATA")
            .ok()
            .map(PathBuf::from)
            .or_else(|| std::env::var("APPDATA").ok().map(PathBuf::from))
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::env::var("XDG_CACHE_HOME")
            .ok()
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var("HOME")
                    .ok()
                    .map(|h| PathBuf::from(h).join(".cache"))
            })
    }
}

fn uri_to_cache_filename(uri: &str) -> String {
    format!("{:016x}", crate::util::hash_bytes(uri.as_bytes()))
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
