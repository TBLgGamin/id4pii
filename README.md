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

The model is not bundled. Download `openai/privacy-filter` into a directory —
`scan`/`serve` need `model.onnx` (plus its `.onnx_data` shards), `config.json`
and `tokenizer.json`:

```sh
pip install -U "huggingface_hub[cli]"
hf download openai/privacy-filter config.json tokenizer.json --local-dir model
hf download openai/privacy-filter --include "onnx/model.onnx*" --local-dir model
```

Point id4pii at it with `--model <dir>` or the `ID4PII_MODEL` env var
(default `./model`). Use `--model-file onnx/model.onnx` if the ONNX file sits in
an `onnx/` subfolder.

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
./target/release/id4pii scan --model-file onnx/model_q4.onnx "..."
```

Measured on a Ryzen 5 9600X with the `model_q4` variant:

| Path | Latency |
|---|---|
| CLI one-shot (cold) | ~690 ms |
| `serve` request (warm) | ~33 ms |

A CLI invocation reloads the model and the 200k-vocab tokenizer every time —
the tokenizer parse alone is ~580 ms and cannot be cached. For repeated or
latency-sensitive use, run `serve` once and call `POST /scan`: the model is
loaded a single time and each request is just inference. Set
`RUST_LOG=id4pii_core=debug` to see per-phase load timings.

## Development

```sh
cargo test --workspace
cargo fmt --all
cargo clippy --all-targets
```
