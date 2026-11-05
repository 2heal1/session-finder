# TRAE CLI integration

`--provider traecli` is separate from TRAE IDE's `--provider trae`. The adapter reads existing local history without invoking the TRAE CLI executable, using IDE IPC, or reading authentication files.

```sh
./session-finder recent --provider traecli --sort created
./session-finder search "authentication" --provider traecli --exact
./session-finder recent --provider traecli -p my-project --since 7d
./session-finder show <ID-or-prefix> --provider traecli --json
./session-finder projects --provider traecli
./session-finder browse --provider all
```

## Sources

The CLI data directory is selected in this order:

1. `--traecli-home /path/to/cli`
2. `TRAECLI_HOME`
3. The `cli/` subdirectory of `TRAE_HOME`
4. `~/.trae/cli`

`--home` and `CODEX_HOME` select only Codex; both homes can be supplied with `--provider all`.

The adapter scans `sessions/` and `archived_sessions/` for `rollout-*.jsonl` and `rollout-*.jsonl.zst`. Metadata comes from the latest compatible `state_*.sqlite`, opened read-only with WAL support, and titles are supplemented by `session_index.jsonl`.

Title precedence is `threads.name`, title index, `threads.title`, first user message, then UUID. Creation time, working directory, repository, branch, and subagent identity come from structured metadata. Reply time comes from visible assistant messages.

Only rollouts still present on disk become readable sessions. Orphaned database/title entries are not reported as complete conversations. Reply times are not inferred from UUIDs or file modification times.

## Conversation conversion

The adapter reuses streaming Codex JSONL/zstd reading, text cleanup, summaries, and full-text indexing, with TRAE CLI-specific history handling:

- Legacy `response_item`, `event_msg`, and `message` records preserve visible user/assistant text, merge event mirrors, and retain repeated questions across distinct turns.
- Version-1 `history_mutation` `append` reads user/assistant items. Where `content_item_kinds` exists, `user.*` markers select user content.
- `display_completions` and `item_completed` `AgentMessage` records supply visible replies, retaining phase, message ID, and turn ID. Visible completions take precedence over duplicate internal representations.
- Mutation `rollback` removes records after the target commit. Legacy `thread_rolled_back` follows actual user turns, ignoring empty control turns.
- Tool calls/results, hidden reasoning, developer instructions, and injected environment content are excluded from conversation search. Images retain parser-provided text markers; image contents are not analyzed.
- Unknown mutation versions/operations produce a warning and skip the session rather than presenting known fragments as complete history. Malformed completed JSON records are reported; an unfinished trailing record is retried after the file changes.

Results use `provider=traecli` and `scope=local_conversation`; full metadata includes `indexSource=local_rollout`. Default JSON lists are compact, `--json --verbose` exposes full metadata, and keyword results include up to two original-text matches.

No CLI deep-link format has been verified, so `nativeUrl` stays empty. To resume a conversation, run `traecli resume <UUID>` separately. session-finder does not launch or resume sessions.

## Shared queries and caching

Normal queries default to all three providers, with shared project/time filtering, scoring, sorting, and pagination. An ID or prefix ambiguous across providers requires explicit provider selection. In-memory records are isolated by `(provider, path)`, even when Codex and TRAE CLI point at the same directory. Relationship discovery remains Codex-only.

TRAE CLI has an independent SQLite/zstd namespace. Device/inode, size, nanosecond modification time, and relevant summary metadata decide whether to reread a file. Titles are applied on each query, so renaming does not require reparsing the body. Appended, replaced, and deleted logs invalidate stale data. Refreshing one ID preserves unrelated cache entries.

`--refresh` rereads the logs involved in the request; `--no-cache` bypasses the derived cache. `sync` is IDE-only, since CLI history does not need an intermediate snapshot. Cold CLI queries currently read all logs in the selected ID scope before the shared engine applies project/pagination filtering; warm queries reuse the text index.

## Validation scope

Local validation covered both legacy logs and version-1 paginated/mutation history. Visible user/assistant messages and ordering in sampled modern conversations were cross-checked against the native `thread_history_1.sqlite`; that database was a verification source, not a runtime dependency. Missing rollouts were excluded rather than reconstructed from metadata.

Regression fixtures cover title precedence, WAL renames, full-text matches beyond preview limits, rollback behavior, repeated messages, compressed archives, refresh/no-cache behavior, appended/deleted logs, filtering/pagination, and cross-provider ID/path isolation. These format checks do not imply support for an unknown future mutation version.

The headline [performance benchmark](performance.md) measures synthetic Codex data. TRAE CLI costs depend on its rollout count and format and should be measured separately for provider-specific changes.
