# CLI Specification

## Purpose

Defines the foray command-line interface: all subcommands and their flags, global
options, journal and store resolution chains, the `.forayrc` configuration file format,
and shell completion. The CLI gives humans direct access to the same store backends as
the MCP server, without requiring an MCP client.

## Requirements

### Requirement: serve starts the MCP stdio server

The `foray serve` command SHALL start the foray MCP server, listening on stdin and
writing JSON-RPC responses to stdout. On startup it SHALL print a banner to stderr in
the format `foray <version> (<git-describe>)[ FORAY_HOME=<path>]`. The `FORAY_HOME`
portion is included only when `FORAY_HOME` is set.

#### Scenario: serve prints a startup banner to stderr
- **WHEN** `foray serve` is started
- **THEN** stderr receives a line containing the foray version and git describe string

### Requirement: show displays a journal with all its items

`foray show [name] [--json] [--archived] [--follow]` SHALL display the journal identified
by the resolved journal name. Without `--json` the output is human-readable plain text.
With `--json` the output is NDJSON — each item serialized as a compact JSON object on its
own line. `--archived` causes the command to look in the archived location. `--follow`
(`-f`) polls for new items every 500ms and streams them as they appear; compatible with
both plain text and `--json` output.

#### Scenario: show outputs items in plain text by default
- **WHEN** `foray show auth-triage` is run and the journal has 3 items
- **THEN** stdout contains all 3 items in human-readable form

#### Scenario: --json outputs NDJSON items
- **WHEN** `foray show auth-triage --json` is run
- **THEN** stdout contains one compact JSON object per line, one per item

#### Scenario: --follow streams new items
- **WHEN** `foray show auth-triage --follow` is run and a new item is added
- **THEN** the new item is printed without restarting the command

### Requirement: add appends an item to the current journal

`foray add <content> [--journal|-j NAME] [--type TYPE] [--ref REF] [--tags CSV] [--meta KEY=VALUE]...`
SHALL add a single item to the resolved journal. `--journal`/`-j` explicitly names the
journal for this call (overrides env and `.forayrc`). `--type` sets the item type (default:
`note`). `--ref` populates `meta.ref`. `--tags` accepts a comma-separated list.
`--meta KEY=VALUE` may be repeated to set additional metadata keys. The same input limits
as the MCP server apply: content max 64 KB, max 20 tags each max 64 bytes, meta max 8 KB.

#### Scenario: add with content creates a note item
- **WHEN** `foray add "Found the race condition"` is run
- **THEN** a new item with type `note` and that content is appended to the journal

#### Scenario: --ref populates meta.ref
- **WHEN** `foray add "Race condition" --ref "src/auth/session.go:142"` is run
- **THEN** the item is stored with `meta.ref: "src/auth/session.go:142"`

#### Scenario: --type sets the item type
- **WHEN** `foray add "Chose append-only" --type decision` is run
- **THEN** the item is stored with type `decision`

### Requirement: create creates a new journal

`foray create <name> --title "..." [--meta KEY=VALUE]...` SHALL create a new journal.
`--title` is required; omitting it is an error. The command always creates — it does
not open an existing journal.

#### Scenario: create with name and title creates the journal
- **WHEN** `foray create auth-triage --title "Auth cache investigation"` is run
- **THEN** the journal is created and a confirmation is printed

#### Scenario: create without --title fails
- **WHEN** `foray create auth-triage` is run without `--title`
- **THEN** the command exits with an error indicating `--title` is required

### Requirement: list shows all journals in the active store

`foray list [--json] [--archived] [--completion]` SHALL list journals in the resolved
store. `--archived` controls which set is shown: without it only active journals are
listed; with it only archived journals are listed. `--json` controls the output format:
without it output is plain text; with it output is `{"total": N, "journals": [...]}`.
`--completion` outputs bare journal names one per line (used by shell completion).
`--completion` and `--json` are mutually exclusive.

#### Scenario: list outputs active journals in plain text
- **WHEN** `foray list` is run with three active journals
- **THEN** stdout contains all three journal names

