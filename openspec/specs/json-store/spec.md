# JSON File Store Specification

## Purpose

Defines how foray persists journals to disk using the default local store. Covers the
directory layout, file format, FORAY_HOME override, archiving, atomic write safety,
concurrent access locking, schema versioning, and the self-healing migration mechanism.

## Requirements

### Requirement: Default store persists journals as JSON files under a well-known directory

The default store SHALL persist each journal as a single JSON file under
`~/.foray/journals/`. The filename SHALL be `<journal-name>.json`. The directory SHALL be
created automatically on first use. There SHALL be one file per journal with no
subdirectories except for `archive/`.

#### Scenario: Journal file created on first write
- **WHEN** a journal named `auth-triage` is created
- **THEN** a file `~/.foray/journals/auth-triage.json` is created

#### Scenario: Journal directory created automatically
- **WHEN** the journals directory does not exist and a journal is created
- **THEN** the directory is created before writing the file

### Requirement: FORAY_HOME overrides the base directory

When the environment variable `FORAY_HOME` is set to a non-empty string, it SHALL
replace `~/.foray/` as the foray base directory. The journals directory SHALL resolve to
`$FORAY_HOME/journals/` and the config file to `$FORAY_HOME/config.toml`. A leading `~`
or `~/` in `FORAY_HOME` SHALL be expanded to the current user's home directory.
`~otheruser/` expansions SHALL NOT be performed.

#### Scenario: FORAY_HOME redirects journal storage
- **WHEN** `FORAY_HOME=/tmp/foray-test` and a journal named `test` is created
- **THEN** the file is written to `/tmp/foray-test/journals/test.json`

#### Scenario: Tilde in FORAY_HOME is expanded
- **WHEN** `FORAY_HOME=~/foray-data` and a journal is created
- **THEN** the `~` is expanded to the user's home directory before resolving the path

### Requirement: Archived journals are stored in an archive subdirectory

Archiving a journal SHALL move its JSON file from `journals/` to `journals/archive/`.
Unarchiving SHALL move it back. An archived journal is readable but not writable.

#### Scenario: Archive moves file to archive subdirectory
- **WHEN** journal `auth-triage` is archived
- **THEN** `~/.foray/journals/auth-triage.json` is moved to `~/.foray/journals/archive/auth-triage.json`

#### Scenario: Unarchive moves file back to active location
- **WHEN** archived journal `auth-triage` is unarchived
- **THEN** `~/.foray/journals/archive/auth-triage.json` is moved back to `~/.foray/journals/auth-triage.json`

#### Scenario: Archived journal appears in listing with archived flag
- **WHEN** the store lists journals and `auth-triage` is archived
- **THEN** the result includes an entry for `auth-triage` with `archived: true`

### Requirement: Journal files use a versioned JSON format

Each journal file SHALL be a JSON object with the following top-level fields: `schema`
(integer, current schema version), `name` (journal name), `title` (human-readable title),
`items` (array of item objects), and optionally `meta` (free-form metadata map). Unknown
fields at the top level or within item objects SHALL be rejected on read — the format is
strict to prevent silent data loss on write-back.

Each item in the array SHALL have: `id`, `type`, `content`, `added_at` (ISO 8601 UTC
timestamp), optionally `tags` (array of strings), and optionally `meta` (metadata map).

#### Scenario: Written journal file is valid JSON with required fields
- **WHEN** a journal is created and written
- **THEN** the JSON file contains `schema`, `name`, `title`, and `items` fields

#### Scenario: File with unknown top-level field is rejected
- **WHEN** a journal file contains an unrecognized top-level field
- **THEN** the file is rejected and an error is returned to the caller

### Requirement: Writes are atomic

All writes to journal files SHALL be performed atomically using a write-to-temp,
fsync, rename sequence. A crash or power loss during a write SHALL never leave a
journal file in a partially-written or corrupt state.

#### Scenario: Crash during write does not corrupt the file
- **WHEN** a write is interrupted before completion
- **THEN** the journal file retains its previous valid state

### Requirement: Schema version 0 journals are migrated transparently to version 1

Journals written by foray v0.2.0 or earlier have no `schema` field. These SHALL be
treated as schema version 0 and migrated to version 1 on read. The migration SHALL:
strip `created_at` and `updated_at` fields from the journal object, move any top-level
`ref` field on items into `meta.ref`, remove any top-level `id` field on the journal,
remove any top-level `_note` field, inject `"schema": 1`, and drop any items whose
`type` is `"fork"`. The `fork` type was a pre-v1 artefact retired before the schema was
stabilised; it has no equivalent in the v1 data model. The migration runs on the raw
JSON before typed deserialization.

#### Scenario: Schema-0 file without schema field is read successfully
- **WHEN** a journal file has no `schema` field
- **THEN** the file is read and returned as a valid version-1 journal

