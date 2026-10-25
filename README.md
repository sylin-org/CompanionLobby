# Companion Lobby

Companion Lobby is ssh-agent for companions.

ssh-agent holds your keys so that programs can use them without ever seeing
them. Companion Lobby holds the accounts your companions use, so that a mind
can act as a companion on the services you allowed without ever seeing a
token, and you can see and stop everything it does from one page.

## The seven nouns

| Noun | What it is |
| --- | --- |
| **Sponsor** | You: the person who owns the companions and decides what each may reach. |
| **Companion** | A named identity, with a moniker, a display name and a portrait. |
| **Mind** | Whatever model or agent an MCP host runs. A mind attaches as one companion. |
| **Credential** | An account the sponsor signed in to for a companion. The lobby keeps its material sealed. |
| **Service** | Something a companion can reach, spoken to by a service adapter in its own words. |
| **Connection** | The sponsor's decision that a companion may use one credential to reach a service at an address. |
| **Journal** | Who did what, when, through which connection, with which outcome. No content and no secrets. |

A mind attaches as a companion and sees `reach`, which tells it what that
companion can reach, plus the native tools of each service it is connected to,
named `<service>_<tool>`.
Each service and each kind of account has a page of its own under
`docs/services` and `docs/credentials`.

The sponsor signs in once per account, turns connections on and off, revokes
in one click, and reads the journal, all on the manager page.

## Build

Companion Lobby is a Rust workspace pinned to Rust 1.98.1 by
`rust-toolchain.toml`.

```sh
cargo build --release
```

The one binary is `target/release/companion-lobby` (`companion-lobby.exe` on
Windows). Its subcommands:

| Command | What it does |
| --- | --- |
| `companion-lobby run` | Runs the daemon (the default subcommand). |
| `companion-lobby attach <companion>` | The stdio shim an MCP host launches; starts the daemon if needed. |
| `companion-lobby open` | Opens the manager page. |
| `companion-lobby stop` | Asks the daemon to drain and exit. |

The checks every change passes:

```sh
cargo fmt --all --check
cargo build --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
pwsh tools/vocabulary.ps1   # or: sh tools/vocabulary.sh
```

## Attaching an MCP host

The companion is the server name. Each companion gets one entry in the host's
configuration, and the entry's only argument is the companion's id or moniker:

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

`docs/hosts.md` has the configuration for each host and how to record what a
host sends. The host launches `companion-lobby attach ada`; the shim starts
the daemon in the background if it is not running, proves itself with the
per-install attach secret, and relays the host's MCP frames. `docs/daemon.md` describes the
daemon, the files it keeps, its loopback boundary and the attach protocol.
`docs/credentials/` has a page for each credential adapter: what a sign-in
sends and keeps, what it refuses, and the live check the owner runs by hand.

## Where the lobby keeps its files

| System | Data directory |
| --- | --- |
| Windows | `%LOCALAPPDATA%\CompanionLobby`, resolved through the known-folder API |
| macOS | `~/Library/Application Support/CompanionLobby` |
| Others | `$XDG_STATE_HOME/companion-lobby` |

`LOBBY_DATA_DIR` overrides the location. The directory is owner-only; the
attach secret and credential material inside it are sealed (DPAPI on Windows).

## Out of scope, by decision

Companion Lobby attaches minds to accounts and keeps the record. It does not:

- decide when a mind wakes up or what it does next (wake policy);
- meter or cap what a companion spends (budgets);
- remember anything on a mind's behalf (memory);
- enforce a companion's charter or persona;
- coordinate several minds with each other.

Those belong to the host or to the mind.

## License

MIT. See [LICENSE](LICENSE).
