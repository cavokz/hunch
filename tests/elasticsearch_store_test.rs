//! Online integration tests for ElasticsearchStore.
//!
//! All tests are marked `#[ignore]` and require a live Elasticsearch instance.
//! Each test creates and tears down its own uniquely-named index so tests can
//! run in parallel safely.
//!
//! Usage:
//!   ES_TEST_URL=http://localhost:9292 \
//!   ES_TEST_USER=elastic \
//!   ES_TEST_PASSWORD=changeme \
//!   cargo test --test elasticsearch_store_test -- --include-ignored

use chrono::Utc;
use foray::store::{Store, StoreError};
use foray::store_elasticsearch::ElasticsearchStore;
use foray::types::{ItemType, JournalItem, Pagination, item_id};
use rand::Rng;

// ── Test harness ─────────────────────────────────────────────────────

fn es_env() -> (String, String, String) {
    let url =
        std::env::var("ES_TEST_URL").expect("ES_TEST_URL must be set to run ES integration tests");
    let user = std::env::var("ES_TEST_USER").unwrap_or_else(|_| "elastic".into());
    let pass = std::env::var("ES_TEST_PASSWORD").unwrap_or_else(|_| "changeme".into());
    (url, user, pass)
}

struct TestIndex {
    pub store: ElasticsearchStore,
    base_url: String,
    index: String,
    user: String,
    pass: String,
}

impl TestIndex {
    fn new() -> Self {
        let (base_url, user, pass) = es_env();
        let suffix: u32 = rand::rng().random();
        let index = format!("foray-test-{suffix:08x}");
        let index_url = format!("{base_url}/{index}");
        let store =
            ElasticsearchStore::new(index_url, None, Some(user.clone()), Some(pass.clone()))
                .expect("ElasticsearchStore::new");
        Self {
            store,
            base_url,
            index,
            user,
            pass,
        }
    }

    /// Force all pending writes visible. Call after every mutation.
    async fn refresh(&self) {
        reqwest::Client::new()
            .post(format!("{}/{}/_refresh", self.base_url, self.index))
            .basic_auth(&self.user, Some(&self.pass))
            .send()
            .await
            .expect("refresh")
            .error_for_status()
            .expect("refresh returned non-success status");
    }

    /// Delete the test index.
    async fn cleanup(&self) {
        let _ = reqwest::Client::new()
            .delete(format!("{}/{}", self.base_url, self.index))
            .basic_auth(&self.user, Some(&self.pass))
            .send()
            .await;
    }
}

impl Drop for TestIndex {
    fn drop(&mut self) {
        // Best-effort cleanup: delete the test index even if a test panics.
        let url = format!("{}/{}", self.base_url, self.index);
        let user = self.user.clone();
        let pass = self.pass.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = reqwest::Client::new()
                    .delete(&url)
                    .basic_auth(&user, Some(&pass))
                    .send()
                    .await;
            });
        }
    }
}

