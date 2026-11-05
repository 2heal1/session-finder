# TRAE IDE integration

TRAE IDE uses `--provider trae`; the separate [TRAE CLI provider](traecli.md) uses `traecli`. Native database access is currently verified with **TRAE CN 3.4.15 on macOS arm64**. Other native library builds are rejected explicitly rather than interpreted with an unverified layout.

## Usage and source selection

```sh
./session-finder search "authentication" --provider trae --sort relevance
./session-finder recent --provider trae --json

# Force reconstruction of the native derived text index.
./session-finder search "authentication" --provider trae --refresh
./session-finder sync --provider trae --id <conversation_id>
# Without an ID, rebuild all conversations.
./session-finder sync --provider trae

# Export only the selected conversation.
./session-finder show <conversation_id> --provider trae
./session-finder show <conversation_id> --provider trae --json

# Read an offline JSONL export or a compatible query snapshot.
./session-finder search "authentication" --provider trae --trae-index ./export.jsonl

# Explicitly select a ContextFS installation.
./session-finder search "authentication" --provider trae \
  --trae-ctx "/Applications/Trae CN.app/Contents/Resources/app/modules/ai-agent/bin/ctx-cli"
```

Omitting `--provider` combines all three supported sources before shared filtering, scoring, sorting, and pagination. Session identity is `(provider, id)`. An ambiguous `show` prefix requires a provider; a provider failure in `all` produces a warning and allows other sources to return results. Explicit refresh failures remain errors. `relations` is Codex-only; IDE turn IDs do not establish parent/child relationships.

The default native database is `~/Library/Application Support/Trae CN/ModularData/ai-agent/database.db`, with `Trae` checked next. Override the database and matching native library with `--trae-db` and `--trae-library`. Explicit `--trae-ctx` or `--trae-index` selects the compatible snapshot path; when no native database is found, existing ContextFS discovery is also attempted.

## Conversation files

`show` creates a file only for the selected IDE conversation, including when it is selected through `--provider all`. Plain stdout contains just the absolute path; diagnostics remain on stderr. Search, recent, and project queries do not create conversation exports.

The path is `<system-temp>/session-finder/trae/<source-hash>/<full-conversation-id>.jsonl`. It follows system temporary-directory rules, including `TMPDIR` on macOS/Linux. Different databases, offline indexes, and installations use distinct source namespaces.

The file is UTF-8 JSONL. Its first line contains `type=session_meta`, `schemaVersion=1`, and full public `session` metadata. Each remaining line contains `type=message` with `role`, `text`, `timestamp`, `phase`, `id`, and an optional `turn_id`. It preserves all user/assistant text supplied by the converter without length limits. It is a readable conversation export, not a raw database dump or the query-index format accepted by `--trae-index`.

`show --json` retains the common `schemaVersion`, `source`, `home`, `timeZone`, `warnings`, and `stats` fields, plus a compact `session` and a `file` object:

```json
{
  "path": "/absolute/temp/session-finder/trae/<source-hash>/<conversation-id>.jsonl",
  "format": "jsonl",
  "bytes": 12345
}
```

The response includes no inline `messages` or body previews. `bytes` is the actual UTF-8 file length; read `file.path` for full metadata and messages.

Each request reads the snapshot available from its selected source. Identical file contents are reused; changed titles or text are replaced atomically. Directories use `0700` and files `0600`. Files survive process exit and may be removed by the user or system cleanup; a later `show` recreates them. Write failures cause an error rather than falling back to inline text. `--no-cache` still produces this requested output file and does not change ContextFS source freshness.

## Titles, metadata, and matching

Native titles come from `chat_session.session_title`, with first-question and ID fallbacks. `titleSource` is `metadata`, `first_user`, or `id`. Lists, search, and `show` share that title. Explicit live ContextFS query indexes supply questions rather than IDE titles and report their narrower coverage.

Offline JSONL and schema-1 query snapshots may provide normalized per-conversation metadata:

```json
{"conversation_id":"010000000000400080000001","sessions":[],"metadata":{"title":"Investigate authentication","createdAt":"2026-11-04T00:00:00Z","cwd":"/tmp/example","repositoryUrl":null,"branch":null}}
```

`createdAt` must be RFC 3339 and is normalized to UTC. Missing values remain unknown. Project/repository fields enter the shared project resolver. Creation time is not substituted for question/reply time. Duplicate conversation rows merge their turns; the latest explicit metadata replaces earlier metadata, while an absent metadata field does not erase known values. Blank/null titles follow normal fallback rules.

All providers use the shared matcher, including English typo tolerance and bounded Chinese character-gap matching. Chinese-only terms of 4–64 characters can omit one internal character if endpoints and order are preserved within the bounded span. Substring matches rank above complete gap matches, which rank above one-missing-character matches. `--exact` disables gap/typo tolerance. These rules are not semantic search and do not replace a title source.

## Native database access

`src/trae_db.rs` opens the database with `SQLITE_OPEN_READ_ONLY`, `query_only`, and a read transaction. Reads include live WAL data; `immutable=1`, which can ignore WAL changes, is not used. The adapter does not start the IDE, Node.js, or a background process.

For the supported encrypted build, it first verifies the installed `libai_agent.dylib` Mach-O build UUID, then derives SQLCipher parameters in memory using that verified layout. Temporary material is cleared with `Zeroizing`. Keys are not stored in source, cache identities, snapshots, or errors. Unsupported builds fail explicitly. Non-macOS builds can still use compatible plaintext database exports and snapshots; this does not imply native access to arbitrary installed TRAE versions.

