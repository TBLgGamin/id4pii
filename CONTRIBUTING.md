# Contributing to id4pii

Thanks for considering a contribution. id4pii is MIT-licensed and welcomes PRs from anyone — bug fixes, new chat-site adapters, doc clarifications, packaging improvements, the works.

For architecture, internal protocols, and the full CLI/HTTP reference, see **[CLAUDE.md](CLAUDE.md)**.

## TL;DR

```sh
git clone https://github.com/TBLgGamin/id4pii
cd id4pii
cp .env.example .env
cargo test --workspace
```

That's it for the test suite. To build and run the app crate, you also need:

```sh
cargo build --release -p id4pii-app
./target/release/id4pii scan "Hi, I'm Alice <alice@example.com>"
```

## Required `.env`

The app crate refuses to build without a `.env` file at the repo root. **End users never deal with this** — they install the prebuilt `.exe` from GitHub Releases or the extension from the Chrome Web Store. The `.env` is purely for building from source.

```sh
cp .env.example .env
```

Then edit `.env`. The default values in `.env.example` are fine for local dev. The only key you might want to override is `ID4PII_PUBLISHED_EXTENSION_ID` — leave it blank unless you've published your own fork to the Chrome Web Store and want guard to pin that ID.

The build fails with a clear error if `.env` is missing or a required key is absent. Don't add fallbacks.

### `.env` keys

| Key                                 | Purpose                                                                                                                         |
| ----------------------------------- | ------------------------------------------------------------------------------------------------------------------------------- |
| `ID4PII_PUBLISHED_EXTENSION_ID`     | 32-char Chrome Web Store extension ID. Empty = bridge rejects every browser origin (use `--dev-extensions` for unpacked loads). |
| `ID4PII_INSTALLER_URL`              | URL the extension's onboarding page points to when the engine isn't installed.                                                  |
| `ID4PII_APP_VERSION`                | Version stamp baked into the installer and the extension zip.                                                                   |
| `ID4PII_INSTALLER_SIGNTOOL`         | Full `signtool` command for Inno's `SignTool=` directive. Empty = unsigned dev installer.                                       |
| `ID4PII_INSTALLER_SIGN_UNINSTALLER` | `yes` or `no`.                                                                                                                  |
| `ID4PII_GITHUB_REPO`                | `owner/repo` slug.                                                                                                              |

For CI, the same keys come from repository secrets — see `.github/workflows/release.yml`.

## Toolchain

- Rust **1.95.0** (pinned in `rust-toolchain.toml`).
- For the installer: Inno Setup 6 on PATH (`iscc.exe`) or installed in the default location.
- For Windows-side development you need a Windows machine — the `guard` subcommand and the installer are `cfg(windows)`-gated.

## Development loop

```sh
cargo test --workspace              # all tests
cargo test -p id4pii-core           # core only, no .env needed
cargo fmt --all                     # format
cargo clippy --all-targets          # lints (workspace is configured strict)
cargo bench -p id4pii-core          # hot-path benches (deanonymize, anonymize_with_subs)
```

CI runs fmt + clippy + check + test on both `ubuntu-latest` and `windows-latest` (with `RUSTFLAGS=-D warnings`, so any clippy `pedantic`/`unwrap`/`expect` warning fails the build). The Windows job is the one that compiles the `cfg(windows)` guard and runs its engine integration tests, so test guard changes on Windows before pushing. CI provisions its own `.env` with `cp .env.example .env`.

## Running guard locally

The shipped product is `id4pii-guard.exe` — a Windows-subsystem binary with no console. For development you usually want the console-attached version so you can watch logs in stderr:

```sh
cargo run --release -p id4pii-app -- guard --dev-extensions
```

Same code, console output. Logs also land in `%LOCALAPPDATA%\id4pii\logs\guard.log` either way.

To exercise the no-console binary that end users actually run:

```sh
cargo run --release --bin id4pii-guard -- --dev-extensions
```