#### Scenario: Top-level ref on items is moved to meta.ref
- **WHEN** a schema-0 journal file has items with a top-level `ref` field
- **THEN** after migration each item's `ref` value is accessible as `meta.ref`

#### Scenario: fork-type items are dropped during migration
- **WHEN** a schema-0 journal file contains an item with `"type": "fork"`
- **THEN** that item is absent from the migrated journal

### Requirement: A journal with a schema version newer than supported is rejected

If a journal file's `schema` field contains a value greater than the maximum supported
schema version, the store SHALL return a hard error. The error SHALL include the journal
name, the found version, the maximum supported version, and advise upgrading the foray
binary.

#### Scenario: Too-new schema version returns an error
- **WHEN** a journal file contains `"schema": 2` and the maximum supported version is 1
- **THEN** the store returns a schema-too-new error with the journal name, the found version, and the max version

### Requirement: Migration is applied lazily and persisted on the next write

Reading a journal that requires migration SHALL apply the migration in memory but SHALL
NOT immediately rewrite the file. The migrated content SHALL be persisted the next time
items are added, because the write path already holds the exclusive lock. This ensures
migration is safe under concurrent access — the sole writer that holds the lock performs
the rewrite.

#### Scenario: Migration does not rewrite on read
- **WHEN** a schema-0 journal is read via a read-only operation
- **THEN** the file on disk still has no `schema` field immediately after the read

#### Scenario: Migration is persisted on next item add
- **WHEN** a schema-0 journal is read and then an item is added
- **THEN** the file on disk contains `"schema": 1` after the write

### Requirement: Size statistics are computed as ceil-rounded integers

For each non-empty journal, the store SHALL compute `avg_item_size` as the mean
serialized JSON byte size of all items rounded up to the nearest integer, and
`std_item_size` as the population standard deviation of item sizes rounded up to the
nearest integer. Both values SHALL be integers. For a single-item journal,
`std_item_size` SHALL be 0. These values SHALL be absent for empty journals.

#### Scenario: Non-empty journal includes size statistics
- **WHEN** the store lists journals and a journal has 5 items
- **THEN** the entry for that journal includes `avg_item_size` and `std_item_size`

#### Scenario: Empty journal omits size statistics
- **WHEN** the store lists journals and a journal has 0 items
- **THEN** the entry for that journal omits `avg_item_size` and `std_item_size`

#### Scenario: Single-item journal has std_item_size of zero
- **WHEN** the store lists journals and a journal has exactly 1 item
- **THEN** `avg_item_size` is present and `std_item_size` is 0

### Requirement: Unreadable journal files are surfaced as error entries in the listing

If a journal file exists but cannot be fully loaded (malformed JSON, invalid schema, etc.),
the store's listing SHALL include an entry for it with an `error` field set, `title` empty,
and `item_count` zero. The `schema` field SHALL be present if the file was at least
parseable as a JSON object. Any attempt to read or write an error entry SHALL return an
error.

#### Scenario: Malformed journal appears as error entry
- **WHEN** a journal file contains invalid JSON
- **THEN** the store listing includes an entry for that journal with `error` set

#### Scenario: Error entry has empty title and zero item count
- **WHEN** a journal file cannot be loaded
- **THEN** the entry has `title: ""` and `item_count: 0`

### Requirement: Concurrent item additions are serialized via exclusive file locking

When multiple processes attempt to add items to the same journal simultaneously, each
writer SHALL acquire an exclusive lock on a per-journal `{name}.lock` sidecar file in
the journals directory before reading and writing. The lock SHALL be released after the
write completes. This ensures that concurrent appends from multiple MCP server processes
or CLI invocations are serialized without conflicts or item loss.

#### Scenario: Concurrent writers do not lose items
- **WHEN** two processes simultaneously add items to the same journal
- **THEN** both items are present in the journal after both writes complete

### Requirement: The filename is the authoritative source of journal name

On every read, the journal name SHALL be set from the filename stem, overriding whatever
the `name` field inside the JSON says. `auth-triage.json` always produces a journal
named `auth-triage`. Editing the `name` field inside a journal file has no effect.

#### Scenario: File stem overrides JSON name field
- **WHEN** a journal file `auth-triage.json` contains `"name": "something-else"`
- **THEN** the journal is returned with `name: "auth-triage"`

### Requirement: Store operations enforce strict location by archive state

Read and write operations SHALL look only in the location (active or archive) that
matches the `archived` flag passed by the caller. If `archived: true` is passed and the
journal is in the active location (or does not exist at all), the operation SHALL return
a not-found error. If `archived: false` is passed and the journal is in the archive
location, writes return a read-only error and reads return a not-found error.

#### Scenario: Read with wrong archived flag returns not found
- **WHEN** a read is requested with `archived: true` for a journal that is active
- **THEN** a not-found error is returned

#### Scenario: Write to archived journal returns read-only error
- **WHEN** a write is requested with `archived: false` for a journal that is archived
- **THEN** a read-only error is returned
