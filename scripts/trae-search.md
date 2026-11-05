# Legacy TRAE IDE search prototype

`trae-search.mjs` is a standalone Node.js prototype that searches user questions in a TRAE IDE query index and groups matching turns by conversation. It is not included in the production CLI or npm package. For current native database and full-conversation support, use the [Rust TRAE IDE provider](../docs/trae.md).

```sh
node scripts/trae-search.mjs "thredline"
node scripts/trae-search.mjs "authentication" -n 5
node scripts/trae-search.mjs "threadline" --exact --json
```

## Terminal use and refresh

`ctx-cli` is a ContextFS client and requires the TRAE agent's injected `CFS_TOKEN` and IPC configuration. Finding its executable, or opening an integrated terminal, does not grant those credentials.

- In an authorized TRAE agent environment, the script reads the current index and refreshes its local cache.
- In an ordinary terminal, it searches the saved cache and reports its source and timestamp.
- With no initial cache, it asks for an index refresh from the agent environment.
- `--refresh` requires a live read. Missing credentials, read failures, or an invalid index produce an error rather than silently returning stale results.

Run this in an authorized TRAE agent environment to refresh the index:

```sh
node scripts/trae-search.mjs "thredline" --refresh
```

An ordinary terminal does not automatically acquire newer conversations. Refresh through the agent environment before searching newly added questions. Do not copy or save `CFS_TOKEN`.

## Cache and limits

On macOS, the default directory is `~/Library/Caches/session-finder/trae-query-index`. `--cache-dir /path` selects a dedicated directory. Directory/file permissions are `0700`/`0600`. Caches are separated by the reader's canonical path and replaced atomically.

The cache stores conversation IDs, turn IDs, user questions, reader path, schema version, and fetch time. It contains local plaintext question text, but no ContextFS credentials. JSON `indexSource` is `live` or `cache`; `indexedAt` records when this script fetched the index, and `cacheAgeMs` is the elapsed time since that fetch.

The helper recognizes TRAE IDE and TRAE CN application identities. `--ctx /path/to/ctx-cli` selects an application explicitly; it will not use credentials from a different current agent application to refresh that source.

The index does not guarantee complete history. The prototype does not search assistant messages or provide creation/reply times, project paths, or fork relationships. A cached `context://history/...` path is a ContextFS reference; an ordinary terminal cannot use the question cache alone to open full conversations.