The `--dev-extensions` flag relaxes the bridge's origin check so any `chrome-extension://` ID can connect. Production builds pin the single Web Store ID from `.env`.

## Loading the extension unpacked

```sh
.\scripts\sync-extension-assets.ps1
```

Then in Chrome: `chrome://extensions` → enable **Developer mode** → **Load unpacked** → select the `extension/` directory. Guard must be running with `--dev-extensions` for the bridge to accept the connection (the production allowlist pins the single Web Store ID).

## Building the installer locally

```powershell
.\scripts\build-installer.ps1
```

Output: `installer\dist\id4pii-setup.exe`. The script reads `.env`, builds the release binary, syncs assets, and invokes `iscc` with the right `/D` defines. Pass `-SkipCargo` if you already have a fresh `target/release/id4pii.exe`.

## Packaging the extension

```powershell
.\scripts\package-extension.ps1
```

Output: `dist\id4pii-extension-v<version>.zip`. This is the artifact you'd upload to the Chrome Web Store dashboard. The script substitutes the installer URL placeholder in `onboarding.js` and stamps the manifest version, all from `.env`.

## Adding a new chat site

The extension uses one adapter per site under `extension/src/adapters/`. Each adapter registers a host regex list, a chat-URL pattern list, and a body anonymizer. To add a new site:

1. Copy `extension/src/adapters/chatgpt.js` to `extension/src/adapters/<site>.js`. Update `name`, `hosts` (regexes matched against `location.hostname`), and `chatPatterns` (regexes matched against outbound request URLs). For ChatGPT-style sites that POST a JSON body with a `messages[]` array, the default `core.helpers.anonymizeJsonBody` handles it — no further work needed.
2. If the site uses a request shape that doesn't match the generic JSON-messages assumption (Gemini's `BardChatUi` form encoding is the existing example in `extension/src/adapters/gemini.js`), write a custom `anonymizeBody(core, reqId, rawBody)` and set `wrapsResponse: false` if the response shouldn't be streamed through the restore transformer.
3. Add the new adapter script to the MAIN-world `js` array in `extension/manifest.json` (before `src/main/boot.js`), and add the host to every `content_scripts.matches` array plus the `web_accessible_resources.matches` entry. No `host_permissions` entry — static content scripts inject without one, and skipping `host_permissions` keeps the extension out of Chrome Web Store's "in-depth review" queue.
4. Reload the unpacked extension.

The model, the detector, the vault, the response-restore stream, and the DOM mutation observer are all owned by `core.js` and reused — your adapter only owns "which host am I, which URLs are chat requests on this site, and how do I dig the user prompt out of the body."

## Code style

- **No comments**. Code must self-document via names. The CI runs `cargo clippy --all-targets` against a strict workspace lint set (`#![deny(unsafe_code)]`, `clippy::pedantic`, `unwrap_used`/`expect_used` as warnings).
- Use the dedicated tools rather than shell-outs where possible. The codebase prefers `winreg`-style direct calls only when sandboxing makes shell-outs awkward — `reg.exe` shell-outs are fine for one-shot installer logic.
- Keep PRs focused. A bug fix, a feature, or a refactor — not all three at once.

## Releasing

Maintainers cut a release by pushing a `v<version>` tag. The `Release` workflow:

1. Builds the release binary on `windows-latest`.
2. Reads the `ID4PII_*` repo secrets, writes them as `.env`.
3. Runs `scripts/build-installer.ps1` and `scripts/package-extension.ps1`.
4. Creates a GitHub Release with both artifacts attached.

The extension `.zip` then needs to be uploaded manually to the Chrome Web Store dashboard for review.

## Conduct

Be kind, assume good faith, and stay on-topic. id4pii is a small project — there's no formal Code of Conduct yet, but the [Contributor Covenant](https://www.contributor-covenant.org/) is the de-facto baseline.

## License

By contributing, you agree that your contributions are licensed under the [MIT License](LICENSE) — same as the rest of the project.
