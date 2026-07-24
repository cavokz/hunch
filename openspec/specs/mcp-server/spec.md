# MCP Server Specification

## Purpose

Defines the behavior of foray's MCP server: its identity, the session handshake and
nuance preflight mechanism, all six tools with their parameters and response formats,
the three prompt templates, the companion skill resource, structured error contracts,
and per-call logging. The server is fully stateless — every tool call carries all
context needed to process it and no session state is retained between calls.

## Requirements

### Requirement: Server advertises a stable identity on initialization

The server SHALL respond to the MCP `initialize` request with a `serverInfo` object
containing: `name` set to `"foray"`, `version` set to the binary's version string,
`title` set to `"Foray — Persistent Journals for AI Agents"`, and `description` set to
the package description. The `title` field is the human-readable display name shown
in MCP client UIs.

#### Scenario: Initialize returns server identity
- **WHEN** an MCP client connects and sends `initialize`
- **THEN** the response `serverInfo` contains `name: "foray"`, a non-empty `version`, a `title`, and a `description`

### Requirement: Server sends bootstrap instructions to every client on initialization

The server SHALL include a static `instructions` string in the MCP initialization
response. The instructions SHALL direct clients to call `hello` first, pass `nuance` and
`store` on all subsequent calls, and fetch the `foray://skill` MCP resource for full
workflow guidance if the companion skill is not already loaded.

#### Scenario: Instructions field present on initialization
- **WHEN** an MCP client connects
- **THEN** the initialization response includes a non-empty `instructions` field

### Requirement: hello establishes the session and returns the nuance token

The `hello` tool SHALL be called before any other tool in every session. It SHALL return
`version` (server version string), `nuance` (session epoch token), `protocol` (integer
wire protocol version), `stores` (array of `{name, description}` objects for all
configured stores), and `skill_uri` (the MCP resource URI for the companion skill,
`"foray://skill"`).

#### Scenario: hello returns all required fields
- **WHEN** `hello` is called
- **THEN** the response contains `version`, `nuance`, `protocol`, `stores`, and `skill_uri`

#### Scenario: stores list reflects configured stores
- **WHEN** the server is configured with two stores named `local` and `work`
- **THEN** `hello` returns `stores: [{name:"local",...}, {name:"work",...}]`

### Requirement: nuance is required on all tool calls except hello

Every tool except `hello` SHALL require a `nuance` parameter. If `nuance` is missing
or does not match the current server nuance, the tool SHALL return an `invalid_params`
error with `data.hint` set to `"call 'hello' to get the current nuance"`. The nuance
is a deterministic fingerprint of all store configuration and the current schema and
protocol versions; any configuration change or binary upgrade that would make a cached
session incorrect automatically changes the nuance.

#### Scenario: Missing nuance returns an error with hint
- **WHEN** `list_journals` is called without a `nuance` parameter
- **THEN** the response is an error with `data.hint` containing guidance to call `hello`

#### Scenario: Wrong nuance returns an error with hint
- **WHEN** `list_journals` is called with a nuance from a previous session
- **THEN** the response is an error with `data.hint` containing guidance to call `hello`

#### Scenario: Nuance changes when store configuration changes
- **WHEN** a store is added to the configuration and `hello` is called again
- **THEN** the returned `nuance` differs from the one returned before the change

### Requirement: store parameter is required on all tools that access a journal

Every tool that operates on journals SHALL require a `store` parameter identifying which
configured store to use. If `store` is absent or names an unknown store, the tool SHALL
return an `invalid_params` error listing the available store names.

#### Scenario: Missing store returns an error
- **WHEN** `list_journals` is called without a `store` parameter
- **THEN** the response is an error with a hint listing available store names

#### Scenario: Unknown store returns an error
- **WHEN** `list_journals` is called with `store: "nonexistent"`
- **THEN** the response is an error with a hint listing available store names

### Requirement: create_journal creates a new journal and errors if it already exists

`create_journal` SHALL accept `name`, `title`, `store`, `nuance`, and optionally `meta`.
It SHALL validate the name and title before accessing the store. If the journal does not
exist, it SHALL create it and return `{name, title}`. If a journal with that name already
exists, it SHALL return an `invalid_params` error with `data.type: "journal_already_exists"`.
`title` is mandatory; an empty or whitespace-only title SHALL be rejected.