#### Scenario: --json outputs structured JSON
- **WHEN** `foray list --json` is run
- **THEN** stdout is valid JSON with `total` and `journals` fields

#### Scenario: --completion outputs bare names
- **WHEN** `foray list --completion` is run
- **THEN** stdout contains one journal name per line with no decoration

### Requirement: delete permanently removes a journal

`foray delete <name> [--archived] [--force]` SHALL permanently remove the journal from
the store. Without `--force` the command SHALL prompt for confirmation. `--archived`
deletes from the archived location; without it, from the active location. The command
SHALL error if the journal does not exist in the expected location.

#### Scenario: delete prompts for confirmation without --force
- **WHEN** `foray delete auth-triage` is run without `--force`
- **THEN** the command prints a confirmation prompt before deleting

#### Scenario: delete --force skips the prompt
- **WHEN** `foray delete auth-triage --force` is run
- **THEN** the journal is deleted immediately without prompting

#### Scenario: delete --archived targets the archive location
- **WHEN** `foray delete auth-triage --archived` is run
- **THEN** the journal is deleted from the archived location

### Requirement: archive and unarchive change journal state

`foray archive <name>` SHALL archive the named active journal. `foray unarchive <name>`
SHALL restore the named archived journal. Both commands SHALL error if the journal is not
in the expected location.

#### Scenario: archive moves journal to archive
- **WHEN** `foray archive auth-triage` is run on an active journal
- **THEN** the journal is archived and a confirmation is printed

#### Scenario: unarchive restores journal to active
- **WHEN** `foray unarchive auth-triage` is run on an archived journal
- **THEN** the journal is restored and a confirmation is printed

### Requirement: export writes journal JSON to stdout or a file

`foray export <name> [--file PATH] [--archived]` SHALL write the full journal JSON to
stdout, or to a file if `--file` is given. Without `--archived` it exports only active
journals; with `--archived` only archived journals. It SHALL error with "not found" if
the journal is not in the expected location.

#### Scenario: export writes JSON to stdout
- **WHEN** `foray export auth-triage` is run
- **THEN** stdout contains the full journal JSON

#### Scenario: export --file writes to a file
- **WHEN** `foray export auth-triage --file backup.json` is run
- **THEN** `backup.json` contains the full journal JSON and stdout is empty

### Requirement: import reads journal JSON from stdin or a file

`foray import <name> [--file PATH] [--merge] [--archived]` SHALL import a journal.
`<name>` is the destination journal name. The source JSON is schema-migrated before
import; sources with a schema version newer than supported SHALL be rejected with a
schema-too-new error. Without `--merge` it creates a new journal (fails if the name
already exists in either the active or archived location). With `--merge` it appends
items to an existing active journal, skipping items whose ID already exists; source
`title` and `meta` are ignored in merge mode; `added_at` timestamps from the source are
preserved. `--archived` creates the imported journal directly in archived state (mutually
exclusive with `--merge`).

#### Scenario: import without --merge creates a new journal
- **WHEN** `foray import auth-triage --file backup.json` is run for a non-existent name
- **THEN** a new journal named `auth-triage` is created with the content from `backup.json`

#### Scenario: import --merge appends new items and skips duplicates
- **WHEN** `foray import auth-triage --merge --file updates.json` is run
- **THEN** items not already present by ID are appended; duplicate IDs are skipped with a warning

#### Scenario: import --merge with --archived is rejected
- **WHEN** `foray import auth-triage --merge --archived` is run
- **THEN** the command exits with an error indicating the flags are mutually exclusive

### Requirement: completions prints a shell completion script

`foray completions <shell>` SHALL print a shell completion script for the specified
shell to stdout. Supported shells SHALL include bash, zsh, fish, elvish, and powershell.

#### Scenario: completions zsh outputs a zsh script
- **WHEN** `foray completions zsh` is run
- **THEN** stdout contains a valid zsh completion script

### Requirement: Journal name is resolved via a precedence chain

