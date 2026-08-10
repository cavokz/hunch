use crate::migrate::{self, MigrateResult};
use crate::paths::resolve_foray_home;
use crate::store::{Store, StoreError};
use crate::types::{JournalFile, JournalItem, JournalSummary, Pagination};
use async_trait::async_trait;
use fs2::FileExt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Flat-file JSON store at `~/.foray/journals/`.
pub(crate) struct JsonFileStore {
    base_dir: PathBuf,
}

impl JsonFileStore {
    pub(crate) fn new(base_dir: PathBuf) -> Self {
        Self { base_dir }
    }

    pub(crate) fn default_dir() -> Result<PathBuf, StoreError> {
        Ok(resolve_foray_home()?.join("journals"))
    }

    fn journal_path(&self, name: &str) -> PathBuf {
        self.base_dir.join(format!("{name}.json"))
    }

    fn archive_path(&self, name: &str) -> PathBuf {
        self.base_dir.join("archive").join(format!("{name}.json"))
    }

    fn lock_path(&self, name: &str) -> PathBuf {
        self.base_dir.join(format!("{name}.lock"))
    }

    fn with_lock(&self, name: &str) -> Result<fs::File, StoreError> {
        if let Some(parent) = self.lock_path(name).parent() {
            fs::create_dir_all(parent)?;
        }
        let lock_file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(self.lock_path(name))?;
        lock_file.lock_exclusive()?;
        Ok(lock_file)
    }

    pub(crate) fn read_journal(&self, path: &Path) -> Result<JournalFile, StoreError> {
        let data = fs::read_to_string(path)?;
        let raw: serde_json::Value = serde_json::from_str(&data)?;
        self.parse_journal(raw)
    }

    /// Parse a raw JSON [`Value`] into a [`JournalFile`], running migration and validation.
    ///
    /// Extracted from [`Self::read_journal`] so that [`list_dir`] can reuse the already-parsed
    /// raw value without re-reading the file.
    fn parse_journal(&self, raw: serde_json::Value) -> Result<JournalFile, StoreError> {
        let value = match migrate::migrate(raw) {
            MigrateResult::Current(v) | MigrateResult::Migrated(v) => v,
            MigrateResult::TooNew { found, max } => {
                return Err(StoreError::SchemaTooNew {
                    found,
                    max,
                    origin: crate::store::SchemaOrigin::Storage,
                });
            }
            MigrateResult::Invalid => {
                return Err(StoreError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "journal file is not a JSON object",
                )));
            }
        };
        let journal: JournalFile = serde_json::from_value(value)?;
        if journal.name.trim().is_empty() {
            return Err(StoreError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "journal name is empty",
            )));
        }
        if journal.title.trim().is_empty() {
            return Err(StoreError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "journal title is empty",
            )));
        }
        Ok(journal)
    }

    fn write_journal(&self, path: &Path, journal: &JournalFile) -> Result<(), StoreError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_string_pretty(journal)?;
        let dir = path.parent().unwrap_or(Path::new("."));
        let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
        tmp.write_all(data.as_bytes())?;
        tmp.write_all(b"\n")?;
        tmp.as_file().sync_all()?;
        tmp.persist(path).map_err(std::io::Error::other)?;
        Ok(())
    }

    fn list_dir(&self, dir: &Path) -> Result<Vec<JournalSummary>, StoreError> {
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut summaries = Vec::new();
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "json") && path.is_file() {
                let name = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.to_string_lossy().into_owned());

                // Read and parse raw JSON first so we can extract the schema version
                // before attempting full deserialization.
                let raw_result =
                    fs::read_to_string(&path)
                        .map_err(StoreError::Io)
                        .and_then(|data| {
                            serde_json::from_str::<serde_json::Value>(&data)
                                .map_err(StoreError::Json)
                        });

                let (raw_schema, journal_result) = match raw_result {
                    Err(e) => {
                        // I/O or JSON parse error: schema is unknown.
                        summaries.push(JournalSummary {
                            name,
                            title: String::new(),
                            item_count: 0,
                            archived: false,
                            avg_item_size: None,
                            std_item_size: None,
                            schema: None,
                            meta: None,
                            error: Some(e.to_string()),
                        });
                        continue;
                    }
                    Ok(raw) => {
                        let raw_schema = raw.as_object().map(|obj| {
                            obj.get("schema")
                                .and_then(serde_json::Value::as_u64)
                                .map(|n| u32::try_from(n).unwrap_or(u32::MAX))
                                .unwrap_or(0)
                        });
                        let result = self.parse_journal(raw);
                        (raw_schema, result)
                    }
                };

                match journal_result {
                    Ok(j) => {
                        let mut summary = JournalSummary::from(&j);
                        summary.name = name;
                        summaries.push(summary);
                    }
                    Err(e) => {
                        let schema = match &e {
                            StoreError::SchemaTooNew { found, .. } => Some(*found),
                            _ => raw_schema,
                        };
                        summaries.push(JournalSummary {
                            name,
                            title: String::new(),
                            item_count: 0,
                            archived: false,
                            avg_item_size: None,
                            std_item_size: None,
                            schema,
                            meta: None,
                            error: Some(e.to_string()),
                        });
                    }
                }
            }
        }
        summaries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(summaries)
    }
}

