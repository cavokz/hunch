use crate::types::{JournalFile, JournalItem, JournalSummary, Pagination};
use async_trait::async_trait;
use std::collections::HashMap;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("journal not found: {0}")]
    NotFound(String),
    #[error("journal already exists: {0}")]
    AlreadyExists(String),
    #[error("journal is read-only: {0}")]
    ReadOnly(String),
    #[error("journal schema {found} is too new (max supported: {max})")]
    SchemaTooNew {
        found: u32,
        max: u32,
        origin: SchemaOrigin,
    },
    #[error("wire protocol {found} is too new (max supported: {max})")]
    ProtocolTooNew { found: u32, max: u32 },
    #[error("operation not supported on remote stores: {0}")]
    Unsupported(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Where a schema-too-new condition was detected.
#[derive(Debug, Clone, Copy)]
pub enum SchemaOrigin {
    /// Detected reading a storage file (server binary is older than the file).
    Storage,
    /// Detected reading a wire response (client binary is older than the server).
    Wire,
}

/// Backend-agnostic journal storage.
#[async_trait]
pub trait Store: Send + Sync {
    /// Load a journal page.
    ///
    /// `archived` determines which storage location to look in. Returns
    /// [`StoreError::NotFound`] if the journal does not exist there.
    ///
    /// [`list`] returns all journals from both locations; callers filter by
    /// [`JournalSummary::archived`] as needed.
    async fn load(
        &self,
        name: &str,
        pagination: &Pagination,
        archived: bool,
    ) -> Result<(JournalFile, usize), StoreError>;
    async fn create(
        &self,
        name: &str,
        title: String,
        meta: Option<HashMap<String, serde_json::Value>>,
    ) -> Result<(), StoreError>;
    /// Add items to the journal. Returns the IDs of items that could not be stored
    /// (e.g. due to an ID collision). An empty vec means all items were stored.
    /// Fatal errors (network, permission, etc.) are returned as `Err`.
    async fn add_items(
        &self,
        name: &str,
        items: Vec<JournalItem>,
        archived: bool,
    ) -> Result<Vec<String>, StoreError>;
    /// Import a journal from an external [`JournalFile`].
    ///
    /// - `merge: false` — create a new journal (fails if it already exists).
    ///   Source `title` and `meta` are used. `archived` controls whether the
    ///   new journal is created in archived state.
    /// - `merge: true` — append items to an existing active journal, skipping
    ///   any whose `id` already exists in the destination. Source `title`,
    ///   `meta`, and `archived` are ignored.
    ///
    /// Returns `(added, skipped)`.
    async fn import(
        &self,
        name: &str,
        journal: JournalFile,
        merge: bool,
        archived: bool,
    ) -> Result<(usize, usize), StoreError>;
    async fn list(&self) -> Result<(Vec<JournalSummary>, usize), StoreError>;
    async fn delete(&self, name: &str, archived: bool) -> Result<(), StoreError>;
    async fn archive(&self, name: &str) -> Result<(), StoreError>;
    async fn unarchive(&self, name: &str) -> Result<(), StoreError>;
}
