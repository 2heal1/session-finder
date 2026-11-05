# Project guide

## Purpose and scope

session-finder is a native Rust command-line tool for finding and inspecting existing local coding-agent conversations. It supports Codex, TRAE IDE, and TRAE CLI through a shared search model, with project filtering, terminal navigation, JSON output, and Codex relationship discovery.

Keep this project focused on reading, searching, and exporting existing conversations. Session creation, message sending, and session orchestration belong in clients such as Threadline. Provider support must describe the formats and versions actually implemented.

## Development setup

Use Rust with edition 2024 support and a native C compiler. CI pins **Rust 1.97.1**, **Node.js 24.18.1**, and **npm 11.16.0** for publishing. Prefer those versions when reproducing CI behavior.

SQLite/SQLCipher and Zstandard are built into the Rust executable. Node.js is used for development tests, benchmarks, and npm packaging, not for running the installed CLI. The root npm package has no dependencies, so `npm install` is not a prerequisite for development.

```sh
# Build the release executable at ./session-finder
sh build.sh

# Formatting, clippy with warnings denied, and Rust tests
sh build.sh check

# Node tests for benchmarks, release guards, and the legacy TRAE helper
npm test

# All standard code checks
npm run check

# Synthetic performance suite; requires a release build
npm run bench
```

`build.sh` honors `CARGO_TARGET_DIR`. On macOS it uses installed Command Line Tools through process-local environment variables; do not change the machine's global `xcode-select` setting to build this project.

## Code map

| Area | Files | Responsibility |
| --- | --- | --- |
| Entry and commands | `src/main.rs`, `src/cli.rs` | Parse options, validate requests, select actions |
| Shared model and queries | `src/model.rs`, `src/query.rs` | Provider identity, filtering, ordering, pagination, JSON contracts |
| Codex input | `src/catalog.rs`, `src/parser.rs` | Read local metadata and streaming JSONL/zstd logs |
| TRAE IDE input | `src/trae_db.rs`, `src/trae.rs`, `src/contextfs.rs` | Native database access, offline/snapshot conversion, read-only IPC |
| TRAE CLI input | `src/traecli.rs` | Rollout formats, visible history, rollback handling |
| Search and presentation | `src/text.rs`, `src/snippets.rs`, `src/render.rs` | Normalize text, score matches, preserve original snippets, render terminal output |
| Projects and time | `src/project.rs`, `src/time.rs` | Repository/worktree identity and time boundaries |
| Relationships | `src/relations.rs` | Evidence-backed Codex graph traversal |
| Derived data | `src/cache.rs`, `src/transcript.rs` | Private incremental caches and on-demand TRAE conversation files |
| Validation | `tests/`, `scripts/*.test.mjs`, `scripts/bench*.mjs`, `bench/budgets.json` | Regression coverage and performance gates |
| Distribution | `npm/session-finder`, `scripts/npm-release.mjs`, `scripts/release-version.mjs`, `.github/workflows/` | Native launcher, version selection, archive validation, CI, and OIDC release |

## Implementation principles

1. **Keep source data read-only.** Do not mutate agent databases, logs, authentication files, application settings, or shell configuration. Normal search does not start agents or make network/model requests. Explicit ContextFS operations use the authorized local IPC source.
2. **Preserve evidence and uncertainty.** Use source timestamps and structured relationship metadata. Do not infer reply times or parents from file modification times, IDs, similar titles, nearby timestamps, or text mentions. Keep unavailable fields unknown and report incomplete coverage.
3. **Index visible conversations.** Preserve user text, visible assistant progress, and final replies. Exclude hidden reasoning, tool payloads, system/developer instructions, and injected context. Distinct turns may legitimately repeat text; deduplicate only with format-specific evidence.
4. **Keep providers isolated.** Treat `(provider, id)` as session identity and namespace caches by provider and source. Convert into shared `Session`/`Message` structures and reuse filtering, scoring, snippets, and rendering instead of implementing parallel behavior.
5. **Preserve the CLI contract.** Omitted providers mean `all` for normal queries; `relations` defaults to Codex, and `sync` requires `--provider trae`. Apply shared ordering and pagination to combined results. Keep JSON on stdout and diagnostics on stderr.
6. **Version output deliberately.** Session lists use JSON schema 2; other commands use schema 1. Compact lists, `--verbose`, original-text snippets, and the provider-specific `show` behavior are public interfaces. Update their documentation and regression coverage together when changing them.
7. **Make caches optional for correctness.** Maintain consistent cold, warm, refreshed, and uncached results. Detect appends, replacements, renames, deletions, WAL changes, and writes during reads. Use transactions or atomic replacement; never cache a partial source snapshot as complete.
8. **Treat derived text as private data.** Use private directories/files (`0700`/`0600`) and source-specific namespaces. Do not persist or print database keys, IPC credentials, authentication tokens, or raw user transcripts in fixtures or reports. Verify supported native library builds before interpreting their layout.
9. **Protect useful performance.** Filter before expensive reads where the provider supports it. Keep summary and full-text paths separate, generate snippets only for returned results, and retain bounded parsing. Speed improvements must preserve result content, ordering, and coverage.
10. **Keep distribution simple.** Preserve the native executable and POSIX launcher, with all four supported binaries in one npm archive. Avoid runtime JavaScript dependencies, install scripts, and post-install downloads.

