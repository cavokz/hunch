//! Schema migration for journal files.
//!
//! The migration chain runs on raw [`serde_json::Value`] **before** serde
//! deserialization, so it can add, remove, or reshape known versioned fields
//! freely (add, remove, reshape). A `Current` or `Migrated` result means the
//! value is a JSON object after that version-aware normalization. It does **not**
//! guarantee that serde deserialization will succeed — callers must still handle
//! errors for missing required fields or unexpected keys rejected by
//! `#[serde(deny_unknown_fields)]`.
//!
//! # Versioning
//! - `schema` absent in the JSON → version 0 (pre-versioning era)
//! - `schema == CURRENT_SCHEMA` → already up to date, no-op
//! - `schema > CURRENT_SCHEMA` → written by a newer foray; return [`MigrateResult::TooNew`]
//! - non-object input → return [`MigrateResult::Invalid`]

use serde_json::{Map, Value};

pub(crate) use crate::types::CURRENT_SCHEMA;

/// The wire protocol version produced by this build.
///
/// Tracks envelope-level changes to `SyncJournalResponse` (fields like
/// `from`, `added_ids`, etc.) that are independent of `CURRENT_SCHEMA`.
/// `StdioStore` checks this at connect time and returns
/// [`StoreError::ProtocolTooNew`] if the server's protocol is newer.
pub(crate) const CURRENT_PROTOCOL: u32 = 1;

/// Synthetic store name injected into `hello` responses from protocol 0
/// servers that do not emit a `stores` list. `adapt_send` strips the `store`
/// param when it matches this value, since protocol 0 servers do not accept
/// a `store` argument.
pub(crate) const PROTOCOL_0_IMPLICIT_STORE: &str = "local";

/// Typed error returned by [`adapt_receive`].
///
/// Lets callers convert each variant to the appropriate [`StoreError`] without
/// re-parsing strings.
///
/// [`StoreError`]: crate::store::StoreError
#[derive(Debug)]
pub(crate) enum AdaptError {
    /// The response was not a JSON object — adaptation is impossible.
    NonObject(String),
    /// Protocol 0 server signalled that the journal already exists
    /// (`created: false` in the `open_journal` / `create_journal` response).
    AlreadyExists(String),
}

/// Result of running [`migrate`].
pub(crate) enum MigrateResult {
    /// The value was already at the current schema — returned unchanged.
    Current(Value),
    /// The value was migrated — the caller should rewrite the file.
    Migrated(Value),
    /// The value was written by a newer foray — cannot safely read.
    TooNew { found: u32, max: u32 },
    /// The value is not a JSON object and cannot be migrated.
    Invalid,
}

/// Migrate a raw journal [`Value`] to the current schema.
///
/// Consumes the value and returns a [`MigrateResult`].
///
/// See `doc/compatibility.md` — *Axis 1 — Schema* for detection/resolution
/// scenarios, and *Bumping the schema version* for the checklist to follow
/// when adding a new schema version.
pub(crate) fn migrate(value: Value) -> MigrateResult {
    let schema = value
        .get("schema")
        .and_then(Value::as_u64)
        .map(|n| u32::try_from(n).unwrap_or(u32::MAX))
        .unwrap_or(0);

    if schema > CURRENT_SCHEMA {
        return MigrateResult::TooNew {
            found: schema,
            max: CURRENT_SCHEMA,
        };
    }

    if schema == CURRENT_SCHEMA {
        return MigrateResult::Current(value);
    }

    // Run the migration chain from `schema` up to CURRENT_SCHEMA.
    let mut obj = match value {
        Value::Object(m) => m,
        _ => return MigrateResult::Invalid,
    };

    if schema < 1 {
        obj = v0_to_v1(obj);
    }

    MigrateResult::Migrated(Value::Object(obj))
}

