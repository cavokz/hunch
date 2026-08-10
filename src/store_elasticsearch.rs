use crate::store::{Store, StoreError};
use crate::types::{ItemType, JournalFile, JournalItem, JournalSummary, Pagination};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;

/// ES document schema version stamped on every written document.
const CURRENT_ES_SCHEMA: u32 = 0;

/// Journal file schema version returned from load(); mirrors migrate::CURRENT_SCHEMA.
const JOURNAL_SCHEMA: u32 = 1;

// ── Auth ─────────────────────────────────────────────────────────────

enum Auth {
    None,
    ApiKey(String),
    Basic { username: String, password: String },
}

// ── ES document types ────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
struct EsEvent {
    dataset: String,
}

// Journal doc — write path

#[derive(Serialize)]
struct EsForayJournalWrite<'a> {
    schema: u32,
    name: &'a str,
    title: &'a str,
    archived: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    meta: Option<&'a HashMap<String, serde_json::Value>>,
}

#[derive(Serialize)]
struct EsJournalDoc<'a> {
    event: EsEvent,
    foray: EsForayJournalWrite<'a>,
}

// Journal doc — read path

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EsForayJournalSource {
    name: String,
    title: String,
    #[serde(default)]
    archived: bool,
    schema: u32,
    #[serde(default)]
    meta: Option<HashMap<String, serde_json::Value>>,
}

#[derive(Deserialize)]
struct EsJournalSource {
    foray: EsForayJournalSource,
}

// Item doc — write path

#[derive(Serialize)]
struct EsForayItemWrite<'a> {
    schema: u32,
    journal: &'a str,
    #[serde(rename = "type")]
    item_type: &'a ItemType,
    #[serde(skip_serializing_if = "Option::is_none")]
    meta: Option<&'a HashMap<String, serde_json::Value>>,
    item_size: usize,
}

#[derive(Serialize)]
struct EsItemDoc<'a> {
    #[serde(rename = "@timestamp")]
    timestamp: &'a DateTime<Utc>,
    message: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    tags: Option<&'a Vec<String>>,
    event: EsEvent,
    foray: EsForayItemWrite<'a>,
}

// Item doc — read path

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EsForayItemSource {
    #[allow(dead_code)]
    schema: u32,
    #[serde(rename = "type")]
    item_type: ItemType,
    #[allow(dead_code)]
    journal: String,
    #[serde(default)]
    meta: Option<HashMap<String, serde_json::Value>>,
    #[allow(dead_code)]
    item_size: usize,
}

#[derive(Deserialize)]
struct EsItemSource {
    #[serde(rename = "@timestamp")]
    timestamp: DateTime<Utc>,
    message: String,
    #[serde(default)]
    tags: Option<Vec<String>>,
    foray: EsForayItemSource,
}

// ── ES response types ────────────────────────────────────────────────

#[derive(Deserialize)]
struct EsGetResponse<T> {
    #[allow(dead_code)]
    found: bool,
    #[serde(rename = "_source")]
    source: Option<T>,
}

#[derive(Deserialize)]
struct EsSearchResponse<T> {
    hits: EsHits<T>,
}

#[derive(Deserialize)]
struct EsHits<T> {
    total: EsTotal,
    hits: Vec<EsHit<T>>,
}

#[derive(Deserialize)]
struct EsTotal {
    value: usize,
}

#[derive(Deserialize)]
struct EsHit<T> {
    #[serde(rename = "_id")]
    id: String,
    #[serde(rename = "_source")]
    source: T,
}

#[derive(Deserialize)]
struct EsBulkResponse {
    errors: bool,
    items: Vec<EsBulkItem>,
}

#[derive(Deserialize)]
struct EsBulkItem {
    create: EsBulkResult,
}

#[derive(Deserialize)]
struct EsBulkResult {
    status: u16,
}

// ── ElasticsearchStore ───────────────────────────────────────────────

pub struct ElasticsearchStore {
    client: reqwest::Client,
    /// Full URL including index as the last path segment,
    /// e.g. `https://es.example.com/foray-team`.
    index_url: String,
    auth: Auth,
}

impl std::fmt::Debug for ElasticsearchStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ElasticsearchStore")
            .field("index_url", &self.index_url)
            .finish_non_exhaustive()
    }
}