fn item(content: &str) -> JournalItem {
    JournalItem {
        id: item_id(),
        item_type: ItemType::Note,
        content: content.to_string(),
        tags: None,
        added_at: Utc::now(),
        meta: None,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires ES_TEST_URL"]
async fn es_create_and_load() {
    let t = TestIndex::new();

    t.store
        .create("my-journal", "My Journal".into(), None)
        .await
        .expect("create");
    t.refresh().await;

    let (loaded, count) = t
        .store
        .load("my-journal", &Pagination::all(), false)
        .await
        .expect("load");
    assert_eq!(loaded.name, "my-journal");
    assert_eq!(loaded.title, "My Journal");
    assert_eq!(count, 0);
    assert!(loaded.items.is_empty());

    t.cleanup().await;
}

#[tokio::test]
#[ignore = "requires ES_TEST_URL"]
async fn es_create_duplicate_errors() {
    let t = TestIndex::new();

    t.store
        .create("dup", "D".into(), None)
        .await
        .expect("first create");

    let err = t.store.create("dup", "D".into(), None).await.unwrap_err();
    assert!(
        matches!(err, StoreError::AlreadyExists(_)),
        "expected AlreadyExists, got {err:?}"
    );

    t.cleanup().await;
}

#[tokio::test]
#[ignore = "requires ES_TEST_URL"]
async fn es_load_not_found() {
    let t = TestIndex::new();

    let err = t
        .store
        .load("no-such", &Pagination::all(), false)
        .await
        .unwrap_err();
    assert!(
        matches!(err, StoreError::NotFound(_)),
        "expected NotFound, got {err:?}"
    );

    t.cleanup().await;
}

#[tokio::test]
#[ignore = "requires ES_TEST_URL"]
async fn es_add_items_and_load() {
    let t = TestIndex::new();

    t.store
        .create("items-test", "Items Test".into(), None)
        .await
        .expect("create");
    t.refresh().await;

    let failed = t
        .store
        .add_items(
            "items-test",
            vec![item("first"), item("second"), item("third")],
            false,
        )
        .await
        .expect("add_items");
    assert!(failed.is_empty(), "no items should fail: {failed:?}");
    t.refresh().await;

    let (loaded, total) = t
        .store
        .load("items-test", &Pagination::all(), false)
        .await
        .expect("load");
    assert_eq!(total, 3);
    assert_eq!(loaded.items.len(), 3);
    let contents: Vec<&str> = loaded.items.iter().map(|i| i.content.as_str()).collect();
    assert!(contents.contains(&"first"));
    assert!(contents.contains(&"second"));
    assert!(contents.contains(&"third"));

    t.cleanup().await;
}

#[tokio::test]
#[ignore = "requires ES_TEST_URL"]
async fn es_list() {
    let t = TestIndex::new();

    for name in ["alpha", "beta", "gamma"] {
        t.store
            .create(name, name.into(), None)
            .await
            .expect("create");
    }
    assert!(
        t.store
            .add_items("beta", vec![item("x"), item("y")], false)
            .await
            .expect("add_items")
            .is_empty(),
        "no items should fail"
    );
    t.refresh().await;

    let (summaries, total) = t.store.list().await.expect("list");
    // total includes all journals (active + archived)
    assert!(total >= 3);
    let active: Vec<_> = summaries.iter().filter(|s| !s.archived).collect();
    assert_eq!(active.len(), 3);

    let beta = summaries.iter().find(|s| s.name == "beta").expect("beta");
    assert_eq!(beta.item_count, 2);

    t.cleanup().await;
}

#[tokio::test]
#[ignore = "requires ES_TEST_URL"]
async fn es_list_excludes_archived() {
    let t = TestIndex::new();

    for name in ["active", "archived-one"] {
        t.store
            .create(name, name.into(), None)
            .await
            .expect("create");
    }
    t.store.archive("archived-one").await.expect("archive");
    t.refresh().await;

    let (summaries, _) = t.store.list().await.expect("list");
    let active: Vec<_> = summaries.iter().filter(|s| !s.archived).collect();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].name, "active");

    let archived: Vec<_> = summaries.iter().filter(|s| s.archived).collect();
    assert_eq!(archived.len(), 1);
    assert_eq!(archived[0].name, "archived-one");

    t.cleanup().await;
}

#[tokio::test]
#[ignore = "requires ES_TEST_URL"]
async fn es_delete() {
    let t = TestIndex::new();

    t.store
        .create("to-delete", "To Delete".into(), None)
        .await
        .expect("create");
    assert!(
        t.store
            .add_items("to-delete", vec![item("item1"), item("item2")], false)
            .await
            .expect("add_items")
            .is_empty(),
        "no items should fail"
    );
    t.refresh().await;

    t.store.delete("to-delete", false).await.expect("delete");
    t.refresh().await;

    let err = t
        .store
        .load("to-delete", &Pagination::all(), false)
        .await
        .unwrap_err();
    assert!(
        matches!(err, StoreError::NotFound(_)),
        "expected NotFound after delete, got {err:?}"
    );

    t.cleanup().await;
}

#[tokio::test]
#[ignore = "requires ES_TEST_URL"]
async fn es_delete_not_found() {
    let t = TestIndex::new();

    let err = t.store.delete("no-such", false).await.unwrap_err();
    assert!(
        matches!(err, StoreError::NotFound(_)),
        "expected NotFound, got {err:?}"
    );

    t.cleanup().await;
}

