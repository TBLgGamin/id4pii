# Changelog

All notable changes to id4pii. The format is loosely based on [Keep a Changelog](https://keepachangelog.com/), and the project follows [Semantic Versioning](https://semver.org/).

## [0.2.0] — 2026-05-27

### Added

- **Per-site adapter pattern in the browser extension.** `extension/src/adapters/{chatgpt,claude,gemini}.js` each register a host regex list, chat URL patterns, and a body anonymizer. The shared `extension/src/main/core.js` owns fetch/XHR patching, response streaming, vault IPC, and the DOM restore observer. Adding a new chat site is now one new file plus a manifest entry — see `CONTRIBUTING.md`.
- **Step-by-step onboarding wizard** (`extension/onboarding/`). Replaces the old three-card status page with a linear flow: install → waiting → connected. The waiting step uses a split-screen layout (info on the left, full-height vertical carousel of promo screenshots on the right) and an indeterminate shadcn-style progress bar. The connected step shows a provider catalog grid (logos + hosts) and fires a one-shot canvas-confetti burst on first connect.
- **Provider catalog** on the success page with monochrome logos from [simple-icons](https://simpleicons.org/) (CC0). Each card opens the site in a new tab.
- **`canvas-confetti`** vendored at `extension/onboarding/confetti.browser.min.js` (10.8 KB, MIT). Multi-burst sequence with the project's monochrome + accent-green palette. Respects `prefers-reduced-motion`.
- **`/add-site` Claude Code skill** (`.claude/skills/add-site/SKILL.md`). Walks through adding a new chat site end-to-end: capturing chat URLs from DevTools, writing the adapter, updating the manifest, adding the provider card to the onboarding catalog.
- **`/release` Claude Code skill** (`.claude/skills/release/SKILL.md`). Bumps version in all four locations, updates CHANGELOG.md, commits with conventional commit messages, and pushes a `v*` tag to trigger the release workflow.

### Changed

- **Extension folder reorganized** to `extension/src/{isolated,main,adapters}/` plus a dedicated `extension/onboarding/` subdir. Old flat `main_world.js`, `content.js`, `ui.js`, `logger.js`, `logger_main.js` are gone.
- **`sync-extension-assets.ps1`** now also copies `assets/promotional/*.webp` (for the onboarding carousel) and `assets/providers/*.svg` (for the catalog logos) into `extension/assets/`.
- **`CONTRIBUTING.md`** and **`CLAUDE.md`** updated for the adapter structure and new add-a-site recipe.

### Fixed

- **Buggy `findInputForOverlay` / `rectOf` references** in the old XHR non-Gemini path that would have thrown `ReferenceError` at runtime. The unified intercept now calls `cursorAnchor()` consistently.
- **Both `id4pii.exe` and `id4pii-guard.exe` now embed the icon resource** so Explorer / Task Manager / Alt-Tab show the shield icon for both binaries. Inno installer wizard images now render on a white background instead of the previous transparent-on-dark mismatch.

## [0.1.0] — 2026-05-26

Initial public release.

- Local PII detection via OpenAI's privacy-filter ONNX model.
- CLI (`scan`, `anonymize`, `deanonymize`, `serve`).
- Windows tray guard with Ctrl+Shift+A/Z/U hotkeys and a UI-Automation-based universal text-field anonymizer.
- Loopback WebSocket bridge for the Chrome MV3 extension.
- Reversible anonymization with a DPAPI-encrypted vault.
- Inno Setup installer, GitHub Release workflow (`v*` tag), `id4pii install/uninstall/doctor` subcommands.
