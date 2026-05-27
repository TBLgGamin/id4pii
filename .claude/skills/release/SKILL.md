---
name: release
description: Cut a new id4pii release. Bump the version in every required location, prepend a CHANGELOG entry summarizing the changes since the last tag, commit using Conventional Commits with heredoc-style messages, push to main, then push a v<version> tag to trigger the GitHub release workflow. Trigger on "release v0.X.0", "/release", "cut a release", "tag a release", "publish v0.X.0".
---

# release

Bumps the version everywhere, writes a CHANGELOG entry, and pushes the tag that triggers `.github/workflows/release.yml`. The workflow builds the installer + extension zip and attaches them to a new GitHub Release.

## Inputs

The user provides (or implies) the **target version** (e.g. `0.2.0`, `0.3.1`). If they don't, ask via `AskUserQuestion` what kind of bump (patch / minor / major) before doing anything else — semver is judgment, not arithmetic.

## Pre-flight checks

Run these in parallel before touching any file. If any check fails, stop and surface the problem to the user.

1. `git status --short` — working tree should either be clean or contain only the changes this release is documenting. Untracked junk = bail.
2. `git log --oneline $(git describe --tags --abbrev=0)..HEAD` — list every commit since the last tag. This becomes the CHANGELOG raw material.
3. `git fetch && git status -sb` — confirm local main is at or ahead of `origin/main`. If diverged, stop.
4. Confirm `CHANGELOG.md` exists. If not, create it with the structure used in the existing repo (Keep a Changelog format, semver).

## Steps

Track with `TaskCreate`/`TaskUpdate` since this is a 5+ step sequence.

### 1. Bump version in all four locations

These four MUST match. Missing one ships an inconsistent artifact.

| File | Field |
|---|---|
| `.env.example` | `ID4PII_APP_VERSION=<version>` |
| `crates/app/Cargo.toml` | `version = "<version>"` (under `[package]`) |
| `crates/core/Cargo.toml` | `version = "<version>"` (under `[package]`) |
| `extension/manifest.json` | `"version": "<version>"` |

The installer build script reads `.env`'s `ID4PII_APP_VERSION`; the extension packager stamps `manifest.json` from `.env` too — but the source-tree manifest version should still match for any unpacked dev loads and review.

After bumping, run `cargo check --workspace` once to confirm `Cargo.lock` is updated and nothing got broken.

### 2. Write the CHANGELOG entry

Prepend a new `## [<version>] — <YYYY-MM-DD>` section to `CHANGELOG.md` above the previous release. Use the commit log from pre-flight check #2 as raw material. Group by Conventional Commit type into these subsections (skip any subsection that has no entries):

- **Added** — new features (`feat:` commits)
- **Changed** — non-breaking changes (`refactor:`, `chore:` that change behavior)
- **Fixed** — bug fixes (`fix:` commits)
- **Removed** — anything dropped
- **Security** — anything that fixes a vulnerability (call out CVE / advisory ID if any)

Write entries from the **user's perspective**, not the implementer's. "Added per-site adapter pattern in the browser extension" — not "split main_world.js into adapters/*.js". The CHANGELOG is product release notes; the commit log is engineering history.

Keep entries one sentence each unless the change really needs more. Link to files / docs where the user would want to learn more (e.g. "see `CONTRIBUTING.md`").

### 3. Stage and commit

Group the working tree changes into a small number of conventional commits. The default sequence works for most releases:

```
feat(...): <human-summary-of-the-feature-work>
fix(...): <bug-fix-summary>
refactor(...): <refactor-summary>
chore(release): bump to <version> + changelog
```

Collapse to fewer commits if the changes are tightly related; expand if reviewers will want them separate. The final commit (`chore(release): ...`) always exists and groups the version bumps + CHANGELOG entry.

**Always use a heredoc for commit messages** so the body formats correctly and `Co-Authored-By` works:

```bash
git commit -m "$(cat <<'EOF'
feat(onboarding): redesign as step-by-step wizard

Replace the three-card status page with a linear install -> waiting ->
connected flow. The waiting step uses a split-screen layout with a
vertical carousel of promo screenshots; the connected step shows a
provider catalog grid and fires canvas-confetti on first connect.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

Stage files explicitly (`git add <file> <file>`) — never `git add -A` or `git add .` (could grab `.env`, secrets, or build artifacts).

### 4. Push to main

```bash
git push origin main
```

Confirm the push succeeded before tagging.

### 5. Tag and push the tag

```bash
git tag v<version>
git push origin v<version>
```

The `v*` tag push triggers `.github/workflows/release.yml`. It will:
1. Materialize `.env` from repo secrets
2. Build the release binary on `windows-latest`
3. Run `scripts/build-installer.ps1` + `scripts/package-extension.ps1`
4. Create a GitHub Release with `id4pii-setup.exe` and `id4pii-extension-v<version>.zip` attached

### 6. Confirm the workflow kicked off

```bash
gh run list --workflow=release.yml --limit 3
```

The newest run should be marked `queued` or `in_progress` against the new tag ref. If it's not listed, the tag push didn't reach GitHub or the workflow file is misconfigured — surface that to the user.

### 7. Tell the user what to do next

```
Release v<version> in flight.

- Build & artifact upload: watch with `gh run watch`
- Release page: https://github.com/TBLgGamin/id4pii/releases/tag/v<version>
- Chrome Web Store: upload `dist/id4pii-extension-v<version>.zip` manually
  (download the artifact from the release once the workflow finishes)
- If the README "Add to Chrome" link still points at the bare
  chromewebstore.google.com root, update it once the Web Store listing is approved.
```

## Notes

- The release workflow needs the repo secrets `ID4PII_PUBLISHED_EXTENSION_ID`, `ID4PII_INSTALLER_URL`, `ID4PII_INSTALLER_SIGNTOOL`, `ID4PII_INSTALLER_SIGN_UNINSTALLER` to be set. If a fresh repo, ask the user to set them before tagging.
- Never use `--no-verify` to skip hooks, never force-push to main, never delete a tag without explicit confirmation. The user's no-auto-push memory does NOT apply to releases the user has explicitly asked for, but never assume more than what was requested.
- If the workflow fails mid-build, the tag still exists. Don't auto-recreate it — fix the underlying issue, delete the tag locally + remote (with explicit user permission), and retry.