## Making changes

Inspect the working tree first and preserve unrelated work. Keep implementation changes limited to the requested behavior. Read the relevant provider guide before changing its adapter.

For parser or query changes, use synthetic fixtures in temporary directories and explicitly select the provider and source home. Default queries inspect all supported agents, so tests must not accidentally depend on the developer's real history. Cover malformed completed records, an incomplete trailing record, duplicates, missing metadata, and cache invalidation when those cases are affected.

Run focused tests while iterating, then `sh build.sh check` and `npm test` for code changes. Run the performance suite for changes to parsing, queries, indexing, caching, or rendering. Save the previous release binary before a performance-sensitive change and compare both builds on the same machine:

```sh
cp session-finder /tmp/session-finder-baseline
# Edit, run checks, and rebuild.
npm run bench -- --baseline /tmp/session-finder-baseline
```

Do not relax `bench/budgets.json` or reduce returned results to make a regression pass. Explain intentional semantic changes and budget changes with the corresponding report. Packaging changes also need the archive and installation checks in the [release guide](docs/release.md).

For documentation-only changes, check examples against the CLI, relative links, language, and formatting; a full native rebuild is normally unnecessary. Describe what was checked and any remaining limitations.

## Documentation

Write repository documentation in **English**. The README is for users: explain the product, measured performance, supported agents, installation, and common tasks. Keep architecture, schema details, release operations, and development guidance in the linked references.

Use generic examples rather than personal paths, real conversation IDs, or private transcripts. Performance claims must name the workload, provider, platform, measurement method, and source. Do not present one provider's benchmark as a guarantee for every provider. Keep installation instructions consistent with the actual published package.

References:

- [CLI behavior and JSON](docs/cli.md)
- [Performance measurements and gates](docs/performance.md)
- [TRAE IDE integration](docs/trae.md)
- [TRAE CLI integration](docs/traecli.md)
- [npm releases](docs/release.md)
- [Legacy TRAE search prototype](scripts/trae-search.md)

## Release model

The repository's `package.json` remains `private: true`, and Cargo has `publish = false`. The release script constructs the public npm package. Keep versions synchronized across `package.json`, `Cargo.toml`, and the root package entry in `Cargo.lock`.

Merging a PR runs CI. To publish, run **Publish npm release** (`publish.yml`) on `main`. Its optional version input accepts stable `X.Y.Z` or `vX.Y.Z`. Blank uses the current manifest version if it is newer than every stable npm version and release tag; otherwise it increments the highest stable patch version. Bootstrap/prerelease versions do not affect that choice.

The release button pins the selected main commit, updates the three version files when needed, and atomically pushes the version commit and tag. It explicitly dispatches `release.yml` on that tag: pushes made with `GITHUB_TOKEN` do not trigger another workflow. Main must permit the workflow's version commit; do not bypass branch rules or force-push to release.

`release.yml` runs full CI and publishes through GitHub OIDC, without long-lived npm tokens. Human-pushed matching tags still trigger it. Direct manual runs of `release.yml` default to packaging only; publishing requires the matching tag. Publication jobs are serialized, and new versions cannot move npm `latest` backwards. Retry a failed release from its original run/tag; the release button reserves existing tags and never moves them.

A successful preparation, build, or trust setup is not proof of a successful npm publication; verify the release workflow result, registry version, archive integrity, and provenance.
