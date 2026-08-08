# Schema & Protocol: Developer Reference

This document is a developer guide for safely evolving foray's two independent version
axes. Behavioral contracts (what happens at runtime when versions mismatch) live in the
`stdio-store` and `storage` specs. This document covers only the implementation
checklists and enforcement patterns.

---

## Version Axes

- **Schema** (`CURRENT_SCHEMA`) — the on-disk journal file format. Embedded in every
  journal file as `"schema": N`. Checked on every read regardless of which store backend
  is reading.
- **Protocol** (`CURRENT_PROTOCOL`) — the wire envelope between a StdioStore client and
  a remote foray MCP server. Only relevant when the remote transport is in use.

---

## Bumping the Schema Version (`migrate`)

Schema bumps affect on-disk journal files. The migration chain runs forward-only — each
step `vN_to_vN+1` transforms a raw JSON value from the previous schema to the next.

**Checklist when adding schema version N+1:**

1. Increment `CURRENT_SCHEMA` to `N+1` in `migrate.rs`.
2. Add a private function `vN_to_vN1(obj)` that applies the transform and injects `"schema": N+1`.
3. Add `if schema < N+1 { obj = vN_to_vN1(obj); }` to the migration chain in `migrate()`,
   **after** all earlier steps and in ascending order. A file at schema 0 must migrate
   through every intermediate version in sequence.
4. Update any struct fields in `types.rs` that the new schema changes.
5. Add tests: one for the new migration step, one that verifies `MigrateResult::TooNew`
   is returned for `schema: N+1`.

Fields are always **added at the top of the chain** (newest step last) and **never
removed from earlier steps** — earlier steps are frozen history.

---

## Bumping the Protocol Version (`adapt_send` / `adapt_receive`)

Protocol bumps affect the wire envelope between StdioStore and the foray MCP server.
They are independent of schema bumps, but often accompany them when new tool parameters
or response fields are introduced.

**Checklist when adding protocol version N+1:**

1. Increment `CURRENT_PROTOCOL` to `N+1` in `migrate.rs`.
2. In `adapt_send`, add a new block `if server_protocol < N+1 { … }` **below** all
   existing blocks. Inside, strip or reject every parameter that old servers do not
   understand. Blocks must be ordered lowest-to-highest.
3. In `adapt_receive`, add a matching block `if server_protocol < N+1 { … }` **below**
   all existing blocks. Inside, inject synthesised defaults for every field that old
   servers did not emit.
4. In `store_stdio.rs`, update the relevant wire struct to declare any new fields.
   Because all wire structs use `#[serde(deny_unknown_fields)]`, omitting a field here
   will surface as a `from_value` failure — the compiler enforces that `adapt_receive`
   and the struct stay in sync.
5. Add tests for `adapt_send` (new param stripped/rejected) and `adapt_receive` (new
   field synthesised) for the old protocol value.

**Field ordering rule:** new fields are documented and synthesised in the **newest
block** (highest protocol threshold). Older blocks are frozen — never modify them to
add new fields.

**Removal rule:** removing a field is also a protocol bump. Add it to `adapt_send` as
a field to strip before sending to old servers, and remove it from the wire struct.

---

## `deny_unknown_fields` — The Enforcement Backstop

All wire structs use `#[serde(deny_unknown_fields)]`. This turns adaptation gaps into
loud failures rather than silent data loss.

| Scenario | Effect |
|----------|--------|
| Future server adds a new response field | `from_value` fails — `adapt_receive` is missing a rule |
| `adapt_receive` synthesises a field not in the wire struct | `from_value` fails — struct declaration is incomplete |
| Both wire struct and `adapt_receive` are in sync | Clean deserialization, no silent data loss |

`adapt_receive` and the wire struct field declarations form a jointly-enforced contract:
neither half can silently diverge from the other.
