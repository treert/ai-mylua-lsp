use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use tower_lsp_server::ls_types::Uri;

#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UriId(i32);

impl UriId {
    fn new(raw: i32) -> Self {
        assert!(raw >= 0, "UriId must be non-negative");
        Self(raw)
    }

    fn index(self) -> usize {
        usize::try_from(self.0).expect("UriId must be non-negative")
    }
}

#[derive(Debug)]
struct UriRegistry {
    inner: Mutex<Inner>,
}

#[derive(Debug)]
struct Inner {
    by_uri: HashMap<&'static str, UriId>,
    by_id: Vec<UriMeta>,
}

#[derive(Debug)]
struct UriMeta {
    uri: Uri,
    path: &'static str,
    priority: UriPriority,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct UriPriority {
    annotation_key: u16,
    depth: u16,
    len: u32,
}

static URI_REGISTRY: OnceLock<UriRegistry> = OnceLock::new();

/// Intern a URI into the process-global append-only registry.
pub fn intern(uri: &Uri) -> UriId {
    registry().intern(uri)
}

/// Resolve a registered `UriId` back to its LSP `Uri`.
///
/// Panics if the id was fabricated rather than returned by `intern`.
pub fn resolve(id: UriId) -> Uri {
    registry().resolve(id)
}

/// Return the registered URI string for an id.
///
/// This is `Uri::as_str()` (for example `file:///tmp/a.lua`), not a
/// decoded filesystem path.
pub fn path(id: UriId) -> &'static str {
    registry().path(id)
}


/// Return the precomputed candidate ordering key for an id.
pub fn priority(id: UriId) -> UriPriority {
    registry().priority(id)
}

impl UriRegistry {
    fn new() -> Self {
        let empty_uri = empty_uri();
        let empty_meta = UriMeta::new(empty_uri);
        let mut by_uri = HashMap::new();
        by_uri.insert(empty_meta.path, UriId::new(0));
        Self {
            inner: Mutex::new(Inner {
                by_uri,
                by_id: vec![empty_meta],
            }),
        }
    }

    fn intern(&self, uri: &Uri) -> UriId {
        let mut inner = self.inner.lock().unwrap();
        if let Some(id) = inner.by_uri.get(uri.as_str()).copied() {
            return id;
        }

        let raw = i32::try_from(inner.by_id.len()).expect("UriId exhausted");
        let id = UriId::new(raw);
        let meta = UriMeta::new(uri.clone());
        inner.by_uri.insert(meta.path, id);
        inner.by_id.push(meta);
        id
    }

    fn resolve(&self, id: UriId) -> Uri {
        self.inner
            .lock()
            .unwrap()
            .by_id
            .get(id.index())
            .map(|meta| meta.uri.clone())
            .expect("UriId must be registered before resolve")
    }

    fn path(&self, id: UriId) -> &'static str {
        self.inner
            .lock()
            .unwrap()
            .by_id
            .get(id.index())
            .map(|meta| meta.path)
            .expect("UriId must be registered before path lookup")
    }

    fn priority(&self, id: UriId) -> UriPriority {
        self.inner
            .lock()
            .unwrap()
            .by_id
            .get(id.index())
            .map(|meta| meta.priority)
            .expect("UriId must be registered before priority lookup")
    }
}

impl UriMeta {
    fn new(uri: Uri) -> Self {
        let path = leak_path(&uri);
        Self {
            uri,
            path,
            priority: UriPriority::from_path(path),
        }
    }
}

impl UriPriority {
    pub(crate) fn worst() -> Self {
        Self {
            annotation_key: u16::MAX,
            depth: u16::MAX,
            len: u32::MAX,
        }
    }

    fn from_path(path: &str) -> Self {
        let lower = path.to_ascii_lowercase();
        let annotation_count = lower.matches("annotation").count();
        let annotation_count = annotation_count.min(u16::MAX as usize) as u16;
        let depth = path.matches('/').count().min(u16::MAX as usize) as u16;
        let len = path.len().min(u32::MAX as usize) as u32;

        Self {
            annotation_key: u16::MAX - annotation_count,
            depth,
            len,
        }
    }
}

fn registry() -> &'static UriRegistry {
    URI_REGISTRY.get_or_init(UriRegistry::new)
}

fn empty_uri() -> Uri {
    "file:".parse().expect("empty path URI should be valid")
}

fn leak_path(uri: &Uri) -> &'static str {
    Box::leak(uri.as_str().to_string().into_boxed_str())
}
