# Setup Guide

> **For AI assistants:** You are being directed to set up foray for yourself. You know which MCP client you are (Claude Code, Claude Desktop, Cursor, VS Code / GitHub Copilot / VS Code Agents). Determine the OS from system context, or ask the user. Then follow **Steps 2–4** for your client and OS. Step 1 is a one-time user action — confirm with the user whether the binary is already installed before attempting it.

Step 1 is done once. Steps 2–4 must be repeated for each AI assistant/client you want to use foray with — you can direct each assistant to this guide for further support.

## 1. Install the Binary (one-time)

Check if foray is already installed:

```sh
foray --version
```

If not installed, install via cargo:

```sh
cargo install foray
```

Or download a pre-built binary from [GitHub Releases](https://github.com/cavokz/foray/releases/latest):

| Platform | Binary |
|----------|--------|
| macOS (universal) | `foray-macos-universal` |
| Linux x86_64 | `foray-linux-x86_64` |
| Linux arm64 | `foray-linux-aarch64` |
| Windows x86_64 | `foray-windows-x86_64.exe` |
| Windows arm64 | `foray-windows-aarch64.exe` |

Place the binary somewhere on your `PATH` (e.g. `/usr/local/bin`, `~/.local/bin`).

<details>
<summary>Alternative: build from source</summary>

```sh
git clone https://github.com/cavokz/foray.git
cd foray
cargo install --path .
```

</details>

---

## Per-Assistant Setup (Steps 2–4)

Complete steps 2–4 for each AI assistant/client you use. You can direct each assistant to this guide for further support.

## 2. Configure MCP Server

The MCP server exposes foray's journal tools to your AI client. It runs locally as a subprocess — no network ports, no authentication.

After adding or changing MCP server config, restart the application or start a new chat session for the tools to appear.

### Claude Code

Run this command to register foray as a global MCP server:

```sh
claude mcp add --scope user foray foray serve
```

### Claude Desktop

> **Note for Claude Desktop assistants:** You cannot configure your own MCP server — it is loaded before the session starts. Guide the user to edit the config file manually, then ask them to restart Claude Desktop before proceeding.

Config file:
- macOS: `~/Library/Application Support/Claude/claude_desktop_config.json`
- Windows: `%APPDATA%\Claude\claude_desktop_config.json`

```json
{
  "mcpServers": {
    "foray": {
      "command": "foray",
      "args": ["serve"]
    }
  }
}
```

### Cursor

Config file:
- macOS / Linux: `~/.cursor/mcp.json`
- Windows: `%USERPROFILE%\.cursor\mcp.json`

```json
{
  "mcpServers": {
    "foray": {
      "type": "stdio",
      "command": "foray",
      "args": ["serve"]
    }
  }
}
```

### VS Code / GitHub Copilot / VS Code Agents

VS Code Agents shares the same configuration directory as VS Code.

Config file:
- macOS: `~/Library/Application Support/Code/User/mcp.json`
- Linux: `~/.config/Code/User/mcp.json`
- Windows: `%APPDATA%\Code\User\mcp.json`

```json
{
  "servers": {
    "foray": {
      "type": "stdio",
      "command": "foray",
      "args": ["serve"]
    }
  }
}
```

## 3. Install Companion Skill (Recommended)

The companion skill teaches your AI assistant when and how to use foray effectively.

Download `SKILL.md` from the [latest release](https://github.com/cavokz/foray/releases/latest/download/SKILL.md) and install it in:

| Location | macOS / Linux | Windows |
|----------|---------------|---------|
| Universal (VS Code, GitHub Copilot CLI, Cursor, Codex, most tools) | `~/.agents/skills/foray/SKILL.md` | `%USERPROFILE%\.agents\skills\foray\SKILL.md` |
| Claude Code | `~/.claude/skills/foray/SKILL.md` | `%USERPROFILE%\.claude\skills\foray\SKILL.md` |

Install `SKILL.md` in both locations if you use Claude Code alongside other tools.

> **Note for Claude Desktop assistants:** Claude Desktop has no file-based skill path. The practical alternative is project instructions: guide the user to open Claude Desktop, go to **Projects**, create or open the project they use for development work, click **"Set project instructions"** on the right panel, paste the full contents of `SKILL.md`, and click **"Save instructions"**. The skill will then be active for all conversations in that project. Projects and project instructions are available to all users including free accounts.

## 4. Verify

If you changed the MCP config in Step 2, make sure the application has been restarted or a new chat session has been started before continuing.

Invoke the `list_journals` MCP tool now. If the server is running correctly, it will respond (the list may be empty — that is fine).

---

## Optional: Configure Stores

By default foray uses a local JSON file store at `~/.foray/journals/`. To add additional stores — including remote or shared ones — create `~/.foray/config.toml`:

```toml
# SSH remote: journals stored on a server running foray
[stores.remote]
description = "My remote server"
type = "foray_stdio"
command = "ssh"
args = ["user@myserver", "--", "foray"]

# Elasticsearch: journals stored in an ES index
[stores.team]
description = "Team shared journals"
type = "elasticsearch"
url = "https://your-es-host/foray-team"   # index is the last path segment
api_key = "..."        # recommended: base64(id:api_key) from ES API key creation
# username = "elastic" # alternative to api_key
# password = "changeme"
```

Use a named store with the `--store` flag or by setting `current-store` in `.forayrc`:

```sh
foray list --store team
foray open my-journal --store team   # also sets current-store in .forayrc
```

### Elasticsearch: Cluster Setup (admin, once per cluster/team)

These steps require admin credentials and are done once per cluster (index template) or once per team (role). They prepare ES to host foray indices safely.

**1 — Provision the index template:**

Download [`doc/es-index-template.json`](https://raw.githubusercontent.com/cavokz/foray/main/doc/es-index-template.json) from the repository, then:

```sh
curl -u elastic:ADMIN_PASSWORD \
  -X PUT https://your-es-host/_index_template/foray \
  -H 'Content-Type: application/json' \
  -d @es-index-template.json
```

The template applies to any index matching `foray-*` and enforces strict field mappings. It must exist before foray creates its first index.

**2 — Create a dedicated role (once per team):**

Foray needs these index-level privileges on its indices:

| Privilege | Used for |
|-----------|----------|
| `create_index` | Auto-creating the index on first write |
| `index` | Indexing journal and item documents (`_bulk`, `_update`) |
| `read` | Searching journals and items (`_search`, `_doc`) |

Create one role per team, scoped to that team's indices only (e.g. `foray-team-a-*` for team A). Run in [Kibana Dev Tools](https://www.elastic.co/guide/en/kibana/current/console-kibana.html). If you prefer `curl`, ask an AI assistant to translate.

```
PUT /_security/role/foray-team-a
{
  "indices": [{
    "names": ["foray-team-a-*"],
    "privileges": ["create_index", "index", "read"]
  }]
}
```

### Elasticsearch: User Setup (per user)

These steps are performed by each user to obtain credentials scoped to foray indices only.

**1 — Create a scoped API key:**

Open [Kibana Dev Tools](https://www.elastic.co/guide/en/kibana/current/console-kibana.html) and run. If you prefer `curl`, ask an AI assistant to translate.

```
POST /_security/api_key
{
  "name": "foray-team-a",
  "role_descriptors": {
    "foray-team-a": {
      "indices": [{
        "names": ["foray-team-a-*"],
        "privileges": ["create_index", "index", "read"]
      }]
    }
  }
}
```

Use the `encoded` field from the response as the `api_key` value in config. The `role_descriptors` cap the key's access to `foray-team-a-*` indices only, regardless of the creating user's broader permissions.

**2 — Add the store to `~/.foray/config.toml`:**

```toml
[stores.team-a]
description = "Team A shared journals"
type = "elasticsearch"
url = "https://your-es-host/foray-team-a"
api_key = "<encoded value from step 1>"
```

**3 — Verify:**

```sh
foray list --store team-a
```

An empty list (no error) confirms the store is reachable and correctly configured.