#[async_trait]
impl Store for JsonFileStore {
    async fn load(
        &self,
        name: &str,
        pagination: &Pagination,
        archived: bool,
    ) -> Result<(JournalFile, usize), StoreError> {
        let path = if archived {
            self.archive_path(name)
        } else {
            self.journal_path(name)
        };
        if !path.exists() {
            return Err(StoreError::NotFound(name.into()));
        }
        let mut journal = self.read_journal(&path)?;
        // The file stem is the authoritative name; override whatever the JSON says.
        journal.name = name.to_string();
        let total = journal.items.len();
        let (items, _) = pagination.apply(&journal.items);
        journal.items = items;
        Ok((journal, total))
    }

    async fn create(
        &self,
        name: &str,
        title: String,
        meta: Option<std::collections::HashMap<String, serde_json::Value>>,
    ) -> Result<(), StoreError> {
        let path = self.journal_path(name);
        if path.exists() || self.archive_path(name).exists() {
            return Err(StoreError::AlreadyExists(name.to_string()));
        }
        let journal = JournalFile::new(name, title, meta);
        self.write_journal(&path, &journal)
    }

    async fn add_items(
        &self,
        name: &str,
        items: Vec<JournalItem>,
        archived: bool,
    ) -> Result<Vec<String>, StoreError> {
        if archived {
            return Err(if self.archive_path(name).exists() {
                StoreError::ReadOnly(name.into())
            } else {
                StoreError::NotFound(name.into())
            });
        }
        let path = self.journal_path(name);
        if !path.exists() {
            return Err(StoreError::NotFound(name.into()));
        }
        let _lock = self.with_lock(name)?;
        let mut journal = self.read_journal(&path)?;
        // The file stem is the authoritative name; keep it consistent on write-back.
        journal.name = name.to_string();
        journal.items.extend(items);
        self.write_journal(&path, &journal)?;
        Ok(vec![])
    }

    async fn import(
        &self,
        name: &str,
        journal: JournalFile,
        merge: bool,
        archived: bool,
    ) -> Result<(usize, usize), StoreError> {
        if merge {
            let path = self.journal_path(name);
            if !path.exists() {
                return Err(StoreError::NotFound(name.into()));
            }
            let _lock = self.with_lock(name)?;
            let mut dest = self.read_journal(&path)?;
            dest.name = name.to_string();
            let existing_ids: std::collections::HashSet<&str> =
                dest.items.iter().map(|i| i.id.as_str()).collect();
            let mut skipped = 0usize;
            let to_add: Vec<JournalItem> = journal
                .items
                .into_iter()
                .filter(|i| {
                    if existing_ids.contains(i.id.as_str()) {
                        skipped += 1;
                        false
                    } else {
                        true
                    }
                })
                .collect();
            let added = to_add.len();
            dest.items.extend(to_add);
            self.write_journal(&path, &dest)?;
            Ok((added, skipped))
        } else {
            let path = self.journal_path(name);
            if path.exists() || self.archive_path(name).exists() {
                return Err(StoreError::AlreadyExists(name.to_string()));
            }
            let mut new_journal = JournalFile::new(name, journal.title, journal.meta);
            let added = journal.items.len();
            new_journal.items = journal.items;
            let dest_path = if archived {
                let archive_dir = self.base_dir.join("archive");
                fs::create_dir_all(&archive_dir)?;
                archive_dir.join(format!("{name}.json"))
            } else {
                path
            };
            self.write_journal(&dest_path, &new_journal)?;
            Ok((added, 0))
        }
    }