#### Scenario: New journal is created successfully
- **WHEN** `create_journal` is called with a valid name and title for a non-existent journal
- **THEN** the response is `{name, title}` and the journal exists in the store

#### Scenario: Duplicate name returns AlreadyExists error
- **WHEN** `create_journal` is called with the name of an existing journal
- **THEN** the response is an error with `data.type: "journal_already_exists"`

#### Scenario: Empty title is rejected before store access
- **WHEN** `create_journal` is called with an empty title
- **THEN** the response is an `invalid_params` error and no journal is created

### Requirement: sync_journal reads and writes items in a single call

`sync_journal` SHALL accept `name`, `from` (integer offset), `size` (max items to
return), `archived` (boolean, must match the journal's current archive state), `store`,
`nuance`, and optionally `items` (array of items to add). It SHALL return `{schema,
name, title, items, added_ids, from, total}` where `from` is the next offset for the
subsequent call, `total` is the total item count, and `added_ids` lists IDs assigned to
newly added items in order. Adding items and reading are performed in a single atomic
operation.

`from` is a plain integer offset (0 = start). The server imposes no upper bound on
`size`; callers are responsible for choosing a size that fits their output budget.

#### Scenario: Read from beginning returns items and next offset
- **WHEN** `sync_journal` is called with `from: 0` and `size: 10` on a journal with 25 items
- **THEN** the response contains the first 10 items, `from: 10`, and `total: 25`

#### Scenario: Adding items returns assigned IDs
- **WHEN** `sync_journal` is called with two items to add
- **THEN** `added_ids` contains exactly two IDs in insertion order

#### Scenario: Read and write in one call
- **WHEN** `sync_journal` is called with `from: 5`, `size: 3`, and one item to add
- **THEN** the response returns up to 3 items starting from offset 5, and `added_ids` contains the new item's ID

#### Scenario: archived flag mismatch returns an error
- **WHEN** `sync_journal` is called with `archived: true` for a journal that is active
- **THEN** the response is a not-found error

#### Scenario: Write to archived journal returns read-only error
- **WHEN** `sync_journal` is called with items and `archived: false` for a journal that is archived
- **THEN** the response is a read-only error with `data.type: "journal_read_only"`

### Requirement: list_journals returns all journals in one call

`list_journals` SHALL accept `store` and `nuance`. It SHALL return `{journals, total}`
where `journals` is an array of entries — both active and archived — each containing
`name`, `title`, `item_count`, `archived` (boolean), and optionally `avg_item_size`,
`std_item_size`, `schema`, and `error`. The `archived` field SHALL always be present.
Size statistics SHALL be present for non-empty readable journals.

#### Scenario: Returns all active and archived journals
- **WHEN** `list_journals` is called with two active and one archived journal in the store
- **THEN** all three appear in the response, each with the correct `archived` value

#### Scenario: Response includes size statistics for non-empty journals
- **WHEN** `list_journals` is called and a journal has items
- **THEN** the entry includes `avg_item_size` and `std_item_size`

### Requirement: archive_journal and unarchive_journal manage journal archiving

`archive_journal` SHALL accept `name`, `store`, and `nuance`. It SHALL move the journal
to the archived location and return `{archived: "<name>"}`. If no active journal with
that name exists, it SHALL return a not-found error.

`unarchive_journal` SHALL accept `name`, `store`, and `nuance`. It SHALL move the journal
to the active location and return `{unarchived: "<name>"}`. If no archived journal with
that name exists, it SHALL return a not-found error.

#### Scenario: archive_journal returns confirmation
- **WHEN** `archive_journal` is called on an active journal
- **THEN** the response is `{archived: "<name>"}` and the journal is now archived

#### Scenario: unarchive_journal returns confirmation
- **WHEN** `unarchive_journal` is called on an archived journal
- **THEN** the response is `{unarchived: "<name>"}` and the journal is now active

#### Scenario: archive_journal on non-existent journal returns not-found
- **WHEN** `archive_journal` is called for a journal that does not exist
- **THEN** the response is a not-found error

### Requirement: Errors include a structured data object for programmatic dispatch

Input validation errors (invalid name, invalid title, unknown item type, content too
large, too many tags, tag too long, meta too large) SHALL return `invalid_params` with
`data.type: "invalid_input"` and `data.hint` describing the violation. All other error
conditions SHALL include a `data` object as described in the table below.

| Condition | Error code | `data.type` | `data.remedy` |
|-----------|------------|-------------|---------------|
| nuance missing or wrong | `invalid_params` | *(none)* | — |
| Input validation failure | `invalid_params` | `"invalid_input"` | — |
| Journal not found | `invalid_params` | `"journal_not_found"` | — |
| Journal already exists | `invalid_params` | `"journal_already_exists"` | — |
| Journal is read-only (archived) | `invalid_params` | `"journal_read_only"` | — |
| Schema too new | `internal_error` | `"schema_too_new"` | `"upgrade_foray"` |
| Protocol too new | `internal_error` | `"protocol_too_new"` | `"upgrade_foray"` |
| I/O error | `internal_error` | `"io_error"` | — |
| JSON error | `internal_error` | `"json_error"` | — |
| Unsupported operation | `internal_error` | `"unsupported"` | — |

#### Scenario: Journal not found error includes data.type and hint
- **WHEN** `sync_journal` is called for a journal that does not exist
- **THEN** the error has `data.type: "journal_not_found"` and a non-empty `data.hint`

#### Scenario: Schema too new error includes remedy
- **WHEN** a journal file has a schema version higher than the server supports
- **THEN** the error has `data.type: "schema_too_new"` and `data.remedy: "upgrade_foray"`

### Requirement: Server exposes three prompt templates for common workflows

The server SHALL expose three MCP prompts: `start_journal` (params: `name`, `title` —
guides listing existing journals, creating a new one, and beginning to record items),
`resume_journal` (params: `name` — guides loading the journal, summarizing recent items,
and continuing), and `summarize` (params: `name` — guides reading all items and producing
a synthesis). Prompts are the fallback for clients without the companion skill.

#### Scenario: Prompts are discoverable via MCP prompts list
- **WHEN** an MCP client requests the prompts list
- **THEN** the response includes `start_journal`, `resume_journal`, and `summarize`

### Requirement: Server exposes the companion skill as an MCP resource

The server SHALL expose the companion skill as an MCP resource at URI `foray://skill`,
returned as `text/markdown`. The resource SHALL be embedded in the binary at compile
time. The `hello` response SHALL include `skill_uri: "foray://skill"` so clients can
discover and fetch it immediately after the handshake.

#### Scenario: foray://skill resource is readable
- **WHEN** an MCP client fetches the resource `foray://skill`
- **THEN** the response is the companion skill content as `text/markdown`

#### Scenario: Unknown resource URI returns an error
- **WHEN** an MCP client fetches a resource at an unknown URI
- **THEN** the server returns an `INVALID_PARAMS` error

### Requirement: Every tool invocation is logged to stderr

Each tool call SHALL produce a log line on stderr. On success, exactly one line is
emitted. On failure, a second line `error: <message>` is emitted after the call line.
Stdout carries only the JSON-RPC wire protocol. The log format per tool:

| Tool | Log line |
|------|----------|
| `hello` | `hello` |
| `create_journal` | `create_journal (<store>) <name>` |
| `sync_journal` | `sync_journal (<store>) <name> from=N size=N [+N items]` — `+N items` omitted when no items are added |
| `list_journals` | `list_journals (<store>)` |
| `archive_journal` | `archive_journal (<store>) <name>` |
| `unarchive_journal` | `unarchive_journal (<store>) <name>` |

#### Scenario: sync_journal with items logs item count
- **WHEN** `sync_journal` is called with 3 items to add
- **THEN** stderr contains a line matching `sync_journal (<store>) <name> from=N size=N +3 items`

#### Scenario: sync_journal without items omits item count
- **WHEN** `sync_journal` is called with no items
- **THEN** the stderr log line does not contain `+N items`

#### Scenario: Failed tool call emits an error line
- **WHEN** a tool call returns an error
- **THEN** stderr contains the call line followed by a second line `error: <message>`
