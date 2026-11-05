# CLI reference

For a quick introduction, see the [README](../README.md). Run `./session-finder --help` for the complete option list. Examples below use a binary built in the repository; after global installation, use `session-finder`.

## Commands and defaults

| Command | Purpose |
| --- | --- |
| `recent` | List sessions, optionally filtered by keywords |
| `search <words>` | Search titles and user/assistant text |
| `show <ID>` | Read a conversation; TRAE IDE returns a conversation file |
| `projects [words]` | List projects and session counts |
| `browse` | Open the interactive terminal browser |
| `relations <ID>` | Explore Codex forks and subagents; alias: `related` |
| `sync --provider trae [--id <ID>]` | Rebuild native TRAE text indexes or synchronize ContextFS conversation snapshots |

Without arguments, a terminal opens the interactive browser; a pipe or redirected invocation lists recent replies. Bare keywords are shorthand for a search.

Normal queries default to `--provider all`, combining Codex (`codex`), TRAE IDE (`trae`), and TRAE CLI (`traecli`). Results share project/time filtering, scoring, sorting, and pagination. An unavailable provider produces a warning while `all` returns available sources; an explicit refresh failure remains an error.

`relations` defaults to Codex and accepts no other provider. `sync` requires `--provider trae`; select sessions with `--id`, not project, time, or pagination options.

## Search, projects, and pagination

```sh
./session-finder search "modren federation" --sort relevance
./session-finder search "authentication" -p my-project --exact
./session-finder recent --sort created -n 10 --offset 10
./session-finder recent -p my-project --since 7d
./session-finder projects my-project --json
```

Multiple search terms must all match. Search covers titles and user/assistant messages, excluding tool output and injected system context. For TRAE snapshots, the searchable content depends on which messages have been synchronized.

Matching normalizes case, Unicode width, and punctuation. Fuzzy matching tolerates English spelling errors and adjacent-letter transpositions, plus bounded character gaps in Chinese text. A Chinese-only term of 4–64 characters may omit one internal character while preserving its endpoints, order, and bounded span. `--exact` disables typo/gap tolerance but retains normalized substring matching. There is no semantic or pinyin search.

Project selection accepts a name, repository URL, or worktree path. Exact project identifiers take precedence over fuzzy matches before subagent/time filtering. Project identity uses recorded working directories and Git metadata, including local worktree `commondir` resolution.

The default sort is `replied`. Use `created` for creation time or `relevance` for text score. The default page size is 10; `-n`/`--limit` accepts 1–10,000, and `--offset` skips preceding results.

Subagents are hidden by default. Use `--include-subagents` or an ID lookup to include them. Ordinary forks remain visible.

## Time boundaries

- `--since` includes its starting instant.
- An ISO timestamp passed to `--until` excludes its endpoint. A date-only value includes that entire day.
- Relative values include `7d`, `24h`, `30m`, and `now`.
- Date-only input and terminal timestamps use the system timezone. JSON timestamps use UTC.
- Time filters apply to the selected `created` or `replied` timeline. They cannot be combined with `--sort relevance`.

The latest reply is the time of actual visible assistant text, including progress and final replies. File changes, tool activity, later user messages, and database update times are not replies. Sessions without a known timestamp sort last and do not match a filter on that timeline.

## IDs and conversation output

Use a complete UUID, a supported TRAE conversation ID, a prefix of at least eight characters, or a link containing a UUID. `--id` filters lists; `show` and `relations` require an unambiguous match.

```sh
./session-finder --id <ID-prefix> --json
./session-finder show <ID> --provider codex
./session-finder show "codex://threads/<UUID>" --json
./session-finder show <ID> --provider traecli | less -R
./session-finder show <ID> --provider trae --json
```

When IDs or prefixes overlap across providers, specify `--provider`. `show` does not accept project or time filters.

Codex and TRAE CLI print the complete user/assistant conversation. Their JSON output includes `messages[]`; neither message count nor individual message length is truncated. Images are represented by text markers where available; image content is not analyzed. Missing history is not reconstructed, and parent conversations are not automatically appended.

