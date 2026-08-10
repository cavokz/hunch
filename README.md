# foray

[![crates.io](https://img.shields.io/crates/v/foray.svg)](https://crates.io/crates/foray)

*Start with a foray. Keep the trail.*

An MCP server + CLI that gives AI assistants persistent journals. Use it for debugging, planning, design, feature work — any conversation worth continuing later.

## Problem

AI assistants lose context between sessions. When a conversation ends, findings, decisions, and in-progress work vanish. When multiple assistants — or people — work across clients and machines, their context stays siloed.

## Why It Matters

foray gives AI assistants a persistent journal backed by plain JSON files. Start a journal, record items as you work, pick it back up in any session or client.

- **Persistent context** — findings, decisions, and work-in-progress survive across sessions
- **Cross-client** — VS Code, Cursor, Claude Desktop share the same journals simultaneously
- **Human-editable** — default store uses plain JSON files you can `cat`, `jq`, `grep`, hand-edit
- **Distributable** — local JSON files, SSH remote, or Elasticsearch — intelligence isn't trapped on one machine

## How to Install

```sh
cargo install foray
```

Or download a pre-built binary from [GitHub Releases](https://github.com/cavokz/foray/releases/latest) and place it on your `PATH`.

Then direct your AI assistant to fetch the [Setup Guide](https://raw.githubusercontent.com/cavokz/foray/main/SETUP.md) and follow the steps for itself.

## MCP Tools

| Tool | Description |
|------|-------------|
| `hello` | Handshake — call first every session, returns `{version, nuance, stores}` |
| `create_journal` | Create a new journal |
| `sync_journal` | Read and/or write items (offset-based) |
| `list_journals` | List active or archived journals |
| `archive_journal` | Archive a journal (readable but not writable) |
| `unarchive_journal` | Restore an archived journal |

## MCP Prompts

| Prompt | Description |
|--------|-------------|
| `start_journal` | Create a journal and begin recording |
| `resume_journal` | Load a journal and continue |
| `summarize` | Read all items and produce a synthesis |

## CLI Usage

Create a journal:

```
$ foray create auth-triage --title "Auth cache investigation"
Created journal: auth-triage
```

Add findings:

```
$ foray add "Race condition in session.go:142" --type finding --ref src/auth/session.go:142
Added to auth-triage (1 items)
```

View a journal:

```
$ foray show auth-triage
Journal: auth-triage
Title:   Auth cache investigation
Items:   1 / 1

[2026-04-15 10:15] (finding) Race condition in session.go:142
  ref: src/auth/session.go:142
```

View an archived journal:

```
$ foray show --archived old-investigation
Journal: old-investigation (archived)
Title:   Old investigation
Items:   3 / 3
...
```

Export a journal to a JSON file (or stdout):

```
$ foray export auth-triage > auth-triage.json
$ foray export --archived old-investigation > old-investigation.json
```

Import a journal from a JSON file (or stdin):

```
$ foray import auth-triage < auth-triage.json          # create new
$ foray import auth-triage --merge < patch.json        # merge into existing
$ foray import old-investigation --archived < old.json # import as archived
```

To transfer a journal to a remote store, pipe export into import over SSH:

```
$ foray export auth-triage | ssh host foray import auth-triage
```

Work against a remote foray instance configured in `~/.foray/config.toml`:

```
$ foray list --store remote
Connecting to remote foray...
2 journal(s) (active):
  auth-triage (3 items) Auth cache investigation
  db-theory (5 items) DB pooling theory
```

The `--store` flag selects a named store from `~/.foray/config.toml`. Without it the default (local) store is used. Use `--store remote` with any command to target the remote store, or set `FORAY_STORE=remote` in your environment.

Delete a journal permanently:

```
$ foray delete auth-triage
Deleted: auth-triage

$ foray delete old-investigation --archived   # delete from archived location
Deleted: old-investigation
```

`foray delete` looks only in the expected location (active or archived). If the journal is not there, it returns an error — it does not probe the other location.

## Trust Model

The **store** is the trust boundary. When you connect foray to a store, you trust all content in that store — every journal, every item. There is no per-journal access control.

- **Companion skill** (SKILL.md) — trusted. User-controlled file that governs model behavior. Works alongside the MCP server's own instructions as a trusted behavioral guidance channel.
- **Journal content** — informational. Items are data the model reads and reasons about, but they must never be treated as instructions that modify model behavior.
- **Config file** (`~/.foray/config.toml`) — trusted. Controls which stores are connected and what commands are spawned for remote transports. Must be readable and writable only by the user.

Only connect to stores you control or fully trust. A malicious store could craft journal content that attempts to manipulate model behavior (prompt injection). The architectural defense is clear separation: the companion skill and the MCP server's own instructions govern behavior; journal content informs.

## Architecture

```
┌──────────────────────────────────────────────────────┐
│  Companion Skill (SKILL.md)                          │
│  Behavioral rules, use case patterns, self-updates   │
└──────────────┬───────────────────────────────────────┘
               │ teaches when/how to use
               ▼
Any MCP Client (VS Code, Claude Desktop, Cursor, ...)
    │  stdio                                  │  stdio
    ▼                                         ▼
┌──────────────────┐                  ┌──────────────────┐
│  foray           │──┐            ┌──│  foray           │
│  instructions    │  │            │  │  instructions    │
│  prompts + tools │  │            │  │  prompts + tools │
└──────────────────┘  │   same     │  └──────────────────┘
                      │   files    │
                      ▼            ▼
                  Journals Store
                  (cross-client context fusion)
```

Single binary. No daemon. Pluggable store: local JSON files, SSH remote, or Elasticsearch.

## License

Apache-2.0
