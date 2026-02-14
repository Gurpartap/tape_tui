use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};

use crate::error::SessionStoreError;
use crate::schema::{SessionEntry, SessionHeader};

pub struct SessionStore {
    pub(crate) path: PathBuf,
    #[allow(dead_code)]
    pub(crate) file: File,
    pub(crate) header: SessionHeader,
    #[allow(dead_code)]
    pub(crate) entries: Vec<SessionEntry>,
    #[allow(dead_code)]
    pub(crate) index_by_id: HashMap<String, usize>,
    pub(crate) current_leaf_id: Option<String>,
}

impl SessionStore {
    pub fn create_new(_cwd: &Path) -> Result<Self, SessionStoreError> {
        todo!("implemented in bundle B3")
    }

    pub fn open(_path: &Path) -> Result<Self, SessionStoreError> {
        todo!("implemented in bundle B2")
    }

    pub fn append(&mut self, _entry: SessionEntry) -> Result<(), SessionStoreError> {
        todo!("implemented in bundle B4")
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn header(&self) -> &SessionHeader {
        &self.header
    }

    #[must_use]
    pub fn current_leaf_id(&self) -> Option<&str> {
        self.current_leaf_id.as_deref()
    }
}
