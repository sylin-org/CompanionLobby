# MCP hosts

Every host attaches through the shim: its configuration names
`companion-lobby attach <companion>`, and the companion's moniker or id is the
only argument. The server name in the configuration is the companion's
moniker. Examples below use `C:\Tools\companion-lobby.exe`; use the path of
your build.

## What each host was seen to do

| Host | Exercised | What it does |
| --- | --- | --- |
| Claude Code 2.1.303, Windows | Yes, headless (`claude -p` with `--mcp-config`) | Probes `server/discover` at 2026-07-28 first; on "method not found" it falls back to `initialize` at 2025-11-25, sends `notifications/initialized`, then `tools/list`, and calls `reach` with `_meta.progressToken` and its own tool-use id. It passes a server's configured `env` through. It answers within milliseconds once the shim has attached. When the shim's attach is refused it reports the server as failed and retries once. A daemon the shim started keeps running after Claude Code exits. |
| Claude Desktop, Windows | Not yet: it needs its window | Installed as a packaged (MSIX) app. To be recorded with the configuration below. |
| Codex, Windows | Not yet: not installed on the recording machine | To be recorded with the configuration below. |

The Claude Code conversation is a test fixture (`hosts::claude_code` in the
test kit) and is replayed against the shim on every test run.

## Configurations

### Claude Code

A project `.mcp.json`, or a file passed with `--mcp-config`:

```json
{
  "mcpServers": {
    "ada": {
      "command": "C:\\Tools\\companion-lobby.exe",
      "args": ["attach", "ada"]
    }
  }
}
```

### Claude Desktop

In `claude_desktop_config.json` (Settings, Developer, Edit Config), with the
shim recording the conversation:

```json
{
  "mcpServers": {
    "ada": {
      "command": "C:\\Tools\\companion-lobby.exe",
      "args": ["attach", "ada", "--log", "C:\\Tools\\ada-claude-desktop.jsonl"]
    }
  }
}
```

### Codex

In `config.toml` under the Codex home folder:

```toml
[mcp_servers.ada]
command = 'C:\Tools\companion-lobby.exe'
args = ["attach", "ada", "--log", 'C:\Tools\ada-codex.jsonl']
```

## Recording a host

1. Start the daemon and create the companion, for example with
   `companion-lobby open` and then
   `curl -X POST -H "Content-Type: application/json" -H "Origin: http://127.0.0.1:5219" -d "{\"moniker\":\"ada\",\"displayName\":\"Ada\"}" http://127.0.0.1:5219/api/companions`.
2. Add the configuration above with `--log <file>`, restart the host, and ask
   it to call `reach` once.
3. Close the host. The log holds every frame in both directions, plus the
   shim's own `start` (including whether `LOBBY_DATA_DIR` and `LOBBY_PORT`
   reached it from the host's environment), `attached` (with how long the shim
   took to find or start the daemon) and `stdin closed` records.
4. Check that the daemon still answers (`companion-lobby open` prints the
   page address without starting a second one) and that `trace.log` in the
   data directory shows one daemon, not one per host start.

If the shim reports that the daemon on the port "serves another data
directory", the host runs the shim with a different view of the data
directory than your own shell has, which a packaged host can do. Record the
log and the message; do not work around it by copying files between
directories.
