# Data Model Specification

## Purpose

Defines the core domain concepts of foray: journals and items. All other capabilities
(storage, MCP server, CLI) are built on top of these abstractions. This spec establishes
the structure, constraints, and behavioral invariants that govern every journal and item
in the system, regardless of which store backend is in use.

## Requirements

### Requirement: Journal is a named collection of items

A journal SHALL be identified by a unique name within a store and SHALL contain an ordered
sequence of items. A journal SHALL also carry a human-readable title, an optional
free-form metadata map, and a schema version stamp. Journals capture ongoing work:
debugging, design, planning, research, stand-ups, or any work that spans multiple sessions.

#### Scenario: Journal has required fields
- **WHEN** a journal is created with a name and title
- **THEN** the journal has a name, a title, an empty item list, and a schema version stamp

#### Scenario: Journal carries optional metadata
- **WHEN** a journal is created with a metadata map
- **THEN** the metadata is stored at the journal level and returned on read

### Requirement: Journal name must conform to a strict character set

A journal name SHALL contain only lowercase ASCII letters, digits, hyphens, and underscores
(`[a-z0-9_-]`). The name SHALL be at most 64 characters. The name SHALL be rejected at
creation time if it violates these rules; no journal with an invalid name SHALL be persisted.

#### Scenario: Valid name accepted
- **WHEN** a journal is created with name `auth-triage-2026`
- **THEN** the journal is created successfully

#### Scenario: Name with uppercase letters rejected
- **WHEN** a journal creation is requested with name `Auth-Triage`
- **THEN** the request is rejected with an invalid name error before any store access

#### Scenario: Name with spaces rejected
- **WHEN** a journal creation is requested with name `auth triage`
- **THEN** the request is rejected with an invalid name error

#### Scenario: Name exceeding 64 characters rejected
- **WHEN** a journal creation is requested with a name that is 65 characters long
- **THEN** the request is rejected with an invalid name error

### Requirement: Journal title must be non-empty

A journal title SHALL be a non-empty string. A title that is empty or contains only
whitespace SHALL be rejected. The title SHALL be at most 512 characters.

#### Scenario: Whitespace-only title rejected
- **WHEN** a journal creation is requested with title `   `
- **THEN** the request is rejected with a title validation error

#### Scenario: Valid title accepted
- **WHEN** a journal is created with title `Investigating auth cache race conditions`
- **THEN** the journal is created with that title

### Requirement: Item is a typed, timestamped entry within a journal

An item SHALL have a unique ID, a type, a content string, a creation timestamp, optional
tags, and an optional free-form metadata map. The item type SHALL be one of: `finding`,
`decision`, `snippet`, or `note`. The timestamp SHALL be set by the server at insertion
time; callers do not supply it (except during import, which preserves source timestamps).

#### Scenario: Item created with required fields
- **WHEN** an item is added with content `"Race condition in auth cache"`
- **THEN** the item is stored with a server-assigned ID, a server-assigned timestamp, and type `note` (default)

#### Scenario: Item created with explicit type
- **WHEN** an item is added with content `"Lock added around cache access"` and type `decision`
- **THEN** the item is stored with type `decision`

#### Scenario: Item created with tags and metadata
- **WHEN** an item is added with tags `["auth", "race-condition"]` and metadata `{"ref": "src/auth/session.go:142"}`
- **THEN** the item is stored with those tags and that metadata

### Requirement: Item ID uses a consonant-only random format

Each item SHALL be assigned a unique ID by the server at insertion time. The ID SHALL
follow the format `xxxx-xxxx-xxxx-xxxx` where each character is a randomly chosen
consonant (16 characters, approximately 70 bits of entropy). IDs SHALL be assigned
server-side; callers do not control or predict them.

#### Scenario: ID format on insertion
- **WHEN** an item is added to a journal
- **THEN** the returned ID matches the pattern `[b-df-hj-np-tv-z]{4}-[b-df-hj-np-tv-z]{4}-[b-df-hj-np-tv-z]{4}-[b-df-hj-np-tv-z]{4}`

### Requirement: Item content has a maximum size

