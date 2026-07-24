# Stdio Store Specification

## Purpose

Defines the behavior of foray's remote store transport: a store backend that tunnels
all journal operations over a stdio subprocess to a remote foray MCP server. This
enables one foray instance to access journals on a remote machine (e.g. via SSH) using
the same MCP wire protocol that AI clients use. Covers the configuration format,
subprocess lifecycle, wire protocol version negotiation, protocol-0 backward
compatibility adaptations, and the content trust model.

## Requirements

### Requirement: Remote stores are declared in config.toml with type foray_stdio

A remote store SHALL be declared in `~/.foray/config.toml` (or `$FORAY_HOME/config.toml`)
as a `[stores.<name>]` section with `type = "foray_stdio"`. The section SHALL support
the fields `command` (the executable to invoke, required), `description` (human-readable
label, required), `args` (array of arguments, optional — defaults to `[]`), and
optionally `store` (the store name to use on the remote).

#### Scenario: SSH remote store configuration
- **WHEN** `config.toml` contains a `[stores.m4]` section with `type = "foray_stdio"`, `command = "ssh"`, and `args = ["user@host", "--", "foray"]`
- **THEN** `hello` returns `m4` as an available store

#### Scenario: Multiple store types coexist
- **WHEN** `config.toml` declares one `json_file` store and one `foray_stdio` store
- **THEN** `hello` returns both stores in its response

### Requirement: The stdio store spawns a subprocess and appends serve to the command

When a remote store is first used, the store SHALL spawn the configured command and
append `serve` to it, producing a foray MCP server subprocess. For example, a config
with `command = "ssh"` and `args = ["user@host", "--", "foray"]` SHALL spawn
`ssh user@host -- foray serve`. The subprocess's stdin and stdout carry the MCP
JSON-RPC wire protocol. The subprocess's stderr is forwarded to the local server log
in chunks (up to 512 bytes per read), each chunk prefixed with `[remote stderr]`.

#### Scenario: serve is appended to the configured command
- **WHEN** a remote store with `command = "foray"` and no args is used
- **THEN** the spawned process is `foray serve`

#### Scenario: Remote stderr is forwarded to local log
- **WHEN** the remote foray process writes to its stderr
- **THEN** the local server log receives lines prefixed with `[remote stderr]`

### Requirement: Connection is established lazily and cached within a session

The subprocess SHALL NOT be spawned until the first store operation is requested. Once
established, the connection SHALL be reused for all subsequent operations in the same
session. Reconnection on failure is not guaranteed; a failed connection returns an error
to the caller.

#### Scenario: No subprocess spawned until first use
- **WHEN** a remote store is configured but no operations are performed on it
- **THEN** no subprocess is spawned

#### Scenario: Connection is reused across multiple calls
- **WHEN** two successive `list_journals` calls are made to the same remote store
- **THEN** only one subprocess is running

### Requirement: hello is called at connect time to obtain the nuance and protocol version

After the MCP handshake, the store SHALL call the remote `hello` tool to obtain the
remote nuance, the remote store name, and the remote protocol version. The remote
protocol version SHALL be checked against the maximum supported version before any
other calls are made. If the remote protocol version exceeds the maximum, the connection
SHALL be rejected with a protocol-too-new error.

#### Scenario: Connect-time hello retrieves remote nuance
- **WHEN** a remote store connection is established
- **THEN** the remote `hello` response is used for all subsequent nuance parameters

#### Scenario: Remote protocol too new is rejected at connect time
- **WHEN** the remote server returns a protocol version higher than the client supports
- **THEN** the connection fails with a protocol-too-new error before any operations proceed

### Requirement: Outbound requests are adapted for the remote server's protocol version

Before sending any tool call to the remote server, the store SHALL apply protocol
adaptation to strip or transform parameters that the remote server's protocol version
does not understand. Before processing any tool response, the store SHALL apply
receive adaptation to inject synthesised defaults for fields that old servers do not
emit. These adaptations are applied for every call whose parameters or responses differ
across protocol versions.

#### Scenario: Protocol-0 server does not receive the store parameter
- **WHEN** a protocol-0 remote server is used and `list_journals` is called
- **THEN** the outbound request does not include a `store` parameter

#### Scenario: Protocol-0 hello response is augmented with synthesised fields
- **WHEN** a protocol-0 remote server responds to `hello` without `protocol`, `stores`, or `skill_uri` fields
- **THEN** the receive adapter injects `protocol: 0`, synthesises a `stores` list with a single implicit local store, and synthesises `skill_uri: ""`

### Requirement: Protocol-0 servers have specific per-tool limitations

