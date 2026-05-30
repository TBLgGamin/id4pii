# id4pii — internals & developer guide

The technical reference: how the system is built, where each piece lives, and
the load-bearing decisions behind it. The repo-root `README.md` is the
promotional landing page for end users; build/contributor setup lives in
`CONTRIBUTING.md`.

> Altitude note: this doc is a country map, not a street atlas. It names files
> and types but avoids line-level links that rot — symbol-search for specifics.
> The **Invariants** and **Decisions** sections are the highest-value part; read
> them before "optimizing" anything.

## What it is

A local PII layer for text — **detect, redact, and reversibly anonymize** —
exposed as a CLI, a local HTTP API, a system-tray daemon, and a Chrome MV3
extension. Inference is [OpenAI Privacy Filter](https://huggingface.co/openai/privacy-filter)
run locally through ONNX Runtime; no data leaves the machine.

It tags eight categories: `account_number`, `private_address`, `private_date`,
`private_email`, `private_person`, `private_phone`, `private_url`, `secret`.

The headline use is to sit **between your app and an LLM**: swap real PII for
realistic fake surrogates, send the harmless text to the model, then swap the
real values back into the reply — the model never sees real data.

## Architecture in one breath

One I/O-free **engine** (detection + anonymization + vault) with a single smart
entry point, wrapped by thin **surfaces** (CLI, HTTP, corpus, daemon, extension)
that only marshal transport in/out and call the same engine. Everything is one
crate now: `crates/id4pii` (library + two binaries). The engine never knows
which surface is driving it.

## Codemap

```
crates/id4pii/
  src/
    detect/                  the hybrid recognizer
      regex.rs               compiled-regex pre-pass (one OR-ed engine, Luhn-gated cards)
      mask.rs                sentinel-masking + offset segment map (original <-> masked)
      model.rs               ONNX session, tokenizer, windowing, adaptive batching, decode
      mod.rs                 Detector: detect / detect_batch, regex+model fusion (combine/merge)
    anonymize.rs             surrogate generation, Vault + IndexedVault, deanonymize, Rng
    redact.rs                label / block / char masking (irreversible)
    labels.rs eval.rs        category map; labelled-corpus precision/recall/F1 scoring
    paths.rs model_dir.rs model_fetch.rs error.rs   one path source-of-truth, model fetch, errors
    detector_service.rs      ONE Detector on ONE thread; serve + corpus share it
    cli.rs main.rs           subcommand dispatch (scan/anonymize/deanonymize/corpus/serve/daemon/…)
    serve.rs                 HTTP API over the shared service (coalescing)
    corpus.rs                streaming corpus pipeline over the shared service (FIFO)
    document.rs              same-shape file anonymization (OOXML/PDF plan -> rewrite)
    model_setup.rs           load_detector() = ensure_model + Detector::load (used everywhere)
    install.rs logging.rs progress.rs
    daemon/                  the system-tray app (cfg(windows))
      mod.rs                 tray, hotkeys, event loop, subsystem wiring
      engine.rs              single-threaded Command->Event state machine (owns vault + undo)
      bridge.rs              loopback WebSocket server for the extension
      bus.rs store.rs automation.rs feedback.rs   EventBus, DPAPI vault store, UIA IO, overlay
    bin/id4pii_daemon.rs     the shipped GUI binary (no console)
  benches/ examples/ tests/ data/   engine benches, evaluate example, regex-floor test, corpus
extension/                   MV3 Chrome extension (adapters/ + main/core.js)
installer/                   Inno Setup script + wizard images
```

On-disk layout, all under `paths::data_root()` (=`%LOCALAPPDATA%\id4pii\` on
Windows, XDG/Library equivalents elsewhere) so `id4pii uninstall` removes it
atomically:

```
data_root/
├── model/   config.json + onnx/model_q4.onnx{,_data}
├── logs/    daemon.log (rolling daily, 7 retained)
└── vault.bin   DPAPI-encrypted shared vault
```

## Detection pipeline

`Detector` (`detect/mod.rs`) is a **hybrid** of two recognizers; every surface
goes through it unchanged.

1. **Regex pre-pass** (`regex.rs`) — all patterns OR-ed into one compiled
   backtrack-free `Regex`, each in exactly one named group so the capture index
   maps to a category in O(1). Built once behind a `OnceLock`. Covers
   email/URL/phone/IBAN/card/SSN/dates and a broad secret set (AWS/GitHub/Google/
   Slack/Stripe/OpenAI keys, JWTs, PEM blocks, bearer tokens). Cards are
   **Luhn**-gated so a bad match never masks real text from the model. Hits are
   reported with confidence `1.0`.
2. **Masking** (`mask.rs`) — each hit becomes a single-space sentinel, shrinking
   the token count the transformer sees while preserving word boundaries. A
   segment map translates model span offsets back to the original document.
3. **Model** (`model.rs`) — the ONNX recognizer runs on the *masked* (shorter)
   text. See **Inference** below.
4. **Merge** (`combine`/`merge_overlapping` in `mod.rs`) — model spans are
   remapped to original coordinates, any overlapping a regex hit is dropped
   (regex wins), and same-category overlaps are unioned.

Set `ID4PII_REGEX=0` to run the model on the full text (A/B); `set_regex_enabled(false)`
does the same programmatically. `regex_scan(text)` exposes the pre-pass alone.

### Inference (the one detection path)

There are exactly two public methods — `Detector::detect(text)` and
`Detector::detect_batch(texts)` — and `detect` is just `detect_batch` of one.
`detect_batch` is the single smart path used by `scan`, `serve`, `corpus`, and
the daemon:

- **Windowing.** Inputs ≤ `DETECT_WINDOW` (1024) tokens are one window
  (byte-identical to a whole-text run); longer inputs split into overlapping
  windows (`DETECT_OVERLAP` = 128) stitched by `merge_overlapping`.
- **Adaptive batching.** All windows are length-sorted, then chunked by a token
  budget (`plan_batch`: `TOKEN_BUDGET` 4096 ÷ padded-seq-len, capped at
  `CPU_BATCH_CAP` 16 / `GPU_WINDOW_BATCH` 32). Long windows run few-at-a-time
  (bounding the `heads × seq × seq` attention tensor — the old fixed `MAX_BATCH`=4
  behaviour at 1024 tokens); short ones pack densely to amortise the fixed
  per-`run` cost. `set_batch_override(Some(n))` (the `corpus --batch` flag) pins
  it; the default is adaptive. Output is independent of window order and batch
  composition (padding is masked, each row decodes in isolation).
- **Execution providers.** `Detector::load` registers any compiled-in GPU
  provider (`--features directml`/`cuda`) then the CPU provider, each non-fatal,
  so a machine without the runtime falls back silently. `ID4PII_CPU=1` forces CPU.
- **GPU sequence bucketing.** DirectML/TensorRT recompile per input shape, so on
  GPU sequences pad up to a fixed bucket (`SEQ_BUCKETS` 64/128/256/512/1024). On
  CPU this is skipped (no recompile to avoid).
- **Memoized token offsets.** `model.rs` caches token-id → byte-length so the
  warm request loop never re-decodes the same id.

## Anonymize / deanonymize (the LLM shield)

`anonymize` replaces each span with a same-category fake surrogate and emits a
**vault** (fake→real). The same real value always maps to the same surrogate, so
the text stays coherent; `deanonymize` restores it. Surrogates are fiction-safe
(two-word sci-fi names, `555-01xx` fiction phone numbers, `example.com` URLs);
restoration is exact string-matching, bucketed by first byte for near-linear
restore even on a large shared vault.

The **vault** is the id system that makes everything reversible: each distinct
real value is stored once with its category and a unique surrogate. `Vault` is
the on-disk source of truth (linear scan, fine for small vaults); `IndexedVault`
is a transient O(1) in-memory index built for corpus/daemon scale and **never
serialized**, so the on-disk format is unchanged.

## Surfaces

All five are thin adapters over the same engine.

- **CLI** (`cli.rs`): `scan` / `anonymize` / `deanonymize` / `corpus` / `serve` /
  `daemon` / `install` / `uninstall` / `doctor`. Shared `ModelArgs`
  (`--model`/`--model-file`/`--threads`/`--min-score`).
- **HTTP** (`serve.rs`): `GET /health`, `POST /scan|/anonymize|/deanonymize`.
  Runs the model on the shared `DetectorService` with `Coalesce::UpTo(16)` —
  concurrent requests coalesce into one batched run with no added latency for a
  lone request. Submits at `min_score = 0.0` and filters per request.
- **Corpus** (`corpus.rs`): a streaming reader → `DetectorService` (`Coalesce::Off`)
  → writer pipeline over arbitrarily large inputs (files / jsonl / lines / tsv).
  One shared vault across the run; bounded channels cap memory.
- **Daemon** (`daemon/`): a single-threaded `Command`→`Event` state machine that
  anonymizes the focused field of *any* app via UI Automation, plus three global
  hotkeys (Ctrl+Shift+A/Z/U = anonymize/restore/undo). `Detect` and `Field` are
  injected behind traits so the engine tests run with no model and no desktop.
- **Extension** (`extension/`): MV3 for ChatGPT/Claude/Gemini. It owns no model,
  no detector, no vault — it is just another command source on the daemon's bus
  over a loopback WebSocket. One adapter per site under `extension/src/adapters/`;
  the shared `main/core.js` owns fetch/XHR patching, streaming restore, and the
  file-upload path (`anonymize_file` → `document.rs` plan/rewrite, same file type
  in, same out).

## Invariants — do not break these

These are mostly *absences* — rules you cannot infer by reading code. Each has a
one-line reason.

- **Exactly one model inference thread per process; never raise `--threads` to
  the core count.** ONNX Runtime's intra-op pool deadlocks under the many
  sequential `run` calls windowed detection makes once threads are high (≥~4 on a
  6-core box). Default is `DEFAULT_INTRA_THREADS` = 2 with spinning off. The
  `DetectorService` is the single enforcement point.
- **Large inputs must be windowed.** A single massive `run` blows the
  `heads × seq × seq` attention tensor to tens of GB, and ORT's arena retains that
  peak for the process lifetime (the documented 20 GB+ failure). Preventing the
  giant run matters more than freeing after it.
- **The small-text path is byte-for-byte unchanged by batching.** `corpus --op scan`
  must equal per-text `scan`; keep that cross-surface equality test.
- **Restoration is exact string match.** If an LLM rewrites a surrogate (truncates
  a name, splits an email) that fragment won't restore; if a surrogate coincides
  with unrelated text it gets swapped too. Person surrogates are always two words
  to make both rare. Don't "improve" restore into fuzzy matching.
- **The extension owns no engine state.** Model, detector, and vault live only in
  the daemon; the extension is a command source. The shared vault is what lets a
  name anonymized in Notepad restore on chatgpt.com.
- **GPU sequence bucketing only when a GPU provider is active.** On CPU there is
  no per-shape recompile to avoid, so padding would only waste compute.
- **On-disk vault format and the WebSocket wire tags are byte-compatible.** There
  is no protocol version negotiation; renaming a serde tag silently breaks a
  shipped extension. Rename Rust identifiers freely, wire strings never.
- **Fail closed on the security path, open on the convenience path.** A file-upload
  plan/anonymize/rewrite error returns `error` → synthetic 400; real bytes are
  never sent. A bridge-down chat submit passes through with a `console.warn` so
  you're never stuck. The bridge binds `127.0.0.1` only and pins the published
  extension `Origin` (relaxed only by `--dev-extensions`).
- **Never log message text, response bodies, or vault entries** — only lengths,
  counts, kinds, durations, and the `reqId`. A derived `Debug` on `Vault` would
  leak PII into logs.
- **Coalesced detector jobs share a threshold by construction** (`serve` submits
  0.0 and filters afterwards; `corpus` uses `Coalesce::Off`). Don't submit mixed
  `min_score`s under `Coalesce::UpTo`.

## Decisions & tradeoffs (and roads not taken)

- **Hybrid regex+model, not model-only.** Masking the structural PII out before
  inference removes redundant work and shrinks the sequence; the pre-pass is also
  *more* accurate on exact account/phone/URL matches. Net: faster and broader.
- **Adaptive token-budget batching, not a fixed batch size.** Subsumes the old
  per-request `MAX_BATCH`=4 and corpus `--batch` defaults; bounds memory on long
  windows while packing short ones.
- **`serve` coalesces, `corpus` streams.** Coalescing is a latency win for
  concurrent requests; the corpus path deliberately does *not* coalesce, because
  its shard-at-a-time backpressure is what bounds memory on a multi-GB corpus and
  keeps shared-vault surrogate minting in record order.
- **I/O binding investigated and skipped** — the bottleneck is compute, not host
  allocation, so reusing buffers wouldn't move it. **No warm-up step** — a warm-up
  `run` doesn't speed the next (cost is per-`run`, not one-time).
- **CPU is the default build.** An integrated GPU stays slower here (dispatch +
  host↔device copy exceeds the tiny compute); a discrete CUDA card and/or larger
  batches are where GPU wins.
- **FIFO vault cap, not LRU.** LRU needs access tracking the `#[serde(transparent)]`
  on-disk format deliberately doesn't carry; eviction is a lossy backstop, set the
  cap well above the working set.
- **PDF is regenerated as a text-only PDF**, not edited in place (PDF stores
  positioned glyphs, not flowing text). Keeps the `.pdf` type but drops the
  original layout; WinAnsi is Latin-1 only, so non-Latin text becomes `?`.
- **Rejected this pass:** `panic = "abort"` (would break the daemon's
  panic-recovery and its test), fat-LTO (marginal vs ONNX-dominated latency, large
  build cost), and `FxHashMap`/`SmallVec` micro-opts (a new dependency for a few
  percent behind a 150–350 ms/`run` model is not worth it under a "no unnecessary
  things" bar). **Deferred:** newtypes for the masked-vs-original offset spaces
  (`OrigByte`/`MaskByte`) — a real safety win against silent mis-redaction, but
  invasive, and the mask/merge offset code is well-tested. Worth doing if that
  code churns again.
- **Native messaging was rejected** in favour of the loopback WebSocket bridge.

## Performance

Always benchmark the optimized build (`cargo build --release`). The release
profile is `lto = "thin"`, `codegen-units = 1`, `strip = true`.

Measured on a Ryzen 5 9600X with the `model_q4` variant (pin numbers to the
machine + model they were taken on; these predate the unification refactor and
have not been re-measured):

| Path | Latency |
|---|---|
| CLI one-shot (cold) | ~195 ms |
| `serve` request (warm) | ~33 ms |

Cold CLI is dominated by model load + one inference (the embedded tokenizer
loads in ~85 ms, in parallel). `serve` wins by loading the session once.

The **regex pre-pass is the main detection speedup** because attention cost grows
super-linearly with tokens. On a PII-dense ~370-token doc: model-only fed 366
tokens at ~1.27 s warm inference; hybrid fed 191 tokens at ~0.76 s — **48% fewer
tokens → ~40% lower inference time**, and *broader* (it caught an extra phone and
secret). Over the general 1500-example corpus the pre-pass scores 99.4% precision
(100% F1 on email/URL, 95.3% on account numbers) at ~98k examples/s, and hybrid
beats model-only on F1 (81.4% → 84.7%) while running ~1.30× faster.

## Debugging / observability

A request flows `main → iso → bg → bridge → engine` and back, threaded by an
8-char `reqId` so you can grep one message end to end.

- **Rust side:** `RUST_LOG=id4pii=debug,ort=warn`. Every WS frame, vault lock, and
  engine step logs with `req_id`. Per-phase load timings at `id4pii=debug`.
- **Extension side:** in the service-worker DevTools (`chrome://extensions` →
  id4pii → Inspect views: service worker), run `id4pii.debug(true)` (persists in
  `chrome.storage.local`). Components are `bg` / `iso` / `main` / `bridge` /
  `engine`; lines are `[id4pii:<component>] <event> key=value …`.

Privacy invariant holds at every level: lengths/counts/kinds/durations/reqId only.

## Build, test, CI

```sh
cargo test --workspace      # needs .env (the merged crate builds from .env)
cargo fmt --all
cargo clippy --all-targets  # workspace lints are strict: pedantic + unwrap/expect, -D warnings in CI
cargo bench -p id4pii       # engine hot-path benches over the labelled corpus
cargo run --release --example evaluate -p id4pii   # model-present P/R/F1 + model-only-vs-hybrid A/B
```

Building requires a `.env` at the repo root (placeholders from `.env.example`
satisfy `build.rs`; see `CONTRIBUTING.md`). CI runs fmt + clippy + check + test
on `ubuntu-latest` and `windows-latest` with `RUSTFLAGS=-D warnings`; the Windows
job is the one that compiles the `cfg(windows)` daemon and runs its engine
state-machine tests (which need no ONNX model — `Detect`/`Field`/vault are faked).

The engine bench suite (`benches/engine.rs`) covers the model-free hot paths
(regex scan, anonymize, deanonymize, vault scaling) over the committed corpus;
model latency is exercised by the `evaluate` example rather than a bench, because
a model-loading bench would not run in CI.

## Install / uninstall / doctor

The CLI binary backs the installer:

- `id4pii install --with-model --register-extension <id> --autostart` — fetches
  the model, writes the Chrome external-extension registry key, and registers
  autostart (`HKCU\…\Run\id4pii`, value name stays `id4pii`) pointing at the
  sibling `id4pii-daemon.exe`.
- `id4pii uninstall [--keep-model]` — removes `data_root`, the Run entry, and the
  Chrome key.
- `id4pii doctor [--extension-id <id>]` — prints a JSON health report.

The shipped daemon binary is `id4pii-daemon.exe` (Windows GUI subsystem, no
console); `id4pii.exe daemon` is the console-attached dev form of the same code.
