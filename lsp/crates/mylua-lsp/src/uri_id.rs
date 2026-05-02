#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Mutex;

use tower_lsp_server::ls_types::Uri;

#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct UriId(i32);

impl UriId {
    pub(crate) fn new(raw: i32) -> Self {
        assert!(raw > 0, "UriId must be positive");
        Self(raw)
    }

    pub(crate) fn raw(self) -> i32 {
        self.0
    }
}

#[derive(Debug)]
pub(crate) struct UriInterner {
    inner: Mutex<Inner>,
}

#[derive(Debug)]
struct Inner {
    next_id: Option<i32>,
    by_uri: HashMap<Uri, UriId>,
    by_id: HashMap<UriId, Uri>,
}

impl UriInterner {
    pub(crate) fn new() -> Self {
        Self::with_next_id(1)
    }

    #[cfg(test)]
    pub(crate) fn for_test_next_id(next_id: i32) -> Self {
        Self::with_next_id(next_id)
    }

    fn with_next_id(next_id: i32) -> Self {
        assert!(next_id > 0, "UriId must be positive");
        Self {
            inner: Mutex::new(Inner {
                next_id: Some(next_id),
                by_uri: HashMap::new(),
                by_id: HashMap::new(),
            }),
        }
    }

    pub(crate) fn intern(&self, uri: Uri) -> UriId {
        let mut inner = self.inner.lock().unwrap();
        if let Some(id) = inner.by_uri.get(&uri).copied() {
            return id;
        }

        let raw = inner.next_id.expect("UriId exhausted");
        let id = UriId::new(raw);
        inner.next_id = if raw == i32::MAX {
            None
        } else {
            Some(raw + 1)
        };
        inner.by_uri.insert(uri.clone(), id);
        inner.by_id.insert(id, uri);
        id
    }

    pub(crate) fn resolve(&self, id: UriId) -> Option<Uri> {
        self.inner.lock().unwrap().by_id.get(&id).cloned()
    }
}
