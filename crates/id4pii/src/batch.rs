use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread;

use crate::{IndexedVault, PiiSpan, RedactStyle, Rng, anonymize_into, redact};
use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, ValueEnum};
use serde::Serialize;
use serde_json::Value;

use crate::cli::{ModelArgs, Style};
use crate::detector_service::{Coalesce, DetectorService, SpansResult};
use crate::model_setup;

const CHANNEL_DEPTH: usize = 2;

fn strip_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

fn without_bom(text: String) -> String {
    match text.strip_prefix('\u{feff}') {
        Some(rest) => rest.to_string(),
        None => text,
    }
}

#[derive(Args, Debug)]
pub struct BatchArgs {
    #[arg(short, long)]
    input: Option<PathBuf>,
    #[arg(short, long)]
    output: Option<PathBuf>,
    #[arg(long, value_enum, default_value_t = BatchOp::Anonymize)]
    op: BatchOp,
    #[arg(long, value_enum, default_value_t = CorpusFormat::Auto)]
    format: CorpusFormat,
    #[arg(long)]
    vault_out: Option<PathBuf>,
    #[arg(long)]
    seed: Option<u64>,
    #[arg(long)]
    batch: Option<usize>,
    #[arg(long, default_value_t = 256)]
    shard_records: usize,
    #[arg(long, default_value = "text")]
    jsonl_field: String,
    #[arg(long)]
    delimiter: Option<String>,
    #[arg(long, default_value_t = 0)]
    tsv_column: usize,
    #[arg(long, value_enum, default_value_t = Style::Label)]
    style: Style,
    #[command(flatten)]
    model: ModelArgs,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum, Debug)]
pub enum BatchOp {
    Anonymize,
    Scan,
    Redact,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum, Debug)]
pub enum CorpusFormat {
    Auto,
    Files,
    Jsonl,
    Lines,
    Tsv,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Files,
    Jsonl,
    Lines,
    Tsv,
}

struct Record {
    id: String,
    text: String,
    raw: Option<Value>,
}

type Source = Box<dyn Iterator<Item = Result<Record>> + Send>;

#[derive(Serialize)]
struct ScanLine<'a> {
    id: &'a str,
    spans: &'a [PiiSpan],
}

pub(crate) fn run(args: &BatchArgs) -> Result<()> {
    let kind = resolve_format(args);
    let mut detector = model_setup::load_detector(
        &args.model.model,
        &args.model.model_file,
        args.model.threads,
    )?;
    // `--batch` pins the inference batch size; default (None) is adaptive.
    detector.set_batch_override(args.batch);
    let (service, model_handle) = DetectorService::spawn(detector, Coalesce::Off, CHANNEL_DEPTH)?;

    let source = build_source(args, kind)?;
    let mut sink = build_sink(args, kind)?;

    let (pair_tx, pair_rx) = sync_channel::<(Vec<Record>, Receiver<SpansResult>)>(CHANNEL_DEPTH);

    let shard_records = args.shard_records.max(1);
    let min_score = args.model.min_score;
    let reader_service = service.clone();
    let reader = thread::Builder::new()
        .name("id4pii-batch-reader".to_string())
        .spawn(move || reader_loop(source, shard_records, min_score, &reader_service, &pair_tx))
        .context("failed to spawn reader thread")?;
    // Drop our handle so the model thread winds down once the reader's clone is gone.
    drop(service);

    let mut vault = IndexedVault::new();
    let mut rng = args.seed.map_or_else(Rng::from_entropy, Rng::new);
    let ctx = RenderCtx {
        op: args.op,
        kind,
        field: &args.jsonl_field,
        style: args.style.into(),
    };

    // Drain shards in submission order; each shard's spans arrive on its own
    // reply channel, so the shared vault mints surrogates in record order.
    let mut docs = 0usize;
    while let Ok((records, reply)) = pair_rx.recv() {
        let spans = reply
            .recv()
            .map_err(|_| anyhow!("detector dropped a shard"))?
            .map_err(|message| anyhow!(message))
            .context("detection failed")?;
        for (record, spans) in records.iter().zip(&spans) {
            let content = render(&ctx, record, spans, &mut vault, &mut rng)?;
            sink.put(&record.id, &content)?;
            docs += 1;
        }
    }

    reader
        .join()
        .map_err(|_| anyhow!("reader thread panicked"))?;
    model_handle
        .join()
        .map_err(|_| anyhow!("model thread panicked"))?;
    sink.finish()?;

    let entries = vault.len();
    if matches!(args.op, BatchOp::Anonymize)
        && let Some(path) = &args.vault_out
    {
        let vault = vault.into_vault();
        fs::write(path, serde_json::to_string_pretty(&vault)?)
            .with_context(|| format!("failed to write {}", path.display()))?;
    }
    tracing::info!(docs, vault_entries = entries, "batch complete");
    eprintln!("id4pii batch: processed {docs} documents, {entries} vault entries");
    Ok(())
}