Item content SHALL be at most 64 KB. A request to add an item whose content exceeds this
limit SHALL be rejected.

#### Scenario: Oversized content rejected
- **WHEN** an item with content exceeding 64 KB is submitted
- **THEN** the request is rejected with an input validation error

### Requirement: Item tags have count and length limits

An item SHALL have at most 20 tags. Each tag SHALL be at most 64 bytes. A request
that violates either limit SHALL be rejected.

#### Scenario: Too many tags rejected
- **WHEN** an item is submitted with 21 tags
- **THEN** the request is rejected with an input validation error

#### Scenario: Tag too long rejected
- **WHEN** an item is submitted with a tag that is 65 bytes long
- **THEN** the request is rejected with an input validation error

### Requirement: Item metadata has a maximum serialized size

The metadata map on an item SHALL be at most 8 KB when serialized. A request whose item
metadata exceeds this limit SHALL be rejected.

#### Scenario: Oversized metadata rejected
- **WHEN** an item is submitted with a metadata map whose serialized size exceeds 8 KB
- **THEN** the request is rejected with an input validation error

### Requirement: Journal metadata has a maximum serialized size

The metadata map on a journal SHALL be at most 8 KB when serialized. A request whose
journal metadata exceeds this limit SHALL be rejected at creation time.

#### Scenario: Oversized journal metadata rejected on create
- **WHEN** `create_journal` is called with a metadata map whose serialized size exceeds 8 KB
- **THEN** the request is rejected with an input validation error

### Requirement: Journals are append-only

Items in a journal SHALL NOT be edited or deleted after insertion. The full investigation
trail is always preserved. This invariant holds across all store backends and all clients.
Concurrent writes from multiple clients are safe because appending never conflicts.

To correct a wrong item, a new item SHALL be added that supersedes it. The correction
item explicitly references or describes what it is correcting.

#### Scenario: Correction via new item
- **WHEN** an item contains an incorrect finding
- **THEN** a new item is added with content explaining the correction; the original item remains unchanged

#### Scenario: No delete operation exists
- **WHEN** a caller attempts to delete an individual item from a journal
- **THEN** no such operation is available; the item persists

### Requirement: Metadata maps provide free-form extensibility

Both journals and items SHALL carry an optional metadata map — a free-form key-value
store where values may be any JSON type. This map accommodates client-specific data
(AI model name, conversation ID, user annotations, confidence levels) without requiring
schema changes.

#### Scenario: Arbitrary metadata keys preserved
- **WHEN** an item is added with metadata `{"model": "claude-opus-4", "confidence": "high"}`
- **THEN** both keys and their values are returned unchanged on read

### Requirement: meta.ref records external references

When an item relates to a specific file, URL, ticket, pull request, or another journal,
the reference SHALL be stored in the `ref` key of the item's metadata map. The CLI
exposes a `--ref` flag as a convenience shortcut that populates `meta.ref`.

Cross-journal references SHALL use the format `foray:<journal-name>` (or
`foray:<journal-name>#<item-id>` to reference a specific item).

#### Scenario: File reference on item
- **WHEN** an item is added with `meta.ref` set to `src/auth/session.go:142`
- **THEN** the reference is stored and returned on read

#### Scenario: Cross-journal reference on item
- **WHEN** an item is added with `meta.ref` set to `foray:auth-triage#gfnd-cpht-xvmr-sjlk`
- **THEN** the reference is stored and returned on read

### Requirement: VCS metadata anchors items to a codebase state

When adding items in a version-controlled checkout, callers SHALL set VCS metadata on
the item so that `meta.ref` file paths can be resolved to an exact codebase state. The
conventional keys are `meta.vcs-repo` (remote URL), `meta.vcs-branch`, and
`meta.vcs-revision` (commit SHA or changelist).

#### Scenario: VCS metadata stored alongside item
- **WHEN** an item is added with metadata `{"ref": "src/auth/session.go:142", "vcs-repo": "https://github.com/org/repo", "vcs-branch": "main", "vcs-revision": "abc123"}`
- **THEN** all VCS metadata keys are preserved and returned on read