#[tokio::test]
#[ignore = "requires ES_TEST_URL"]
async fn es_archive_and_unarchive() {
    let t = TestIndex::new();

    t.store
        .create("archivable", "Archivable".into(), None)
        .await
        .expect("create");
    t.refresh().await;

    // Archive it.
    t.store.archive("archivable").await.expect("archive");
    t.refresh().await;

    let (summaries, _) = t.store.list().await.expect("list after archive");
    let active: Vec<_> = summaries.iter().filter(|s| !s.archived).collect();
    assert!(
        !active.iter().any(|s| s.name == "archivable"),
        "archived journal should not appear in active list"
    );
    let archived: Vec<_> = summaries.iter().filter(|s| s.archived).collect();
    assert!(
        archived.iter().any(|s| s.name == "archivable"),
        "archived journal should appear in archived list"
    );

    // Unarchive — should come back to active list.
    t.store.unarchive("archivable").await.expect("unarchive");
    t.refresh().await;

    let (summaries, _) = t.store.list().await.expect("list after unarchive");
    let active: Vec<_> = summaries.iter().filter(|s| !s.archived).collect();
    assert!(
        active.iter().any(|s| s.name == "archivable"),
        "unarchived journal should appear in active list"
    );

    t.cleanup().await;
}

#[tokio::test]
#[ignore = "requires ES_TEST_URL"]
async fn es_archive_not_found() {
    let t = TestIndex::new();

    let err = t.store.archive("no-such").await.unwrap_err();
    assert!(
        matches!(err, StoreError::NotFound(_)),
        "expected NotFound, got {err:?}"
    );

    t.cleanup().await;
}

#[tokio::test]
#[ignore = "requires ES_TEST_URL"]
async fn es_create_with_items() {
    let t = TestIndex::new();

    t.store
        .create("with-items", "Has Items".into(), None)
        .await
        .expect("create");
    assert!(
        t.store
            .add_items("with-items", vec![item("alpha"), item("beta")], false)
            .await
            .expect("add_items")
            .is_empty(),
        "no items should fail"
    );
    t.refresh().await;

    let (loaded, total) = t
        .store
        .load("with-items", &Pagination::all(), false)
        .await
        .expect("load");
    assert_eq!(loaded.title, "Has Items");
    assert_eq!(total, 2);
    assert_eq!(loaded.items.len(), 2);
    let contents: Vec<&str> = loaded.items.iter().map(|i| i.content.as_str()).collect();
    assert!(contents.contains(&"alpha"));
    assert!(contents.contains(&"beta"));

    t.cleanup().await;
}

#[tokio::test]
#[ignore = "requires ES_TEST_URL"]
async fn es_add_items_to_archived_errors() {
    let t = TestIndex::new();

    t.store
        .create("frozen", "Frozen".into(), None)
        .await
        .expect("create");
    t.refresh().await;
    t.store.archive("frozen").await.expect("archive");
    t.refresh().await;

    let err = t
        .store
        .add_items("frozen", vec![item("nope")], false)
        .await
        .unwrap_err();
    assert!(
        matches!(err, StoreError::NotFound(_)),
        "expected NotFound, got {err:?}"
    );

    t.cleanup().await;
}

#[tokio::test]
#[ignore = "requires ES_TEST_URL"]
async fn es_import_non_merge_creates_journal() {
    use foray::types::JournalFile;

    let t = TestIndex::new();

    let items = vec![item("imported-one"), item("imported-two")];
    let item_ids: Vec<String> = items.iter().map(|i| i.id.clone()).collect();

    let journal = JournalFile {
        name: "imported".into(),
        title: "Imported Journal".into(),
        schema: 1,
        items,
        meta: None,
    };

    let (added, skipped) = t
        .store
        .import("imported", journal, false, false)
        .await
        .expect("import");
    assert_eq!(added, 2);
    assert_eq!(skipped, 0);
    t.refresh().await;

    // Journal and items should be loadable.
    let (loaded, total) = t
        .store
        .load("imported", &Pagination::all(), false)
        .await
        .expect("load");
    assert_eq!(loaded.title, "Imported Journal");
    assert_eq!(total, 2);
    let loaded_ids: Vec<&str> = loaded.items.iter().map(|i| i.id.as_str()).collect();
    for id in &item_ids {
        assert!(loaded_ids.contains(&id.as_str()), "missing id {id}");
    }

    t.cleanup().await;
}

