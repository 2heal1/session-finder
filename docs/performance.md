# Performance

session-finder uses a native Rust executable, incremental SQLite/zstd indexes, and selective reads to keep repeated local queries quick. Performance checks validate returned data and read counts as well as elapsed time.

## Measured results

The following results come from [CI run 41993442270](https://github.com/2heal1/session-finder/actions/runs/41993442270), commit `ea6da5488b28b5e9673e2ad8ca2201422b2a1974`, on November 5, 2026. The runner was GitHub-hosted Ubuntu 24.04, Linux x64, using Rust 1.97.1 and Node.js 24.18.1. The `performance-report` artifact contains the raw samples and environment metadata.

The dataset contains **1,200 synthetic Codex sessions**, 16 messages per session, and 24 projects, including compressed/archived logs, forks, subagents, SQLite metadata, and a title index. After default subagent filtering, 1,130 sessions are visible; project-scoped cases select 47.

| Scenario | Empty tool cache | Warm tool cache |
| --- | ---: | ---: |
| Newest 10 sessions by creation time | 31.7 ms | 26.2 ms |
| Recent sessions by latest reply | 113.4 ms | 53.1 ms |
| Recent sessions in one project | 34.4 ms | 27.4 ms |
| Text search in one project | 46.1 ms | 33.2 ms |
| Fuzzy search in one project | 51.5 ms | 38.6 ms |
| Project list | 113.5 ms | 53.5 ms |
| Read one conversation | 13.4 ms | 13.3 ms |
| Discover relationships to depth 3 | 43.6 ms | 28.3 ms |

Values are median wall-clock times over seven runs, including process startup and JSON output. List cases request full metadata with `--verbose`. The fuzzy query is `modren fedration`, scoped to `project-03`. These measurements do not measure all-provider searches or promise equivalent performance for TRAE IDE/CLI.

An empty tool cache means a fresh session-finder cache directory, not a cleared OS file cache. The executable and OS pages are preheated before sampling. `show` deliberately rereads its selected transcript in both modes. The base commit for this run predates the Rust implementation, so it passed absolute-time and operation gates without a relative Rust baseline comparison.

## Reproduce

```sh
sh build.sh check
npm test
sh build.sh
npm run bench

# Before changing the implementation, save a release build.
cp session-finder /tmp/session-finder-baseline
# Make the change and rebuild, then compare on the same machine.
npm run bench -- --baseline /tmp/session-finder-baseline
```

Node.js 24 is required for built-in SQLite and zstd support; no npm dependencies are needed. `--binary` selects the candidate, `--output` selects the report directory, and `--runs` accepts 3–31 local samples. CI uses the configured seven runs.

Fixtures are generated in a temporary directory. Benchmarks explicitly select `--provider codex` and a temporary `--home`; they do not inspect real Codex, TRAE IDE, or TRAE CLI history.

Reports default to `bench-results/report.json` and `report.md`. JSON includes environment details, samples, query times, read counters, and result digests. CI writes Markdown to its job summary and uploads `performance-report`, including reports produced before a failure.

## Regression gates

The shared configuration is [`bench/budgets.json`](../bench/budgets.json).

| Gate | Rule |
| --- | --- |
| Relative wall time | Fail when the median exceeds the baseline by both 25% and 10 ms |
| Empty-cache wall time | At most 1,500 ms |
| Warm-cache wall time | At most 750 ms |
| Individual process timeout | 15 seconds |
| Source reads | Match each scenario's expected body/header/cache counts |
| Results | Preserve expected totals, messages, relationships, ordering, and content |

Baseline and candidate runs alternate on the same machine, dataset, and separate caches. Creation-order pages read only their 10 displayed bodies on a cold run. Relationship queries scan headers without building a body index. Warm cases do not reread bodies or headers, except `show`, which is allowed to read one body.

Cross-version comparison fills missing legacy Codex fields with `provider=codex` and `scope=local_conversation`. For list-schema compatibility it normalizes the list version and excludes newly introduced `matches` only in cross-version digests. Cold/warm comparisons of the current binary still include the full output and snippets. This benchmark measures full metadata output; compact JSON output size needs a separate measurement.

Wall time includes launching the process and serializing JSON. `stats.elapsedMs` measures query/cache work. Terminal timing likewise excludes process startup and interactive waiting.

PRs compare against the target branch's base SHA; main pushes use the preceding SHA. Release tags fall back to the parent commit when no usable previous SHA exists. A base without `Cargo.toml` is explicitly reported as bootstrap. A Rust baseline that fails to build is an error, not a reason to skip comparison.

Review budget changes with a rationale and report. Intentional query changes must update synthetic expectations and explain any baseline difference. Repository branch rules must require `Tests and performance` and `Validate npm package` if failing checks should block a merge.

## Implementation choices

- Summary records and compressed full-text records are stored separately. Ordinary recent-session queries do not load the full-text index.
- Codex creation-order queries filter/sort metadata before reading the displayed page. Project searches select candidate projects before reading full text. A cold latest-reply query still reads the relevant bodies to establish real reply times.
- At most four parsing workers process candidate logs. Normalized search text and cleaned original messages are cached together; snippets are generated only for the returned page without rereading logs.
- File device/inode, size, nanosecond modification time, and relevant database metadata participate in invalidation. Titles and project metadata refresh independently; removed logs stop appearing.
- SQLite transactions and WAL support concurrent CLI processes. Changing source fingerprints prevent an unstable read from being cached. Cache failures fall back to source data with warnings.

TRAE providers have different source costs. Native IDE indexing tracks database/WAL changes and per-conversation revisions. TRAE CLI cold queries currently read logs within the selected ID scope before shared project/pagination filtering. See the [IDE](trae.md) and [CLI](traecli.md) guides before changing their performance paths.