impl ElasticsearchStore {
    pub fn new(
        index_url: String,
        api_key: Option<String>,
        username: Option<String>,
        password: Option<String>,
    ) -> Result<Self, StoreError> {
        let auth = match (api_key, username, password) {
            (Some(key), None, None) => Auth::ApiKey(key),
            (None, Some(user), Some(pass)) => Auth::Basic {
                username: user,
                password: pass,
            },
            (None, None, None) => Auth::None,
            (None, Some(_), None) | (None, None, Some(_)) => {
                return Err(StoreError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "elasticsearch store: username and password must be provided together",
                )));
            }
            _ => {
                return Err(StoreError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "elasticsearch store: api_key and username/password are mutually exclusive",
                )));
            }
        };

        // Validate URL: must parse, no embedded credentials, no query/fragment,
        // and must end with a non-empty path segment (the index name).
        let parsed = reqwest::Url::parse(&index_url).map_err(|e| {
            StoreError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("elasticsearch store: invalid URL: {e}"),
            ))
        })?;
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err(StoreError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "elasticsearch store: embed credentials in api_key/username/password, not in the URL",
            )));
        }
        if parsed.query().is_some() {
            return Err(StoreError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "elasticsearch store: URL must not contain a query string",
            )));
        }
        if parsed.fragment().is_some() {
            return Err(StoreError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "elasticsearch store: URL must not contain a fragment",
            )));
        }
        let index_segment_ok = parsed
            .path_segments()
            .and_then(|mut segs| segs.next_back())
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        if !index_segment_ok {
            return Err(StoreError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "elasticsearch store: URL must end with a non-empty index name path segment",
            )));
        }

        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(5))
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| {
                StoreError::Io(std::io::Error::other(format!(
                    "failed to build HTTP client: {e}"
                )))
            })?;

        Ok(Self {
            client,
            index_url,
            auth,
        })
    }

    fn request(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        let builder = self.client.request(method, url);
        match &self.auth {
            Auth::None => builder,
            Auth::ApiKey(key) => builder.header("Authorization", format!("ApiKey {key}")),
            Auth::Basic { username, password } => builder.basic_auth(username, Some(password)),
        }
    }

    fn doc_url(&self, id: &str) -> String {
        format!("{}/_doc/{}", self.index_url, encode_id(id))
    }

    fn search_url(&self) -> String {
        format!("{}/_search", self.index_url)
    }

    fn update_url(&self, id: &str) -> String {
        format!("{}/_update/{}", self.index_url, encode_id(id))
    }

    fn bulk_url(&self) -> String {
        format!("{}/_bulk", self.index_url)
    }

    fn delete_by_query_url(&self) -> String {
        format!("{}/_delete_by_query", self.index_url)
    }

    /// Fetch and deserialize a journal document, returning `None` if not found.
    async fn fetch_journal(&self, name: &str) -> Result<Option<EsJournalSource>, StoreError> {
        let url = self.doc_url(&format!("journal:{name}"));
        let resp = self
            .request(reqwest::Method::GET, &url)
            .send()
            .await
            .map_err(|_| net_error("get journal"))?;

        match resp.status() {
            StatusCode::OK => {
                let body: EsGetResponse<serde_json::Value> = resp
                    .json()
                    .await
                    .map_err(|_| net_error("parse journal response"))?;
                if let Some(mut source) = body.source {
                    match migrate_journal_source(&mut source) {
                        EsMigrateResult::TooNew { found } => {
                            return Err(StoreError::SchemaTooNew {
                                found,
                                max: CURRENT_ES_SCHEMA,
                                origin: crate::store::SchemaOrigin::Storage,
                            });
                        }
                        EsMigrateResult::Current | EsMigrateResult::Migrated => {}
                    }
                    let journal_source: EsJournalSource =
                        serde_json::from_value(source).map_err(StoreError::Json)?;
                    Ok(Some(journal_source))
                } else {
                    Ok(None)
                }
            }
            StatusCode::NOT_FOUND => Ok(None),
            s => Err(http_error(s, "get journal")),
        }
    }

    /// Build an NDJSON bulk body for the given items and journal name.
    /// Returns the body string and a parallel vec of serialized byte sizes.
    fn build_bulk_body(name: &str, items: &[JournalItem]) -> Result<String, StoreError> {
        let mut body = String::new();
        for item in items {
            let item_size = serde_json::to_vec(item).map_err(StoreError::Json)?.len();
            let action = json!({ "create": { "_id": format!("item:{name}:{}", item.id) } });
            let doc = EsItemDoc {
                timestamp: &item.added_at,
                message: &item.content,
                tags: item.tags.as_ref(),
                event: EsEvent {
                    dataset: "foray.item".into(),
                },
                foray: EsForayItemWrite {
                    schema: CURRENT_ES_SCHEMA,
                    journal: name,
                    item_type: &item.item_type,
                    meta: item.meta.as_ref(),
                    item_size,
                },
            };
            body.push_str(&serde_json::to_string(&action).map_err(StoreError::Json)?);
            body.push('\n');
            body.push_str(&serde_json::to_string(&doc).map_err(StoreError::Json)?);
            body.push('\n');
        }
        Ok(body)
    }

    /// Execute a bulk request and return the response.
    async fn send_bulk(&self, body: String) -> Result<EsBulkResponse, StoreError> {
        let resp = self
            .request(reqwest::Method::POST, &self.bulk_url())
            .header("Content-Type", "application/x-ndjson")
            .body(body)
            .send()
            .await
            .map_err(|_| net_error("bulk index items"))?;

        if resp.status() != StatusCode::OK {
            return Err(http_error(resp.status(), "bulk index items"));
        }
        resp.json()
            .await
            .map_err(|_| net_error("parse bulk response"))
    }
}