fn reader_loop(
    source: Source,
    shard_records: usize,
    min_score: f32,
    service: &DetectorService,
    pair_tx: &SyncSender<(Vec<Record>, Receiver<SpansResult>)>,
) {
    let mut shard = Vec::with_capacity(shard_records);
    for item in source {
        match item {
            Ok(record) => {
                shard.push(record);
                if shard.len() >= shard_records {
                    let batch = std::mem::replace(&mut shard, Vec::with_capacity(shard_records));
                    if !submit_shard(service, pair_tx, min_score, batch) {
                        return;
                    }
                }
            }
            Err(error) => tracing::warn!(%error, "skipping unreadable record"),
        }
    }
    let _ = submit_shard(service, pair_tx, min_score, shard);
}

/// Queue a shard for detection and hand the writer its records paired with the
/// reply channel. Returns `false` once the downstream has hung up.
fn submit_shard(
    service: &DetectorService,
    pair_tx: &SyncSender<(Vec<Record>, Receiver<SpansResult>)>,
    min_score: f32,
    shard: Vec<Record>,
) -> bool {
    if shard.is_empty() {
        return true;
    }
    let texts: Vec<String> = shard.iter().map(|record| record.text.clone()).collect();
    match service.submit_async(texts, min_score) {
        Ok(reply) => pair_tx.send((shard, reply)).is_ok(),
        Err(_) => false,
    }
}

struct RenderCtx<'a> {
    op: BatchOp,
    kind: Kind,
    field: &'a str,
    style: RedactStyle,
}

fn render(
    ctx: &RenderCtx,
    record: &Record,
    spans: &[PiiSpan],
    vault: &mut IndexedVault,
    rng: &mut Rng,
) -> Result<String> {
    match ctx.op {
        BatchOp::Scan => Ok(serde_json::to_string(&ScanLine {
            id: &record.id,
            spans,
        })?),
        BatchOp::Redact => frame(ctx, record, redact(&record.text, spans, ctx.style)),
        BatchOp::Anonymize => {
            let text = anonymize_into(&record.text, spans, rng, vault);
            frame(ctx, record, text)
        }
    }
}

fn frame(ctx: &RenderCtx, record: &Record, text: String) -> Result<String> {
    if ctx.kind == Kind::Jsonl {
        let mut map = match record.raw.clone() {
            Some(Value::Object(map)) => map,
            _ => serde_json::Map::new(),
        };
        map.insert(ctx.field.to_string(), Value::String(text));
        return Ok(serde_json::to_string(&Value::Object(map))?);
    }
    Ok(text)
}

enum Sink {
    Dir { root: PathBuf },
    Line { writer: BufWriter<Box<dyn Write>> },
}

impl Sink {
    fn put(&mut self, id: &str, content: &str) -> Result<()> {
        match self {
            Sink::Dir { root } => {
                let path = root.join(id);
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)
                        .with_context(|| format!("failed to create {}", parent.display()))?;
                }
                fs::write(&path, content)
                    .with_context(|| format!("failed to write {}", path.display()))?;
            }
            Sink::Line { writer } => {
                writer.write_all(content.as_bytes())?;
                writer.write_all(b"\n")?;
            }
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        if let Sink::Line { writer } = self {
            writer.flush()?;
        }
        Ok(())
    }
}