#[tokio::test]
#[ignore = "requires ES_TEST_URL"]
async fn es_import_non_merge_errors_if_journal_exists() {
    use foray::types::JournalFile;

    let t = TestIndex::new();

    t.store
        .create("existing", "Existing".into(), None)
        .await
        .expect("create");
    t.refresh().await;

    let journal = JournalFile {
        name: "existing".into(),
        title: "Existing".into(),
        schema: 1,
        items: vec![item("x")],
        meta: None,
    };

    let err = t
        .store
        .import("existing", journal, false, false)
        .await
        .unwrap_err();
    assert!(
        matches!(err, StoreError::AlreadyExists(_)),
        "expected AlreadyExists, got {err:?}"
    );

    t.cleanup().await;
}

#[tokio::test]
#[ignore = "requires ES_TEST_URL"]
async fn es_import_merge_appends_and_skips_duplicates() {
    use foray::types::JournalFile;

    let t = TestIndex::new();

    // Seed: create journal with one item.
    t.store
        .create("mergeable", "Mergeable".into(), None)
        .await
        .expect("create");
    let existing_item = item("already-there");
    let existing_id = existing_item.id.clone();
    assert!(
        t.store
            .add_items("mergeable", vec![existing_item], false)
            .await
            .expect("add_items")
            .is_empty()
    );
    t.refresh().await;

    // Merge: one new item + one duplicate (same id as the existing item).
    let new_item = item("new-one");
    let dup_item = JournalItem {
        id: existing_id.clone(),
        item_type: ItemType::Note,
        content: "duplicate".into(),
        tags: None,
        added_at: Utc::now(),
        meta: None,
    };
    let journal = JournalFile {
        name: "mergeable".into(),
        title: "Mergeable".into(),
        schema: 1,
        items: vec![new_item, dup_item],
        meta: None,
    };

    let (added, skipped) = t
        .store
        .import("mergeable", journal, true, false)
        .await
        .expect("import merge");
    assert_eq!(added, 1, "one new item added");
    assert_eq!(skipped, 1, "one duplicate skipped");
    t.refresh().await;

    // Total items should be 2 (original + new), not 3.
    let (loaded, total) = t
        .store
        .load("mergeable", &Pagination::all(), false)
        .await
        .expect("load");
    assert_eq!(total, 2);
    let contents: Vec<&str> = loaded.items.iter().map(|i| i.content.as_str()).collect();
    assert!(contents.contains(&"already-there"));
    assert!(contents.contains(&"new-one"));
    assert!(
        !contents.contains(&"duplicate"),
        "dup should have been skipped"
    );

    t.cleanup().await;
}

#[tokio::test]
#[ignore = "requires ES_TEST_URL"]
async fn es_import_merge_errors_if_journal_not_found() {
    use foray::types::JournalFile;

    let t = TestIndex::new();

    let journal = JournalFile {
        name: "ghost".into(),
        title: "Ghost".into(),
        schema: 1,
        items: vec![item("x")],
        meta: None,
    };

    let err = t
        .store
        .import("ghost", journal, true, false)
        .await
        .unwrap_err();
    assert!(
        matches!(err, StoreError::NotFound(_)),
        "expected NotFound, got {err:?}"
    );

    t.cleanup().await;
}