    async fn list(&self) -> Result<(Vec<JournalSummary>, usize), StoreError> {
        let mut summaries = self.list_dir(&self.base_dir)?;
        for s in &mut summaries {
            s.archived = false;
        }
        let mut archived = self.list_dir(&self.base_dir.join("archive"))?;
        for s in &mut archived {
            s.archived = true;
        }
        summaries.extend(archived);
        let total = summaries.len();
        Ok((summaries, total))
    }

    async fn delete(&self, name: &str, archived: bool) -> Result<(), StoreError> {
        let path = if archived {
            self.archive_path(name)
        } else {
            self.journal_path(name)
        };
        if !path.exists() {
            return Err(StoreError::NotFound(name.into()));
        }
        fs::remove_file(path)?;
        Ok(())
    }

    async fn archive(&self, name: &str) -> Result<(), StoreError> {
        let active = self.journal_path(name);
        if !active.exists() {
            return Err(StoreError::NotFound(name.into()));
        }
        let dest = self.archive_path(name);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(active, dest)?;
        Ok(())
    }

    async fn unarchive(&self, name: &str) -> Result<(), StoreError> {
        let archived = self.archive_path(name);
        if !archived.exists() {
            return Err(StoreError::NotFound(name.into()));
        }
        let dest = self.journal_path(name);
        fs::rename(archived, dest)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ItemType, item_id};
    use chrono::Utc;

    fn make_store() -> (JsonFileStore, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let store = JsonFileStore::new(dir.path().to_path_buf());
        (store, dir)
    }

    fn make_item(content: &str) -> JournalItem {
        JournalItem {
            id: item_id(),
            item_type: ItemType::Finding,
            content: content.into(),
            tags: None,
            added_at: Utc::now(),
            meta: None,
        }
    }

    #[tokio::test]
    async fn create_and_load() {
        let (store, _dir) = make_store();
        let journal = JournalFile::new("my-ctx", "Test".into(), None);
        store
            .create(&journal.name, journal.title.clone(), journal.meta.clone())
            .await
            .unwrap();
        let (loaded, total) = store
            .load("my-ctx", &Pagination::all(), false)
            .await
            .unwrap();
        assert_eq!(loaded.name, "my-ctx");
        assert_eq!(total, 0);
    }

    #[tokio::test]
    async fn create_duplicate_errors() {
        let (store, _dir) = make_store();
        store.create("dup", "A".into(), None).await.unwrap();
        let result = store.create("dup", "B".into(), None).await;
        assert!(matches!(result, Err(StoreError::AlreadyExists(_))));
    }

    #[tokio::test]
    async fn load_not_found() {
        let (store, _dir) = make_store();
        let result = store.load("nonexistent", &Pagination::all(), false).await;
        assert!(matches!(result, Err(StoreError::NotFound(_))));
    }

    #[tokio::test]
    async fn add_item_and_load() {
        let (store, _dir) = make_store();
        store.create("my-ctx", "T".into(), None).await.unwrap();
        let item = make_item("found a bug");
        store.add_items("my-ctx", vec![item], false).await.unwrap();
        let (loaded, total) = store
            .load("my-ctx", &Pagination::all(), false)
            .await
            .unwrap();
        assert_eq!(total, 1);
        assert_eq!(loaded.items[0].content, "found a bug");
    }

