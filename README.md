# id4pii

Fast PII detection and redaction for text — CLI and HTTP API — powered by
[OpenAI Privacy Filter](https://huggingface.co/openai/privacy-filter) running
locally through ONNX Runtime. No data leaves the machine.

It tags eight categories: `account_number`, `private_address`, `private_date`,
`private_email`, `private_person`, `private_phone`, `private_url`, `secret`.

## Layout

```
crates/
  core/   id4pii-core  — ONNX inference, BIOES span decoding, redaction
  app/    id4pii-app   — `id4pii` binary: `scan` (CLI) and `serve` (HTTP API)
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

## HTTP API

```sh
cargo run -p id4pii-app -- serve --addr 127.0.0.1:8080
```

- `GET /health` → `ok`
- `POST /scan` with `{"text": "...", "redact": true}` →
  `{"spans": [...], "redacted": "..."}`

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
