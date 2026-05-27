# id4pii installer

This directory builds `id4pii-setup.exe` — the Windows installer for the EXE channel. All configurable values come from a `.env` file at the repo root.

## Build

Prereqs: [Inno Setup 6](https://jrsoftware.org/isdl.php). The build script auto-installs it via `winget install JRSoftware.InnoSetup` if missing.

```powershell
cp .env.example .env
notepad .env
.\scripts\build-installer.ps1
```

Output: `installer\dist\id4pii-setup.exe`.

`build-installer.ps1` does three things, in order:
1. `cargo build --release -p id4pii-app` — produces both `id4pii.exe` (CLI) and `id4pii-guard.exe` (GUI). Skip with `-SkipCargo` if you already have a fresh release build.
2. Syncs extension assets via `sync-extension-assets.ps1`.
3. Invokes `iscc` with `/D` defines populated from `.env` (parsed by the shared `scripts/lib/env.ps1`).

`build.rs` in `crates/app` also reads `.env` so the Rust binary picks up the same extension ID and other values at compile time — no runtime `.env` lookup, the values are baked into the exe.

## Wizard images

The two `wizard-*.bmp` files in this directory are the dark monochrome shield rendered from `assets/icon-256.png`. To regenerate after a logo change:

```sh
python scripts/generate-wizard-images.py
```

The result is the closest approximation of shadcn dark-mode look the Inno installer supports — Inno's controls are native VCL widgets, so true HTML/CSS theming isn't possible. Switching to Tauri's bundler or WiX with WPF would be required for a fully shadcn-styled installer; out of scope for now.

## What the installer wires up

- `id4pii.exe` and `id4pii-guard.exe` into `Program Files\id4pii\`.
- Start Menu group **id4pii** with: shortcut to `id4pii-guard.exe`, **Open id4pii log folder**, **Uninstall id4pii**.
- Optional Desktop shortcut (off by default, presented as a Task).
- `HKCU\…\Run\id4pii` pointing at `id4pii-guard.exe` for login auto-start.
- Pre-registered Chrome extension via `HKLM\…\Chrome\Extensions\<id>` (only when `ID4PII_PUBLISHED_EXTENSION_ID` is non-empty).
- Post-install run of `id4pii.exe install --with-model` to fetch the model.

## Config keys

All keys live in `.env.example`. Copy to `.env` and fill in.

| Key | What it sets |
|---|---|
| `ID4PII_PUBLISHED_EXTENSION_ID` | 32-char Chrome Web Store extension ID. Empty = bridge rejects all browser connections (except `--dev-extensions`); installer hides the "pre-register Chrome extension" task. |
| `ID4PII_INSTALLER_URL` | URL the extension's onboarding page points users at when the local engine is missing. |
| `ID4PII_APP_VERSION` | Installer + extension version stamp. |
| `ID4PII_INSTALLER_SIGNTOOL` | Full `signtool` command for Inno's `SignTool=` directive. Empty = no signing. |
| `ID4PII_INSTALLER_SIGN_UNINSTALLER` | `yes` or `no`. |
| `ID4PII_GITHUB_REPO` | `owner/repo` slug. Used to template the publisher URL. |

In CI, the same keys are read from repo secrets — see `.github/workflows/release.yml`. Add each as a repository secret with the exact `ID4PII_*` name.

## Uninstall behavior

By default `id4pii.exe uninstall` (which Inno calls on uninstall) removes:

- The model directory, the rolling log files, and the encrypted vault — the whole `%LOCALAPPDATA%\id4pii\` tree.
- The Windows startup entry.
- The Chrome external-extension registry key.

Pass `--keep-model` to keep the model on disk (useful when reinstalling, since re-downloading the 875 MB shard is slow).

## Code signing

Set `ID4PII_INSTALLER_SIGNTOOL` in `.env` (or as a repo secret in CI). Example:

```
ID4PII_INSTALLER_SIGNTOOL=signtool sign /fd SHA256 /tr http://timestamp.digicert.com /td SHA256 $f
ID4PII_INSTALLER_SIGN_UNINSTALLER=yes
```

Inno picks this up via the conditional `SignTool=` directive in `id4pii.iss`.
