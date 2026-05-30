# id4pii — developer & internals guide

This file is the technical reference: architecture, CLI surface, HTTP API, internal protocols, performance numbers, debugging knobs. The repo root `README.md` is the promotional landing page for end users; build/contributor steps live in `CONTRIBUTING.md`.

## What it is

A local PII layer for text — detect, redact, and **reversibly anonymize** — as a CLI, HTTP API, system-tray daemon, and Chrome MV3 extension. Inference is [OpenAI Privacy Filter](https://huggingface.co/openai/privacy-filter) running locally through ONNX Runtime. No data leaves the machine.

It tags eight categories: `account_number`, `private_address`, `private_date`, `private_email`, `private_person`, `private_phone`, `private_url`, `secret`.

Detection is **hybrid**: a fast compiled-regex pre-pass catches the structurally-regular PII and secrets (emails, URLs, phones, card/account numbers, dates, API keys) in one linear pass, those matches are masked out of the text, and only the *shortened* text is handed to the model — which then focuses on the context-dependent categories (people, addresses) it alone can do. Fewer tokens reach the expensive transformer, so detection is both faster and broader. See **Detection pipeline** below.

Main use: sit **between your app and an LLM**. Swap real PII for realistic fake surrogates before the call, send the harmless text to the model, then swap the real values back into the response — the model never sees real data.

## Repo layout

One crate, `crates/id4pii/`, is the whole engine + surfaces — library plus
two binaries:

```
crates/id4pii/
  src/
    detect/                 — hybrid detection (regex pre-pass, ONNX model, masking, merge)
    anonymize.rs redact.rs  — surrogate vault, reversible anonymize/deanonymize, redaction
    labels.rs eval.rs       — categories; labelled-corpus scoring
    paths.rs model_dir.rs model_fetch.rs error.rs   — shared paths + model fetch + errors
    detector_service.rs     — one Detector on a dedicated thread; serve + batch share it
    cli.rs serve.rs batch.rs extract.rs             — CLI dispatch, HTTP API, corpus path, docs
    model_setup.rs logging.rs progress.rs install.rs
    daemon/                 — system-tray daemon: engine state machine, WS bridge, UIA, vault store
    main.rs                 — id4pii          (CLI; scan / anonymize / deanonymize / batch /
                                              serve / daemon / install / uninstall / doctor;
                                              console subsystem)
    bin/id4pii_daemon.rs    — id4pii-daemon   (GUI subsystem, no console; reads the same
                                              DaemonArgs as `id4pii daemon` but is the binary
                                              shipped to end users for auto-start and Start Menu)
  benches/ examples/ tests/ data/   — engine bench suite, evaluate example, regex eval, corpus
extension/                  — MV3 Chrome extension
installer/                  — Inno Setup script + monochrome wizard BMPs
scripts/                    — build/package scripts; lib/env.ps1 is the shared .env reader
.env.example                — every configurable value (extension ID, signing, version, ...)
```

## Paths

All on-disk locations are derived from `id4pii::paths::data_root()` (=`%LOCALAPPDATA%\id4pii\` on Windows, XDG/Library equivalents elsewhere). One source of truth — vault, model, and logs all live under here, so `id4pii uninstall` removes the directory atomically.

```
data_root/
├── model/                # paths::model_dir()
│   ├── config.json
│   └── onnx/model_q4.onnx{,_data}
├── logs/                 # paths::log_dir()
│   └── daemon.log         # rolling, daily, 7 retained
└── vault.bin             # paths::vault_file() — DPAPI-encrypted vault
```

## Model

- Default model dir: `data_root/model/` (see above). Falls back to `./model` if it has files (legacy dev layout).
- Override with `--model <dir>` or `ID4PII_MODEL`.
- All entry points (`scan`, `anonymize`, `serve`, `daemon`) load through `model_setup::load_detector`, which calls `ensure_model` then `Detector::load` (one shared helper, no per-command boilerplate). If files are missing, the fetcher downloads `config.json`, `onnx/model_q4.onnx`, and `onnx/model_q4.onnx_data` from `huggingface.co/openai/privacy-filter/resolve/main/…`. Idempotent — sizes are HEAD-checked against the remote.
- The tokenizer is **not** downloaded: id4pii embeds the `o200k_base` vocab via `tiktoken-rs`, which produces token ids identical to privacy-filter's own tokenizer (guarded by a regression test in `crates/id4pii/src/detect/model.rs`).
- id4pii feeds the model `input_ids` and `attention_mask`. If a run fails with an ONNX error naming a missing required input, that input name needs wiring into `crates/id4pii/src/detect/model.rs`.
- **Long inputs are windowed.** A text at or below `DETECT_WINDOW` (1024) tokens is one window (byte-identical to feeding the whole text); longer inputs are split into overlapping windows (`DETECT_OVERLAP` = 128 tokens) and the per-window spans are stitched by `merge_overlapping` — same-category spans whose byte ranges strictly overlap are unioned. This keeps detection cost linear in length rather than quadratic and avoids depending on the model's context limit. Tune the consts in `detect/model.rs`. Without windowing, a large input (e.g. the daemon reading a big focused field) runs one massive inference whose `heads × seq × seq` attention tensor balloons to tens of GB — and ONNX Runtime's arena retains that peak for the process lifetime, which is how the daemon could sit at 20 GB+.
- **Inference is batched, adaptively.** A single `run_and_decode` primitive pads a chunk of sequences to the longest, runs them as one batched `run` (batch dim, padding masked out so each row is independent), and decodes per row. There is one detection path — `ModelDetector::detect_batch` — used by *every* caller (single text via `Detector::detect` is just a batch of one). It length-sorts all windows so each chunk pads tightly, then picks the chunk size adaptively (`plan_batch`): rows = `TOKEN_BUDGET` (4096) ÷ padded-sequence-length, clamped to a cap (`CPU_BATCH_CAP` 16 / `GPU_WINDOW_BATCH` 32). So 1024-token windows run ~4-at-a-time (the old fixed `MAX_BATCH` memory behaviour — each row carries its own up-to-1024-token attention) while short sequences pack densely to amortise the fixed per-`run` cost. `Detector::set_batch_override(Some(n))` pins the size for throughput tuning (the `batch --batch` flag); the default is adaptive.
- **The fixed per-`run` cost dominates short inputs.** Measured: a 2-token and a 30-token inference both cost ~150–350 ms on CPU — i.e. the cost is per-`run` (graph execution / q4 weight dequant), not proportional to tokens, and not one-time (a warm-up `run` does *not* speed the next one, so there is no warm-up step). The levers that actually help are therefore batching (fewer `run`s) and a GPU provider; `with_memory_pattern(true)` and the CPU arena allocator are enabled but only trim the edges. I/O binding was investigated and skipped: the bottleneck is compute, not host allocation, so reusing buffers would not move it.
- **Execution providers.** `Detector::load` registers, in order, any GPU provider compiled in (see features below) then the CPU provider (arena on), each with non-fatal registration — so a machine without the GPU/runtime silently falls back to CPU. Build with `cargo build --release --features directml` (Windows, any DirectX 12 GPU incl. integrated) or `--features cuda` to bundle a GPU provider; the default build is CPU-only and needs no GPU runtime. `ID4PII_CPU=1` forces CPU even in a GPU build (for A/B).
- **Sequence shapes are bucketed on GPU.** DirectML (and TensorRT) recompile their graph for every distinct input shape, so variable-length sequences make a GPU *slower* than CPU. When a GPU provider is active, sequences are padded up to a fixed bucket from `SEQ_BUCKETS` (64/128/256/512/1024) instead of to the exact max, so the GPU compiles a handful of graphs and reuses them. On CPU this padding is skipped (exact length) — there is no recompile to avoid and the per-`run` cost is fixed anyway, so it would only waste compute. **Caveat from measurement:** even bucketed, this model is small enough and inputs short enough that an integrated GPU stays slower than the CPU (GPU dispatch + host↔device copy overhead exceeds the tiny compute); a discrete CUDA GPU and/or larger batches are where GPU wins. CPU remains the right default.
- **Token byte-offsets are memoized.** Building the token→byte-offset table (needed to map spans back to the source) decodes each token's bytes; `ModelDetector` caches `token id → byte length` so the warm `serve`/daemon request loop never re-decodes the same id twice.
- **Intra-op threads are capped.** When `--threads` is `0` (the default), the ONNX session is built with `DEFAULT_INTRA_THREADS` (2), not the core count. ONNX Runtime's intra-op pool deadlocks under the many sequential `run` calls that windowed detection makes once the thread count is high (≥~4 on a 6-core box); 1–2 threads run reliably and the model is small enough that more threads add little. Spinning is also disabled (`with_intra_op_spinning(false)`). Pass `--threads N` to override.

## Detection pipeline

`Detector::detect` (in `crates/id4pii/src/detect/mod.rs`) is a **hybrid** of two recognizers, and every entry point (`scan`, `anonymize`, `serve`, `daemon`, the extension) goes through it.

1. **Regex pre-pass** (`detect/regex.rs`). All patterns are OR-ed into a *single* compiled `Regex` and scanned in one linear, backtrack-free pass; each pattern is wrapped in exactly one named capture group (inner groups are all non-capturing) so the capture index maps directly to a category in `O(1)`. The engine is built once behind a `OnceLock` and shared process-wide. Patterns are RE2-compatible (the `regex` crate rejects look-around / back-references) and cover email, URL, phone, IBAN / card / SSN, dates, and a broad secret set (AWS/GitHub/Google/Slack/Stripe/OpenAI keys, JWTs, PEM private-key blocks, bearer tokens). Card numbers are gated by a **Luhn** check so a bad match never masks real text out from under the model. Regex hits are reported with confidence `1.0`.
2. **Masking** (`detect/mask.rs`). Each regex hit is replaced by a single-space sentinel, which shrinks the token count the transformer must process while preserving word boundaries. A segment map records the `gap → sentinel → gap …` layout so `map_start`/`map_end` translate model span offsets back to the original document in `O(log segments)`.
3. **Model** (`detect/model.rs`). The ONNX `ModelDetector` runs over the *masked* (shorter) text — see **Model** above for windowing/threads/offset-cache details.
4. **Merge** (`combine` in `detect/mod.rs`). Model spans are remapped to original coordinates; any model span overlapping a regex hit is dropped (regex wins); the union is sorted and same-category overlaps are merged by `merge_overlapping`.

If the regex pre-pass finds nothing, the model runs directly on the original text (no masking overhead). Set `ID4PII_REGEX=0` (or `false`) to disable the pre-pass and run the model over the full text — used for A/B latency comparison; `Detector::set_regex_enabled(false)` exposes the same path programmatically. The pure regex pre-pass is also exported as `id4pii::regex_scan(text)` for callers/benches that want the cheap structural matches without a model.

## CLI

```sh
id4pii scan "Email alice@acme.com or call 555-0142"
id4pii scan --redact --style block -f notes.txt
echo "ssn 123-45-6789" | id4pii scan --format text
```

`scan` reads text from the positional argument, `--file`, or stdin. Output is JSON spans by default (`--format text` for a table, `--redact` for masked text; `--style label|block|char`).

`--min-score <f32>` (default `0.0`, i.e. keep every detection) drops spans whose averaged token confidence is below the threshold — the precision/recall dial. It is a shared detection flag, so it also applies to `anonymize` and `serve` (all share `ModelArgs`) and the daemon (`--min-score`).

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

The model runs on one dedicated thread — the shared `DetectorService` (`detector_service.rs`), the same scheduler `batch` uses. `serve` spawns it with `Coalesce::UpTo(16)`: each handler submits its text on a `spawn_blocking` task and awaits a reply, and the thread drains all requests currently queued (up to `MAX_REQUEST_BATCH` = 16, sharing a `min_score`) into one batched inference. Requests that arrive while an inference is in flight form the next batch, so concurrent load is coalesced with no added latency for a lone request. `serve` submits at `min_score = 0.0` and each handler filters its own response, so requests with different thresholds still share a batch.

## Bulk / corpus ingestion

`id4pii batch` is the **throughput side-path** for ingesting and anonymizing arbitrarily large corpuses. It is *additive*: the small-text/latency path (`scan`, `anonymize`, `serve`) is byte-for-byte and timing unchanged — `batch` is a separate gear, not a rewrite.

```sh
id4pii batch --input corpus.jsonl --output safe.jsonl --vault-out vault.json   # anonymize, shared vault
id4pii batch --input docs/ --output safe/ --format files                        # mirror a directory tree
cat big.txt | id4pii batch --op scan > spans.jsonl                              # stream stdin → JSONL spans
```

- **Generic record stream.** Input is a stream of `(id, text)` records behind thin format adapters: `files` (recursive directory, one document per file, output mirrors the tree), `jsonl`/`ndjson` (one object per line, `--jsonl-field` selects the text field — default `text` — and the rest of the object is preserved on output), `lines` (one document per line, or `--delimiter` to split a single file on a custom separator), and `tsv` (`--tsv-column`). `--format auto` (default) picks by directory/extension; stdin defaults to `lines`. A leading UTF-8 BOM is stripped. Per-record read/parse errors are logged and skipped; the run continues.
- **`--op`** is `anonymize` (default), `scan` (emits `{"id", "spans"}` JSONL), or `redact` (`--style`). Only `anonymize` writes `--vault-out`.
- **One shared vault** for the whole run — the same real value maps to the same surrogate across every document, so `deanonymize` against the single `--vault-out` works corpus-wide. Surrogate lookup is kept O(1) by `IndexedVault` (a transient in-memory `HashMap`/`HashSet` over the `Vec`, **never serialized** — the on-disk vault format is unchanged); without it, `Vault::surrogate_for`'s linear scan makes a large run quadratic (measured ≈10× at 4 000 unique values, widening with size).
- **Three-stage pipeline, exactly one model thread.** A reader thread streams shards (bounded by `--shard-records`, default 256) and submits each to the shared `DetectorService` (spawned with `Coalesce::Off`, so each shard is its own inference and the streaming backpressure is preserved) → the main thread drains shards in submission order, pairing each shard's records with its reply channel, anonymizes into the shared vault, and streams output as documents complete (skip-on-error keeps finished work on a crash). Because replies are FIFO, the shared vault mints surrogates in record order. Bounded channels (`CHANNEL_DEPTH`) cap memory so a multi-GB corpus never loads whole. **Never** add concurrent inference threads or raise `--threads` to the core count — that reintroduces the ORT intra-op deadlock (see Model).
- **Same detection path as `scan`.** `batch` runs the *same* `Detector::detect_batch` as everything else (verified end-to-end: `batch --op scan` == per-text `scan`). It is not a separate code path — the throughput difference is the optional `--batch <n>` override (`set_batch_override`, pinning sequences-per-`run`; default is the adaptive `plan_batch` sizing). Window length-sorting (which makes GPU sequence bucketing pay off on large batches) is now always on, everywhere. The length-sort makes seeded surrogate assignment processing-order-dependent (surrogates stay valid and consistent, just not identical to unsorted order for a given `--seed`).

## Daemon — system-wide hotkey (Windows)

The daemon is a system-tray app that anonymizes PII in *any* application's text field — Claude Desktop, ChatGPT/Codex desktop, chatgpt.com/claude.ai in any browser, anything. It works through the Windows UI Automation accessibility layer (the same layer Grammarly uses), which sits *above* the network: the app itself sends the already-anonymized text, so there is no proxy, no certificate, and nothing for TLS pinning or anti-bot checks to detect.

It ships as **two binaries that share one codebase**:

| Binary | Subsystem | When |
|---|---|---|
| `id4pii.exe daemon` | console | dev iteration; piping stderr is useful |
| `id4pii-daemon.exe` | windows (no console) | the binary shipped to end users — autostart, Start Menu, post-install launch |

Both parse the same `DaemonArgs` (`crates/id4pii/src/daemon/mod.rs`) and call `daemon::run`. The GUI binary additionally calls `logging::init_daemon()` first, which writes to `data_root/logs/daemon.log` (rotating daily, 7 retained). The CLI binary writes log lines to stderr through the existing `tracing_subscriber::fmt()` setup.

Three global hotkeys, all operating on the currently focused editable field:

- **`Ctrl+Shift+A`** — anonymize the field in place. Detected PII gets swapped for surrogates; the vault learns the mapping.
- **`Ctrl+Shift+Z`** — restore the field in place using the vault.
- **`Ctrl+Shift+U`** — undo the last operation on this field (within a TTL).

All three rewrite the focused field directly — there is no popup or read-only overlay. The vault is shared across every app and every browser tab for the life of the process, so a real value anonymized once is restored consistently everywhere.

The tray menu carries: bridge status (informational), **Open log file**, **Open log folder**, and **Quit id4pii daemon**. The log items shell out via `cmd /C start "" <path>` so whatever the user has registered for `.log` (Notepad by default) and Explorer open the resolved `data_root/logs/...` path.

A single **vault** (persisted via DPAPI at `data_root/vault.bin`) is the id system that makes this reversible: every distinct real value is stored once with its category and a unique surrogate, so the same name always maps to the same surrogate and an email maps to its own — restoration is unambiguous in both directions. The vault is shared across every app and every tab, so a value anonymized in one place restores in another. Surrogates are generated procedurally (street addresses, URLs) or from large name pools, so the supply is effectively unbounded — fiction-safe phone numbers (`555-01xx`) are the one deliberately small set.

`--max-vault-entries <n>` (default `0` = unbounded) caps the vault as a safety ceiling for a daemon that runs for weeks. When exceeded, the oldest entries are evicted FIFO (`Vault::enforce_cap`) and the count is logged. **Eviction is lossy**: text previously anonymized with an evicted entry can no longer be restored, so set the cap well above the working set — it is a backstop, not a routine bound. (LRU would be safer but needs access tracking that the `#[serde(transparent)]` on-disk format deliberately doesn't carry.)

Notes: the daemon reads and writes via UI Automation, falling back to a clipboard select-all + copy/paste for rich editors (browser `contenteditable`, some Electron apps) that do not expose direct value access. The `daemon` subcommand is Windows only (macOS AX API and Linux AT-SPI are future work); the module is `cfg(windows)`-gated, so the workspace still builds on other platforms — only the subcommand is absent there.

### Engine architecture & testing

The engine (`crates/id4pii/src/daemon/engine.rs`) is a single-threaded state machine: it owns the vault and undo state, consumes `Command`s off a `sync_channel`, and publishes `Event`s to the `EventBus`. Its two external dependencies are injected behind traits so it can run without a model or a desktop:

- **`Detect`** — the PII detector. The real `Detector` implements it; tests inject a scripted fake.
- **`Field`** — the focused-field IO (read / write / targeted substitution). `UiaField` wraps the UI Automation backend in production; tests inject an in-memory fake.

`Engine::load` wires up the real backends; `Engine::with_components` takes the trait objects plus an `EngineConfig { min_score, max_vault_entries }` and is what the tests call (with a `MemoryStore` vault). The integration tests in `engine.rs` drive each `Command` and assert the resulting `Event`s, `BridgeReply`, undo behaviour, and the vault cap — covering the state machine end-to-end with no ONNX model. Because the whole `daemon` module is `cfg(windows)`, these tests run on the **Windows CI job** (see Development loop).

## Browser extension

The MV3 extension in `extension/` is `id4pii` for LLM sites: ChatGPT, Claude, Gemini (and any others wired into the `content_scripts.matches` allowlist). When you submit, it pulls your text out of the input, hands it to the local `daemon` process over a loopback WebSocket, replaces it with the anonymized version, then submits. When the assistant's reply streams in, surrogates are auto-restored to real values in the rendered text — a `MutationObserver` driving the same pure `deanonymize` string-replace used by the CLI.

It's **part of the daemon, not a parallel anonymizer**: the extension owns no model, no detector, and no vault. All of that lives in the existing engine — the extension is just another command source on the same bus, alongside the global hotkey. The vault is shared: a name anonymized via `Ctrl+Shift+A` in Notepad reuses its surrogate when you type it into chatgpt.com.

The extension shares its assets with the desktop daemon: the toolbar icon, extension icons, and the lock-close/lock-open animation frames all live in the repo-root `assets/` directory. A small sync script copies them into `extension/assets/` (which is gitignored, treated as a build artifact):

```powershell
.\scripts\sync-extension-assets.ps1
```

Run it whenever `assets/icon-*.png` or `assets/lock_frames/*.png` change. After running it once, the extension can be loaded unpacked from `extension/`.

### Unpacked dev setup

Production users install the extension from the Chrome Web Store and the engine via the EXE installer. For development against an unpublished extension build:

1. Run `cargo run -p id4pii -- daemon --dev-extensions` (console; live logs on stderr) or `cargo run --bin id4pii-daemon -- --dev-extensions` (no console; logs to file). The bridge listens on `ws://127.0.0.1:7878/ws`; `--dev-extensions` relaxes the origin check so any locally-loaded `chrome-extension://…` can connect. Production builds pin a single published Web Store ID from `.env`.
2. In Chrome: `chrome://extensions` → **Developer mode** → **Load unpacked** → select the `extension/` directory.
3. The toolbar icon shows a solid green badge when connected, `!` when the bridge is unreachable.

### Triggers

- **On submit** (Enter or send-button click) on a whitelisted host: the extension intercepts, anonymizes via the daemon, then re-fires the submit with the anonymized text. Same lock-close animation as the desktop daemon.
- **`Ctrl+Shift+A`** in any input on a whitelisted host: anonymize the input in place without submitting, so you can review.
- **Auto-restore**: as the assistant streams text into the page, surrogates in plain text nodes are swapped back. Code blocks (`<code>`, `<pre>`) are skipped to keep copy-paste intact.
- **File uploads** on a whitelisted host are intercepted in the same fetch/XHR patch (e.g. claude.ai fires both `…/upload-file` and `…/convert_document` with the real bytes — both are caught). The file is anonymized **in its original format and uploaded as the same file type** — a `.docx` stays a `.docx`, never a `.txt`.

### File uploads — same-shape anonymization

`extension/src/main/core.js` `anonymizeUpload(reqId, formData)` owns the routing for a multipart upload's file part:

- **`.docx` / `.pptx` / `.xlsx` / `.pdf`** → the bytes (base64) go to the daemon bridge (`anonymize_file`), which returns **anonymized bytes of the same type**. The extension rebuilds the `File` with the **original name and MIME**, so the site (and the model) only ever see the same-shape, surrogate-bearing document. Results are cached by `name|size|lastModified` so the two requests one upload triggers don't double-process.
- **Text formats** (`.txt/.csv/.md/.json/source code/…` by extension or MIME) → anonymized in-JS via the existing text path, name preserved.
- **Everything else** (images, archives, …) → passed through untouched, no lock.

Bridge/engine path (`crates/id4pii/src/extract.rs`, `bridge.rs`, `engine.rs`):

1. `extract::plan(bytes, filename)` parses the document and returns a `DocPlan` whose `text` is the **exact concatenation** of every editable text node across all covered parts (no trimming — offsets must stay faithful), plus the structural map needed to write it back. A single offset-aligned traversal (`process_part`, shared by the collect and rewrite passes so they can never drift) handles two node modes: **`Tagged`** anonymizes the schema's text tags only — `w:t`/`w:p` (docx body, headers/footers, comments, footnotes, endnotes), `a:t`/`a:p` (pptx slides + notes slides), `t`/`si` (xlsx `sharedStrings.xml`); **`AllText`** anonymizes *every* text node regardless of tag, used for parts with irregular/embedded text — `*/charts/chart*.xml` (titles + cached category/series values), `*/diagrams/data*.xml` (SmartArt), and `docProps/custom.xml`. In **both** modes it also anonymizes image **alt-text attributes** (`descr`/`title`) by treating each attribute value as a node and rebuilding the element. PDF text comes from `pdf-extract`.
2. The bridge sends `DocPlan.text` to the engine via **`Command::AnonymizeSpans`**, a sibling of `AnonymizeText` that returns byte-offset **`Placement`s** (span + surrogate) instead of a spliced string — the in-place splice needs offsets. It shares the same detect → vault → save → `VaultDelta` core (`Engine::anonymize_core`), so the vault learns the mapping and the page can auto-restore surrogates in the reply.
3. `DocPlan.finish(&placements)` writes the document back:
   - **OOXML**: re-streams each part with `quick-xml`, splicing surrogates into `w:t`/`a:t`/`t` nodes (a span crossing run boundaries gets the surrogate in the node where it starts; the overlapped tail in later nodes is deleted), copies every other zip entry **verbatim** (`raw_copy_file`, preserving compression), and **scrubs author metadata** (`dc:creator`, `cp:lastModifiedBy`, `Manager`, `Company` in `docProps/core.xml`/`app.xml`) — those never pass through the body, so uploading the whole container would otherwise leak the real author.
   - **PDF**: regenerates a **new text-only PDF** from the anonymized text (Helvetica/WinAnsi, hand-written PDF writer, round-trip-tested via `pdf-extract`). This keeps the `.pdf` file type but **does not preserve the original visual layout** — PDF stores positioned glyphs, not flowing text, so true in-place editing isn't feasible in this stack. WinAnsi is Latin-1 only, so **non-Latin text (CJK, Cyrillic, …) is replaced with `?`** — content loss, not just layout loss; embed a font if that matters.
4. **Fail closed**: any plan/anonymize/rewrite error → the bridge returns `error`, the extension throws an `id4piiBlock`, and `patchFetch` returns a synthetic 400 so the real bytes are never sent (red lock). Even a no-PII office doc is still re-emitted (metadata scrubbed); even a no-PII PDF is regenerated (drops original PDF metadata).

**Known passthrough surfaces (not anonymized).** Raster **images embedded in documents are passed through untouched** — text baked into a logo/screenshot/scanned page is *not* anonymized (the model may still read it). Likewise an **image-only / no-extractable-text** document is re-emitted and uploaded anyway (a scanned PDF becomes a blank text PDF; an image-only Office doc ships its images verbatim) rather than blocked. These are deliberate usability choices (most real docs embed a logo); if a stricter posture is wanted, block on embedded-image presence / empty-text instead.

### Failure modes

- Bridge down → submit interception fails open (passes through with a `console.warn`), so you never get stuck unable to send.
- Site interception is keyed on outbound chat-completion URLs matched against each adapter's `chatPatterns` array. One adapter per site lives in `extension/src/adapters/` (`chatgpt.js`, `claude.js`, `gemini.js`); the shared core in `extension/src/main/core.js` owns fetch/XHR patching, response streaming, vault IPC, and DOM restore. ChatGPT and Claude reuse `core.helpers.anonymizeJsonBody` (generic JSON walker); Gemini owns its own `f.req` form-encoded extractor. To add a new site: drop a new file in `extension/src/adapters/`, register it via `window.__id4pii_main.registerAdapter(...)`, and add the script + host match to `manifest.json`.

### Debugging

Both sides of the extension log silently by default. To turn on verbose tracing:

- **Daemon (Rust)**: set `RUST_LOG=id4pii=debug,ort=warn` before starting the daemon. Every WebSocket frame, vault lock, and engine step is logged with a `req_id` field so you can grep one request end to end.
- **Extension (browser)**: in the extension's service-worker DevTools (find it at `chrome://extensions` → id4pii → **Inspect views: service worker**), type `id4pii.debug(true)` in the console. The setting persists in `chrome.storage.local` and applies to background, content script, and in-page main world. Disable with `id4pii.debug(false)`.

Lines have a uniform format: `[id4pii:<component>] <event> key=value …`. Components are `bg` (service worker), `iso` (content script), `main` (in-page), `bridge` and `engine` (Rust). Each fetch interception generates an 8-char `reqId` that is threaded through every hop in both directions, so you can follow a single message through `main → iso → bg → bridge → engine` and back.

Privacy invariant: no message text, response body, or vault entry is ever logged at any level — only lengths, counts, kinds, durations, and the request ID.

### Security

- Bridge binds `127.0.0.1` only.
- WebSocket handshake rejects unless `Origin` matches `chrome-extension://<published-id>` (baked in from `ID4PII_PUBLISHED_EXTENSION_ID` in `.env`). Regular web pages on `localhost` can't open a session, and neither can other browser extensions.
- `--dev-extensions` widens the allowlist to any `chrome-extension://`, `moz-extension://`, or `safari-web-extension://` origin. Use this only for unpacked dev loads.
- `--no-bridge` disables the bridge entirely.

## Install / uninstall / doctor

The CLI binary (`id4pii.exe`) ships three configuration subcommands that the Inno installer calls:

- `id4pii install --with-model --register-extension <id> --autostart` — fetches the model, writes the Chrome external-extension registry key (`HKLM\…\Chrome\Extensions\<id>` with the Web Store `update_url`), and registers `HKCU\…\Run\id4pii` to launch the *daemon* binary (sibling `id4pii-daemon.exe`) on login.
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

The tokenizer is embedded and loads in ~85 ms (in parallel with the ONNX session), so a cold CLI run is dominated by model load and one inference pass. For repeated or latency-sensitive use, `serve` still wins decisively — the model loads once and each request is just inference. Set `RUST_LOG=id4pii=debug` to see per-phase load timings.

**The regex pre-pass is the main detection speedup.** Because the model's attention cost grows super-linearly with token count, masking the regex-found PII out of the text before inference both removes redundant work and shrinks the sequence. Measured on a PII-dense ~370-token document (`scan -f`, same binary/model, `RUST_LOG=id4pii=debug`, comparing the `inference complete tokens=… elapsed=…` line):

| | Tokens fed to model | Warm inference |
|---|---|---|
| `ID4PII_REGEX=0` (model only) | 366 | ~1.27 s |
| default (hybrid) | 191 | ~0.76 s |

That is **48% fewer tokens → ~40% lower inference time**, and detection is *broader* (the regex pre-pass caught an extra phone and secret the model missed: 22 vs 20 spans). The win grows with PII density and input length because of the quadratic attention term. The pre-pass itself is single-digit microseconds (`regex_scan_pii_heavy` ≈ 6.5 µs) — negligible next to inference.

Long inputs no longer blow up latency either: windowed detection (see Model) keeps inference cost linear in token count instead of the model's quadratic attention.

`deanonymize` buckets vault pairs by first byte into a flat 256-slot table (plain array indexing, no per-position hashing), so restore is roughly linear in text length even as the shared daemon vault grows large.

## Benchmark & evaluation suite

Speed and correctness are measured against a committed, labelled corpus rather than ad-hoc strings — see `crates/id4pii/data/README.md` (1500 synthetic examples from Microsoft Presidio-research, MIT, byte-offset span labels, regenerate with `python scripts/fetch-pii-dataset.py`). The loader (`id4pii::eval::load_tsv`) is a lean single-pass TSV reader; scoring (`eval::evaluate` / `Report`) is type-aware overlap precision/recall/F1 that treats out-of-schema `other` spans as don't-care so the engine is not penalized for entity types it does not target.

**Speed** — one criterion suite, one group per engine area, all run over the corpus:

```sh
cargo bench -p id4pii        # benches/engine.rs
#   parse/load_tsv            — TSV loader throughput (MB/s)
#   detect_regex/…_corpus     — regex pre-pass over every example (MB/s)
#   anonymize/…_corpus        — anonymize_with_subs over gold spans
#   deanonymize/…_corpus      — restore the whole corpus against one shared vault
#   scaling/…                 — fixed-size synthetic regression guards
#   vault_scaling/…           — indexed vs plain vault on 4000 unique inserts (anonymize-at-scale)
```

**Correctness + model A/B** — an example that prints per-category P/R/F1 and, when the model is present, compares model-only vs hybrid on accuracy and wall-clock:

```sh
cargo run --release --example evaluate -p id4pii
```

It always reports the regex pre-pass scores and the token reduction masking buys the model; with the model present it adds model-only and hybrid tables plus a speed/F1 summary. A model-free CI test (`crates/id4pii/tests/regex_eval.rs`) asserts the regex pre-pass F1 on its target categories stays above a floor.

Representative results (Ryzen 5 9600X, `model_q4`):

- **Regex pre-pass** over the full 1500-example corpus: 99.4% precision overall, 100% F1 on email/URL, 95.3% on account numbers — at ~98k examples/s. (Recall is "low" only because person/address, 75% of gold spans, have no regex coverage by design.)
- **Hybrid vs model-only** (sample): overall F1 **81.4% → 84.7%** and detection **1.30× faster** — the regex pre-pass is both more accurate (exact account/phone/URL matching) *and* cheaper. On PII-dense input the speed gap is far larger (≈40%, see above); on this general corpus regex masks ~6% of tokens, so the corpus-wide speedup is smaller but still net-positive with a correctness gain.

## Development loop

```sh
cargo test --workspace
cargo fmt --all
cargo clippy --all-targets
cargo bench -p id4pii        # optional: engine benchmark suite over the labelled corpus
```

Building the `id4pii` crate requires a `.env` at the repo root (see `CONTRIBUTING.md`); the merged crate is built from source via `.env`, so there is no `.env`-free build. CI provisions it with `cp .env.example .env`.

CI (`.github/workflows/ci.yml`) runs fmt + clippy + check + test with `RUSTFLAGS=-D warnings` (clippy `pedantic`/`unwrap_used`/`expect_used` are warns promoted to hard errors), on a **matrix of `ubuntu-latest` and `windows-latest`**. The Windows job is what actually compiles the `cfg(windows)` daemon and runs its engine tests; the Linux job covers the cross-platform surface. Each job provisions a build `.env` with `cp .env.example .env` (the placeholder values satisfy `build.rs`; no secrets are needed for checks).