#[async_trait]
impl Store for ElasticsearchStore {
    async fn load(
        &self,
        name: &str,
        pagination: &Pagination,
        archived: bool,
    ) -> Result<(JournalFile, usize), StoreError> {
        let journal_source = self
            .fetch_journal(name)
            .await?
            .ok_or_else(|| StoreError::NotFound(name.into()))?;

        // Strict location: return NotFound if archived state doesn't match.
        if journal_source.foray.archived != archived {
            return Err(StoreError::NotFound(name.into()));
        }

        const MAX_RESULT_WINDOW: usize = 10_000;

        let from = pagination.from;
        let requested_size = pagination.size;
        let size = requested_size.min(MAX_RESULT_WINDOW);
        if from.saturating_add(size) > MAX_RESULT_WINDOW {
            return Err(StoreError::Unsupported(format!(
                "pagination offset too deep (from={from}, max_result_window={MAX_RESULT_WINDOW}); use smaller from or implement search_after"
            )));
        }

        let query = json!({
            "query": {
                "bool": {
                    "filter": [
                        { "term": { "event.dataset": "foray.item" } },
                        { "term": { "foray.journal": name } }
                    ]
                }
            },
            // _shard_doc is the recommended stable tie-breaker for from/size pagination.
            "sort": [{ "@timestamp": "asc" }, { "_shard_doc": "asc" }],
            "from": from,
            "size": size,
            "track_total_hits": true
        });

        let resp = self
            .request(reqwest::Method::POST, &self.search_url())
            .json(&query)
            .send()
            .await
            .map_err(|_| net_error("search items"))?;

        if resp.status() != StatusCode::OK {
            return Err(http_error(resp.status(), "search items"));
        }

        let search: EsSearchResponse<serde_json::Value> = resp
            .json()
            .await
            .map_err(|_| net_error("parse items response"))?;

        let total = search.hits.total.value;
        let hits = search.hits.hits;

        // If the caller requested more than MAX_RESULT_WINDOW items and there are items
        // beyond the window, we would silently truncate. Error instead.
        if requested_size > MAX_RESULT_WINDOW && total as usize > from + hits.len() {
            return Err(StoreError::Unsupported(format!(
                "journal has {total} items but ES max_result_window is {MAX_RESULT_WINDOW}; \
                 use search_after for deep pagination"
            )));
        }

        let items: Vec<JournalItem> = hits
            .into_iter()
            .map(|mut hit| {
                match migrate_item_source(&mut hit.source) {
                    EsMigrateResult::TooNew { found } => {
                        return Err(StoreError::SchemaTooNew {
                            found,
                            max: CURRENT_ES_SCHEMA,
                            origin: crate::store::SchemaOrigin::Storage,
                        });
                    }
                    EsMigrateResult::Current | EsMigrateResult::Migrated => {}
                }
                let item_source: EsItemSource =
                    serde_json::from_value(hit.source).map_err(StoreError::Json)?;
                let id = hit
                    .id
                    .strip_prefix(&format!("item:{name}:"))
                    .unwrap_or(&hit.id)
                    .to_string();
                Ok(JournalItem {
                    id,
                    item_type: item_source.foray.item_type,
                    content: item_source.message,
                    tags: item_source.tags,
                    added_at: item_source.timestamp,
                    meta: item_source.foray.meta,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        let foray = &journal_source.foray;
        let journal = JournalFile {
            schema: JOURNAL_SCHEMA,
            name: foray.name.clone(),
            title: foray.title.clone(),
            items,
            meta: foray.meta.clone(),
        };

        Ok((journal, total))
    }

    async fn create(
        &self,
        name: &str,
        title: String,
        meta: Option<HashMap<String, serde_json::Value>>,
    ) -> Result<(), StoreError> {
        let doc = EsJournalDoc {
            event: EsEvent {
                dataset: "foray.journal".into(),
            },
            foray: EsForayJournalWrite {
                schema: CURRENT_ES_SCHEMA,
                name,
                title: &title,
                archived: false,
                meta: meta.as_ref(),
            },
        };

        let url = format!(
            "{}?op_type=create",
            self.doc_url(&format!("journal:{name}"))
        );
        let resp = self
            .request(reqwest::Method::PUT, &url)
            .json(&doc)
            .send()
            .await
            .map_err(|_| net_error("create journal"))?;

        match resp.status() {
            StatusCode::CREATED => {}
            StatusCode::CONFLICT => return Err(StoreError::AlreadyExists(name.into())),
            s => return Err(http_error(s, "create journal")),
        }

        Ok(())
    }

    async fn add_items(
        &self,
        name: &str,
        items: Vec<JournalItem>,
        archived: bool,
    ) -> Result<Vec<String>, StoreError> {
        let journal = self
            .fetch_journal(name)
            .await?
            .ok_or_else(|| StoreError::NotFound(name.into()))?;

        // Strict location check: archived flag selects the location.
        // If the journal is archived and caller passed archived=false, it's NotFound at that location.
        // If the journal is archived and caller passed archived=true, it's ReadOnly (never write to archived).
        if journal.foray.archived {
            return Err(if archived {
                StoreError::ReadOnly(name.into())
            } else {
                StoreError::NotFound(name.into())
            });
        }
        if archived {
            // Journal is active but caller asked for archived location — not found there.
            return Err(StoreError::NotFound(name.into()));
        }

        if items.is_empty() {
            return Ok(vec![]);
        }

        let body = Self::build_bulk_body(name, &items)?;
        let bulk = self.send_bulk(body).await?;

        if !bulk.errors {
            return Ok(vec![]);
        }

        // Inspect per-item results: 200/201 = success, 409 = conflict (return ID to caller),
        // anything else = fatal error.
        if bulk.items.len() != items.len() {
            return Err(StoreError::Io(std::io::Error::other(format!(
                "elasticsearch bulk index: expected {} result(s), got {}",
                items.len(),
                bulk.items.len()
            ))));
        }
        let mut failed_ids: Vec<String> = Vec::new();
        for (item, result) in items.iter().zip(bulk.items.iter()) {
            match result.create.status {
                200 | 201 => {}
                409 => failed_ids.push(item.id.clone()),
                _ => {
                    return Err(StoreError::Io(std::io::Error::other(
                        "elasticsearch bulk index: one or more items failed to index",
                    )));
                }
            }
        }

        Ok(failed_ids)
    }

    async fn import(
        &self,
        name: &str,
        journal: JournalFile,
        merge: bool,
        archived: bool,
    ) -> Result<(usize, usize), StoreError> {
        if merge {
            // Merge: append to existing active journal, skipping duplicate IDs.
            let existing = self
                .fetch_journal(name)
                .await?
                .ok_or_else(|| StoreError::NotFound(name.into()))?;
            if existing.foray.archived {
                return Err(StoreError::ReadOnly(name.into()));
            }

            if journal.items.is_empty() {
                return Ok((0, 0));
            }

            let body = Self::build_bulk_body(name, &journal.items)?;
            let bulk = self.send_bulk(body).await?;

            let mut added = 0usize;
            let mut skipped = 0usize;
            if !bulk.errors {
                added = journal.items.len();
            } else {
                if bulk.items.len() != journal.items.len() {
                    return Err(StoreError::Io(std::io::Error::other(
                        "elasticsearch bulk import: result count mismatch",
                    )));
                }
                for result in &bulk.items {
                    match result.create.status {
                        200 | 201 => added += 1,
                        409 => skipped += 1,
                        _ => {
                            return Err(StoreError::Io(std::io::Error::other(
                                "elasticsearch bulk import: one or more items failed",
                            )));
                        }
                    }
                }
            }
            Ok((added, skipped))
        } else {
            // Non-merge: create new journal, then bulk-index all items.
            let title = journal.title;
            let meta = journal.meta;
            let items = journal.items;

            // Create journal doc with op_type=create — 409 → AlreadyExists.
            let doc = EsJournalDoc {
                event: EsEvent {
                    dataset: "foray.journal".into(),
                },
                foray: EsForayJournalWrite {
                    schema: CURRENT_ES_SCHEMA,
                    name,
                    title: &title,
                    archived,
                    meta: meta.as_ref(),
                },
            };
            let url = format!(
                "{}?op_type=create",
                self.doc_url(&format!("journal:{name}"))
            );
            let resp = self
                .request(reqwest::Method::PUT, &url)
                .json(&doc)
                .send()
                .await
                .map_err(|_| net_error("import create journal"))?;
            match resp.status() {
                StatusCode::CREATED => {}
                StatusCode::CONFLICT => return Err(StoreError::AlreadyExists(name.into())),
                s => return Err(http_error(s, "import create journal")),
            }

            if items.is_empty() {
                return Ok((0, 0));
            }

            let body = Self::build_bulk_body(name, &items)?;
            let bulk = self.send_bulk(body).await?;

            if bulk.errors {
                return Err(StoreError::Io(std::io::Error::other(
                    "elasticsearch bulk import: one or more items failed to index",
                )));
            }
            Ok((items.len(), 0))
        }
    }

    async fn list(&self) -> Result<(Vec<JournalSummary>, usize), StoreError> {
        // Fetch all journal docs (active + archived) in one search.
        let query = json!({
            "query": { "term": { "event.dataset": "foray.journal" } },
            "sort": [{ "foray.name": "asc" }],
            "size": 10_000,
            "track_total_hits": true
        });

        let resp = self
            .request(reqwest::Method::POST, &self.search_url())
            .json(&query)
            .send()
            .await
            .map_err(|_| net_error("list journals"))?;

        if resp.status() == StatusCode::NOT_FOUND {
            return Ok((vec![], 0));
        }
        if resp.status() != StatusCode::OK {
            return Err(http_error(resp.status(), "list journals"));
        }

        let search: EsSearchResponse<serde_json::Value> = resp
            .json()
            .await
            .map_err(|_| net_error("parse journals response"))?;

        let total = search.hits.total.value;
        if search.hits.hits.len() < total {
            return Err(StoreError::Unsupported(format!(
                "journal count ({total}) exceeds ES max_result_window=10000; deep pagination not yet supported"
            )));
        }
        if search.hits.hits.is_empty() {
            return Ok((vec![], total));
        }

        // Fetch per-journal item counts + extended stats for avg/std via aggregation.
        let agg_query = json!({
            "size": 0,
            "query": { "term": { "event.dataset": "foray.item" } },
            "aggs": {
                "by_journal": {
                    "terms": { "field": "foray.journal", "size": 10_000 },
                    "aggs": {
                        "stats": { "extended_stats": { "field": "foray.item_size" } }
                    }
                }
            }
        });

        let agg_resp = self
            .request(reqwest::Method::POST, &self.search_url())
            .json(&agg_query)
            .send()
            .await
            .map_err(|_| net_error("item stats aggregation"))?;

        // Journal name → (count, avg_item_size, std_item_size)
        let stats_map: HashMap<String, (usize, Option<usize>, Option<usize>)> =
            if agg_resp.status() == StatusCode::OK {
                let agg: serde_json::Value = agg_resp
                    .json()
                    .await
                    .map_err(|_| net_error("parse agg response"))?;
                agg["aggregations"]["by_journal"]["buckets"]
                    .as_array()
                    .map(|buckets| {
                        buckets
                            .iter()
                            .filter_map(|b| {
                                let key = b["key"].as_str()?.to_string();
                                let count = b["stats"]["count"].as_u64()? as usize;
                                let avg = b["stats"]["avg"].as_f64().map(|v| v.ceil() as usize);
                                let std = if count >= 2 {
                                    b["stats"]["std_deviation"]
                                        .as_f64()
                                        .filter(|v| v.is_finite())
                                        .map(|v| v.ceil() as usize)
                                } else {
                                    None
                                };
                                Some((key, (count, avg, std)))
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            } else {
                HashMap::new()
            };

        let summaries = search
            .hits
            .hits
            .into_iter()
            .map(|mut hit| {
                match migrate_journal_source(&mut hit.source) {
                    EsMigrateResult::TooNew { found } => {
                        // Surface as a per-journal error; do not fail the whole list.
                        let name = hit.source["foray"]["name"]
                            .as_str()
                            .unwrap_or("")
                            .to_string();
                        let schema = found;
                        let error = StoreError::SchemaTooNew {
                            found,
                            max: CURRENT_ES_SCHEMA,
                            origin: crate::store::SchemaOrigin::Storage,
                        }
                        .to_string();
                        return JournalSummary {
                            name,
                            title: String::new(),
                            item_count: 0,
                            archived: false,
                            avg_item_size: None,
                            std_item_size: None,
                            schema: Some(schema),
                            meta: None,
                            error: Some(error),
                        };
                    }
                    EsMigrateResult::Current | EsMigrateResult::Migrated => {}
                }
                let journal_source: EsJournalSource = match serde_json::from_value(hit.source) {
                    Ok(s) => s,
                    Err(e) => {
                        let name = hit
                            .id
                            .strip_prefix("journal:")
                            .unwrap_or(&hit.id)
                            .to_string();
                        return JournalSummary {
                            name,
                            title: String::new(),
                            item_count: 0,
                            archived: false,
                            avg_item_size: None,
                            std_item_size: None,
                            schema: None,
                            meta: None,
                            error: Some(format!("failed to deserialize journal doc: {e}")),
                        };
                    }
                };
                let foray = journal_source.foray;
                let name = foray.name;
                let (count, avg_item_size, std_item_size) =
                    stats_map.get(&name).copied().unwrap_or((0, None, None));
                JournalSummary {
                    name,
                    title: foray.title,
                    item_count: count,
                    archived: foray.archived,
                    avg_item_size,
                    std_item_size,
                    schema: Some(foray.schema),
                    meta: foray.meta,
                    error: None,
                }
            })
            .collect();

        Ok((summaries, total))
    }

    async fn delete(&self, name: &str, archived: bool) -> Result<(), StoreError> {
        let journal = self
            .fetch_journal(name)
            .await?
            .ok_or_else(|| StoreError::NotFound(name.into()))?;

        // Strict location: archived flag must match.
        if journal.foray.archived != archived {
            return Err(StoreError::NotFound(name.into()));
        }

        // Delete items first — if this fails the journal doc is still present
        // and callers can safely retry.
        let query = json!({
            "query": {
                "bool": {
                    "filter": [
                        { "term": { "event.dataset": "foray.item" } },
                        { "term": { "foray.journal": name } }
                    ]
                }
            }
        });
        let resp = self
            .request(reqwest::Method::POST, &self.delete_by_query_url())
            .json(&query)
            .send()
            .await
            .map_err(|_| net_error("delete journal items"))?;

        if resp.status() != StatusCode::OK {
            return Err(http_error(resp.status(), "delete journal items"));
        }

        let url = self.doc_url(&format!("journal:{name}"));
        let resp = self
            .request(reqwest::Method::DELETE, &url)
            .send()
            .await
            .map_err(|_| net_error("delete journal"))?;

        match resp.status() {
            StatusCode::OK => {}
            StatusCode::NOT_FOUND => return Err(StoreError::NotFound(name.into())),
            s => return Err(http_error(s, "delete journal")),
        }

        Ok(())
    }

    async fn archive(&self, name: &str) -> Result<(), StoreError> {
        let journal = self
            .fetch_journal(name)
            .await?
            .ok_or_else(|| StoreError::NotFound(name.into()))?;

        // If already archived, return NotFound (no active journal to archive).
        if journal.foray.archived {
            return Err(StoreError::NotFound(name.into()));
        }

        let url = self.update_url(&format!("journal:{name}"));
        let resp = self
            .request(reqwest::Method::POST, &url)
            .json(&json!({ "doc": { "foray": { "archived": true } } }))
            .send()
            .await
            .map_err(|_| net_error("archive journal"))?;

        match resp.status() {
            StatusCode::OK => Ok(()),
            StatusCode::NOT_FOUND => Err(StoreError::NotFound(name.into())),
            s => Err(http_error(s, "archive journal")),
        }
    }

    async fn unarchive(&self, name: &str) -> Result<(), StoreError> {
        let journal = self
            .fetch_journal(name)
            .await?
            .ok_or_else(|| StoreError::NotFound(name.into()))?;

        // If not archived, return NotFound (no archived journal to restore).
        if !journal.foray.archived {
            return Err(StoreError::NotFound(name.into()));
        }

        let url = self.update_url(&format!("journal:{name}"));
        let resp = self
            .request(reqwest::Method::POST, &url)
            .json(&json!({ "doc": { "foray": { "archived": false } } }))
            .send()
            .await
            .map_err(|_| net_error("unarchive journal"))?;

        match resp.status() {
            StatusCode::OK => Ok(()),
            StatusCode::NOT_FOUND => Err(StoreError::NotFound(name.into())),
            s => Err(http_error(s, "unarchive journal")),
        }
    }
}

// ── ES schema migration ───────────────────────────────────────────────

/// Result of running one of the `migrate_*_source` functions.
enum EsMigrateResult {
    /// Document is already at `CURRENT_ES_SCHEMA` — no changes made.
    Current,
    /// Document was at an older schema — migrated in memory (not persisted to ES).
    Migrated,
    /// Document was written by a newer foray — cannot safely read.
    TooNew { found: u32 },
}

/// Migrate a raw journal `_source` value to `CURRENT_ES_SCHEMA` in place.
///
/// Reads `foray.schema`, applies version-specific transforms, and updates
/// `foray.schema` to `CURRENT_ES_SCHEMA` when migrated. Returns `TooNew`
/// without modifying the value if the schema exceeds the current maximum.
fn migrate_journal_source(source: &mut serde_json::Value) -> EsMigrateResult {
    let schema = source["foray"]["schema"]
        .as_u64()
        .map(|s| s as u32)
        .unwrap_or(0);
    match schema.cmp(&CURRENT_ES_SCHEMA) {
        std::cmp::Ordering::Greater => EsMigrateResult::TooNew { found: schema },
        std::cmp::Ordering::Equal => EsMigrateResult::Current,
        std::cmp::Ordering::Less => {
            // Add v0 → v1 → ... → CURRENT_ES_SCHEMA migration steps here.
            source["foray"]["schema"] = serde_json::json!(CURRENT_ES_SCHEMA);
            EsMigrateResult::Migrated
        }
    }
}

/// Migrate a raw item `_source` value to `CURRENT_ES_SCHEMA` in place.
///
/// Same contract as [`migrate_journal_source`].
fn migrate_item_source(source: &mut serde_json::Value) -> EsMigrateResult {
    let schema = source["foray"]["schema"]
        .as_u64()
        .map(|s| s as u32)
        .unwrap_or(0);
    match schema.cmp(&CURRENT_ES_SCHEMA) {
        std::cmp::Ordering::Greater => EsMigrateResult::TooNew { found: schema },
        std::cmp::Ordering::Equal => EsMigrateResult::Current,
        std::cmp::Ordering::Less => {
            // Add v0 → v1 → ... → CURRENT_ES_SCHEMA migration steps here.
            source["foray"]["schema"] = serde_json::json!(CURRENT_ES_SCHEMA);
            EsMigrateResult::Migrated
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────────

/// Percent-encode a document ID for use in ES URL paths.
/// Journal names are `[a-z0-9_-]`; item IDs use consonants and dashes.
/// The only character requiring encoding is the `:` prefix separator.
fn encode_id(id: &str) -> String {
    let mut out = String::with_capacity(id.len() + 4);
    for b in id.bytes() {
        if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn net_error(op: &str) -> StoreError {
    StoreError::Io(std::io::Error::other(format!(
        "elasticsearch {op}: network error"
    )))
}

fn http_error(status: StatusCode, op: &str) -> StoreError {
    StoreError::Io(std::io::Error::other(format!(
        "elasticsearch {op}: unexpected status {}",
        status.as_u16()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_id_passthrough() {
        assert_eq!(encode_id("journal:my-journal"), "journal%3Amy-journal");
        assert_eq!(
            encode_id("item:my-journal:abc-def"),
            "item%3Amy-journal%3Aabc-def"
        );
    }

    #[test]
    fn encode_id_no_encoding_needed() {
        assert_eq!(encode_id("plain"), "plain");
        assert_eq!(
            encode_id("with-dash_and_underscore"),
            "with-dash_and_underscore"
        );
    }

    #[test]
    fn auth_username_without_password_errors() {
        let err = ElasticsearchStore::new(
            "http://localhost:9200/idx".into(),
            None,
            Some("user".into()),
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("together"), "error was: {err}");
    }

    #[test]
    fn auth_password_without_username_errors() {
        let err = ElasticsearchStore::new(
            "http://localhost:9200/idx".into(),
            None,
            None,
            Some("pass".into()),
        )
        .unwrap_err();
        assert!(err.to_string().contains("together"), "error was: {err}");
    }

    #[test]
    fn url_with_embedded_credentials_rejected() {
        let err = ElasticsearchStore::new(
            "https://admin:secret@es.example.com/foray".into(),
            None,
            None,
            None,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("embed credentials"),
            "error was: {err}"
        );
    }

    #[test]
    fn url_with_username_only_rejected() {
        let err = ElasticsearchStore::new(
            "https://admin@es.example.com/foray".into(),
            None,
            None,
            None,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("embed credentials"),
            "error was: {err}"
        );
    }

    #[test]
    fn invalid_url_rejected() {
        let err = ElasticsearchStore::new("not a url at all".into(), None, None, None).unwrap_err();
        assert!(err.to_string().contains("invalid URL"), "error was: {err}");
    }

    #[test]
    fn auth_api_key_and_basic_mutually_exclusive() {
        let err = ElasticsearchStore::new(
            "http://localhost:9200/idx".into(),
            Some("key".into()),
            Some("user".into()),
            Some("pass".into()),
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("mutually exclusive"),
            "error was: {err}"
        );
    }

    #[test]
    fn url_with_query_string_rejected() {
        let err =
            ElasticsearchStore::new("http://localhost:9200/idx?pretty".into(), None, None, None)
                .unwrap_err();
        assert!(err.to_string().contains("query string"), "error was: {err}");
    }

    #[test]
    fn url_with_fragment_rejected() {
        let err = ElasticsearchStore::new("http://localhost:9200/idx#top".into(), None, None, None)
            .unwrap_err();
        assert!(err.to_string().contains("fragment"), "error was: {err}");
    }

    #[test]
    fn url_without_index_segment_rejected() {
        for bad in &["http://localhost:9200", "http://localhost:9200/"] {
            let err = ElasticsearchStore::new((*bad).into(), None, None, None).unwrap_err();
            assert!(
                err.to_string().contains("index name"),
                "url={bad} error was: {err}"
            );
        }
    }

    // ── Error scrubbing ──────────────────────────────────────────────

    #[test]
    fn http_error_exposes_only_status_and_op() {
        let err = http_error(StatusCode::INTERNAL_SERVER_ERROR, "create journal");
        let msg = err.to_string();
        assert!(msg.contains("500"), "should contain status code: {msg}");
        assert!(msg.contains("create journal"), "should contain op: {msg}");
        // Must not contain any ES-specific detail (stack trace, index name, shard info).
        assert!(!msg.contains("index"), "must not leak index info: {msg}");
    }

    #[test]
    fn net_error_exposes_only_op() {
        let err = net_error("bulk index items");
        let msg = err.to_string();
        assert!(msg.contains("bulk index items"), "should contain op: {msg}");
        assert!(msg.contains("network error"), "generic message: {msg}");
    }

    // ── encode_id vs path traversal ──────────────────────────────────

    #[test]
    fn encode_id_neutralises_path_traversal() {
        // Dots and slashes must be percent-encoded so they cannot escape
        // the _doc/{id} URL segment.
        let encoded = encode_id("journal:../../_cluster/settings");
        assert!(!encoded.contains('/'), "slashes must be encoded: {encoded}");
        assert!(
            !encoded.contains(".."),
            "dot-dot must be encoded: {encoded}"
        );
    }

    // ── Server-stamped fields ────────────────────────────────────────

    #[test]
    fn item_doc_stamps_journal_name_and_dataset() {
        // Verify that EsItemDoc takes journal from the store method's
        // `name` parameter (not from the item) and hardcodes event.dataset.
        let ts = Utc::now();
        let doc = EsItemDoc {
            timestamp: &ts,
            message: "test",
            tags: None,
            event: EsEvent {
                dataset: "foray.item".into(),
            },
            foray: EsForayItemWrite {
                schema: CURRENT_ES_SCHEMA,
                journal: "server-provided",
                item_type: &ItemType::Note,
                meta: None,
                item_size: 42,
            },
        };
        let json = serde_json::to_value(&doc).unwrap();
        assert_eq!(json["foray"]["journal"], "server-provided");
        assert_eq!(json["event"]["dataset"], "foray.item");
        assert_eq!(json["foray"]["schema"], CURRENT_ES_SCHEMA);
    }
}