    #[tokio::test]
    async fn add_item_not_found() {
        let (store, _dir) = make_store();
        let result = store.add_items("nope", vec![make_item("x")], false).await;
        assert!(matches!(result, Err(StoreError::NotFound(_))));
    }

    #[tokio::test]
    async fn list_journals() {
        let (store, _dir) = make_store();
        store.create("alpha", "A".into(), None).await.unwrap();
        store.create("beta", "B".into(), None).await.unwrap();
        let (summaries, total) = store.list().await.unwrap();
        assert_eq!(total, 2);
        assert_eq!(summaries[0].name, "alpha");
        assert_eq!(summaries[1].name, "beta");
    }

    #[tokio::test]
    async fn list_all() {
        let (store, _dir) = make_store();
        for name in ["a", "b", "c", "d"] {
            store.create(name, name.into(), None).await.unwrap();
        }
        let (page, total) = store.list().await.unwrap();
        assert_eq!(total, 4);
        assert_eq!(page.len(), 4);
    }

    #[tokio::test]
    async fn delete_journal() {
        let (store, _dir) = make_store();
        store.create("to-delete", "D".into(), None).await.unwrap();
        store.delete("to-delete", false).await.unwrap();
        assert!(matches!(
            store.load("to-delete", &Pagination::all(), false).await,
            Err(StoreError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn delete_archived_journal() {
        let (store, _dir) = make_store();
        store.create("to-delete", "D".into(), None).await.unwrap();
        store.archive("to-delete").await.unwrap();
        // deleting active when archived → NotFound (not at active location)
        assert!(matches!(
            store.delete("to-delete", false).await,
            Err(StoreError::NotFound(_))
        ));
        // deleting archived → ok
        store.delete("to-delete", true).await.unwrap();
        assert!(matches!(
            store.load("to-delete", &Pagination::all(), true).await,
            Err(StoreError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn delete_wrong_location_errors() {
        let (store, _dir) = make_store();
        store.create("to-delete", "D".into(), None).await.unwrap();
        assert!(matches!(
            store.delete("to-delete", true).await,
            Err(StoreError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn archive_and_unarchive() {
        let (store, _dir) = make_store();
        store.create("arch-test", "A".into(), None).await.unwrap();

        store.archive("arch-test").await.unwrap();

        let (loaded, _) = store
            .load("arch-test", &Pagination::all(), true)
            .await
            .unwrap();
        assert_eq!(loaded.name, "arch-test");
        // &[...] temporary lives to end-of-statement, safely covering .await.
        assert!(matches!(
            store
                .add_items("arch-test", vec![make_item("x")], true)
                .await,
            Err(StoreError::ReadOnly(_))
        ));
        let (archived_list, _) = store.list().await.unwrap();
        let archived_list: Vec<_> = archived_list.into_iter().filter(|s| s.archived).collect();
        assert_eq!(archived_list.len(), 1);
        let (all, _) = store.list().await.unwrap();
        let active: Vec<_> = all.into_iter().filter(|s| !s.archived).collect();
        assert_eq!(active.len(), 0);

        store.unarchive("arch-test").await.unwrap();
        let (all, _) = store.list().await.unwrap();
        let active: Vec<_> = all.into_iter().filter(|s| !s.archived).collect();
        assert_eq!(active.len(), 1);
    }

    #[tokio::test]
    async fn archive_already_archived_errors() {
        let (store, _dir) = make_store();
        store.create("to-archive", "A".into(), None).await.unwrap();
        store.archive("to-archive").await.unwrap();
        assert!(matches!(
            store.archive("to-archive").await,
            Err(StoreError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn unarchive_already_active_errors() {
        let (store, _dir) = make_store();
        store.create("active", "A".into(), None).await.unwrap();
        assert!(matches!(
            store.unarchive("active").await,
            Err(StoreError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn unarchive_not_found_errors() {
        let (store, _dir) = make_store();
        assert!(matches!(
            store.unarchive("missing").await,
            Err(StoreError::NotFound(_))
        ));
    }

    // ── empty name / title rejection ─────────────────────────────────

    #[test]
    fn read_journal_rejects_empty_name() {
        let (store, dir) = make_store();
        let path = dir.path().join("empty-name.json");
        let raw = serde_json::json!({
            "schema": migrate::CURRENT_SCHEMA,
            "name": "",
            "title": "Some Title",
            "items": []
        });
        std::fs::write(&path, serde_json::to_string_pretty(&raw).unwrap()).unwrap();

        let err = store.read_journal(&path).unwrap_err();
        assert!(
            matches!(err, StoreError::Io(ref e) if e.kind() == std::io::ErrorKind::InvalidData),
            "expected InvalidData, got {err:?}"
        );
        assert!(
            err.to_string().contains("name"),
            "message should mention name: {err}"
        );
    }

    #[test]
    fn read_journal_rejects_empty_title() {
        let (store, dir) = make_store();
        let path = dir.path().join("empty-title.json");
        let raw = serde_json::json!({
            "schema": migrate::CURRENT_SCHEMA,
            "name": "my-journal",
            "title": "",
            "items": []
        });
        std::fs::write(&path, serde_json::to_string_pretty(&raw).unwrap()).unwrap();

        let err = store.read_journal(&path).unwrap_err();
        assert!(
            matches!(err, StoreError::Io(ref e) if e.kind() == std::io::ErrorKind::InvalidData),
            "expected InvalidData, got {err:?}"
        );
        assert!(
            err.to_string().contains("title"),
            "message should mention title: {err}"
        );
    }

    #[test]
    fn read_journal_rejects_whitespace_only_name() {
        let (store, dir) = make_store();
        let path = dir.path().join("ws-name.json");
        let raw = serde_json::json!({
            "schema": migrate::CURRENT_SCHEMA,
            "name": "   ",
            "title": "Some Title",
            "items": []
        });
        std::fs::write(&path, serde_json::to_string_pretty(&raw).unwrap()).unwrap();

        let err = store.read_journal(&path).unwrap_err();
        assert!(
            matches!(err, StoreError::Io(ref e) if e.kind() == std::io::ErrorKind::InvalidData),
            "expected InvalidData, got {err:?}"
        );
        assert!(
            err.to_string().contains("name"),
            "message should mention name: {err}"
        );
    }

    #[test]
    fn read_journal_rejects_whitespace_only_title() {
        let (store, dir) = make_store();
        let path = dir.path().join("ws-title.json");
        let raw = serde_json::json!({
            "schema": migrate::CURRENT_SCHEMA,
            "name": "my-journal",
            "title": "   ",
            "items": []
        });
        std::fs::write(&path, serde_json::to_string_pretty(&raw).unwrap()).unwrap();

        let err = store.read_journal(&path).unwrap_err();
        assert!(
            matches!(err, StoreError::Io(ref e) if e.kind() == std::io::ErrorKind::InvalidData),
            "expected InvalidData, got {err:?}"
        );
        assert!(
            err.to_string().contains("title"),
            "message should mention title: {err}"
        );
    }

    #[tokio::test]
    async fn list_includes_empty_name_as_error() {
        let (store, dir) = make_store();
        store.create("valid", "Valid".into(), None).await.unwrap();
        let path = dir.path().join("empty-name.json");
        let raw = serde_json::json!({
            "schema": migrate::CURRENT_SCHEMA,
            "name": "",
            "title": "Some Title",
            "items": []
        });
        std::fs::write(&path, serde_json::to_string_pretty(&raw).unwrap()).unwrap();

        let (summaries, total) = store.list().await.unwrap();
        assert_eq!(total, 2, "both valid and error journals should appear");
        let valid = summaries.iter().find(|s| s.name == "valid").unwrap();
        assert!(valid.error.is_none(), "valid journal should have no error");
        let error_entry = summaries.iter().find(|s| s.name == "empty-name").unwrap();
        assert!(
            error_entry.error.is_some(),
            "invalid journal should have an error"
        );
        assert_eq!(
            error_entry.schema,
            Some(migrate::CURRENT_SCHEMA),
            "schema should be reported even for error entries"
        );
    }

    #[tokio::test]
    async fn list_includes_empty_title_as_error() {
        let (store, dir) = make_store();
        store.create("valid", "Valid".into(), None).await.unwrap();
        let path = dir.path().join("empty-title.json");
        let raw = serde_json::json!({
            "schema": migrate::CURRENT_SCHEMA,
            "name": "my-journal",
            "title": "",
            "items": []
        });
        std::fs::write(&path, serde_json::to_string_pretty(&raw).unwrap()).unwrap();

        let (summaries, total) = store.list().await.unwrap();
        assert_eq!(total, 2, "both valid and error journals should appear");
        let error_entry = summaries.iter().find(|s| s.name == "empty-title").unwrap();
        assert!(
            error_entry.error.is_some(),
            "invalid journal should have an error"
        );
        assert_eq!(
            error_entry.schema,
            Some(migrate::CURRENT_SCHEMA),
            "schema should be reported even for error entries"
        );
    }

    #[tokio::test]
    async fn read_journal_migrates_v0() {
        let (store, dir) = make_store();
        // Write a raw schema-0 file (no schema field, has created_at/updated_at).
        let path = dir.path().join("legacy.json");
        let raw = serde_json::json!({
            "_note": "old file",
            "id": "aaaaa-bbbbb-ccccc",
            "name": "legacy",
            "title": "Legacy Journal",
            "items": [
                {
                    "id": "xxxx-xxxx-xxxx-xxxx",
                    "type": "note",
                    "content": "old note",
                    "added_at": "2026-01-01T00:00:00Z"
                }
            ],
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-02T00:00:00Z"
        });
        std::fs::write(&path, serde_json::to_string_pretty(&raw).unwrap()).unwrap();

        let journal = store.read_journal(&path).unwrap();

        // Migration should produce a journal at the current schema version.
        assert_eq!(journal.schema, migrate::CURRENT_SCHEMA);
        assert_eq!(journal.name, "legacy");
        assert_eq!(journal.items.len(), 1);
        assert_eq!(journal.items[0].content, "old note");

        // File on disk is NOT rewritten by read_journal — migration is lazy.
        // The old fields are still present until the next add_items write.
        let on_disk: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(
            on_disk.get("schema").is_none(),
            "file not yet healed — schema should still be absent"
        );
    }

    #[tokio::test]
    async fn add_items_heals_v0_journal() {
        let (store, dir) = make_store();
        // Write a raw schema-0 file directly into the store directory.
        let path = dir.path().join("legacy.json");
        let raw = serde_json::json!({
            "id": "aaaaa-bbbbb-ccccc",
            "name": "legacy",
            "title": "Legacy Journal",
            "items": [],
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-02T00:00:00Z"
        });
        std::fs::write(&path, serde_json::to_string_pretty(&raw).unwrap()).unwrap();

        // add_items holds the lock and rewrites the file — this is the heal path.
        // &[...] temporary lives to end-of-statement, safely covering .await.
        store
            .add_items("legacy", vec![make_item("new item")], false)
            .await
            .unwrap();

        let on_disk: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            on_disk["schema"],
            serde_json::json!(migrate::CURRENT_SCHEMA)
        );
        assert!(
            on_disk.get("created_at").is_none(),
            "created_at should be gone"
        );
        assert!(
            on_disk.get("updated_at").is_none(),
            "updated_at should be gone"
        );
        assert_eq!(on_disk["items"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn read_journal_too_new() {
        let (store, dir) = make_store();
        let path = dir.path().join("future.json");
        let raw = serde_json::json!({
            "schema": 9999,
            "id": "aaaaa-bbbbb-ccccc",
            "name": "future",
            "items": []
        });
        std::fs::write(&path, serde_json::to_string_pretty(&raw).unwrap()).unwrap();

        let err = store.read_journal(&path).unwrap_err();
        assert!(
            matches!(
                err,
                StoreError::SchemaTooNew {
                    found: 9999,
                    max: migrate::CURRENT_SCHEMA,
                    origin: crate::store::SchemaOrigin::Storage,
                }
            ),
            "expected SchemaTooNew, got {err:?}"
        );
    }

    #[tokio::test]
    async fn list_includes_schema_too_new_as_error() {
        // A journal with a schema newer than CURRENT_SCHEMA must appear in the
        // list as an error entry with the schema version populated.
        let (store, dir) = make_store();
        // Create a normal journal first so there's something in the directory.
        store.create("normal", "Normal".into(), None).await.unwrap();
        // Drop a future-schema file directly into the journals directory.
        let path = dir.path().join("future.json");
        let raw = serde_json::json!({
            "schema": 9999,
            "id": "aaaaa-bbbbb-ccccc",
            "name": "future",
            "items": []
        });
        std::fs::write(&path, serde_json::to_string_pretty(&raw).unwrap()).unwrap();

        let (summaries, total) = store.list().await.unwrap();
        assert_eq!(total, 2, "both journals should appear");
        let error_entry = summaries.iter().find(|s| s.name == "future").unwrap();
        assert!(
            error_entry.error.is_some(),
            "future-schema journal should have an error"
        );
        assert_eq!(
            error_entry.schema,
            Some(9999),
            "schema version should match the on-disk value"
        );
    }

    #[tokio::test]
    async fn list_includes_corrupt_json_as_error() {
        let (store, dir) = make_store();
        store.create("valid", "Valid".into(), None).await.unwrap();
        let path = dir.path().join("corrupt.json");
        std::fs::write(&path, b"this is not valid json {{{").unwrap();

        let (summaries, total) = store.list().await.unwrap();
        assert_eq!(total, 2, "both journals should appear");
        let error_entry = summaries.iter().find(|s| s.name == "corrupt").unwrap();
        assert!(
            error_entry.error.is_some(),
            "corrupt file should have an error"
        );
        assert_eq!(
            error_entry.schema, None,
            "schema should be absent when JSON cannot be parsed"
        );
    }

    #[tokio::test]
    async fn list_includes_non_object_json_as_error() {
        // Valid JSON but not a JSON object (e.g. an array) — schema must be absent.
        let (store, dir) = make_store();
        store.create("valid", "Valid".into(), None).await.unwrap();
        let path = dir.path().join("array.json");
        std::fs::write(&path, b"[1, 2, 3]").unwrap();

        let (summaries, total) = store.list().await.unwrap();
        assert_eq!(total, 2, "both journals should appear");
        let error_entry = summaries.iter().find(|s| s.name == "array").unwrap();
        assert!(
            error_entry.error.is_some(),
            "non-object JSON file should have an error"
        );
        assert_eq!(
            error_entry.schema, None,
            "schema should be absent when the JSON is not an object"
        );
    }

    #[tokio::test]
    async fn load_paginated_items() {
        let (store, _dir) = make_store();
        store.create("pag", "P".into(), None).await.unwrap();
        for i in 0..5 {
            // &[...] temporary lives to end-of-statement, safely covering .await.
            store
                .add_items("pag", vec![make_item(&format!("item-{i}"))], false)
                .await
                .unwrap();
        }
        let p = Pagination { from: 1, size: 2 };
        let (journal, total) = store.load("pag", &p, false).await.unwrap();
        assert_eq!(total, 5);
        assert_eq!(journal.items.len(), 2);
        assert_eq!(journal.items[0].content, "item-1");
        assert_eq!(journal.items[1].content, "item-2");
    }

    #[test]
    fn default_dir_expands_tilde_in_foray_home() {
        let fake_home = tempfile::tempdir().unwrap();
        let foray_home = fake_home.path().join("foray-root");
        std::fs::create_dir_all(foray_home.join("journals")).unwrap();
        let _env = crate::paths::TestForayHomeEnv::with(fake_home.path(), "~/foray-root");
        let resolved = JsonFileStore::default_dir().unwrap();
        assert_eq!(resolved, foray_home.join("journals"));
    }
}
