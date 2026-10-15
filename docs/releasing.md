# Development and releases

## Prerequisites

The app targets **Windows 10/11 x64**. Install Node.js 20 or newer, Git, Rust with the `x86_64-pc-windows-msvc` target, Visual Studio C++ Build Tools with a Windows SDK, and Microsoft Edge WebView2. The release script uses Windows PowerShell and its built-in archive module. End users only need the portable EXE and WebView2.

```powershell
npm ci
npm run dev
npm run version:check
npm test
npm run check
npm run build
```

`npm run build` validates versions, remaps local compiler paths, builds the Windows x64 EXE, scans the binary, and creates a ZIP, SHA-256 checksum file and release manifest in `dist/releases/`. Only the EXE, README, changelog and MIT license are included in the ZIP. It does not read your usage database, package screenshots, install the app, commit files, or create Git tags.

The raw executable is `src-tauri/target/x86_64-pc-windows-msvc/release/claude-cache-watch.exe`. Publish the verified ZIP and checksum from `dist/releases/`; older ad hoc EXEs and ZIPs elsewhere in `dist/` may contain local compiler paths. If additional compiler flags are needed, supply them through `CARGO_ENCODED_RUSTFLAGS`; the script preserves that variable and restores it after building.

## Semantic versions

`package.json` is the version source. The version check requires exact agreement with `package-lock.json` (including its root package), `src-tauri/Cargo.toml`, `src-tauri/Cargo.lock`, and `src-tauri/tauri.conf.json`.

```powershell
npm run version:bump -- patch
npm run version:bump -- minor
npm run version:bump -- major
npm run version:bump -- 1.0.0-beta.1
```

Choose one bump, then review the resulting files. Patch versions normally cover fixes, minor versions add compatible features, and major versions signal breaking changes. Explicit prerelease versions are supported. Versions must increase; whitespace, non-SemVer strings and downgrades are rejected before files are written. The command does not stage, commit, tag or push anything.

Add a nonempty `## [VERSION]` section to `CHANGELOG.md` for every release. A tag must be exactly `vVERSION`, including any prerelease or build identifiers. Tag validation rejects mismatched manifest versions or missing release notes.

## Connect GitHub

1. Create or choose a GitHub repository and configure this checkout's `origin` using its URL.
2. Review the source and commit metadata before pushing. Existing author names and emails are retained intentionally.
3. Push the reviewed commits. GitHub Actions validates pull requests and pushes to `main`/`master`, and builds tags beginning with `v`.

The workflow uses GitHub-hosted `windows-2022` for native compilation and `ubuntu-24.04` to publish releases. First-party actions are pinned to full commit SHAs. Ordinary builds have read-only repository permissions. Only the release job gets `contents: write`, and it runs for pushed tags after the build succeeds. Fork pull requests do not get a publishing token.

No personal access token is required. The publishing job uses the repository's automatic `GITHUB_TOKEN`. The GitHub repository and API URL come from the job context, so no account name or project URL is hardcoded.

## Publish a version

Update versions and release notes, run the checks/build, and review and commit the changes. Then create and push the matching tag yourself:

```powershell
$releaseTag = "v" + (Get-Content -Raw package.json | ConvertFrom-Json).version
git tag $releaseTag
git push origin $releaseTag
```

The tag workflow verifies the version and changelog, tests and builds the app, scans the package, and uploads a workflow artifact. The release job downloads that exact package, verifies its checksum and allowed archive members, and checks that the manifest and current tag both identify the workflow's source commit. Annotated tags are resolved to their commit. Publishing requires a clean source checkout. Local builds may record uncommitted changes for testing, but such packages cannot be published by the release script.

The publisher finds managed drafts through GitHub's paginated release listing, uploads only the ZIP and checksum, and rechecks the tag and complete asset list before publishing. Unexpected attachments stop publication without being deleted. SemVer prereleases become GitHub prereleases and do not become the latest stable release; stable releases use GitHub's SemVer-aware latest selection.

A retry can reuse matching uploaded assets. Published assets are immutable in this workflow: conflicting or unverifiable assets cause an error rather than being overwritten. An existing draft without the workflow's ownership marker is also left alone. Fix a failed managed draft or publish a new version; do not move an already published tag.

Run a local, nonpublishing preflight with:

```powershell
node scripts/github-release.mjs --check
```

This checks the current package and release notes. Actual publishing additionally requires a matching GitHub Actions tag context and its token.

## Publication-sensitive data

```powershell
npm run audit:public
npm run audit:public -- --artifact dist/releases/claude-cache-watch-0.1.0-windows-x64-portable.zip
```

The audit scans tracked and nonignored candidate files, reachable Git file history, and any explicitly supplied artifacts for known credential and personal-profile-path patterns. It reports file locations without printing matched secrets. ZIP contents and UTF-16 binary strings are checked too. Pattern scanning cannot prove absence of every possible secret or read private information from image pixels.

Databases, transcripts, exports, environment files, crash dumps, compiler symbols and build outputs are ignored. The app's live history remains outside the repository in Local AppData. Screenshots and JSON exports can contain session titles, project paths and real billing estimates: use synthetic data for public examples. The README's example image is synthetic. Existing local screenshots in `dist/` are not release inputs and should not be uploaded without visual review.