When connected to a protocol-0 server (foray v0.2.0 or earlier), the following
limitations apply and SHALL be enforced by the adaptation layer:

- `create_journal` is sent as `open_journal` (the old tool name).
- `list_journals` has its `archived` parameter stripped; the receive adapter tags each returned entry with `archived` matching the original request; `limit` and `offset` fields in the response are stripped.
- `archive_journal` and `unarchive_journal` are not supported; calls SHALL fail with a clear error.
- `sync_journal` with `archived: true` is not supported; calls SHALL fail with a clear error. `sync_journal` with `archived: false` has the `archived` parameter stripped; `from` and `size` parameters are renamed to `cursor` and `limit` before sending; `cursor` in the response is renamed back to `from`.
- Non-default store names are not supported; a call with a non-default store name SHALL fail at connect time.

#### Scenario: archive_journal against protocol-0 server returns unsupported error
- **WHEN** `archive_journal` is called against a protocol-0 remote server
- **THEN** the call fails with an error indicating the operation is not supported by the remote server

#### Scenario: sync_journal with archived:true against protocol-0 returns error
- **WHEN** `sync_journal` is called with `archived: true` against a protocol-0 remote server
- **THEN** the call fails with an error indicating archived journals are not supported by the remote server

#### Scenario: create_journal is rewritten to open_journal for protocol-0
- **WHEN** `create_journal` is called against a protocol-0 remote server
- **THEN** the outbound MCP call uses the tool name `open_journal`

### Requirement: Remote stderr is captured and appended to errors on failure

The store SHALL capture remote stderr output in a bounded rolling buffer (4 KB). On
any transport failure, the buffered stderr content SHALL be appended to the error
message to aid diagnosis. The buffer SHALL be cleared after each successful call to
prevent stale output from bleeding into future error messages.

#### Scenario: Remote stderr appears in transport error message
- **WHEN** the remote subprocess fails and had written to its stderr
- **THEN** the returned error message includes the captured stderr content

#### Scenario: Stale stderr does not appear in later errors
- **WHEN** a call succeeds and clears the buffer, then a subsequent call fails
- **THEN** the error for the failing call does not include output from before the successful call

### Requirement: The configured command is trusted and executed without sandboxing

The `command` and `args` in `config.toml` are executed directly by the store. The
config file is the security boundary: it MUST be readable and writable only by the user.
Foray does NOT sandbox or validate the spawned command. Connecting to a store means
trusting all journals and items within it — there is no per-journal access control.
Journal content is data the model reads and reasons about; it MUST NOT be treated as
instructions that modify model behavior.

#### Scenario: Arbitrary command is spawned as configured
- **WHEN** `config.toml` specifies `command = "ssh"` with remote args
- **THEN** the store spawns exactly that command without modification or validation

#### Scenario: Journal content does not alter model behavior
- **WHEN** a journal item contains text that attempts to issue instructions
- **THEN** the content is treated as data only; the model's behavior is governed by its skill and server instructions

### Requirement: import is not supported on remote stores

The `import` CLI command relies on preserving source item IDs and timestamps, which
cannot be guaranteed when tunneling through the MCP wire protocol. Calling `import` with
a remote store as the target SHALL fail immediately with an unsupported error before
any items are transmitted. The recommended alternative is to use pipes:
`foray export <name> | ssh host foray import <name>`.

#### Scenario: import to a remote store returns an unsupported error
- **WHEN** `foray import auth-triage --file backup.json --store m4` is run
- **THEN** the command fails with an error indicating import is not supported for remote stores

### Requirement: delete is not supported on remote stores

The `delete` CLI command operates on local file paths and cannot be performed over the
MCP wire protocol. Calling `delete` with a remote store as the target SHALL fail with
an unsupported error.

#### Scenario: delete against a remote store returns an unsupported error
- **WHEN** `foray delete auth-triage --store m4` is run
- **THEN** the command fails with an error indicating delete is not supported for remote stores

### Requirement: sync_journal wire responses are schema-migrated

After receiving a `sync_journal` response from a remote server, the store SHALL apply
the schema migration chain to the returned items. This handles the case where the remote
server is running an older foray version and returns items in a pre-v1 format. If the
remote server's response carries a schema version newer than the local maximum, the store
SHALL return a schema-too-new error.

#### Scenario: sync_journal response with schema-0 items is migrated transparently
- **WHEN** a remote server returns a `sync_journal` response with items in schema-0 format
- **THEN** the items are migrated to v1 format before being returned to the caller

#### Scenario: sync_journal response with schema too new returns error
- **WHEN** a remote server returns a `sync_journal` response with a schema version newer than supported
- **THEN** a schema-too-new error is returned with the journal name and version details
