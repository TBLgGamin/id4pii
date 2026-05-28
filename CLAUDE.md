# id4pii — developer & internals guide

This file is the technical reference: architecture, CLI surface, HTTP API, internal protocols, performance numbers, debugging knobs. The repo root `README.md` is the promotional landing page for end users; build/contributor steps live in `CONTRIBUTING.md`.

## What it is

A local PII layer for text — detect, redact, and **reversibly anonymize** — as a CLI, HTTP API, system-tray guard, and Chrome MV3 extension. Inference is [OpenAI Privacy Filter](https://huggingface.co/openai/privacy-filter) running locally through ONNX Runtime. No data leaves the machine.

It tags eight categories: `account_number`, `private_address`, `private_date`, `private_email`, `private_person`, `private_phone`, `private_url`, `secret`.

Main use: sit **between your app and an LLM**. Swap real PII for realistic fake surrogates before the call, send the harmless text to the model, then swap the real values back into the response — the model never sees real data.

## Repo layout

```
crates/
  core/   id4pii-core      — ONNX inference, span decoding, redaction, anonymization, shared
                             paths (data_root, model_dir, log_dir, vault_file)
  app/    id4pii-app       — lib + two binaries:
                               id4pii         (CLI; scan / anonymize / deanonymize / serve /
                                              guard / install / uninstall / doctor; console
                                              subsystem)
                               id4pii-guard   (GUI subsystem, no console; reads the same
                                              GuardArgs as `id4pii guard` but is the binary
                                              shipped to end users for auto-start and
                                              Start Menu)
extension/                  — MV3 Chrome extension
installer/                  — Inno Setup script + monochrome wizard BMPs
scripts/                    — build/package scripts; lib/env.ps1 is the shared .env reader
.env.example                — every configurable value (extension ID, signing, version, ...)
```

## Paths

All on-disk locations are derived from `id4pii_core::paths::data_root()` (=`%LOCALAPPDATA%\id4pii\` on Windows, XDG/Library equivalents elsewhere). One source of truth — vault, model, and logs all live under here, so `id4pii uninstall` removes the directory atomically.

```
data_root/
├── model/                # paths::model_dir()
│   ├── config.json
│   └── onnx/model_q4.onnx{,_data}
├── logs/                 # paths::log_dir()
│   └── guard.log         # rolling, daily, 7 retained
└── vault.bin             # paths::vault_file() — DPAPI-encrypted vault
```

## Model

- Default model dir: `data_root/model/` (see above). Falls back to `./model` if it has files (legacy dev layout).
- Override with `--model <dir>` or `ID4PII_MODEL`.
- All entry points (`scan`, `anonymize`, `serve`, `guard`) call `model_setup::ensure_model` before `Detector::load`. If files are missing, the fetcher downloads `config.json`, `onnx/model_q4.onnx`, and `onnx/model_q4.onnx_data` from `huggingface.co/openai/privacy-filter/resolve/main/…`. Idempotent — sizes are HEAD-checked against the remote.
- The tokenizer is **not** downloaded: id4pii embeds the `o200k_base` vocab via `tiktoken-rs`, which produces token ids identical to privacy-filter's own tokenizer (guarded by a regression test in `crates/core/src/detector.rs`).
- id4pii feeds the model `input_ids` and `attention_mask`. If a run fails with an ONNX error naming a missing required input, that input name needs wiring into `crates/core/src/detector.rs`.
- **Long inputs are windowed.** `Detector::detect` processes inputs at or below `DETECT_WINDOW` (1024) tokens in a single pass (byte-identical to feeding the whole text); longer inputs are split into overlapping windows (`DETECT_OVERLAP` = 128 tokens) and the per-window spans are stitched by `merge_overlapping` — same-category spans whose byte ranges strictly overlap are unioned. This keeps detection cost linear in length rather than quadratic and avoids depending on the model's context limit. Tune the consts in `detector.rs`. Without windowing, a large input (e.g. the guard reading a big focused field) runs one massive inference whose `heads × seq × seq` attention tensor balloons to tens of GB — and ONNX Runtime's arena retains that peak for the process lifetime, which is how the guard could sit at 20 GB+.
- **Intra-op threads are capped.** When `--threads` is `0` (the default), the ONNX session is built with `DEFAULT_INTRA_THREADS` (2), not the core count. ONNX Runtime's intra-op pool deadlocks under the many sequential `run` calls that windowed detection makes once the thread count is high (≥~4 on a 6-core box); 1–2 threads run reliably and the model is small enough that more threads add little. Spinning is also disabled (`with_intra_op_spinning(false)`). Pass `--threads N` to override.

## CLI

```sh
id4pii scan "Email alice@acme.com or call 555-0142"
id4pii scan --redact --style block -f notes.txt
echo "ssn 123-45-6789" | id4pii scan --format text
```

`scan` reads text from the positional argument, `--file`, or stdin. Output is JSON spans by default (`--format text` for a table, `--redact` for masked text; `--style label|block|char`).

`--min-score <f32>` (default `0.0`, i.e. keep every detection) drops spans whose averaged token confidence is below the threshold — the precision/recall dial. It is a shared detection flag, so it also applies to `anonymize` and `serve` (all share `ModelArgs`) and the guard (`--min-score`).

## Anonymize / deanonymize (the LLM shield)

`anonymize` replaces each detected PII span with a realistic fake surrogate of the same category and emits a **vault** — the fake → real mapping. The same real value always maps to the same surrogate, so the text stays coherent. `deanonymize` uses the vault to restore the real values in whatever comes back.

```sh
id4pii anonymize --vault-out vault.json "I'm Sarah Connor, sarah@skynet.com" > safe.txt
# send safe.txt to your LLM ... get a reply that mentions the fakes
id4pii deanonymize --vault vault.json "$(cat llm_reply.txt)"
```

Without `--vault-out`, `anonymize` prints one JSON object `{"anonymized": "...", "vault": [...]}`. Surrogates are random per run; pass `--seed <n>` for reproducible output. `deanonymize` needs no model.

Surrogates lean fiction-safe and nerdy — full `First Last` names from a sci-fi/hacker pool, phone numbers in the `555-01xx` range reserved for fiction, `example.com` URLs. Restoration is string-matching, with two consequences worth knowing: if the LLM rewrites a surrogate (truncates a name, splits an email) that fragment won't be restored; and if a surrogate happens to appear in unrelated parts of the reply, it will be swapped too. Multi-word surrogates make both rare, which is why person names are always two words.

## HTTP API

```sh
id4pii serve --addr 127.0.0.1:8080
```

- `GET /health` → `ok`
- `POST /scan` — `{"text": "...", "redact": true, "min_score": 0.5}` → `{"spans": [...], "redacted": "..."}`
- `POST /anonymize` — `{"text": "...", "seed": 1337, "min_score": 0.5}` (seed/min_score optional) → `{"anonymized": "...", "vault": [...]}`
- `POST /deanonymize` — `{"text": "...", "vault": [...]}` → `{"text": "..."}`

`min_score` is optional per request; when omitted it falls back to the server default set with `serve --min-score`.

## Guard — system-wide hotkey (Windows)

The guard is a system-tray daemon that anonymizes PII in *any* application's text field — Claude Desktop, ChatGPT/Codex desktop, chatgpt.com/claude.ai in any browser, anything. It works through the Windows UI Automation accessibility layer (the same layer Grammarly uses), which sits *above* the network: the app itself sends the already-anonymized text, so there is no proxy, no certificate, and nothing for TLS pinning or anti-bot checks to detect.

It ships as **two binaries that share one codebase**:

| Binary | Subsystem | When |
|---|---|---|
| `id4pii.exe guard` | console | dev iteration; piping stderr is useful |
| `id4pii-guard.exe` | windows (no console) | the binary shipped to end users — autostart, Start Menu, post-install launch |

Both parse the same `GuardArgs` (`crates/app/src/guard/mod.rs`) and call `guard::run`. The GUI binary additionally calls `logging::init_guard()` first, which writes to `data_root/logs/guard.log` (rotating daily, 7 retained). The CLI binary writes log lines to stderr through the existing `tracing_subscriber::fmt()` setup.

Three global hotkeys, all operating on the currently focused editable field:

- **`Ctrl+Shift+A`** — anonymize the field in place. Detected PII gets swapped for surrogates; the vault learns the mapping.
- **`Ctrl+Shift+Z`** — restore the field in place using the vault.
- **`Ctrl+Shift+U`** — undo the last operation on this field (within a TTL).

All three rewrite the focused field directly — there is no popup or read-only overlay. The vault is shared across every app and every browser tab for the life of the process, so a real value anonymized once is restored consistently everywhere.

The tray menu carries: bridge status (informational), **Open log file**, **Open log folder**, and **Quit id4pii guard**. The log items shell out via `cmd /C start "" <path>` so whatever the user has registered for `.log` (Notepad by default) and Explorer open the resolved `data_root/logs/...` path.

A single **vault** (persisted via DPAPI at `data_root/vault.bin`) is the id system that makes this reversible: every distinct real value is stored once with its category and a unique surrogate, so the same name always maps to the same surrogate and an email maps to its own — restoration is unambiguous in both directions. The vault is shared across every app and every tab, so a value anonymized in one place restores in another. Surrogates are generated procedurally (street addresses, URLs) or from large name pools, so the supply is effectively unbounded — fiction-safe phone numbers (`555-01xx`) are the one deliberately small set.

`--max-vault-entries <n>` (default `0` = unbounded) caps the vault as a safety ceiling for a daemon that runs for weeks. When exceeded, the oldest entries are evicted FIFO (`Vault::enforce_cap`) and the count is logged. **Eviction is lossy**: text previously anonymized with an evicted entry can no longer be restored, so set the cap well above the working set — it is a backstop, not a routine bound. (LRU would be safer but needs access tracking that the `#[serde(transparent)]` on-disk format deliberately doesn't carry.)

Notes: the guard reads and writes via UI Automation, falling back to a clipboard select-all + copy/paste for rich editors (browser `contenteditable`, some Electron apps) that do not expose direct value access. The `guard` subcommand is Windows only (macOS AX API and Linux AT-SPI are future work); the module is `cfg(windows)`-gated, so the workspace still builds on other platforms — only the subcommand is absent there.

### Engine architecture & testing

The engine (`crates/app/src/guard/engine.rs`) is a single-threaded state machine: it owns the vault and undo state, consumes `Command`s off a `sync_channel`, and publishes `Event`s to the `EventBus`. Its two external dependencies are injected behind traits so it can run without a model or a desktop:

- **`Detect`** — the PII detector. The real `Detector` implements it; tests inject a scripted fake.
- **`Field`** — the focused-field IO (read / write / targeted substitution). `UiaField` wraps the UI Automation backend in production; tests inject an in-memory fake.

`Engine::load` wires up the real backends; `Engine::with_components` takes the trait objects plus an `EngineConfig { min_score, max_vault_entries }` and is what the tests call (with a `MemoryStore` vault). The integration tests in `engine.rs` drive each `Command` and assert the resulting `Event`s, `BridgeReply`, undo behaviour, and the vault cap — covering the state machine end-to-end with no ONNX model. Because the whole `guard` module is `cfg(windows)`, these tests run on the **Windows CI job** (see Development loop).

## Browser extension

The MV3 extension in `extension/` is `id4pii guard` for LLM sites: ChatGPT, Claude, Gemini (and any others wired into the `content_scripts.matches` allowlist). When you submit, it pulls your text out of the input, hands it to the local `guard` process over a loopback WebSocket, replaces it with the anonymized version, then submits. When the assistant's reply streams in, surrogates are auto-restored to real values in the rendered text — a `MutationObserver` driving the same pure `deanonymize` string-replace used by the CLI.

It's **part of guard, not a parallel anonymizer**: the extension owns no model, no detector, and no vault. All of that lives in the existing engine — the extension is just another command source on the same bus, alongside the global hotkey. The vault is shared: a name anonymized via `Ctrl+Shift+A` in Notepad reuses its surrogate when you type it into chatgpt.com.

The extension shares its assets with the desktop guard: the toolbar icon, extension icons, and the lock-close/lock-open animation frames all live in the repo-root `assets/` directory. A small sync script copies them into `extension/assets/` (which is gitignored, treated as a build artifact):

```powershell
.\scripts\sync-extension-assets.ps1
```

Run it whenever `assets/icon-*.png` or `assets/lock_frames/*.png` change. After running it once, the extension can be loaded unpacked from `extension/`.

### Unpacked dev setup

Production users install the extension from the Chrome Web Store and the engine via the EXE installer. For development against an unpublished extension build:

1. Run `cargo run -p id4pii-app -- guard --dev-extensions` (console; live logs on stderr) or `cargo run --bin id4pii-guard -- --dev-extensions` (no console; logs to file). The bridge listens on `ws://127.0.0.1:7878/ws`; `--dev-extensions` relaxes the origin check so any locally-loaded `chrome-extension://…` can connect. Production builds pin a single published Web Store ID from `.env`.
2. In Chrome: `chrome://extensions` → **Developer mode** → **Load unpacked** → select the `extension/` directory.
3. The toolbar icon shows a solid green badge when connected, `!` when the bridge is unreachable.

### Triggers

- **On submit** (Enter or send-button click) on a whitelisted host: the extension intercepts, anonymizes via guard, then re-fires the submit with the anonymized text. Same lock-close animation as the desktop guard.
- **`Ctrl+Shift+A`** in any input on a whitelisted host: anonymize the input in place without submitting, so you can review.
- **Auto-restore**: as the assistant streams text into the page, surrogates in plain text nodes are swapped back. Code blocks (`<code>`, `<pre>`) are skipped to keep copy-paste intact.

### Failure modes

- Bridge down → submit interception fails open (passes through with a `console.warn`), so you never get stuck unable to send.
- Site interception is keyed on outbound chat-completion URLs matched against each adapter's `chatPatterns` array. One adapter per site lives in `extension/src/adapters/` (`chatgpt.js`, `claude.js`, `gemini.js`); the shared core in `extension/src/main/core.js` owns fetch/XHR patching, response streaming, vault IPC, and DOM restore. ChatGPT and Claude reuse `core.helpers.anonymizeJsonBody` (generic JSON walker); Gemini owns its own `f.req` form-encoded extractor. To add a new site: drop a new file in `extension/src/adapters/`, register it via `window.__id4pii_main.registerAdapter(...)`, and add the script + host match to `manifest.json`.

### Debugging

Both sides of the extension log silently by default. To turn on verbose tracing:

- **Guard (Rust)**: set `RUST_LOG=id4pii=debug,ort=warn` before starting guard. Every WebSocket frame, vault lock, and engine step is logged with a `req_id` field so you can grep one request end to end.
- **Extension (browser)**: in the extension's service-worker DevTools (find it at `chrome://extensions` → id4pii guard → **Inspect views: service worker**), type `id4pii.debug(true)` in the console. The setting persists in `chrome.storage.local` and applies to background, content script, and in-page main world. Disable with `id4pii.debug(false)`.

Lines have a uniform format: `[id4pii:<component>] <event> key=value …`. Components are `bg` (service worker), `iso` (content script), `main` (in-page), `bridge` and `engine` (Rust). Each fetch interception generates an 8-char `reqId` that is threaded through every hop in both directions, so you can follow a single message through `main → iso → bg → bridge → engine` and back.

Privacy invariant: no message text, response body, or vault entry is ever logged at any level — only lengths, counts, kinds, durations, and the request ID.

### Security

- Bridge binds `127.0.0.1` only.
- WebSocket handshake rejects unless `Origin` matches `chrome-extension://<published-id>` (baked in from `ID4PII_PUBLISHED_EXTENSION_ID` in `.env`). Regular web pages on `localhost` can't open a session, and neither can other browser extensions.
- `--dev-extensions` widens the allowlist to any `chrome-extension://`, `moz-extension://`, or `safari-web-extension://` origin. Use this only for unpacked dev loads.
- `--no-bridge` disables the bridge entirely.

## Install / uninstall / doctor

The CLI binary (`id4pii.exe`) ships three configuration subcommands that the Inno installer calls:

- `id4pii install --with-model --register-extension <id> --autostart` — fetches the model, writes the Chrome external-extension registry key (`HKLM\…\Chrome\Extensions\<id>` with the Web Store `update_url`), and registers `HKCU\…\Run\id4pii` to launch the *guard* binary (sibling `id4pii-guard.exe`) on login.
- `id4pii uninstall` — removes `data_root` (model + DPAPI vault + logs), the Run entry, and the Chrome registry key. Pass `--keep-model` to keep the model on disk.
- `id4pii doctor [--extension-id <id>]` — prints JSON: `{ model_present, model_dir, autostart, registry_chrome, bridge_reachable, published_extension_id_placeholder }`.

## Installer

`installer/id4pii.iss` (Inno Setup 6) bundles both binaries, registers shortcuts (Start Menu group "id4pii" with **id4pii**, **Open id4pii log folder**, **Uninstall**, plus optional desktop icon), and runs `id4pii.exe install --with-model` post-install. The wizard uses Inno's modern style with two custom BMPs at `installer/wizard-image.bmp` and `installer/wizard-small.bmp` rendered from the shield logo on a near-black canvas — closest the installer toolkit gets to a shadcn dark-mode look. Regenerate the BMPs with `python scripts/generate-wizard-images.py` if the logo changes. Full shadcn-styled installer UI is not possible inside Inno (Pascal + native VCL widgets); switching to Tauri's bundler or WiX-with-WPF would be required for that and is out of scope.

## Performance

Always run the optimized build — `cargo run` uses the unoptimized `dev` profile and is many times slower:

```sh
cargo build --release
./target/release/id4pii scan "..."
```

Measured on a Ryzen 5 9600X with the `model_q4` variant:

| Path | Latency |
|---|---|
| CLI one-shot (cold) | ~195 ms |
| `serve` request (warm) | ~33 ms |

The tokenizer is embedded and loads in ~85 ms (in parallel with the ONNX session), so a cold CLI run is dominated by model load and one inference pass. For repeated or latency-sensitive use, `serve` still wins decisively — the model loads once and each request is just inference. Set `RUST_LOG=id4pii_core=debug` to see per-phase load timings.

Long inputs no longer blow up latency: windowed detection (see Model) keeps inference cost linear in token count instead of the model's quadratic attention. The pure hot functions have a criterion bench harness:

```sh
cargo bench -p id4pii-core        # deanonymize (bucketed restore) + anonymize_with_subs
```

`deanonymize` buckets vault pairs by first byte, so restore is roughly linear in text length even as the shared guard vault grows large.

## Development loop

```sh
cargo test --workspace
cargo fmt --all
cargo clippy --all-targets
cargo bench -p id4pii-core        # optional: hot-path benches
```

Building the app crate requires a `.env` at the repo root (see `CONTRIBUTING.md`). Core can be tested standalone (`cargo test -p id4pii-core`) without `.env`.

CI (`.github/workflows/ci.yml`) runs fmt + clippy + check + test with `RUSTFLAGS=-D warnings` (clippy `pedantic`/`unwrap_used`/`expect_used` are warns promoted to hard errors), on a **matrix of `ubuntu-latest` and `windows-latest`**. The Windows job is what actually compiles the `cfg(windows)` guard and runs its engine tests; the Linux job covers core + the cross-platform surface. Each job provisions a build `.env` with `cp .env.example .env` (the placeholder values satisfy `build.rs`; no secrets are needed for checks).
