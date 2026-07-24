# Store Specification

## Purpose

Defines the behavioral contract that every foray store implementation must satisfy,
regardless of its backend (local JSON files, remote stdio transport, Elasticsearch, etc.).
This spec describes what callers can rely on from any store: the operations available,
their error semantics, concurrency guarantees, and invariants that hold across all
implementations. Backend-specific details belong in the spec for that implementation.

## Requirements

### Requirement: Journal names are globally unique across active and archived locations

A journal name SHALL be unique across both active and archived locations within a store.
A store SHALL NOT permit a journal named `auth-triage` in the active location and
simultaneously a journal named `auth-triage` in the archive. Creating or importing a
journal with a name that already exists in either location SHALL return an
`AlreadyExists` error.

#### Scenario: Create rejected when name exists in archive
- **WHEN** `create_journal` is called with a name that matches an archived journal
- **THEN** the request is rejected with a journal-already-exists error

#### Scenario: Import rejected when name exists in either location
- **WHEN** `import` without `--merge` is called with a name that exists in the archive
- **THEN** the request is rejected with a journal-already-exists error

### Requirement: Journals with empty or whitespace-only name or title are unreadable

A journal whose `name` or `title` resolves to an empty or whitespace-only string SHALL
be treated as unreadable. `list_journals` SHALL surface it as an error entry. Any direct
read or write attempt on such a journal SHALL return an error.

#### Scenario: Journal with empty title appears as error entry in list
- **WHEN** a journal with an empty title exists in the store
- **THEN** `list_journals` returns an entry with `error` set for that journal

### Requirement: list_journals returns metadata and schema for readable journals

For each readable journal, `list_journals` SHALL include:
- `name`, `title`, `item_count`, `archived` (always present)
- `avg_item_size`, `std_item_size` (present for non-empty journals, absent for empty)
- `schema` (the journal's schema version, always present for readable journals)
- `meta` (the journal-level metadata map, present when set)

#### Scenario: Readable journal includes meta and schema in list
- **WHEN** `list_journals` is called and a journal has metadata and a schema version
- **THEN** the entry includes both `meta` and `schema` fields

### Requirement: Concurrent item additions are serialized

When multiple callers attempt to add items to the same journal simultaneously, the store
SHALL serialize the writes so that all items from all callers are preserved. No item
SHALL be silently lost due to concurrent access.

#### Scenario: Concurrent adds preserve all items
- **WHEN** two processes simultaneously add one item each to the same journal
- **THEN** both items are present in the journal after both writes complete

### Requirement: delete permanently removes a journal from the store

`delete(name, archived)` SHALL permanently remove the journal from the store. The
`archived` flag specifies which location to look in. If the journal does not exist in
the expected location, the store SHALL return a `NotFound` error. Some store
implementations (e.g. remote stores) may not support `delete`; in that case they SHALL
return an `Unsupported` error.

#### Scenario: delete removes the journal
- **WHEN** `delete` is called on an existing active journal
- **THEN** the journal no longer appears in `list_journals`

#### Scenario: delete with wrong archived flag returns not found
- **WHEN** `delete` is called with `archived: false` for a journal that is archived
- **THEN** a not-found error is returned

### Requirement: import creates or merges a journal from an external JournalFile

`import(name, journal, merge, archived)` SHALL accept an external `JournalFile` and
add it to the store. Two modes:

- `merge: false` — creates a new journal using the source `title`, `meta`, and
  `archived` flag. Fails if the name already exists in either active or archived
  location. Source `added_at` timestamps are preserved.
- `merge: true` — appends items to an existing active journal, skipping any item whose
  `id` already exists in the destination. Source `title` and `meta` are ignored.
  Source `added_at` timestamps are preserved. Cannot be used with `archived: true`.

Some store implementations (e.g. remote stores) may not support `import`; in that case
they SHALL return an `Unsupported` error immediately before any items are transmitted.

#### Scenario: import without merge creates new journal
- **WHEN** `import` with `merge: false` is called for a non-existent journal name
- **THEN** a new journal is created with the source title and items

#### Scenario: import with merge appends and skips duplicates
- **WHEN** `import` with `merge: true` is called on an existing journal with overlapping item IDs
- **THEN** only items with new IDs are appended; existing IDs are skipped

#### Scenario: import unsupported on remote stores
- **WHEN** `import` is called against a remote store
- **THEN** an unsupported error is returned immediately