fn resolve_format(args: &BatchArgs) -> Kind {
    match args.format {
        CorpusFormat::Files => return Kind::Files,
        CorpusFormat::Jsonl => return Kind::Jsonl,
        CorpusFormat::Lines => return Kind::Lines,
        CorpusFormat::Tsv => return Kind::Tsv,
        CorpusFormat::Auto => {}
    }
    let Some(input) = &args.input else {
        return Kind::Lines;
    };
    if input.is_dir() {
        return Kind::Files;
    }
    match input.extension().and_then(|ext| ext.to_str()) {
        Some("jsonl" | "ndjson") => Kind::Jsonl,
        Some("tsv") => Kind::Tsv,
        _ => Kind::Lines,
    }
}

fn build_source(args: &BatchArgs, kind: Kind) -> Result<Source> {
    if kind == Kind::Files {
        let root = args
            .input
            .clone()
            .ok_or_else(|| anyhow!("--input <dir> is required for the files format"))?;
        if !root.is_dir() {
            bail!("--input must be a directory for the files format");
        }
        let paths = walk_files(&root)?;
        return Ok(Box::new(FileSource {
            root,
            paths: paths.into_iter(),
        }));
    }

    if let Some(delimiter) = &args.delimiter {
        let content = read_all(args.input.as_deref())?;
        let records: Vec<Record> = content
            .split(delimiter.as_str())
            .enumerate()
            .map(|(index, chunk)| Record {
                id: index.to_string(),
                text: chunk.to_string(),
                raw: None,
            })
            .collect();
        return Ok(Box::new(records.into_iter().map(Ok)));
    }

    let reader: Box<dyn BufRead + Send> = match &args.input {
        Some(path) => Box::new(BufReader::new(
            File::open(path).with_context(|| format!("failed to open {}", path.display()))?,
        )),
        None => Box::new(BufReader::new(std::io::stdin())),
    };
    let line_kind = match kind {
        Kind::Jsonl => LineKind::Jsonl {
            field: args.jsonl_field.clone(),
        },
        Kind::Tsv => LineKind::Tsv {
            column: args.tsv_column,
        },
        _ => LineKind::Lines,
    };
    Ok(Box::new(LineSource {
        reader,
        kind: line_kind,
        line_no: 0,
    }))
}

fn build_sink(args: &BatchArgs, kind: Kind) -> Result<Sink> {
    if kind == Kind::Files {
        let root = args
            .output
            .clone()
            .ok_or_else(|| anyhow!("--output <dir> is required for the files format"))?;
        fs::create_dir_all(&root)
            .with_context(|| format!("failed to create {}", root.display()))?;
        return Ok(Sink::Dir { root });
    }
    let writer: Box<dyn Write> = match &args.output {
        Some(path) => Box::new(
            File::create(path).with_context(|| format!("failed to create {}", path.display()))?,
        ),
        None => Box::new(std::io::stdout()),
    };
    Ok(Sink::Line {
        writer: BufWriter::new(writer),
    })
}

fn read_all(input: Option<&Path>) -> Result<String> {
    let content = if let Some(path) = input {
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?
    } else {
        let mut buffer = String::new();
        std::io::stdin()
            .read_to_string(&mut buffer)
            .context("failed to read stdin")?;
        buffer
    };
    Ok(without_bom(content))
}

fn walk_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries =
            fs::read_dir(&dir).with_context(|| format!("failed to read {}", dir.display()))?;
        for entry in entries {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                stack.push(entry.path());
            } else if file_type.is_file() {
                files.push(entry.path());
            }
        }
    }
    files.sort();
    Ok(files)
}

struct FileSource {
    root: PathBuf,
    paths: std::vec::IntoIter<PathBuf>,
}

impl Iterator for FileSource {
    type Item = Result<Record>;

