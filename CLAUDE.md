# id4pii — developer & internals guide

This file is the technical reference: architecture, CLI surface, HTTP API, internal protocols, performance numbers, debugging knobs. The repo root `README.md` is the promotional landing page for end users; build/contributor steps live in `CONTRIBUTING.md`.

## What it is

A local PII layer for text — detect, redact, and **reversibly anonymize** — as a CLI, HTTP API, system-tray guard, and Chrome MV3 extension. Inference is [OpenAI Privacy Filter](https://huggingface.co/openai/privacy-filter) running locally through ONNX Runtime. No data leaves the machine.

It tags eight categories: `account_number`, `private_address`, `private_date`, `private_email`, `private_person`, `private_phone`, `private_url`, `secret`.

Detection is **hybrid**: a fast compiled-regex pre-pass catches the structurally-regular PII and secrets (emails, URLs, phones, card/account numbers, dates, API keys) in one linear pass, those matches are masked out of the text, and only the *shortened* text is handed to the model — which then focuses on the context-dependent categories (people, addresses) it alone can do. Fewer tokens reach the expensive transformer, so detection is both faster and broader. See **Detection pipeline** below.

Main use: sit **between your app and an LLM**. Swap real PII for realistic fake surrogates before the call, send the harmless text to the model, then swap the real values back into the response — the model never sees real data.

## Repo layout

```
crates/
  core/   id4pii-core      — hybrid detection (detect/: regex pre-pass, ONNX model, masking,
                             merge), span decoding, redaction, anonymization, shared paths
                             (data_root, model_dir, log_dir, vault_file)
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
- The tokenizer is **not** downloaded: id4pii embeds the `o200k_base` vocab via `tiktoken-rs`, which produces token ids identical to privacy-filter's own tokenizer (guarded by a regression test in `crates/core/src/detect/model.rs`).
- id4pii feeds the model `input_ids` and `attention_mask`. If a run fails with an ONNX error naming a missing required input, that input name needs wiring into `crates/core/src/detect/model.rs`.
- **Long inputs are windowed.** `ModelDetector::detect` processes inputs at or below `DETECT_WINDOW` (1024) tokens in a single pass (byte-identical to feeding the whole text); longer inputs are split into overlapping windows (`DETECT_OVERLAP` = 128 tokens) and the per-window spans are stitched by `merge_overlapping` — same-category spans whose byte ranges strictly overlap are unioned. This keeps detection cost linear in length rather than quadratic and avoids depending on the model's context limit. Tune the consts in `detect/model.rs`. Without windowing, a large input (e.g. the guard reading a big focused field) runs one massive inference whose `heads × seq × seq` attention tensor balloons to tens of GB — and ONNX Runtime's arena retains that peak for the process lifetime, which is how the guard could sit at 20 GB+.
- **Inference is batched.** A single `run_and_decode` primitive pads a set of sequences to the longest, runs them as one batched `run` (batch dim, padding masked out so each row is independent), and decodes per row. The windows of one long input are fed in chunks of `MAX_BATCH` (4) instead of one `run` per window, and `detect_batch` batches *many texts* the same way (used by `serve` — see HTTP API). This amortizes the large fixed per-`run` cost (see next bullet). `MAX_BATCH` is small because each batch element carries its own up-to-1024-token attention, so peak memory grows with it.
- **The fixed per-`run` cost dominates short inputs.** Measured: a 2-token and a 30-token inference both cost ~150–350 ms on CPU — i.e. the cost is per-`run` (graph execution / q4 weight dequant), not proportional to tokens, and not one-time (a warm-up `run` does *not* speed the next one, so there is no warm-up step). The levers that actually help are therefore batching (fewer `run`s) and a GPU provider; `with_memory_pattern(true)` and the CPU arena allocator are enabled but only trim the edges. I/O binding was investigated and skipped: the bottleneck is compute, not host allocation, so reusing buffers would not move it.
- **Execution providers.** `Detector::load` registers, in order, any GPU provider compiled in (see features below) then the CPU provider (arena on), each with non-fatal registration — so a machine without the GPU/runtime silently falls back to CPU. Build with `cargo build --release --features directml` (Windows, any DirectX 12 GPU incl. integrated) or `--features cuda` to bundle a GPU provider; the default build is CPU-only and needs no GPU runtime. `ID4PII_CPU=1` forces CPU even in a GPU build (for A/B).
- **Sequence shapes are bucketed on GPU.** DirectML (and TensorRT) recompile their graph for every distinct input shape, so variable-length sequences make a GPU *slower* than CPU. When a GPU provider is active, sequences are padded up to a fixed bucket from `SEQ_BUCKETS` (64/128/256/512/1024) instead of to the exact max, so the GPU compiles a handful of graphs and reuses them. On CPU this padding is skipped (exact length) — there is no recompile to avoid and the per-`run` cost is fixed anyway, so it would only waste compute. **Caveat from measurement:** even bucketed, this model is small enough and inputs short enough that an integrated GPU stays slower than the CPU (GPU dispatch + host↔device copy overhead exceeds the tiny compute); a discrete CUDA GPU and/or larger batches are where GPU wins. CPU remains the right default.
- **Token byte-offsets are memoized.** Building the token→byte-offset table (needed to map spans back to the source) decodes each token's bytes; `ModelDetector` caches `token id → byte length` so the warm `serve`/guard request loop never re-decodes the same id twice.
- **Intra-op threads are capped.** When `--threads` is `0` (the default), the ONNX session is built with `DEFAULT_INTRA_THREADS` (2), not the core count. ONNX Runtime's intra-op pool deadlocks under the many sequential `run` calls that windowed detection makes once the thread count is high (≥~4 on a 6-core box); 1–2 threads run reliably and the model is small enough that more threads add little. Spinning is also disabled (`with_intra_op_spinning(false)`). Pass `--threads N` to override.

## Detection pipeline

`Detector::detect` (in `crates/core/src/detect/mod.rs`) is a **hybrid** of two recognizers, and every entry point (`scan`, `anonymize`, `serve`, `guard`, the extension) goes through it unchanged — the public API is identical to before.

1. **Regex pre-pass** (`detect/regex.rs`). All patterns are OR-ed into a *single* compiled `Regex` and scanned in one linear, backtrack-free pass; each pattern is wrapped in exactly one named capture group (inner groups are all non-capturing) so the capture index maps directly to a category in `O(1)`. The engine is built once behind a `OnceLock` and shared process-wide. Patterns are RE2-compatible (the `regex` crate rejects look-around / back-references) and cover email, URL, phone, IBAN / card / SSN, dates, and a broad secret set (AWS/GitHub/Google/Slack/Stripe/OpenAI keys, JWTs, PEM private-key blocks, bearer tokens). Card numbers are gated by a **Luhn** check so a bad match never masks real text out from under the model. Regex hits are reported with confidence `1.0`.
2. **Masking** (`detect/mask.rs`). Each regex hit is replaced by a single-space sentinel, which shrinks the token count the transformer must process while preserving word boundaries. A segment map records the `gap → sentinel → gap …` layout so `map_start`/`map_end` translate model span offsets back to the original document in `O(log segments)`.
3. **Model** (`detect/model.rs`). The ONNX `ModelDetector` runs over the *masked* (shorter) text — see **Model** above for windowing/threads/offset-cache details.
4. **Merge** (`combine` in `detect/mod.rs`). Model spans are remapped to original coordinates; any model span overlapping a regex hit is dropped (regex wins); the union is sorted and same-category overlaps are merged by `merge_overlapping`.

If the regex pre-pass finds nothing, the model runs directly on the original text (no masking overhead). Set `ID4PII_REGEX=0` (or `false`) to disable the pre-pass and run the model over the full text — used for A/B latency comparison; `Detector::detect_model_only` exposes the same path programmatically. The pure regex pre-pass is also exported as `id4pii_core::regex_scan(text)` for callers/benches that want the cheap structural matches without a model.

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

The model runs on a single dedicated thread fed by a queue: each handler submits its text and awaits a one-shot reply, and the thread drains all requests currently queued (up to `MAX_REQUEST_BATCH` = 16) into one batched inference (`Detector::detect_batch`). Requests that arrive while an inference is in flight naturally form the next batch, so concurrent load is coalesced with no added latency for a lone request. `min_score` is applied per request after detection, so requests with different thresholds still share a batch.

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

**The regex pre-pass is the main detection speedup.** Because the model's attention cost grows super-linearly with token count, masking the regex-found PII out of the text before inference both removes redundant work and shrinks the sequence. Measured on a PII-dense ~370-token document (`scan -f`, same binary/model, `RUST_LOG=id4pii_core=debug`, comparing the `inference complete tokens=… elapsed=…` line):

| | Tokens fed to model | Warm inference |
|---|---|---|
| `ID4PII_REGEX=0` (model only) | 366 | ~1.27 s |
| default (hybrid) | 191 | ~0.76 s |

That is **48% fewer tokens → ~40% lower inference time**, and detection is *broader* (the regex pre-pass caught an extra phone and secret the model missed: 22 vs 20 spans). The win grows with PII density and input length because of the quadratic attention term. The pre-pass itself is single-digit microseconds (`regex_scan_pii_heavy` ≈ 6.5 µs) — negligible next to inference.

Long inputs no longer blow up latency either: windowed detection (see Model) keeps inference cost linear in token count instead of the model's quadratic attention.

`deanonymize` buckets vault pairs by first byte into a flat 256-slot table (plain array indexing, no per-position hashing), so restore is roughly linear in text length even as the shared guard vault grows large.

## Benchmark & evaluation suite

Speed and correctness are measured against a committed, labelled corpus rather than ad-hoc strings — see `crates/core/data/README.md` (1500 synthetic examples from Microsoft Presidio-research, MIT, byte-offset span labels, regenerate with `python scripts/fetch-pii-dataset.py`). The loader (`id4pii_core::eval::load_tsv`) is a lean single-pass TSV reader; scoring (`eval::evaluate` / `Report`) is type-aware overlap precision/recall/F1 that treats out-of-schema `other` spans as don't-care so the engine is not penalized for entity types it does not target.

**Speed** — one criterion suite, one group per engine area, all run over the corpus:

```sh
cargo bench -p id4pii-core        # benches/engine.rs
#   parse/load_tsv            — TSV loader throughput (MB/s)
#   detect_regex/…_corpus     — regex pre-pass over every example (MB/s)
#   anonymize/…_corpus        — anonymize_with_subs over gold spans
#   deanonymize/…_corpus      — restore the whole corpus against one shared vault
#   scaling/…                 — fixed-size synthetic regression guards
```

**Correctness + model A/B** — an example that prints per-category P/R/F1 and, when the model is present, compares model-only vs hybrid on accuracy and wall-clock:

```sh
cargo run --release --example evaluate -p id4pii-core
```

It always reports the regex pre-pass scores and the token reduction masking buys the model; with the model present it adds model-only and hybrid tables plus a speed/F1 summary. A model-free CI test (`crates/core/tests/regex_eval.rs`) asserts the regex pre-pass F1 on its target categories stays above a floor.

Representative results (Ryzen 5 9600X, `model_q4`):

- **Regex pre-pass** over the full 1500-example corpus: 99.4% precision overall, 100% F1 on email/URL, 95.3% on account numbers — at ~98k examples/s. (Recall is "low" only because person/address, 75% of gold spans, have no regex coverage by design.)
- **Hybrid vs model-only** (sample): overall F1 **81.4% → 84.7%** and detection **1.30× faster** — the regex pre-pass is both more accurate (exact account/phone/URL matching) *and* cheaper. On PII-dense input the speed gap is far larger (≈40%, see above); on this general corpus regex masks ~6% of tokens, so the corpus-wide speedup is smaller but still net-positive with a correctness gain.

## Development loop

```sh
cargo test --workspace
cargo fmt --all
cargo clippy --all-targets
cargo bench -p id4pii-core        # optional: engine benchmark suite over the labelled corpus
```

Building the app crate requires a `.env` at the repo root (see `CONTRIBUTING.md`). Core can be tested standalone (`cargo test -p id4pii-core`) without `.env`.

CI (`.github/workflows/ci.yml`) runs fmt + clippy + check + test with `RUSTFLAGS=-D warnings` (clippy `pedantic`/`unwrap_used`/`expect_used` are warns promoted to hard errors), on a **matrix of `ubuntu-latest` and `windows-latest`**. The Windows job is what actually compiles the `cfg(windows)` guard and runs its engine tests; the Linux job covers core + the cross-platform surface. Each job provisions a build `.env` with `cp .env.example .env` (the placeholder values satisfy `build.rs`; no secrets are needed for checks).