Conversion reads `chat_session`, project/session-project/worktree metadata, `chat_message`, and the corresponding body tables. It includes undeleted history, including messages preceding conversation compaction, and excludes deleted sessions/messages.

User text comes from general messages and appended input. Visible assistant progress comes from the appropriate `thought` content; final replies come from `finish.summary`, `ReplyUser.message`, and `chat.content`. Hidden reasoning, tool arguments/results, hidden plan items, and image content are excluded. An empty task object is valid; unknown body types fail explicitly.

Times come from original seconds-based fields and `generated_at_ms`, not IDs or file modification time. `assistant_sub_chat` follows shared subagent filtering without inventing a parent relationship.

The native catalog checks database/WAL device, inode, size, and nanosecond modification time. Unchanged sources reuse it. Changed sources reload conversation metadata and revision information, converting only changed conversations. Database/WAL fingerprints also cover same-second streaming updates. A catalog is published only if its source remains stable during the read transaction.

Renames, project changes, body revisions, and deletions invalidate the relevant index. `--refresh` and `sync` force text-index reconstruction; `show` reads the selected conversation directly. Results use `scope=local_conversation` and full metadata includes `indexSource=native_database`. Coverage is limited to visible text retained by the current database, excluding deleted content, tools, reasoning, and image/OCR data.

## ContextFS and offline snapshots

`src/contextfs.rs` is a minimal read-only Unix socket client using TTHeader plus framed Thrift binary `ExecuteCommand` to call `icube.common.commands.tooling.readFile`. It enforces frame-size, recursion, and timeout limits. Application discovery verifies Bundle ID, excludes TRAE SOLO, and checks that live requests match the current agent's application.

ContextFS needs the authorized agent's local IPC environment. Finding `ctx-cli` or opening an IDE terminal does not grant those credentials. The native database path does not require `CFS_TOKEN`.

Conversion in `src/trae.rs` follows these rules:

- `conversation_id` in `context://history/conversations_metadata.jsonl` identifies a conversation. Nested `session_id` values identify turns, not child conversations.
- Duplicate conversations merge turns; duplicate turn IDs update those turns. Turns are sorted by ID for deterministic snapshots, not as evidence of chronology.
- Each turn is read from `context://history/<conversation_id>/<turn_id>.jsonl`, supporting strings, string arrays, and typed text arrays.
- Only user/assistant text is extracted. Known skills, collaboration, and attachment wrappers retain their `<user_input>` content; verified internal approval requests and matching approval JSON replies are excluded. Ordinary messages containing the same JSON remain.
- Revisions of a message ID within a turn use the latest version. Identical text alone is insufficient to deduplicate. Messages and `turn_id` provenance are retained across turns, including legitimate repeated questions and history repeated by the source.
- Shared `parser::summarize` is the conversion boundary. Times require explicit source fields; ObjectIDs, snapshot times, and file modification times are not substitutes. Unknown projects/relationships stay unknown. A questions-only index has unknown message/reply counts.

Snapshot queries read the saved index by default. `--refresh` refreshes that index without synchronizing every conversation body. `sync` first refreshes the index, then rereads every indexed turn of each selected conversation, since replies can change without changing the turn ID. A conversation's body snapshot is replaced only after all its turns are read successfully; an interrupted batch retains successful earlier conversations and each failed conversation's previous snapshot.

Body snapshots are bound to the complete turn list and indexed-question fingerprint. Changed questions/turns fall back to question search until synchronized again. Metadata-only changes invalidate the derived search cache without discarding valid body snapshots, including compatible schema-2 body fingerprints. Same-turn content changes require explicit synchronization. `show --refresh` refreshes and synchronizes its selected conversation; an ordinary `show` with a valid body snapshot needs no live credentials.

`scope` distinguishes `indexed_user_queries`, `indexed_conversation`, and `local_conversation`. Indexed coverage does not prove that the entire conversation history is present. Use list `--json --verbose` for `indexSource`, `indexedAt`, and `syncedAt`. Unknown times sort last and do not match time filters; relevance sorting is often useful for snapshot searches.

## Cache and validation

The default cache root is shared with session-finder; `XDG_CACHE_HOME` and `--cache-dir` are supported. Provider/source namespaces prevent one source's cleanup from deleting another's cache.

| Cache | Contents |
| --- | --- |
| `trae-query-index/<source-hash>.json` | Schema-1 query snapshot, compatible with the legacy Node helper; canonical `ctx-cli` paths distinguish installations |
| `trae-transcripts/<source-hash>/<conversation-id>.json` | Schema-2 normalized user/assistant snapshots |
| `index-v1-*.sqlite` | Derived SQLite/zstd index; ContextFS metadata conversion uses `trae-adapter-v3` |
| `trae-native-catalog/<source-hash>.json` | Native metadata/revision catalog; native derived indexes use `trae-native-v1` |

Private directories/files use `0700`/`0600` and same-directory atomic replacement. Credentials and native decryption parameters remain in memory.

Native `--no-cache` disables catalog and derived SQLite reads/writes. ContextFS `--no-cache` bypasses the derived index but still reads source snapshots; explicit refresh/sync writes those snapshots. `--trae-index` is read-only and cannot be refreshed or synchronized. Repair corrupt query snapshots with an authorized ContextFS refresh, and corrupt body snapshots with `sync`.

Native validation includes real titles and visible conversations from the supported TRAE build without ContextFS credentials. Synthetic regression coverage includes encrypted fixtures, read-only/WAL access, same-second revisions, renames/deletions, empty tasks, excluded body fields, metadata filtering, and cold/warm/uncached consistency. Snapshot tests cover synchronization failures, metadata-only changes, duplicate records, and offline `show`. These checks establish the supported formats; they do not establish compatibility with untested native builds.
