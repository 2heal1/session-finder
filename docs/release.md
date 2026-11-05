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

**Merging a PR does not publish.** Pushing a matching `vX.Y.Z` tag triggers a full build and npm publication. A manual release run defaults to `publish=false`; `publish=true` requires selecting the matching version tag. A manual run on a branch can only validate/build.

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

## First full release and later versions

1. Update the three version locations when needed, run checks, and merge the release-ready changes.
2. Wait for the merged commit's CI. Download its `npm-package` artifact and validate/install it against the corresponding source revision.
3. Push the matching tag for the intended release commit. For the first `0.1.0` release, after verifying that `origin/main` is that commit:

```sh
git fetch origin main
git tag v0.1.0 origin/main
git push origin v0.1.0
```

4. Inspect the `Release session-finder` run. It rebuilds and validates all four platforms before publishing the resulting archive to npm under `latest`.
5. Verify `npm view session-finder@0.1.0 version dist.integrity --json --registry=https://registry.npmjs.org`, compare the integrity with the published artifact, and verify npm provenance.

Only successful publication and registry verification establish that OIDC works end to end. A placeholder, trust configuration, dry run, or successful package build does not establish that.

The publish job uses a GitHub-hosted runner and is the only job granted `id-token: write`. Do not configure `NPM_TOKEN` or `NODE_AUTH_TOKEN` for this workflow. Public packages published through supported GitHub trusted publishing receive npm provenance automatically.

The publish script validates and installs the archive before checking the target version. It skips an existing version only if `dist.integrity` matches exactly; different content requires a new version. Registry network/authentication errors are not treated as a missing version. After publication it checks registry integrity again.

If npm accepts a publication but the subsequent registry check fails during propagation, inspect the registry before retrying; the version may already exist. Recover a workflow publication from its matching tag, preserving the exact archive identity.

The bootstrap published locally may lack GitHub provenance. This workflow does not create GitHub Releases; npm archives and their integrity files are retained as Actions artifacts.