/// Migration 0 → 1: remove `created_at` and `updated_at` from the journal
/// root, drop any `fork` items, move top-level `ref` on items into
/// `meta["ref"]`, remove the top-level `id` and `_note` fields, then inject
/// `"schema": 1`.
fn v0_to_v1(mut obj: Map<String, Value>) -> Map<String, Value> {
    obj.remove("created_at");
    obj.remove("updated_at");

    if let Some(Value::Array(items)) = obj.get_mut("items") {
        items.retain(|item| {
            item.get("type")
                .and_then(Value::as_str)
                .map(|t| t != "fork")
                .unwrap_or(true)
        });
        for item in items.iter_mut() {
            if let Value::Object(item_obj) = item
                && item_obj.contains_key("ref")
            {
                // Normalize meta to an object so ref is never silently dropped.
                match item_obj.get_mut("meta") {
                    Some(m) if !m.is_object() => *m = Value::Object(Map::new()),
                    None => {
                        item_obj.insert("meta".to_string(), Value::Object(Map::new()));
                    }
                    _ => {}
                }

                let should_fill = item_obj
                    .get("meta")
                    .and_then(Value::as_object)
                    .map(|mo| !mo.contains_key("ref") || mo.get("ref") == Some(&Value::Null))
                    .unwrap_or(false);

                if should_fill {
                    if let Some(ref_val) = item_obj.remove("ref")
                        && let Some(Value::Object(meta_obj)) = item_obj.get_mut("meta")
                    {
                        meta_obj.insert("ref".to_string(), ref_val);
                    }
                } else {
                    item_obj.remove("ref");
                }
            }
        }
    }

    obj.remove("id");
    obj.remove("_note");
    obj.insert("schema".to_string(), Value::from(1u32));
    obj
}