    fn next(&mut self) -> Option<Self::Item> {
        let path = self.paths.next()?;
        let id = path
            .strip_prefix(&self.root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        Some(match fs::read_to_string(&path) {
            Ok(text) => Ok(Record {
                id,
                text: without_bom(text),
                raw: None,
            }),
            Err(error) => Err(anyhow!("failed to read {}: {error}", path.display())),
        })
    }
}

enum LineKind {
    Lines,
    Jsonl { field: String },
    Tsv { column: usize },
}

struct LineSource<R> {
    reader: R,
    kind: LineKind,
    line_no: usize,
}

impl<R: BufRead> Iterator for LineSource<R> {
    type Item = Result<Record>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let mut buffer = String::new();
            match self.reader.read_line(&mut buffer) {
                Ok(0) => return None,
                Ok(_) => {}
                Err(error) => return Some(Err(anyhow!("read error: {error}"))),
            }
            let mut line = buffer.trim_end_matches(['\n', '\r']);
            if self.line_no == 0 {
                line = strip_bom(line);
            }
            let id = self.line_no.to_string();
            self.line_no += 1;
            match &self.kind {
                LineKind::Lines => {
                    return Some(Ok(Record {
                        id,
                        text: line.to_string(),
                        raw: None,
                    }));
                }
                LineKind::Jsonl { field } => {
                    if line.trim().is_empty() {
                        continue;
                    }
                    return Some(match serde_json::from_str::<Value>(line) {
                        Ok(value) => {
                            let text = value
                                .get(field.as_str())
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string();
                            Ok(Record {
                                id,
                                text,
                                raw: Some(value),
                            })
                        }
                        Err(error) => Err(anyhow!("line {id}: invalid json: {error}")),
                    });
                }
                LineKind::Tsv { column } => {
                    let text = line
                        .split('\t')
                        .nth(*column)
                        .unwrap_or_default()
                        .to_string();
                    return Some(Ok(Record {
                        id,
                        text,
                        raw: None,
                    }));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Kind, LineKind, LineSource, Record, RenderCtx, frame};
    use crate::RedactStyle;
    use crate::batch::BatchOp;
    use serde_json::{Value, json};

    fn collect(kind: LineKind, input: &str) -> Vec<Record> {
        LineSource {
            reader: input.as_bytes(),
            kind,
            line_no: 0,
        }
        .map(|item| item.expect("record"))
        .collect()
    }

    #[test]
    fn jsonl_source_extracts_field_and_keeps_envelope() {
        let records = collect(
            LineKind::Jsonl {
                field: "body".to_string(),
            },
            "{\"id\":7,\"body\":\"hello\"}\n\n{\"body\":\"world\",\"keep\":true}\n",
        );
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].text, "hello");
        assert_eq!(records[0].id, "0");
        assert_eq!(records[1].text, "world");
        assert_eq!(records[1].id, "2");
        assert!(records[1].raw.is_some());
    }

    #[test]
    fn lines_and_tsv_sources_split_as_expected() {
        let lines = collect(LineKind::Lines, "alpha\nbeta\n");
        assert_eq!(
            lines.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(),
            ["alpha", "beta"]
        );

        let tsv = collect(LineKind::Tsv { column: 1 }, "a\tb\tc\nd\te\tf\n");
        assert_eq!(
            tsv.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(),
            ["b", "e"]
        );
    }

    #[test]
    fn frame_reinjects_anonymized_text_into_json_envelope() {
        let ctx = RenderCtx {
            op: BatchOp::Anonymize,
            kind: Kind::Jsonl,
            field: "text",
            style: RedactStyle::Label,
        };
        let record = Record {
            id: "0".to_string(),
            text: "original".to_string(),
            raw: Some(json!({"text": "original", "meta": 42})),
        };
        let framed = frame(&ctx, &record, "SAFE".to_string()).expect("frame");
        let value: Value = serde_json::from_str(&framed).expect("json");
        assert_eq!(value["text"], json!("SAFE"));
        assert_eq!(value["meta"], json!(42));
    }

    #[test]
    fn frame_passthrough_for_non_json_formats() {
        let ctx = RenderCtx {
            op: BatchOp::Redact,
            kind: Kind::Lines,
            field: "text",
            style: RedactStyle::Label,
        };
        let record = Record {
            id: "0".to_string(),
            text: "x".to_string(),
            raw: None,
        };
        assert_eq!(
            frame(&ctx, &record, "plain".to_string()).expect("frame"),
            "plain"
        );
    }
}
