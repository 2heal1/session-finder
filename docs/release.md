# npm releases

The public package is **`session-finder`**. Keep its version synchronized in `package.json`, `Cargo.toml`, and the root package entry in `Cargo.lock`. The repository's root npm package remains `private: true`; release tooling creates a separate public archive. Cargo publishing remains disabled.

## Package and platforms

One npm package contains `bin/session-finder` and four native executables. The POSIX launcher selects the platform and directly executes Rust. It does not start Node.js, run an installation script, or download another binary.

| Target | Build runner | Runtime requirement |
| --- | --- | --- |
| `darwin-arm64` | `macos-15` | macOS 13+, Apple Silicon |
| `darwin-x64` | `macos-15-intel` | macOS 13+, Intel |
| `linux-arm64` | `ubuntu-22.04-arm` | arm64, glibc 2.35+ |
| `linux-x64` | `ubuntu-22.04` | x64, glibc 2.35+ |

Windows and Alpine/musl are unsupported. Builds use Rust 1.97.1; CI scripts use Node.js 24.18.1; publishing uses npm 11.16.0. The public package has no JavaScript dependencies. A source-code license has not yet been declared.

## CI and release triggers

[CI](../.github/workflows/ci.yml) runs for pull requests and pushes to `main`, and is reused by the [release workflow](../.github/workflows/release.yml):

1. Run Rust formatting, clippy, functional tests, and Node performance/release guard tests.
2. Build a release executable and enforce performance, read-count, and result-consistency gates.
3. Build/test each of the four native targets and run an isolated installation smoke check with its local package.
4. Combine the binaries into one archive; check the file allowlist, architectures, executable permissions, version, and absence of install scripts/dependencies; then install and run it.

Artifacts are `native-*`, `performance-report`, and `npm-package`. Temporary single-platform packages have `private: true` and are rejected by the publish script. A public archive missing any platform is rejected.

**Merging a PR does not publish.** Use the [Publish npm release](../.github/workflows/publish.yml) button to prepare a version and start the full release. Human-pushed matching `vX.Y.Z` tags also trigger a full build and npm publication.

## Click to publish

After these workflows are merged into `main`:

1. Open the repository's **Actions → Publish npm release → Run workflow**.
2. Select **main**. Leave **version** blank for automatic selection, or enter an exact stable version such as `0.2.0` (a `v` prefix is also accepted).
3. Click **Run workflow**. The workflow synchronizes the three version files, creates a version commit if needed, pushes the tag, and starts **Release session-finder**.
4. Follow **Release session-finder** to completion. It runs the complete CI pipeline and publishes the validated four-platform archive under npm `latest`. The preparation run's summary links to this workflow; a green preparation run only confirms preparation and dispatch.

Automatic selection compares the repository version, all stable npm versions, and existing `vX.Y.Z` tags. If the repository version is newer than every published/tagged version, it uses that version. Otherwise it increments the highest stable patch: `0.1.0 → 0.1.1`, for example. With only the current bootstrap versions on npm and no stable tags, the first release is **0.1.0**. Bootstrap and other prerelease versions are ignored.

An explicit version must be at least the repository version and newer than every stable npm version and release tag. This allows intentional minor/major releases while rejecting duplicates and downgrades. Prerelease versions and build metadata are not supported.

To preview selection locally without changing files, tags, or the registry:

```sh
npm run release:version
npm run release:version -- --version 0.2.0
```

The button uses the main commit selected when the run was started. If main moves before preparation, start a fresh run from main. Version preparation is serialized; main and the tag are pushed atomically, so a concurrent branch update or tag collision cannot leave a partial remote update. The workflow uses its built-in GitHub token with `contents: write` and `actions: write`. Main must permit the workflow's version commit; branch rules that block it will stop preparation.

The tag push does not itself start another workflow because it uses `GITHUB_TOKEN`. Preparation explicitly dispatches **release.yml** on the exact tag, preserving the existing npm trusted publisher and release provenance. A version commit made by this token also does not trigger the normal main-push CI; the dispatched release runs the complete CI pipeline for it.

## Retry or validate without publishing

For a failed build or publication, rerun the failed jobs in the original **Release session-finder** run. If preparation pushed a tag but dispatch failed, open **Release session-finder → Run workflow**, select that tag, enter its exact `X.Y.Z` version, and enable **publish**. The same recovery can be started with:

```sh
gh workflow run release.yml --repo 2heal1/session-finder \
  --ref v0.1.0 -f version=0.1.0 -f publish=true
```

Use the prepared version in place of `0.1.0`. Keep the existing tag and release commit. The release button treats tags as reserved: a blank new run selects the next available version, and an explicit existing version is rejected.

For packaging-only validation, run **Release session-finder** directly with the exact manifest version and leave **publish** disabled (its default). A direct run on a branch can only validate/build; publication requires the matching tag.

## Local checks

```sh
sh build.sh check
npm test
sh build.sh

# Apple Silicon example; select the matching path on other hosts.
mkdir -p dist/native/darwin-arm64
cp session-finder dist/native/darwin-arm64/session-finder
npm run release:pack -- --local --binaries dist/native --output dist/smoke
npm run release:smoke -- --local --archive dist/smoke/session-finder-0.1.0.tgz
```

For a complete package, obtain all four CI binaries and place them under `dist/native/<platform>/session-finder`:

```sh
npm run release:pack -- --binaries dist/native --output dist/npm
npm run release:validate -- --archive dist/npm/session-finder-0.1.0.tgz
npm run release:smoke -- --archive dist/npm/session-finder-0.1.0.tgz
```

Use the current package version in archive paths. Installation checks use a temporary directory and offline npm installation, covering paths with spaces, global/relative symlinks, version/help, error exit codes, a synthetic JSON query, and timing on stderr.

## Bootstrap and trusted publishing

A package must exist on npm before its trusted publisher can be configured. This repository uses a minimal local bootstrap followed by normal GitHub OIDC releases.

On November 5, 2026:

- `session-finder@0.0.0-bootstrap.0` was published under `bootstrap` and its registry integrity was verified. It contains only a manifest and README, with no CLI. npm also set `latest` to this initial version; until the first full release, default installation therefore yields only the placeholder.
- npm confirmed creation of the GitHub trusted publisher for `2heal1/session-finder`, workflow file **`release.yml`**, with publishing permission.
- The first PR's six CI checks passed. Its complete `0.1.0` archive also passed local validation and isolated installation after download. This is build validation; an actual OIDC publication has not yet been verified.

The setup command is recorded for maintenance; an already configured package does not need it repeated:

```sh
npm trust github session-finder \
  --repo 2heal1/session-finder \
  --file release.yml \
  --allow-publish --yes \
  --registry=https://registry.npmjs.org
```

Local bootstrap publication and trust configuration may require separate npm two-factor challenges. Complete authentication when the operation requests it. Use an OTP only for that operation; do not store it in files, logs, or GitHub secrets. An authorized existing browser profile can be used for browser authentication.

## Verify publication

After **Release session-finder** succeeds, query the published version (substitute the selected version):

```sh
npm view session-finder@0.1.0 version dist.integrity dist.attestations --json \
  --registry=https://registry.npmjs.org
npm view session-finder dist-tags --json --registry=https://registry.npmjs.org
```

Compare registry integrity with the `.integrity` file in that run's `npm-package` artifact, confirm the expected `latest` version, and inspect npm provenance.

Only successful publication and registry verification establish that OIDC works end to end. A placeholder, trust configuration, dry run, or successful package build does not establish that.

The publish job uses a GitHub-hosted runner and is the only job granted `id-token: write`. Do not configure `NPM_TOKEN` or `NODE_AUTH_TOKEN` for this workflow. Public packages published through supported GitHub trusted publishing receive npm provenance automatically.

The publish script validates and installs the archive before checking the target version. It skips an existing version only if `dist.integrity` matches exactly; different content requires a new version. Registry network/authentication errors are not treated as a missing version. Publication jobs are serialized across versions, and immediately before publishing a new version the script requires it to be newer than every stable npm version. An older build finishing later therefore cannot move `latest` backwards. After publication it checks registry integrity again.

If npm accepts a publication but the subsequent registry check fails during propagation, inspect the registry before retrying; the version may already exist. Recover a workflow publication from its matching tag, preserving the exact archive identity.

The bootstrap published locally may lack GitHub provenance. This workflow does not create GitHub Releases; npm archives and their integrity files are retained as Actions artifacts.