#[tokio::test]
#[ignore = "requires ES_TEST_URL"]
async fn es_list_includes_item_stats() {
    let t = TestIndex::new();

    t.store
        .create("stats-test", "Stats Test".into(), None)
        .await
        .expect("create");
    assert!(
        t.store
            .add_items(
                "stats-test",
                vec![item("short"), item("a slightly longer content item")],
                false,
            )
            .await
            .expect("add_items")
            .is_empty()
    );
    t.refresh().await;

    let (summaries, _) = t.store.list().await.expect("list");
    let s = summaries
        .iter()
        .find(|s| s.name == "stats-test")
        .expect("stats-test in list");

    assert_eq!(s.item_count, 2);
    assert!(
        s.avg_item_size.is_some(),
        "avg_item_size should be present for 2 items"
    );
    assert!(
        s.std_item_size.is_some(),
        "std_item_size should be present for 2 items (high variance)"
    );
    let avg = s.avg_item_size.unwrap();
    assert!(avg > 0, "avg_item_size should be positive, got {avg}");

    t.cleanup().await;
}

#[tokio::test]
#[ignore = "requires ES_TEST_URL"]
async fn es_pagination() {
    let t = TestIndex::new();

    t.store
        .create("paged", "Paged".into(), None)
        .await
        .expect("create");
    let items = (0..5)
        .map(|i| item(&format!("item-{i}")))
        .collect::<Vec<_>>();
    assert!(
        t.store
            .add_items("paged", items, false)
            .await
            .expect("add_items")
            .is_empty(),
        "no items should fail"
    );
    t.refresh().await;

    let page = Pagination { from: 0, size: 2 };
    let (loaded, total) = t
        .store
        .load("paged", &page, false)
        .await
        .expect("load page 1");
    assert_eq!(total, 5);
    assert_eq!(loaded.items.len(), 2);

    t.cleanup().await;
}

#[tokio::test]
#[ignore = "requires ES_TEST_URL"]
async fn es_load_schema_too_new_errors() {
    let t = TestIndex::new();

    // Inject a journal doc with a future schema version directly into ES.
    let doc = serde_json::json!({
        "@timestamp": "2025-01-01T00:00:00Z",
        "message": "future-journal",
        "event": { "dataset": "foray.journal" },
        "foray": {
            "schema": 999,
            "name": "future-journal",
            "title": "Future Journal",
            "archived": false
        }
    });
    reqwest::Client::new()
        .put(format!(
            "{}/{}/_doc/journal:future-journal",
            t.base_url, t.index
        ))
        .basic_auth(&t.user, Some(&t.pass))
        .json(&doc)
        .send()
        .await
        .expect("inject doc")
        .error_for_status()
        .expect("inject doc status");
    t.refresh().await;

    let err = t
        .store
        .load("future-journal", &Pagination::all(), false)
        .await
        .expect_err("load should fail with SchemaTooNew");

    assert!(
        matches!(err, StoreError::SchemaTooNew { found: 999, .. }),
        "expected SchemaTooNew, got {err:?}"
    );

    t.cleanup().await;
}

#[tokio::test]
#[ignore = "requires ES_TEST_URL"]
async fn es_list_schema_too_new_sets_error() {
    let t = TestIndex::new();

    // Create one normal journal and one with a future schema.
    t.store
        .create("good-journal", "Good Journal".into(), None)
        .await
        .expect("create good");

    let doc = serde_json::json!({
        "@timestamp": "2025-01-01T00:00:00Z",
        "message": "future-journal",
        "event": { "dataset": "foray.journal" },
        "foray": {
            "schema": 999,
            "name": "future-journal",
            "title": "Future Journal",
            "archived": false
        }
    });
    reqwest::Client::new()
        .put(format!(
            "{}/{}/_doc/journal:future-journal",
            t.base_url, t.index
        ))
        .basic_auth(&t.user, Some(&t.pass))
        .json(&doc)
        .send()
        .await
        .expect("inject doc")
        .error_for_status()
        .expect("inject doc status");
    t.refresh().await;

    let (summaries, _) = t.store.list().await.expect("list should not fail");

    let good = summaries
        .iter()
        .find(|s| s.name == "good-journal")
        .expect("good-journal missing");
    assert!(good.error.is_none(), "good-journal should have no error");

    let bad = summaries
        .iter()
        .find(|s| s.name == "future-journal")
        .expect("future-journal missing");
    assert!(
        bad.error.is_some(),
        "future-journal should have an error set"
    );
    assert_eq!(bad.schema, Some(999));

    t.cleanup().await;
}
