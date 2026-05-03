use std::collections::HashMap;
use std::sync::Mutex;

use tower_lsp_server::ls_types::Uri;

#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UriId(i32);

impl UriId {
    pub(crate) fn new(raw: i32) -> Self {
        assert!(raw >= 0, "UriId must be non-negative");
        Self(raw)
    }
}

#[derive(Debug)]
pub struct UriInterner {
    inner: Mutex<Inner>,
}

#[derive(Debug)]
struct Inner {
    by_uri: HashMap<Uri, UriId>,
    by_id: Vec<Uri>,
}

impl UriInterner {
    pub fn new() -> Self {
        let empty_uri = empty_uri();
        let mut by_uri = HashMap::new();
        by_uri.insert(empty_uri.clone(), UriId::new(0));
        Self {
            inner: Mutex::new(Inner {
                by_uri,
                by_id: vec![empty_uri],
            }),
        }
    }

    pub fn intern(&self, uri: Uri) -> UriId {
        let mut inner = self.inner.lock().unwrap();
        if let Some(id) = inner.by_uri.get(&uri).copied() {
            return id;
        }

        let raw = i32::try_from(inner.by_id.len()).expect("UriId exhausted");
        let id = UriId::new(raw);
        inner.by_uri.insert(uri.clone(), id);
        inner.by_id.push(uri);
        id
    }

    pub fn resolve(&self, id: UriId) -> Uri {
        let raw = usize::try_from(id.0).expect("UriId must be non-negative");
        self.inner
            .lock()
            .unwrap()
            .by_id
            .get(raw)
            .cloned()
            .expect("UriId must be registered before resolve")
    }
}

fn empty_uri() -> Uri {
    "file:".parse().expect("empty path URI should be valid")
}
