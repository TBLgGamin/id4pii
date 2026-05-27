# id4pii

A local PII layer for text — detect, redact, and **reversibly anonymize** —
as a CLI and HTTP API, powered by
[OpenAI Privacy Filter](https://huggingface.co/openai/privacy-filter) running
locally through ONNX Runtime. No data leaves the machine.

It tags eight categories: `account_number`, `private_address`, `private_date`,
`private_email`, `private_person`, `private_phone`, `private_url`, `secret`.

Its main use: sit **between your app and an LLM**. Swap real PII for realistic
fake surrogates before the call, send the harmless text to the model, then swap
the real values back into the response — the model never sees real data.

## Layout

```
crates/
  core/   id4pii-core  — ONNX inference, span decoding, redaction, anonymization
  app/    id4pii-app   — `id4pii` binary: scan / anonymize / deanonymize / serve / guard
```

## Model

The model weights are not bundled. Download `openai/privacy-filter` into a
directory — `scan`/`serve` need an ONNX file (plus its `.onnx_data` shards) and
`config.json`:

```sh
pip install -U "huggingface_hub[cli]"
hf download openai/privacy-filter config.json --local-dir model
hf download openai/privacy-filter --include "onnx/model_q4.onnx*" --local-dir model
```

This matches the defaults — `--model ./model` (or the `ID4PII_MODEL` env var)
and `--model-file onnx/model_q4.onnx`. Swap in `onnx/model.onnx` for the
full-precision variant.

The tokenizer is **not** downloaded: id4pii embeds the `o200k_base` vocab via
`tiktoken-rs`, which produces token ids identical to privacy-filter's own
tokenizer (guarded by a regression test in `crates/core/src/detector.rs`).

id4pii feeds the model `input_ids` and `attention_mask`. If a run fails with an
ONNX error naming a missing required input, that input name needs wiring into
`crates/core/src/detector.rs`.

## CLI

```sh
cargo run -p id4pii-app -- scan "Email alice@acme.com or call 555-0142"
cargo run -p id4pii-app -- scan --redact --style block -f notes.txt
echo "ssn 123-45-6789" | cargo run -p id4pii-app -- scan --format text
```

`scan` reads text from the positional argument, `--file`, or stdin. Output is
JSON spans by default (`--format text` for a table, `--redact` for masked text;
`--style label|block|char`).

## Anonymize / deanonymize (the LLM shield)

`anonymize` replaces each detected PII span with a realistic fake surrogate of
the same category and emits a **vault** — the fake → real mapping. The same
real value always maps to the same surrogate, so the text stays coherent.
`deanonymize` uses the vault to restore the real values in whatever comes back.

```sh
# 1. anonymize, keeping the vault in a file; only the safe text is printed
id4pii anonymize --vault-out vault.json "I'm Sarah Connor, sarah@skynet.com" > safe.txt

# 2. send safe.txt to your LLM ... get a reply that mentions the fakes

# 3. restore the real values from the reply
id4pii deanonymize --vault vault.json "$(cat llm_reply.txt)"
```

Without `--vault-out`, `anonymize` prints one JSON object
`{"anonymized": "...", "vault": [...]}`. Surrogates are random per run; pass
`--seed <n>` for reproducible output. `deanonymize` needs no model.

Surrogates lean fiction-safe and nerdy — full `First Last` names from a
sci-fi/hacker pool, phone numbers in the `555-01xx` range reserved for fiction,
`example.com` URLs. Restoration is string-matching, with two consequences worth
knowing: if the LLM rewrites a surrogate (truncates a name, splits an email)
that fragment won't be restored; and if a surrogate happens to appear in
unrelated parts of the reply, it will be swapped too. Multi-word surrogates make
both rare, which is why person names are always two words.

## HTTP API

```sh
cargo run -p id4pii-app -- serve --addr 127.0.0.1:8080
```

- `GET /health` → `ok`
- `POST /scan` — `{"text": "...", "redact": true}` →
  `{"spans": [...], "redacted": "..."}`
- `POST /anonymize` — `{"text": "...", "seed": 1337}` (seed optional) →
  `{"anonymized": "...", "vault": [...]}`
- `POST /deanonymize` — `{"text": "...", "vault": [...]}` → `{"text": "..."}`

## Guard — system-wide hotkey (Windows)

`id4pii guard` is a tray app that anonymizes PII in *any* application's text
field — Claude Desktop, ChatGPT/Codex desktop, chatgpt.com/claude.ai in any
browser, anything. It works through the Windows UI Automation accessibility
layer (the same layer Grammarly uses), which sits *above* the network: the app
itself sends the already-anonymized text, so there is no proxy, no certificate,
and nothing for TLS pinning or anti-bot checks to detect.

```sh
cargo run --release -p id4pii-app -- guard
```

It runs in the system tray with two global hotkeys, both rewriting the focused
field in place:

- **`Ctrl+Shift+A`** — smart toggle for an **editable field** (your prompt
  box). If the field has no known surrogates it is anonymized in place; press
  it again and the surrogates are mapped back to the real values.
- **`Ctrl+Shift+D`** — restore a **selection**. Select any text — including a
  read-only LLM reply that can't be edited — and a small popup appears at the
  cursor showing the real values, with a Copy button.

`A` writes back in place (for text you're composing); `D` is read-only and
never touches the source app (for reading replies).

A single in-memory **vault** is the id system that makes this reversible: every
distinct real value is stored once with its category and a unique surrogate, so
the same name always maps to the same surrogate and an email maps to its own —
restoration is unambiguous in both directions. The vault is shared across every
app for the life of the process, so a value anonymized in one app restores in
another. Surrogates are generated procedurally (street addresses, URLs) or from
large name pools, so the supply is effectively unbounded — fiction-safe phone
numbers (`555-01xx`) are the one deliberately small set.

Notes: the guard reads and writes via UI Automation, falling back to a
clipboard select-all + copy/paste for rich editors (browser `contenteditable`,
some Electron apps) that do not expose direct value access. The `guard`
subcommand is Windows only (macOS AX API and Linux AT-SPI are future work); the
module is `cfg(windows)`-gated, so the workspace still builds on other
platforms — only the subcommand is absent there.

## Browser extension

The MV3 extension in `extension/` is `id4pii guard` for LLM sites: ChatGPT,
Claude, Gemini (and any others wired into the host allowlist). When you submit,
it pulls your text out of the input, hands it to the local `guard` process over
a loopback WebSocket, replaces it with the anonymized version, then submits.
When the assistant's reply streams in, surrogates are auto-restored to real
values in the rendered text — a `MutationObserver` driving the same pure
`deanonymize` string-replace used by the CLI.

It's **part of guard, not a parallel anonymizer**: the extension owns no
model, no detector, and no vault. All of that lives in the existing engine —
the extension is just another command source on the same bus, alongside the
global hotkey. The vault is shared: a name anonymized via `Ctrl+Shift+A` in
Notepad reuses its surrogate when you type it into chatgpt.com.

The extension shares its assets with the desktop guard: the toolbar icon,
extension icons, and the lock-close/lock-open animation frames all live in
the repo-root `assets/` directory. A small sync script copies them into
`extension/assets/` (which is gitignored, treated as a build artifact):

```powershell
.\scripts\sync-extension-assets.ps1
```

Run it whenever `assets/icon-*.png` or `assets/lock_frames/*.png` change.
After running it once, the extension can be loaded unpacked from
`extension/`.

### Setup

1. Run `id4pii guard` — the bridge listens on `ws://127.0.0.1:7878/ws` by
   default. Disable with `--no-bridge`, change the port with
   `--bridge-port <n>` (must match `BRIDGE_URL` in `extension/background.js`
   if you customize it).
2. In Chrome: `chrome://extensions` → **Developer mode** → **Load unpacked**
   → select the `extension/` directory.
3. The toolbar icon shows a solid green badge when connected, `!` when the
   bridge is unreachable.

### Triggers

- **On submit** (Enter or send-button click) on a whitelisted host: the
  extension intercepts, anonymizes via guard, then re-fires the submit with
  the anonymized text. Same lock-close animation as the desktop guard.
- **`Ctrl+Shift+A`** in any input on a whitelisted host: anonymize the input
  in place without submitting, so you can review.
- **Auto-restore**: as the assistant streams text into the page,
  surrogates in plain text nodes are swapped back. Code blocks (`<code>`,
  `<pre>`) are skipped to keep copy-paste intact.

### Failure modes

- Bridge down → submit interception fails open (passes through with a
  `console.warn`), so you never get stuck unable to send.
- Per-site adapters target the current ChatGPT / Claude / Gemini DOMs. They
  use multiple selector fallbacks but will need updates when those sites
  refactor. New adapters drop into `extension/adapters/` and self-register
  by hostname.

### Debugging

Both sides of the extension log silently by default. To turn on verbose tracing:

- **Guard (Rust)**: set `RUST_LOG=id4pii=debug,ort=warn` before starting guard.
  Every WebSocket frame, vault lock, and engine step is logged with a `req_id`
  field so you can grep one request end to end.
- **Extension (browser)**: in the extension's service-worker DevTools (find it
  at `chrome://extensions` → id4pii guard → **Inspect views: service worker**),
  type `id4pii.debug(true)` in the console. The setting persists in
  `chrome.storage.local` and applies to background, content script, and
  in-page main world. Disable with `id4pii.debug(false)`.

Lines have a uniform format: `[id4pii:<component>] <event> key=value …`.
Components are `bg` (service worker), `iso` (content script), `main` (in-page),
`bridge` and `engine` (Rust). Each fetch interception generates an 8-char
`reqId` that is threaded through every hop in both directions, so you can
follow a single message through `main → iso → bg → bridge → engine` and back.

Privacy invariant: no message text, response body, or vault entry is ever
logged at any level — only lengths, counts, kinds, durations, and the request
ID.

### Security

- Bridge binds `127.0.0.1` only.
- WebSocket handshake rejects unless `Origin` starts with
  `chrome-extension://`, `moz-extension://`, or `safari-web-extension://` —
  regular web pages on `localhost` can't open a session.
- v1 does not pin extension IDs; any installed browser extension can speak
  to guard. If that matters to you, run guard with `--no-bridge`.

## Performance

Always run the optimized build — `cargo run` uses the unoptimized `dev`
profile and is many times slower:

```sh
cargo build --release
./target/release/id4pii scan "..."
```

Measured on a Ryzen 5 9600X with the `model_q4` variant:

| Path | Latency |
|---|---|
| CLI one-shot (cold) | ~195 ms |
| `serve` request (warm) | ~33 ms |

The tokenizer is embedded and loads in ~85 ms (in parallel with the ONNX
session), so a cold CLI run is dominated by model load and one inference pass.
For repeated or latency-sensitive use, `serve` still wins decisively — the
model loads once and each request is just inference. Set
`RUST_LOG=id4pii_core=debug` to see per-phase load timings.

## Development

```sh
cargo test --workspace
cargo fmt --all
cargo clippy --all-targets
```