For `show`, the journal name is resolved from: (1) positional `name` argument, (2)
`FORAY_JOURNAL` environment variable, (3) `current-journal` value from the nearest
`.forayrc` file found walking up from the current directory.

For `add`, the journal name is resolved from: (1) `--journal`/`-j` flag, (2)
`FORAY_JOURNAL` environment variable, (3) `current-journal` value from the nearest
`.forayrc` file.

If none of the sources provide a name, the command SHALL fail with a helpful error
message.

#### Scenario: Positional name used for show
- **WHEN** `foray show auth-triage` is run with `FORAY_JOURNAL=other` set
- **THEN** the journal named `auth-triage` is shown

#### Scenario: --journal flag on add takes precedence over env var
- **WHEN** `foray add "item" --journal explicit` is run with `FORAY_STORE=other` set
- **THEN** the item is added to `explicit`

#### Scenario: FORAY_JOURNAL env var used when no flag
- **WHEN** `FORAY_JOURNAL=auth-triage foray add "item"` is run without `--journal`
- **THEN** the item is added to `auth-triage`

#### Scenario: No journal source produces a helpful error
- **WHEN** `foray add "item"` is run with no flag, no env var, and no .forayrc
- **THEN** the command exits with an error describing all resolution options

### Requirement: Store is resolved via a precedence chain

The active store is resolved in this order: (1) `--store <name>` flag, (2)
`FORAY_STORE` environment variable, (3) `current-store` value from the nearest
`.forayrc` file, (4) the registry default when exactly one store is configured.
If multiple stores are configured and none of the first three sources provide a name,
the command SHALL fail with an error listing available store names.

#### Scenario: --store flag overrides everything
- **WHEN** `foray list --store work` is run with `FORAY_STORE=local` set
- **THEN** journals from the `work` store are listed

#### Scenario: Single configured store is used automatically
- **WHEN** only one store is configured and no store is specified
- **THEN** that store is used without error

#### Scenario: Multiple stores with no selection produces an error
- **WHEN** two stores are configured and `foray list` is run without specifying a store
- **THEN** the command exits with an error listing the available store names

### Requirement: .forayrc is a TOML file resolved by walking up from the current directory

The `.forayrc` file SHALL use TOML syntax and support three keys: `current-journal`
(optional, journal name for CLI resolution), `current-store` (optional, store name for
CLI resolution), and `root` (optional boolean). The walk SHALL start at the current
directory and proceed to parent directories. The first `.forayrc` file that contains the
relevant key wins. A file with `root = true` SHALL halt the walk regardless of whether
it contains the sought key.

#### Scenario: .forayrc in cwd provides journal name
- **WHEN** `.forayrc` in the current directory contains `current-journal = "auth-triage"` and no flag or env var is set
- **THEN** `foray show` displays the `auth-triage` journal

#### Scenario: root = true halts the upward walk
- **WHEN** `.forayrc` in the current directory contains `root = true` (but not `current-journal`) and a parent `.forayrc` does contain it
- **THEN** the parent's journal name is NOT used; the walk stops

### Requirement: Shell completion supports static and dynamic modes

`foray completions <shell>` SHALL generate a static completion script that completes
subcommands and flags. When `COMPLETE=<shell> foray` is invoked by the shell's
completion mechanism, dynamic completion SHALL be enabled: store names are completed by
reading the registry in-process; journal names are completed by calling
`foray list --completion [--archived] [--store <name>]` as a subprocess, with a 10-second
timeout to prevent blocking on slow stores.

#### Scenario: Static completion script includes subcommands
- **WHEN** the static completion script is sourced and the user types `foray <TAB>`
- **THEN** available subcommands are offered as completions

#### Scenario: Dynamic completion completes journal names via subprocess
- **WHEN** the dynamic completion path is active and the user is completing a journal name argument
- **THEN** the completer calls `foray list --completion` and offers the returned names

#### Scenario: Dynamic completion completes store names in-process
- **WHEN** the dynamic completion path is active and the user is completing `--store`
- **THEN** the completer reads the store registry in-process and offers configured store names
