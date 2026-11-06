# session-finder

**Find your past coding-agent conversations, fast.**

session-finder is a native **Rust CLI** for searching and browsing local conversations from **Codex, TRAE IDE, and TRAE CLI**. Find a discussion by a few words, narrow it to a project, or open the conversation you worked on last week.

It searches titles and user/assistant messages, tolerates typos, and provides a terminal browser and JSON output. Your conversations stay on your machine: no uploads, model calls, or background service.

## Built for fast local search

Rust, incremental indexing, and selective reads keep repeat queries quick. The executable runs directly without a Node.js runtime.

| Task | Empty tool cache | Warm tool cache |
| --- | ---: | ---: |
| List the 10 newest sessions | 31.7 ms | **26.2 ms** |
| Search a project with a typo | 51.5 ms | **38.6 ms** |
| List sessions by latest reply | 113.4 ms | **53.1 ms** |
| Explore a session's relationships | 43.6 ms | **28.3 ms** |

Measured on Linux x64 with **1,200 synthetic Codex sessions across 24 projects**, using the median of seven runs. Times include process startup and JSON output; the project search covers 47 sessions. An empty tool cache does not mean an empty OS file cache. Results vary with the agent, conversation size, and machine. See the [benchmark methodology and full results](docs/performance.md).

## Supported agents

| Agent | Provider | Support |
| --- | --- | --- |
| **Codex** | `codex` | Local and archived conversations, full-text search, projects, fork and subagent relationships |
| **TRAE IDE** | `trae` | Local database search, titles, projects, and conversation export; native access verified with **TRAE CN 3.4.15 on macOS arm64** |
| **TRAE CLI** | `traecli` | Local and archived conversations, full-text search, projects, and visible history after rollbacks |

Searches include all three by default. Use `--provider codex`, `--provider trae`, or `--provider traecli` to select one. Relationship discovery currently supports Codex only. TRAE IDE also supports compatible offline exports and ContextFS snapshots; see the [TRAE IDE guide](docs/trae.md) for version and source limitations.

## Install

Install globally with npm, then run `session-finder` from any directory:

```sh
npm install -g session-finder
session-finder --help
```

Supported release targets are **macOS 13+** and **Linux with glibc 2.35+**, each on **arm64 and x64**. Windows and Alpine/musl are not supported. The installed CLI runs as a native executable without a Node.js runtime.

Alternatively, build from source with Rust and a native C toolchain:

```sh
git clone https://github.com/2heal1/session-finder.git
cd session-finder
sh build.sh
./session-finder --help
```

When using a source build, run `./session-finder` from the repository directory in place of `session-finder` in the examples below.

## Find a conversation

Run `session-finder` in a terminal to open the interactive browser. Use arrow keys to navigate, Enter to select, and Esc to go back.

```sh
# Recent conversations across all supported agents
session-finder recent

# Search titles and messages; typos are supported
session-finder search "modren federation" --sort relevance

# Find conversations for a project from the last seven days
session-finder recent -p my-project --since 7d

# Restrict a search to one agent
session-finder search "authentication" --provider codex

# Match a normalized substring without typo tolerance
session-finder search "release workflow" --exact

# List projects or browse conversations interactively
session-finder projects
session-finder browse
```

Results default to the latest assistant reply. Use `--sort created` for creation order or `--sort relevance` for the best text matches. Add `-n 20 --offset 20` to paginate. Subagent sessions are hidden unless you use `--include-subagents` or look them up by ID.

Terminal results show full session IDs with clickable Codex links and optional
[TRAE CN Agent links](docs/trae.md#opening-agent-conversations). Colors emphasize
titles and matched text with purple highlights and deep pink session IDs,
keeping timestamps in the terminal's default text color. Searches show matches as conversations are read,
updating their order in the terminal. Slow queries display a
loading phase and elapsed time. The completed page stays in normal scrollback.
Use `--color never`, `--links never`, or `--no-progress` to disable these features.
Redirected output and JSON remain free of automatic terminal decoration.

## Read and reuse results

```sh
# Open a conversation by ID or a unique prefix of at least eight characters
session-finder show <ID>

# Follow Codex forks and subagents
session-finder relations <ID> --depth 3

# Export compact search results for scripts or other tools
session-finder search "authentication" --json

# Request full session metadata
session-finder recent --json --verbose
```

For Codex and TRAE CLI, `show` prints the user/assistant conversation; `show --json` includes its messages. For TRAE IDE, `show` returns the path to a temporary JSONL conversation file; `show --json` returns that file's metadata.

JSON stays on stdout. Warnings and timing output go to stderr; add `--timings` to see query timing alongside JSON. Default data locations are `~/.codex`, `~/.trae/cli`, and the local TRAE IDE installation. Use `--home`, `--traecli-home`, or the [TRAE source options](docs/trae.md) to override them.

session-finder reads existing conversations. It does not create sessions or send messages. Its local caches and requested conversation exports may contain message text.

See the [CLI reference](docs/cli.md) for filtering, JSON fields, cache controls, and relationship semantics. Contributors should start with [AGENTS.md](AGENTS.md).