TRAE IDE writes only the selected conversation to a private temporary JSONL file. Plain stdout contains its absolute path; JSON contains a compact `session` and `file: {path, format, bytes}`. Messages are read from the file rather than inlined. Search, recent, and project queries do not create these files. See [the TRAE file format](trae.md#conversation-files).

## Codex sources and metadata

The source directory is selected by `--home`, then `CODEX_HOME`, then `~/.codex`. Logs are discovered recursively under `sessions/` and `archived_sessions/`, including `rollout-*.jsonl` and `.jsonl.zst`. Compressed logs are streamed without creating decompressed copies.

Creation time prefers the latest compatible `state_*.sqlite` database's `threads.created_at_ms` or `created_at`, then the session's own metadata timestamp, then the filename. Current database working directory, repository, branch, and source metadata take precedence when present.

Titles prefer the latest `session_index.jsonl` entry, then the database title, the first user question, and the UUID. Renaming a session does not require rereading its body.

A log belongs to its filename UUID. Embedded parent headers or `session_id` aliases must not replace its own metadata. Inherited fork text remains searchable, but messages predating the fork's creation do not count as its own first question or latest reply.

For duplicate logs with one ID, the copy with the latest reply wins, followed by message count; all paths remain in metadata. Mirrored event/response messages are deduplicated, and an expanded copy of the same message ID keeps its longer content. Repeated questions in separate turns remain distinct.

Normal search and browsing require a local log. Relationship queries may also include database-only and referenced nodes. An unfinished trailing record is ignored until the next read; malformed completed records produce warnings. Creation-order pages retain metadata with `detailsAvailable: false` when their bodies cannot be read.

## Codex relationships

```sh
./session-finder relations <ID>
./session-finder relations <ID> --direction children --depth 3
./session-finder relations <ID> --direction parents --depth 10 --json
```

`--direction` is `both` by default. `--depth` defaults to 1 and accepts 1–100. Traversal includes subagents, is not paginated, rejects project/time filters, and terminates on cycles. `both` follows ancestors and descendants separately; it does not traverse from a parent into the root's siblings.

Edges point from parent to child:

| Kind | Meaning | Evidence |
| --- | --- | --- |
| `fork` | A session was forked from another | The child's own `session_meta.payload.forked_from_id` |
| `spawn` | A parent created a child/subagent | `thread_spawn_edges`, structured database source, or the child's own log metadata |

Each edge has a stable `id` (`kind:parentUUID:childUUID`), `from`, `to`, `kind`, and `evidence[]`. Evidence includes a local path and field, with a one-based line number for logs or a table name for database records. Recorded relationship status is preserved rather than used as a filter. Duplicate evidence is merged into one edge.

An explicit parent ID can also identify an automatic review child (`threadSource: guardian_review`). A parent/child pair may legitimately have both `fork` and `spawn` edges, so count sessions by node ID.

When the same relationship kind has conflicting parents, priority is native relationship table, then database source, then the child's own log. Equal-priority conflicts produce no definite edge. `conflicts[]` retains candidates, evidence, and `selectedParentId` or `null`; uncertain edges are not traversed.

Nodes contain `id`, `title`, `createdAt`, `cwd`, `source`, `threadSource`, `nativeUrl`, and `paths[]`, plus:

| Field | Meaning |
| --- | --- |
| `inDatabase` | A `threads` record exists |
| `metadataAvailable` | Own log metadata or database metadata was read |
| `logStatus` | `available`, `unreadable`, or `missing`; available metadata does not guarantee an intact full transcript |
| `parentStatus` | `linked`, `conflicting`, `unknown`, or `not_recorded`; the last two do not prove that a session is a root |

`parentStatus` uses the full discovered graph, including edges outside the requested direction/depth. At the depth limit, `truncated` and `frontier[]` report unexpanded directions and edge counts. `coverage` identifies databases, relationship-table availability, logs without own metadata, unreadable logs, and `toolsScanned: false`.

Only structured local evidence establishes a relationship. Titles, conversation mentions, tool-call counts, temporal proximity, and common projects are not evidence. Some agent-created sessions lack a recorded caller and correctly retain an unknown parent. `truncated: false` means the discovered graph was expanded, not that the complete historical graph is known.

To discover reverse fork links, the first query scans each active/archived log until its own metadata header is found. Inherited prefixes may make this scan longer; logs without an own header are scanned to EOF and reported as coverage gaps. The independent `relation_headers` cache stores metadata/evidence locations, not conversation bodies. Database relationships are reread each time.

## JSON interface

Session lists (`recent`, `search`, bare keywords, and `--id` lists) use `schemaVersion: 2`. Other JSON commands use `schemaVersion: 1`. Common fields include `source`, `home`, `timeZone`, `warnings`, and `stats`.

| Response | Command-specific fields |
| --- | --- |
| Session list | `query`, `project`, `sort`, `total`, `sessions[]` |
| Codex / TRAE CLI `show` | `session`, complete `messages[]` |
| TRAE IDE `show` | Compact `session`, `file: {path, format, bytes}` |
| `projects` | `total`, `projects[]` |
| `relations` | `rootId`, `depth`, `direction`, `nodes[]`, `edges[]`, `conflicts[]`, `truncated`, `frontier[]`, `coverage` |

Compact session entries contain `id`, `provider`, `title` (up to 160 characters), `project: {key, name}`, `createdAt`, `lastReplyAt`, `nativeUrl`, `scope`, `detailsAvailable`, and `isSubagent`.

Without a keyword, `preview` contains the last reply or, if absent, the last question, up to 160 characters. Keyword results instead include `matchScore` and up to two `matches`, each at most 180 characters. Their `field` is `title`, `id`, `message`, or `messages`; a single-message match includes `role`. Snippets preserve original case and punctuation, collapse whitespace, remove control characters, and mark omissions with an ellipsis.

`recent/search --json --verbose` returns full session metadata: timestamps, complete project information, working directory, branch, provider/source, parent/fork IDs, previews, message counts, score, source paths, and archive/compression state. Keyword results retain their matches. Consumers of older detailed list output should add `--verbose` and accept list schema 2. The flag is limited to JSON session lists.

Messages contain `role`, `text`, `timestamp`, `phase`, and `id`. Project entries include key/name/root/repository, session count, latest creation/reply times, and working directories. Full metadata previews are capped at 1,000 Unicode characters each; full-text index contents are not serialized. Terminal output strips control characters, while `show` JSON and conversation files preserve message text.

`stats.mode` is `metadata`, `summary`, `full`, or `relations`. `parsedFiles` counts body parses; `headerFiles` counts header scans, including inherited prefixes and failed attempts; `cachedFiles` counts cache hits. In normal queries, candidate session/file counts reflect project/subagent selection. In relationship queries they describe the returned graph's nodes/logs, and `indexedSessions` is the returned node count.

Plain-text and interactive output include query timing on stderr. JSON retains `stats.elapsedMs`; `--timings` also prints timing to stderr. That value measures query/cache work rather than total process startup or time waiting for interactive input.

## Cache controls and local data

```sh
./session-finder recent -p my-project --refresh
./session-finder search "authentication" --no-cache
./session-finder recent --cache-dir "/path/to/private-cache"
./session-finder recent --provider codex --home "/path/to/codex"
./session-finder recent --provider traecli --traecli-home "/path/to/cli"
```

Default cache roots are `~/Library/Caches/session-finder/` on macOS and `~/.cache/session-finder/` on Linux, with `XDG_CACHE_HOME` support. `--cache-dir` overrides the root. New private directories use mode `0700`, and cache database files use `0600`.

Derived SQLite/zstd caches contain local conversation text. `--refresh` rereads the sources involved in a query; `--no-cache` bypasses derived cache reads/writes. ContextFS source snapshots and requested `show` files have separate semantics described in the [TRAE guide](trae.md).

The source databases are opened read-only. The tool does not read authentication files, change agent or shell settings, upload conversations, or invoke a model. Cargo may download build dependencies, and explicit ContextFS refresh/sync uses local agent IPC.