/// Return the wire tool name to use when calling a server at `server_protocol`.
///
/// Protocol 0 servers do not know `create_journal`; translate to `open_journal`.
pub(crate) fn adapt_tool(server_protocol: u32, tool: &'static str) -> &'static str {
    if server_protocol < 1 && tool == "create_journal" {
        return "open_journal";
    }
    tool
}

/// Adapt outbound request arguments before sending to a remote server with an
/// older protocol version.
///
/// Each `if server_protocol < N` block documents every field that was added or
/// changed at that protocol boundary, stripping or transforming anything the
/// old server does not understand. Wire structs use `deny_unknown_fields`, so
/// an unhandled protocol gap here will surface as a deserialization failure at
/// the call site rather than silent misbehaviour.
///
/// Returns `Err(String)` if a required adaptation cannot be performed.
///
/// See `doc/compatibility.md` — *Protocol 0 (v0.2.0 Servers)* for the
/// current adaptation rules, and *Bumping the protocol version* for the
/// checklist to follow when adding a new protocol version.
pub(crate) fn adapt_send(
    server_protocol: u32,
    tool: &str,
    mut args: Value,
) -> Result<Value, String> {
    // Protocol 0 → 1: several params were added or removed that old servers
    // reject via `deny_unknown_fields`:
    //   all tools:          `store` (protocol 0 servers have a single implicit store)
    //   list_journals:      `archived` stripped — protocol 0 servers only accepted
    //                       `limit`/`offset`/`nuance` and returned all journals in one call;
    //                       protocol 1 returns all journals too but with per-entry archived flag
    //   sync_journal:       `from` renamed from `cursor`, `size` renamed from `limit`
    //   archive_journal:    entire tool did not exist
    //   unarchive_journal:  entire tool did not exist
    //   create_journal:     renamed from `open_journal` (tool name rewrite in adapt_tool)
    if server_protocol < 1 {
        match tool {
            "archive_journal" | "unarchive_journal" => {
                return Err(format!(
                    "'{tool}' is not supported by protocol 0 server; upgrade the remote foray"
                ));
            }
            _ => {}
        }
        if let Value::Object(ref mut obj) = args {
            // Strip or validate `store`.
            match obj.remove("store") {
                Some(Value::String(ref s)) if s == PROTOCOL_0_IMPLICIT_STORE => {
                    // expected — strip silently
                }
                Some(Value::String(s)) => {
                    return Err(format!(
                        "store '{s}' not found: protocol 0 server exposes a single implicit \
                         store '{PROTOCOL_0_IMPLICIT_STORE}'; remove the `store` field from \
                         the config entry or upgrade the remote foray"
                    ));
                }
                _ => {
                    // absent or non-string — pass through
                }
            }
            // list_journals: strip `archived` — protocol 0 servers only accepted
            // `limit`/`offset`/`nuance` and reject unknown fields. They return all
            // journals in one call. adapt_receive uses orig_args["archived"] to tag
            // entries; stripping here does not affect response tagging.
            if tool == "list_journals" {
                obj.remove("archived");
            }
            if tool == "sync_journal" {
                // `archived` was introduced in protocol 1; protocol 0 servers had
                // only active journals.
                match obj.get("archived").and_then(Value::as_bool) {
                    Some(true) => {
                        return Err("archived journals not supported by protocol 0 server; \
                             upgrade the remote foray"
                            .to_string());
                    }
                    _ => {
                        obj.remove("archived");
                    }
                }
                // `from`/`size` were introduced in protocol 1; translate back to
                // `cursor`/`limit` for protocol 0 servers.
                if let Some(from) = obj.remove("from") {
                    // Only send `cursor` if non-zero; protocol 0 servers default to 0.
                    if from.as_u64() != Some(0) {
                        obj.insert("cursor".to_string(), from);
                    }
                }
                if let Some(size) = obj.remove("size") {
                    obj.insert("limit".to_string(), size);
                }
            }
        }
    }
    Ok(args)
}

/// Adapt an inbound response received from a remote server with an older
/// protocol version, normalising it to the current wire shape before typed
/// deserialization.
///
/// Each `if server_protocol < N` block documents every field that was added or
/// changed at that protocol boundary, inserting synthesised defaults for fields
/// that old servers did not emit. Wire structs use `deny_unknown_fields`, so
/// every field the server might send must be explicitly declared in the struct,
/// and every field the struct requires must be inserted here for old servers.
///
/// See `doc/compatibility.md` — *Protocol 0 (v0.2.0 Servers)* for the
/// current adaptation rules, and *Bumping the protocol version* for the
/// checklist to follow when adding a new protocol version.
///
/// Returns `Err(`[`AdaptError`]`)` if the response is not a JSON object
/// (adaptation is not possible) or if a protocol-level conflict is detected
/// (e.g. `created: false` from a protocol 0 `create_journal` response).
pub(crate) fn adapt_receive(
    server_protocol: u32,
    tool: &str,
    request_args: &Value,
    mut response: Value,
) -> Result<Value, AdaptError> {
    // Protocol 0 → 1: the following fields were added or renamed in this transition:
    //   hello:            `protocol`, `stores`  (version was already present)
    //   sync_journal:     `schema`; `cursor` renamed to `from`
    //   create_journal:   v0 response had extra `item_count`, `created` — strip both; `created: false` returns Err
    //   archive_journal:  `archived`
    //   unarchive_journal:`unarchived`
    if server_protocol < 1 {
        let obj = response.as_object_mut().ok_or_else(|| {
            AdaptError::NonObject(format!(
                "adapt_receive({tool}): response is not a JSON object"
            ))
        })?;
        match tool {
            "hello" => {
                obj.entry("version")
                    .or_insert_with(|| Value::String(String::new()));
                obj.entry("protocol").or_insert_with(|| Value::from(0u32));
                // Synthesise a single implicit store so the client can select
                // it as `store_name`. `adapt_send` strips it before sending.
                obj.entry("stores").or_insert_with(|| {
                    serde_json::json!([
                        {"name": PROTOCOL_0_IMPLICIT_STORE,
                         "description": "implicit store (protocol 0 server)"}
                    ])
                });
                obj.entry("skill_uri")
                    .or_insert_with(|| Value::String(String::new()));
            }
            "sync_journal" => {
                obj.entry("schema").or_insert_with(|| Value::from(0u32));
                // `cursor` was renamed to `from` in protocol 1.
                if let Some(cursor) = obj.remove("cursor") {
                    obj.entry("from").or_insert(cursor);
                }
            }
            "create_journal" => {
                // v0 servers returned `name`, `title`, `item_count`, `created`.
                // Map `created: false` (already existed) to an error so the caller can return
                // AlreadyExists. Strip both `item_count` and `created` from the success path
                // so the adapted response is identical to a v1 server response.
                obj.entry("name")
                    .or_insert_with(|| Value::String(String::new()));
                obj.entry("title")
                    .or_insert_with(|| Value::String(String::new()));
                obj.remove("item_count");
                if let Some(Value::Bool(false)) = obj.remove("created") {
                    let name = obj
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    return Err(AdaptError::AlreadyExists(name));
                }
            }
            "archive_journal" => {
                obj.entry("archived")
                    .or_insert_with(|| Value::String(String::new()));
            }
            "unarchive_journal" => {
                obj.entry("unarchived")
                    .or_insert_with(|| Value::String(String::new()));
            }
            "list_journals" => {
                // Protocol 0 servers returned `limit` and `offset` in the
                // response; strip them so `ListJournalsWire` (deny_unknown_fields)
                // can deserialize without error.
                obj.remove("limit");
                obj.remove("offset");
                // Protocol 0 filtered by `archived`; tag every entry with the
                // value that was requested so the new unified format is satisfied.
                let req_archived = request_args
                    .get("archived")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if let Some(Value::Array(journals)) = obj.get_mut("journals") {
                    for entry in journals.iter_mut() {
                        if let Value::Object(e) = entry {
                            e.insert("archived".to_string(), Value::Bool(req_archived));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn migrate_current() {
        let v = json!({ "schema": 1, "name": "test", "items": [] });
        let original = v.clone();
        match migrate(v) {
            MigrateResult::Current(out) => assert_eq!(out, original),
            _ => panic!("expected Current"),
        }
    }

    #[test]
    fn migrate_v0_drops_fork_items() {
        let v = json!({
            "id": "abc",
            "name": "test",
            "items": [
                { "id": "a", "type": "fork", "content": "Forked from parent", "added_at": "2026-01-01T00:00:00Z" },
                { "id": "b", "type": "finding", "content": "real finding", "added_at": "2026-01-01T00:00:00Z" }
            ]
        });
        match migrate(v) {
            MigrateResult::Migrated(out) => {
                assert_eq!(out["schema"], json!(CURRENT_SCHEMA));
                let items = out["items"].as_array().unwrap();
                assert_eq!(items.len(), 1, "fork item should be dropped");
                assert_eq!(items[0]["type"], json!("finding"));
            }
            _ => panic!("expected Migrated"),
        }
    }

    #[test]
    fn migrate_v0_moves_ref_to_meta() {
        let v = json!({
            "id": "abc",
            "name": "test",
            "items": [
                {
                    "id": "x",
                    "type": "finding",
                    "content": "hi",
                    "ref": "src/auth.rs:42",
                    "added_at": "2026-01-01T00:00:00Z"
                },
                {
                    "id": "y",
                    "type": "note",
                    "content": "no ref",
                    "added_at": "2026-01-01T00:00:00Z"
                },
                {
                    "id": "z",
                    "type": "decision",
                    "content": "existing meta",
                    "ref": "src/b.rs",
                    "added_at": "2026-01-01T00:00:00Z",
                    "meta": { "vcs-branch": "main" }
                }
            ]
        });
        match migrate(v) {
            MigrateResult::Migrated(out) => {
                assert_eq!(out["schema"], json!(CURRENT_SCHEMA));
                let item0 = &out["items"][0];
                assert!(
                    item0.get("ref").is_none(),
                    "ref should be removed from item"
                );
                assert_eq!(item0["meta"]["ref"], json!("src/auth.rs:42"));
                let item1 = &out["items"][1];
                assert!(item1.get("ref").is_none());
                assert!(
                    item1.get("meta").is_none(),
                    "meta should not be created when ref absent"
                );
                let item2 = &out["items"][2];
                assert!(item2.get("ref").is_none());
                assert_eq!(item2["meta"]["ref"], json!("src/b.rs"));
                assert_eq!(
                    item2["meta"]["vcs-branch"],
                    json!("main"),
                    "existing meta preserved"
                );
            }
            _ => panic!("expected Migrated"),
        }
    }

    #[test]
    fn migrate_v0_ref_does_not_overwrite_existing_meta_ref() {
        let v = json!({
            "id": "abc",
            "name": "test",
            "items": [{
                "id": "x",
                "type": "note",
                "content": "c",
                "ref": "old",
                "added_at": "2026-01-01T00:00:00Z",
                "meta": { "ref": "existing" }
            }]
        });
        match migrate(v) {
            MigrateResult::Migrated(out) => {
                assert_eq!(out["items"][0]["meta"]["ref"], json!("existing"));
            }
            _ => panic!("expected Migrated"),
        }
    }

    #[test]
    fn migrate_v0_ref_with_null_meta() {
        // meta: null must be normalized to an object so ref is not silently dropped.
        let v = json!({
            "id": "abc",
            "name": "test",
            "items": [{
                "id": "x",
                "type": "note",
                "content": "c",
                "ref": "src/lib.rs:1",
                "added_at": "2026-01-01T00:00:00Z",
                "meta": null
            }]
        });
        match migrate(v) {
            MigrateResult::Migrated(out) => {
                assert!(out["items"][0].get("ref").is_none());
                assert_eq!(out["items"][0]["meta"]["ref"], json!("src/lib.rs:1"));
            }
            _ => panic!("expected Migrated"),
        }
    }

    #[test]
    fn migrate_v0_removes_timestamps() {
        let v = json!({
            "id": "abc",
            "name": "test",
            "items": [
                {
                    "id": "x",
                    "type": "note",
                    "content": "hi",
                    "added_at": "2026-01-01T00:00:00Z"
                }
            ],
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-02T00:00:00Z"
        });
        match migrate(v) {
            MigrateResult::Migrated(out) => {
                assert!(
                    out.get("created_at").is_none(),
                    "created_at should be removed"
                );
                assert!(
                    out.get("updated_at").is_none(),
                    "updated_at should be removed"
                );
                assert_eq!(out["schema"], json!(CURRENT_SCHEMA));
                let item = &out["items"][0];
                assert!(
                    item.get("added_at").is_some(),
                    "item added_at should be preserved"
                );
            }
            _ => panic!("expected Migrated"),
        }
    }

    #[test]
    fn migrate_v0_no_timestamps() {
        // v0 file that never had the timestamp fields — should still get current schema
        let v = json!({ "id": "abc", "name": "test", "items": [] });
        match migrate(v) {
            MigrateResult::Migrated(out) => {
                assert_eq!(out["schema"], json!(CURRENT_SCHEMA));
                assert!(out.get("id").is_none(), "id should be removed by v0_to_v1");
            }
            _ => panic!("expected Migrated"),
        }
    }

    #[test]
    fn migrate_schema_overflow() {
        // Values > u32::MAX must not bypass the TooNew guard via truncation.
        let v = json!({ "schema": (u32::MAX as u64) + 1, "name": "test", "items": [] });
        match migrate(v) {
            MigrateResult::TooNew { found, max } => {
                assert_eq!(found, u32::MAX);
                assert_eq!(max, CURRENT_SCHEMA);
            }
            _ => panic!("expected TooNew"),
        }
    }

    #[test]
    fn migrate_too_new() {
        let v = json!({ "schema": 9999, "name": "test", "items": [] });
        match migrate(v) {
            MigrateResult::TooNew { found, max } => {
                assert_eq!(found, 9999);
                assert_eq!(max, CURRENT_SCHEMA);
            }
            _ => panic!("expected TooNew"),
        }
    }

    #[test]
    fn migrate_non_object_returns_invalid() {
        for v in [json!(null), json!(42), json!("string"), json!([1, 2, 3])] {
            assert!(
                matches!(migrate(v), MigrateResult::Invalid),
                "expected Invalid for non-object input"
            );
        }
    }

    // ── adapt_send ────────────────────────────────────────────────────

    #[test]
    fn adapt_send_list_journals_strips_archived_for_protocol_0() {
        // v0 servers only accept `limit`/`offset`/`nuance` — strip `archived`.
        let args_false = json!({ "store": PROTOCOL_0_IMPLICIT_STORE, "archived": false });
        let result = adapt_send(0, "list_journals", args_false).unwrap();
        assert!(result.get("store").is_none(), "store should be stripped");
        assert!(
            result.get("archived").is_none(),
            "archived should be stripped"
        );

        let args_true = json!({ "store": PROTOCOL_0_IMPLICIT_STORE, "archived": true });
        let result = adapt_send(0, "list_journals", args_true).unwrap();
        assert!(result.get("store").is_none(), "store should be stripped");
        assert!(
            result.get("archived").is_none(),
            "archived should be stripped"
        );
    }

    #[test]
    fn adapt_send_errors_on_unknown_store_for_protocol_0() {
        let args = json!({ "store": "remote", "name": "j" });
        let err = adapt_send(0, "create_journal", args).unwrap_err();
        assert!(err.contains("store 'remote' not found"), "got: {err}");
    }

    #[test]
    fn adapt_send_errors_on_archive_journal_for_protocol_0() {
        let args = json!({ "store": PROTOCOL_0_IMPLICIT_STORE, "name": "j" });
        let err = adapt_send(0, "archive_journal", args).unwrap_err();
        assert!(err.contains("archive_journal"), "got: {err}");
        assert!(err.contains("not supported"), "got: {err}");
    }

    #[test]
    fn adapt_send_errors_on_unarchive_journal_for_protocol_0() {
        let args = json!({ "store": PROTOCOL_0_IMPLICIT_STORE, "name": "j" });
        let err = adapt_send(0, "unarchive_journal", args).unwrap_err();
        assert!(err.contains("unarchive_journal"), "got: {err}");
        assert!(err.contains("not supported"), "got: {err}");
    }

    #[test]
    fn adapt_send_noop_for_protocol_0_create_journal_with_implicit_store() {
        let args = json!({ "store": PROTOCOL_0_IMPLICIT_STORE, "name": "foo" });
        let result = adapt_send(0, "create_journal", args).unwrap();
        assert!(result.get("store").is_none(), "store should be stripped");
        assert_eq!(result["name"], json!("foo"));
    }

    // ── adapt_tool ────────────────────────────────────────────────────

    #[test]
    fn adapt_tool_rewrites_create_journal_for_protocol_0() {
        assert_eq!(adapt_tool(0, "create_journal"), "open_journal");
    }

    #[test]
    fn adapt_tool_keeps_create_journal_for_protocol_1() {
        assert_eq!(adapt_tool(1, "create_journal"), "create_journal");
    }

    #[test]
    fn adapt_tool_leaves_other_tools_unchanged_for_protocol_0() {
        assert_eq!(adapt_tool(0, "sync_journal"), "sync_journal");
        assert_eq!(adapt_tool(0, "list_journals"), "list_journals");
    }

    // ── adapt_receive ─────────────────────────────────────────────────

    #[test]
    fn adapt_receive_hello_inserts_synthetic_store_for_protocol_0() {
        let raw = json!({ "version": "0.2.0", "nuance": "abc" });
        let result = adapt_receive(0, "hello", &json!({}), raw).unwrap();
        assert_eq!(result["protocol"], json!(0));
        assert_eq!(
            result["stores"][0]["name"],
            json!(PROTOCOL_0_IMPLICIT_STORE)
        );
        assert!(result["stores"][0]["description"].is_string());
        assert_eq!(result["nuance"], json!("abc"));
        assert!(result["skill_uri"].as_str() == Some(""));
    }

    #[test]
    fn adapt_receive_hello_preserves_existing_stores_for_protocol_0() {
        // If server somehow sends stores already, do not overwrite them.
        let raw = json!({ "nuance": "abc", "stores": [{"name": "x", "description": "y"}] });
        let result = adapt_receive(0, "hello", &json!({}), raw).unwrap();
        assert_eq!(result["stores"][0]["name"], json!("x"));
    }

    #[test]
    fn adapt_receive_hello_passthrough_for_protocol_1() {
        let raw = json!({
            "version": "1.0",
            "nuance": "abc",
            "protocol": 1,
            "stores": [{"name": "local", "description": "Local store"}]
        });
        let result = adapt_receive(1, "hello", &json!({}), raw.clone()).unwrap();
        assert_eq!(result, raw);
    }

    #[test]
    fn adapt_receive_sync_journal_inserts_schema_for_protocol_0() {
        // v0.2.0 sync_journal response has no `schema` and uses `cursor` not `from`.
        let raw = json!({
            "name": "j", "title": "My Journal",
            "items": [], "added_ids": [], "cursor": 0, "total": 0
        });
        let result = adapt_receive(0, "sync_journal", &json!({}), raw).unwrap();
        assert_eq!(result["schema"], json!(0));
        // cursor → from rename also applied in the protocol 0 → 1 transition.
        assert!(result.get("cursor").is_none());
        assert_eq!(result["from"], json!(0));
    }

    #[test]
    fn adapt_receive_list_journals_strips_limit_offset_and_tags_archived_for_protocol_0() {
        let raw = json!({
            "journals": [{"name": "j", "title": "T", "item_count": 0}],
            "total": 1, "limit": 500, "offset": 0
        });
        let result = adapt_receive(0, "list_journals", &json!({ "archived": false }), raw).unwrap();
        assert!(result.get("limit").is_none());
        assert!(result.get("offset").is_none());
        assert_eq!(result["journals"][0]["archived"], json!(false));
    }

    #[test]
    fn adapt_receive_list_journals_tags_archived_false_on_entries_for_protocol_0() {
        let raw = json!({
            "journals": [{"name": "j", "title": "T", "item_count": 2}],
            "total": 1
        });
        let result = adapt_receive(0, "list_journals", &json!({ "archived": false }), raw).unwrap();
        assert_eq!(result["journals"][0]["archived"], json!(false));
    }

    #[test]
    fn adapt_receive_list_journals_tags_archived_true_on_entries_for_protocol_0() {
        let raw = json!({
            "journals": [{"name": "j", "title": "T", "item_count": 2}],
            "total": 1
        });
        let result = adapt_receive(0, "list_journals", &json!({ "archived": true }), raw).unwrap();
        assert_eq!(result["journals"][0]["archived"], json!(true));
    }

    #[test]
    fn adapt_receive_list_journals_empty_journals_for_protocol_0() {
        let raw = json!({ "journals": [], "total": 0 });
        let result = adapt_receive(
            0,
            "list_journals",
            &json!({ "archived": false }),
            raw.clone(),
        )
        .unwrap();
        assert_eq!(result["journals"], json!([]));
    }

    #[test]
    fn adapt_receive_unknown_tool_is_noop() {
        let raw = json!({ "foo": "bar" });
        let result = adapt_receive(0, "some_future_tool", &json!({}), raw.clone()).unwrap();
        assert_eq!(result, raw);
    }

    #[test]
    fn adapt_receive_non_object_returns_err() {
        let raw = json!([1, 2, 3]);
        assert!(adapt_receive(0, "hello", &json!({}), raw).is_err());
    }

    #[test]
    fn adapt_send_sync_journal_translates_from_size_for_protocol_0() {
        let args = json!({ "name": "j", "from": 10, "size": 5 });
        let result = adapt_send(0, "sync_journal", args).unwrap();
        assert!(
            result.get("from").is_none(),
            "from should be translated away"
        );
        assert!(
            result.get("size").is_none(),
            "size should be translated away"
        );
        assert_eq!(result["cursor"], json!(10));
        assert_eq!(result["limit"], json!(5));
    }

    #[test]
    fn adapt_send_sync_journal_omits_cursor_when_from_is_zero_for_protocol_0() {
        // Protocol 0 servers default cursor to 0; omit it to avoid sending an
        // unexpected field on servers that use `deny_unknown_fields`.
        let args = json!({ "name": "j", "from": 0, "size": 5 });
        let result = adapt_send(0, "sync_journal", args).unwrap();
        assert!(
            result.get("cursor").is_none(),
            "cursor should be omitted when from=0"
        );
        assert_eq!(result["limit"], json!(5));
    }

    #[test]
    fn adapt_send_sync_journal_noop_for_protocol_1() {
        let args = json!({ "name": "j", "from": 10, "size": 5 });
        let result = adapt_send(1, "sync_journal", args.clone()).unwrap();
        assert_eq!(result, args);
    }

    #[test]
    fn adapt_receive_sync_journal_translates_cursor_to_from_for_protocol_0() {
        let raw = json!({
            "schema": 1, "name": "j", "title": "T",
            "items": [], "added_ids": [], "cursor": 22, "total": 100
        });
        let result = adapt_receive(0, "sync_journal", &json!({}), raw).unwrap();
        assert!(result.get("cursor").is_none());
        assert_eq!(result["from"], json!(22));
    }

    #[test]
    fn adapt_receive_sync_journal_passthrough_for_protocol_1() {
        let raw = json!({
            "schema": 1, "name": "j", "title": "T",
            "items": [], "added_ids": [], "from": 22, "total": 100
        });
        let result = adapt_receive(1, "sync_journal", &json!({}), raw.clone()).unwrap();
        assert_eq!(result, raw);
    }

    #[test]
    fn adapt_receive_create_journal_strips_item_count_and_created_for_protocol_0() {
        let raw = json!({ "name": "j", "title": "T", "item_count": 0, "created": true });
        let result = adapt_receive(0, "create_journal", &json!({}), raw).unwrap();
        assert!(
            result.get("item_count").is_none(),
            "item_count should be stripped"
        );
        assert!(
            result.get("created").is_none(),
            "created should be stripped on success"
        );
        assert_eq!(result["name"], json!("j"));
        assert_eq!(result["title"], json!("T"));
    }

    #[test]
    fn adapt_receive_create_journal_errors_on_created_false_for_protocol_0() {
        let raw = json!({ "name": "j", "title": "T", "item_count": 1, "created": false });
        let err = adapt_receive(0, "create_journal", &json!({}), raw).unwrap_err();
        assert!(
            matches!(err, AdaptError::AlreadyExists(ref n) if n == "j"),
            "expected AlreadyExists(\"j\")"
        );
    }

    #[test]
    fn adapt_send_sync_journal_strips_archived_false_for_protocol_0() {
        let args = json!({ "name": "j", "from": 0, "size": 5, "archived": false });
        let result = adapt_send(0, "sync_journal", args).unwrap();
        assert!(
            result.get("archived").is_none(),
            "archived: false should be stripped for protocol 0"
        );
    }

    #[test]
    fn adapt_receive_create_journal_passthrough_for_protocol_1() {
        let raw = json!({ "name": "j", "title": "T" });
        let result = adapt_receive(1, "create_journal", &json!({}), raw.clone()).unwrap();
        assert_eq!(result, raw);
    }

    #[test]
    fn adapt_send_sync_journal_errors_on_archived_true_for_protocol_0() {
        let args = json!({ "name": "j", "from": 0, "size": 5, "archived": true });
        let err = adapt_send(0, "sync_journal", args).unwrap_err();
        assert!(
            err.contains("archived journals not supported"),
            "got: {err}"
        );
    }
}
