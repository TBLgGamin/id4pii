#![allow(clippy::case_sensitive_file_extension_comparisons)]

use std::io::{Cursor, Read, Write};

use crate::{Placement, apply_placements};
use anyhow::{Context, Result, anyhow, bail};
use quick_xml::XmlVersion;
use quick_xml::events::{BytesStart, BytesText, Event};
use quick_xml::reader::Reader;
use quick_xml::writer::Writer;
use zip::write::{SimpleFileOptions, ZipWriter};
use zip::{CompressionMethod, ZipArchive};

type ZipBytes<'a> = ZipArchive<Cursor<&'a [u8]>>;

const PART_SEPARATOR: &str = "\n\n";
const METADATA_PARTS: &[&str] = &["docProps/core.xml", "docProps/app.xml"];
const SCRUB_TAGS: &[&[u8]] = &[b"dc:creator", b"cp:lastModifiedBy", b"Manager", b"Company"];
const ALT_TEXT_ATTRS: &[&[u8]] = &[b"descr", b"title"];

#[derive(Debug, Clone, Copy)]
enum NodeMode {
    Tagged {
        text_tag: &'static [u8],
        para_tag: &'static [u8],
    },
    AllText,
}

const DOCX_MODE: NodeMode = NodeMode::Tagged {
    text_tag: b"w:t",
    para_tag: b"w:p",
};
const PPTX_MODE: NodeMode = NodeMode::Tagged {
    text_tag: b"a:t",
    para_tag: b"a:p",
};
const XLSX_MODE: NodeMode = NodeMode::Tagged {
    text_tag: b"t",
    para_tag: b"si",
};

#[derive(Debug)]
struct PlanPart {
    name: String,
    mode: NodeMode,
    concat_start: usize,
}

#[derive(Debug)]
enum DocKind {
    Ooxml {
        source: Vec<u8>,
        mime: &'static str,
        parts: Vec<PlanPart>,
        scrub: Vec<String>,
    },
    Pdf,
}

#[derive(Debug)]
pub struct DocPlan {
    kind: DocKind,
    pub text: String,
}

#[derive(Debug)]
pub struct RewriteOutput {
    pub data: Vec<u8>,
    pub mime: &'static str,
}

const MIME_DOCX: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";
const MIME_PPTX: &str = "application/vnd.openxmlformats-officedocument.presentationml.presentation";
const MIME_XLSX: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";
const MIME_PDF: &str = "application/pdf";

pub fn plan(bytes: &[u8], filename: &str) -> Result<DocPlan> {
    let ext = filename
        .rsplit('.')
        .next()
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();

    match ext.as_str() {
        "docx" => plan_ooxml(bytes, MIME_DOCX, docx_parts),
        "pptx" => plan_ooxml(bytes, MIME_PPTX, pptx_parts),
        "xlsx" => plan_ooxml(bytes, MIME_XLSX, xlsx_parts),
        "pdf" => Ok(DocPlan {
            kind: DocKind::Pdf,
            text: extract_pdf(bytes)?,
        }),
        other => bail!("unsupported document type for in-place anonymization: .{other}"),
    }
}

impl DocPlan {
    pub fn finish(self, placements: &[Placement]) -> Result<RewriteOutput> {
        match self.kind {
            DocKind::Pdf => Ok(RewriteOutput {
                data: generate_pdf(&apply_placements(&self.text, placements)),
                mime: MIME_PDF,
            }),
            DocKind::Ooxml {
                source,
                mime,
                parts,
                scrub,
            } => {
                let data = rebuild_ooxml(&source, &parts, &scrub, placements)?;
                Ok(RewriteOutput { data, mime })
            }
        }
    }
}

fn docx_parts(names: &[String]) -> Vec<(String, NodeMode)> {
    let mut parts: Vec<(String, NodeMode)> = Vec::new();
    if names.iter().any(|n| n == "word/document.xml") {
        parts.push(("word/document.xml".to_string(), DOCX_MODE));
    }
    let mut decorations: Vec<String> = names
        .iter()
        .filter(|n| {
            (n.starts_with("word/header") || n.starts_with("word/footer")) && n.ends_with(".xml")
        })
        .cloned()
        .collect();
    decorations.sort();
    for name in decorations {
        parts.push((name, DOCX_MODE));
    }
    for tail in [
        "word/comments.xml",
        "word/footnotes.xml",
        "word/endnotes.xml",
    ] {
        if names.iter().any(|n| n == tail) {
            parts.push((tail.to_string(), DOCX_MODE));
        }
    }
    parts
}

fn pptx_parts(names: &[String]) -> Vec<(String, NodeMode)> {
    let mut slides: Vec<String> = names
        .iter()
        .filter(|n| n.starts_with("ppt/slides/slide") && n.ends_with(".xml"))
        .cloned()
        .collect();
    slides.sort_by_key(|n| slide_number(n));
    let mut notes: Vec<String> = names
        .iter()
        .filter(|n| n.starts_with("ppt/notesSlides/notesSlide") && n.ends_with(".xml"))
        .cloned()
        .collect();
    notes.sort_by_key(|n| slide_number(n));
    slides
        .into_iter()
        .chain(notes)
        .map(|n| (n, PPTX_MODE))
        .collect()
}

fn xlsx_parts(names: &[String]) -> Vec<(String, NodeMode)> {
    if names.iter().any(|n| n == "xl/sharedStrings.xml") {
        vec![("xl/sharedStrings.xml".to_string(), XLSX_MODE)]
    } else {
        Vec::new()
    }
}

fn extra_text_parts(names: &[String]) -> Vec<(String, NodeMode)> {
    let mut extra: Vec<String> = names
        .iter()
        .filter(|n| {
            n.ends_with(".xml")
                && (n.contains("/charts/chart")
                    || n.contains("/diagrams/data")
                    || n.as_str() == "docProps/custom.xml")
        })
        .cloned()
        .collect();
    extra.sort();
    extra.into_iter().map(|n| (n, NodeMode::AllText)).collect()
}

fn plan_ooxml(
    bytes: &[u8],
    mime: &'static str,
    select: fn(&[String]) -> Vec<(String, NodeMode)>,
) -> Result<DocPlan> {
    let mut zip = open_zip(bytes)?;
    let names: Vec<String> = zip.file_names().map(str::to_string).collect();
    let specs: Vec<(String, NodeMode)> = select(&names)
        .into_iter()
        .chain(extra_text_parts(&names))
        .collect();

    let mut text = String::new();
    let mut parts: Vec<PlanPart> = Vec::new();
    for (name, mode) in specs {
        let Ok(xml) = read_entry(&mut zip, &name) else {
            continue;
        };
        if !parts.is_empty() {
            text.push_str(PART_SEPARATOR);
        }
        let concat_start = text.len();
        process_part(&xml, mode, concat_start, &mut PartPass::Collect(&mut text))?;
        parts.push(PlanPart {
            name,
            mode,
            concat_start,
        });
    }

    let scrub: Vec<String> = METADATA_PARTS
        .iter()
        .filter(|m| names.iter().any(|n| n == *m))
        .map(|m| (*m).to_string())
        .collect();

    Ok(DocPlan {
        kind: DocKind::Ooxml {
            source: bytes.to_vec(),
            mime,
            parts,
            scrub,
        },
        text,
    })
}

fn rebuild_ooxml(
    source: &[u8],
    parts: &[PlanPart],
    scrub: &[String],
    placements: &[Placement],
) -> Result<Vec<u8>> {
    let mut archive = open_zip(source)?;
    let mut out = ZipWriter::new(Cursor::new(Vec::new()));
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

    for i in 0..archive.len() {
        let name = {
            let raw = archive.by_index_raw(i)?;
            raw.name().to_owned()
        };
        if let Some(part) = parts.iter().find(|p| p.name == name) {
            let xml = read_entry(&mut archive, &name)?;
            let mut writer = Writer::new(Cursor::new(Vec::new()));
            process_part(
                &xml,
                part.mode,
                part.concat_start,
                &mut PartPass::Rewrite {
                    placements,
                    writer: &mut writer,
                },
            )?;
            let rewritten = writer.into_inner().into_inner();
            out.start_file(&name, opts)?;
            out.write_all(&rewritten)?;
        } else if scrub.contains(&name) {
            let xml = read_entry(&mut archive, &name)?;
            let scrubbed = scrub_metadata(&xml)?;
            out.start_file(&name, opts)?;
            out.write_all(&scrubbed)?;
        } else {
            let raw = archive.by_index_raw(i)?;
            out.raw_copy_file(raw)?;
        }
    }

    Ok(out.finish()?.into_inner())
}

fn splice_node(local_text: &str, node_start: usize, placements: &[Placement]) -> String {
    let node_end = node_start + local_text.len();
    let mut out = String::with_capacity(local_text.len());
    let mut cur = node_start;
    for p in placements {
        if p.end <= node_start || p.start >= node_end {
            continue;
        }
        let cov_start = p.start.max(node_start);
        let cov_end = p.end.min(node_end);
        if cur < cov_start {
            out.push_str(
                local_text
                    .get((cur - node_start)..(cov_start - node_start))
                    .unwrap_or(""),
            );
        }
        if p.start >= node_start {
            out.push_str(&p.surrogate);
        }
        cur = cov_end.max(cur);
    }
    if cur < node_end {
        out.push_str(local_text.get((cur - node_start)..).unwrap_or(""));
    }
    out
}

enum PartPass<'a> {
    Collect(&'a mut String),
    Rewrite {
        placements: &'a [Placement],
        writer: &'a mut Writer<Cursor<Vec<u8>>>,
    },
}

impl PartPass<'_> {
    fn emit_text(&mut self, content: &str, node_start: usize) -> Result<()> {
        match self {
            PartPass::Collect(out) => out.push_str(content),
            PartPass::Rewrite { placements, writer } => {
                let spliced = splice_node(content, node_start, placements);
                writer.write_event(Event::Text(BytesText::new(&spliced)))?;
            }
        }
        Ok(())
    }

    fn separator(&mut self) {
        if let PartPass::Collect(out) = self {
            out.push('\n');
        }
    }

    fn write_other(&mut self, event: &Event) -> Result<()> {
        if let PartPass::Rewrite { writer, .. } = self {
            writer.write_event(event.borrow())?;
        }
        Ok(())
    }

    fn write_start(&mut self, element: BytesStart<'static>, empty: bool) -> Result<()> {
        if let PartPass::Rewrite { writer, .. } = self {
            if empty {
                writer.write_event(Event::Empty(element))?;
            } else {
                writer.write_event(Event::Start(element))?;
            }
        }
        Ok(())
    }
}

fn has_alt_text_attr(element: &BytesStart) -> bool {
    element
        .attributes()
        .flatten()
        .any(|a| ALT_TEXT_ATTRS.contains(&a.key.as_ref()))
}

fn handle_alt_text_attrs(
    element: &BytesStart,
    empty: bool,
    cur: &mut usize,
    pass: &mut PartPass<'_>,
) -> Result<()> {
    let name = std::str::from_utf8(element.name().as_ref())?.to_owned();
    let mut rebuilt = matches!(pass, PartPass::Rewrite { .. }).then(|| BytesStart::new(name));
    for attr in element.attributes() {
        let attr = attr.map_err(|e| anyhow!("attribute parse: {e}"))?;
        if ALT_TEXT_ATTRS.contains(&attr.key.as_ref()) {
            let value = attr
                .normalized_value(XmlVersion::Implicit1_0)
                .map_err(|e| anyhow!("attribute value: {e}"))?
                .into_owned();
            match pass {
                PartPass::Collect(out) => {
                    out.push_str(&value);
                    out.push('\n');
                }
                PartPass::Rewrite { placements, .. } => {
                    let spliced = splice_node(&value, *cur, placements);
                    if let Some(el) = rebuilt.as_mut() {
                        let key = std::str::from_utf8(attr.key.as_ref())?;
                        el.push_attribute((key, spliced.as_str()));
                    }
                }
            }
            *cur += value.len() + 1;
        } else if let Some(el) = rebuilt.as_mut() {
            el.push_attribute((attr.key.as_ref(), attr.value.as_ref()));
        }
    }
    if let Some(el) = rebuilt {
        pass.write_start(el, empty)?;
    }
    Ok(())
}

fn process_part(
    xml: &str,
    mode: NodeMode,
    concat_start: usize,
    pass: &mut PartPass<'_>,
) -> Result<usize> {
    let (text_tag, para_tag, mut in_text) = match mode {
        NodeMode::Tagged { text_tag, para_tag } => (Some(text_tag), Some(para_tag), 0usize),
        NodeMode::AllText => (None, None, 1usize),
    };
    let mut reader = Reader::from_str(xml);
    let mut cur = concat_start;
    loop {
        let event = reader.read_event()?;
        match &event {
            Event::Eof => break,
            Event::Start(e) => {
                if has_alt_text_attr(e) {
                    handle_alt_text_attrs(e, false, &mut cur, pass)?;
                } else {
                    pass.write_other(&event)?;
                }
                if Some(e.name().as_ref()) == text_tag {
                    in_text += 1;
                }
            }
            Event::Empty(e) if has_alt_text_attr(e) => {
                handle_alt_text_attrs(e, true, &mut cur, pass)?;
            }
            Event::End(e) => {
                if Some(e.name().as_ref()) == text_tag {
                    in_text = in_text.saturating_sub(1);
                } else if Some(e.name().as_ref()) == para_tag {
                    cur += 1;
                    pass.separator();
                }
                pass.write_other(&event)?;
            }
            Event::Text(e) if in_text > 0 => {
                let content = e.xml_content(XmlVersion::Implicit1_0)?;
                pass.emit_text(&content, cur)?;
                cur += content.len();
                if matches!(mode, NodeMode::AllText) {
                    cur += 1;
                    pass.separator();
                }
            }
            _ => {
                pass.write_other(&event)?;
            }
        }
    }
    Ok(cur)
}

fn scrub_metadata(xml: &str) -> Result<Vec<u8>> {
    let mut reader = Reader::from_str(xml);
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    let mut in_scrub = 0usize;
    loop {
        let event = reader.read_event()?;
        match &event {
            Event::Eof => break,
            Event::Start(e) if SCRUB_TAGS.contains(&e.name().as_ref()) => {
                in_scrub += 1;
                writer.write_event(event.borrow())?;
            }
            Event::End(e) if SCRUB_TAGS.contains(&e.name().as_ref()) => {
                in_scrub = in_scrub.saturating_sub(1);
                writer.write_event(event.borrow())?;
            }
            Event::Text(_) if in_scrub > 0 => {}
            _ => {
                writer.write_event(event.borrow())?;
            }
        }
    }
    Ok(writer.into_inner().into_inner())
}

#[allow(clippy::cast_sign_loss)]
fn generate_pdf(text: &str) -> Vec<u8> {
    const FONT_SIZE: f32 = 11.0;
    const LEADING: f32 = 14.0;
    const MARGIN: f32 = 54.0;
    const PAGE_W: f32 = 612.0;
    const PAGE_H: f32 = 792.0;
    const MAX_CHARS: usize = 95;

    let lines = wrap_lines(text, MAX_CHARS);
    let per_page = (((PAGE_H - 2.0 * MARGIN) / LEADING) as usize).max(1);
    let mut pages: Vec<&[String]> = lines.chunks(per_page).collect();
    if pages.is_empty() {
        pages.push(&[]);
    }

    let total_objs = 3 + 2 * pages.len();
    let mut offsets: Vec<usize> = vec![0; total_objs + 1];
    let mut buf: Vec<u8> = Vec::new();
    buf.extend_from_slice(b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n");

    offsets[1] = buf.len();
    buf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    offsets[2] = buf.len();
    buf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Count ");
    buf.extend_from_slice(pages.len().to_string().as_bytes());
    buf.extend_from_slice(b" /Kids [");
    for p in 0..pages.len() {
        if p > 0 {
            buf.push(b' ');
        }
        buf.extend_from_slice(format!("{} 0 R", 5 + 2 * p).as_bytes());
    }
    buf.extend_from_slice(b"] >>\nendobj\n");

    offsets[3] = buf.len();
    buf.extend_from_slice(
        b"3 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>\nendobj\n",
    );

    let top = PAGE_H - MARGIN;
    for (p, page_lines) in pages.iter().enumerate() {
        let content_id = 4 + 2 * p;
        let page_id = 5 + 2 * p;

        let mut stream: Vec<u8> = Vec::new();
        stream.extend_from_slice(b"BT\n");
        stream.extend_from_slice(format!("/F1 {FONT_SIZE} Tf\n").as_bytes());
        stream.extend_from_slice(format!("{LEADING} TL\n").as_bytes());
        stream.extend_from_slice(format!("{MARGIN} {top} Td\n").as_bytes());
        for line in *page_lines {
            stream.push(b'(');
            pdf_escape_into(&mut stream, line);
            stream.extend_from_slice(b") Tj\nT*\n");
        }
        stream.extend_from_slice(b"ET\n");

        offsets[content_id] = buf.len();
        buf.extend_from_slice(format!("{content_id} 0 obj\n<< /Length ").as_bytes());
        buf.extend_from_slice(stream.len().to_string().as_bytes());
        buf.extend_from_slice(b" >>\nstream\n");
        buf.extend_from_slice(&stream);
        buf.extend_from_slice(b"\nendstream\nendobj\n");

        offsets[page_id] = buf.len();
        buf.extend_from_slice(
            format!(
                "{page_id} 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {PAGE_W} {PAGE_H}] /Resources << /Font << /F1 3 0 R >> >> /Contents {content_id} 0 R >>\nendobj\n"
            )
            .as_bytes(),
        );
    }

    let xref_start = buf.len();
    buf.extend_from_slice(format!("xref\n0 {}\n", total_objs + 1).as_bytes());
    buf.extend_from_slice(b"0000000000 65535 f \n");
    for off in offsets.iter().skip(1) {
        buf.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    buf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF\n",
            total_objs + 1
        )
        .as_bytes(),
    );

    buf
}

fn wrap_lines(text: &str, max_chars: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let raw = raw.trim_end_matches('\r');
        if raw.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut current = String::new();
        for word in raw.split(' ') {
            if word.is_empty() {
                continue;
            }
            let mut word = word;
            while word.chars().count() > max_chars {
                let take: String = word.chars().take(max_chars).collect();
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
                out.push(take.clone());
                let consumed = take.len();
                word = &word[consumed..];
            }
            let extra = usize::from(!current.is_empty());
            if current.chars().count() + extra + word.chars().count() > max_chars
                && !current.is_empty()
            {
                out.push(std::mem::take(&mut current));
            }
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(word);
        }
        out.push(current);
    }
    out
}

fn pdf_escape_into(out: &mut Vec<u8>, s: &str) {
    for ch in s.chars() {
        let c = ch as u32;
        let byte = if c == 0x09 {
            b' '
        } else if (0x20..=0x7e).contains(&c) || (0xa0..=0xff).contains(&c) {
            c as u8
        } else {
            b'?'
        };
        if matches!(byte, b'(' | b')' | b'\\') {
            out.push(b'\\');
        }
        out.push(byte);
    }
}

fn open_zip(bytes: &[u8]) -> Result<ZipBytes<'_>> {
    ZipArchive::new(Cursor::new(bytes)).context("not a valid office (zip) file")
}

fn read_entry(zip: &mut ZipBytes<'_>, name: &str) -> Result<String> {
    let mut file = zip
        .by_name(name)
        .with_context(|| format!("missing zip entry {name}"))?;
    let mut buf = String::new();
    file.read_to_string(&mut buf)?;
    Ok(buf)
}

fn extract_pdf(bytes: &[u8]) -> Result<String> {
    pdf_extract::extract_text_from_mem(bytes)
        .map_err(|e| anyhow!("pdf text extraction failed: {e}"))
}

fn slide_number(name: &str) -> u32 {
    name.rsplit(['e', '.'])
        .find_map(|seg| seg.parse().ok())
        .unwrap_or(u32::MAX)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::io::Write;

    use zip::CompressionMethod;
    use zip::write::{SimpleFileOptions, ZipWriter};

    use super::*;

    fn zip_with(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = ZipWriter::new(&mut cursor);
            let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
            for (name, content) in entries {
                writer.start_file(*name, opts).unwrap();
                writer.write_all(content).unwrap();
            }
            writer.finish().unwrap();
        }
        cursor.into_inner()
    }

    fn placement_for(text: &str, needle: &str, surrogate: &str) -> Placement {
        let start = text
            .find(needle)
            .unwrap_or_else(|| panic!("needle {needle:?} not in {text:?}"));
        Placement {
            start,
            end: start + needle.len(),
            surrogate: surrogate.to_string(),
        }
    }

    fn entry_of(bytes: &[u8], name: &str) -> String {
        let mut zip = open_zip(bytes).unwrap();
        read_entry(&mut zip, name).unwrap()
    }

    #[test]
    fn docx_anonymizes_in_place_and_stays_a_docx() {
        let xml = br#"<?xml version="1.0"?><w:document xmlns:w="x"><w:body>
            <w:p><w:r><w:t>Hello </w:t></w:r><w:r><w:t>Sarah Connor</w:t></w:r></w:p>
            <w:p><w:r><w:t>email sarah@skynet.com</w:t></w:r></w:p>
            </w:body></w:document>"#;
        let bytes = zip_with(&[("word/document.xml", xml)]);
        let plan = plan(&bytes, "resume.docx").unwrap();
        assert!(
            plan.text.contains("Hello Sarah Connor"),
            "got: {:?}",
            plan.text
        );

        let placements = vec![
            placement_for(&plan.text, "Sarah Connor", "Trinity Vance"),
            placement_for(&plan.text, "sarah@skynet.com", "neo@example.com"),
        ];
        let out = plan.finish(&placements).unwrap();
        assert_eq!(out.mime, MIME_DOCX);

        let after = self::plan(&out.data, "resume.docx").unwrap().text;
        assert!(after.contains("Trinity Vance"), "got: {after:?}");
        assert!(after.contains("neo@example.com"), "got: {after:?}");
        assert!(!after.contains("Sarah Connor"), "leaked: {after:?}");
        assert!(!after.contains("sarah@skynet.com"), "leaked: {after:?}");
        let raw = entry_of(&out.data, "word/document.xml");
        assert!(!raw.contains("Sarah Connor"), "raw xml leak: {raw:?}");
    }

    #[test]
    fn span_crossing_run_boundary_is_spliced_once() {
        let xml = br#"<?xml version="1.0"?><w:document xmlns:w="x"><w:body>
            <w:p><w:r><w:t>Sarah </w:t></w:r><w:r><w:t>Connor</w:t></w:r></w:p>
            </w:body></w:document>"#;
        let bytes = zip_with(&[("word/document.xml", xml)]);
        let plan = plan(&bytes, "x.docx").unwrap();
        assert!(plan.text.contains("Sarah Connor"), "got: {:?}", plan.text);
        let placements = vec![placement_for(&plan.text, "Sarah Connor", "Trinity Vance")];
        let out = plan.finish(&placements).unwrap();
        let after = self::plan(&out.data, "x.docx").unwrap().text;
        assert!(after.contains("Trinity Vance"), "got: {after:?}");
        assert!(!after.contains("Sarah"), "leaked head: {after:?}");
        assert!(!after.contains("Connor"), "leaked tail: {after:?}");
    }

    #[test]
    fn xlsx_shared_strings_are_rewritten() {
        let xml = br#"<?xml version="1.0"?><sst xmlns="x">
            <si><t>Kyle Reese</t></si>
            <si><t>kyle@resistance.org</t></si>
            </sst>"#;
        let bytes = zip_with(&[("xl/sharedStrings.xml", xml)]);
        let plan = plan(&bytes, "contacts.xlsx").unwrap();
        let placements = vec![placement_for(&plan.text, "Kyle Reese", "Case Naha")];
        let out = plan.finish(&placements).unwrap();
        assert_eq!(out.mime, MIME_XLSX);
        let after = self::plan(&out.data, "contacts.xlsx").unwrap().text;
        assert!(after.contains("Case Naha"), "got: {after:?}");
        assert!(!after.contains("Kyle Reese"), "leaked: {after:?}");
    }

    #[test]
    fn pptx_plans_slides_in_numeric_order() {
        let slide = |t: &str| {
            format!(
                r#"<?xml version="1.0"?><p:sld xmlns:a="a"><a:p><a:r><a:t>{t}</a:t></a:r></a:p></p:sld>"#
            )
        };
        let s1 = slide("slide one Miles Dyson");
        let s2 = slide("slide two later");
        let bytes = zip_with(&[
            ("ppt/slides/slide2.xml", s2.as_bytes()),
            ("ppt/slides/slide1.xml", s1.as_bytes()),
        ]);
        let plan = plan(&bytes, "deck.pptx").unwrap();
        let one = plan.text.find("slide one").unwrap();
        let two = plan.text.find("slide two").unwrap();
        assert!(one < two, "slides out of order: {:?}", plan.text);
        let placements = vec![placement_for(&plan.text, "Miles Dyson", "Hiro Tanaka")];
        let out = plan.finish(&placements).unwrap();
        let after = self::plan(&out.data, "deck.pptx").unwrap().text;
        assert!(after.contains("Hiro Tanaka"), "got: {after:?}");
        assert!(!after.contains("Miles Dyson"), "leaked: {after:?}");
    }

    #[test]
    fn chart_text_is_anonymized() {
        let doc = br#"<?xml version="1.0"?><w:document xmlns:w="x"><w:body><w:p><w:r><w:t>see chart</w:t></w:r></w:p></w:body></w:document>"#;
        let chart = br#"<?xml version="1.0"?><c:chartSpace xmlns:c="c" xmlns:a="a"><c:title><a:t>Sales by Lars Holm</a:t></c:title><c:cat><c:strRef><c:strCache><c:pt><c:v>Region Erik Vold</c:v></c:pt></c:strCache></c:strRef></c:cat></c:chartSpace>"#;
        let bytes = zip_with(&[
            ("word/document.xml", doc),
            ("word/charts/chart1.xml", chart),
        ]);
        let plan = plan(&bytes, "x.docx").unwrap();
        assert!(
            plan.text.contains("Lars Holm"),
            "title missing: {:?}",
            plan.text
        );
        assert!(
            plan.text.contains("Erik Vold"),
            "cached value missing: {:?}",
            plan.text
        );
        let placements = vec![
            placement_for(&plan.text, "Lars Holm", "Jet Black"),
            placement_for(&plan.text, "Erik Vold", "Ash Crow"),
        ];
        let out = plan.finish(&placements).unwrap();
        let chart_after = entry_of(&out.data, "word/charts/chart1.xml");
        assert!(
            chart_after.contains("Jet Black"),
            "title not rewritten: {chart_after:?}"
        );
        assert!(
            chart_after.contains("Ash Crow"),
            "cache not rewritten: {chart_after:?}"
        );
        assert!(
            !chart_after.contains("Lars Holm"),
            "title leaked: {chart_after:?}"
        );
        assert!(
            !chart_after.contains("Erik Vold"),
            "cache leaked: {chart_after:?}"
        );
    }

    #[test]
    fn alt_text_attribute_is_anonymized() {
        let doc = br#"<?xml version="1.0"?><w:document xmlns:w="x" xmlns:wp="wp"><w:body><w:p><w:r><w:drawing><wp:docPr id="1" name="Picture 1" descr="headshot of Reese Tanner" title="shot by Wade Cole"/></w:drawing></w:r></w:p></w:body></w:document>"#;
        let bytes = zip_with(&[("word/document.xml", doc)]);
        let plan = plan(&bytes, "x.docx").unwrap();
        assert!(
            plan.text.contains("Reese Tanner"),
            "descr missing: {:?}",
            plan.text
        );
        assert!(
            plan.text.contains("Wade Cole"),
            "title missing: {:?}",
            plan.text
        );
        let placements = vec![
            placement_for(&plan.text, "Reese Tanner", "Kai Mercer"),
            placement_for(&plan.text, "Wade Cole", "Rex Vale"),
        ];
        let out = plan.finish(&placements).unwrap();
        let raw = entry_of(&out.data, "word/document.xml");
        assert!(raw.contains("Kai Mercer"), "descr not rewritten: {raw:?}");
        assert!(raw.contains("Rex Vale"), "title not rewritten: {raw:?}");
        assert!(!raw.contains("Reese Tanner"), "descr leaked: {raw:?}");
        assert!(!raw.contains("Wade Cole"), "title leaked: {raw:?}");
        assert!(raw.contains("Picture 1"), "non-target attr lost: {raw:?}");
    }

    #[test]
    fn custom_properties_are_anonymized() {
        let doc = br#"<?xml version="1.0"?><w:document xmlns:w="x"><w:body><w:p><w:r><w:t>body</w:t></w:r></w:p></w:body></w:document>"#;
        let custom = br#"<?xml version="1.0"?><Properties xmlns:vt="v"><property name="Owner"><vt:lpwstr>Dana Frost</vt:lpwstr></property></Properties>"#;
        let bytes = zip_with(&[("word/document.xml", doc), ("docProps/custom.xml", custom)]);
        let plan = plan(&bytes, "x.docx").unwrap();
        assert!(
            plan.text.contains("Dana Frost"),
            "custom prop missing: {:?}",
            plan.text
        );
        let placements = vec![placement_for(&plan.text, "Dana Frost", "Nova Pike")];
        let out = plan.finish(&placements).unwrap();
        let after = entry_of(&out.data, "docProps/custom.xml");
        assert!(after.contains("Nova Pike"), "not rewritten: {after:?}");
        assert!(!after.contains("Dana Frost"), "leaked: {after:?}");
    }

    #[test]
    fn author_metadata_is_scrubbed() {
        let doc = br#"<?xml version="1.0"?><w:document xmlns:w="x"><w:body><w:p><w:r><w:t>body</w:t></w:r></w:p></w:body></w:document>"#;
        let core = br#"<?xml version="1.0"?><cp:coreProperties xmlns:cp="c" xmlns:dc="d"><dc:creator>Real Author</dc:creator><cp:lastModifiedBy>Real Editor</cp:lastModifiedBy></cp:coreProperties>"#;
        let bytes = zip_with(&[("word/document.xml", doc), ("docProps/core.xml", core)]);
        let plan = plan(&bytes, "x.docx").unwrap();
        let out = plan.finish(&[]).unwrap();
        let scrubbed = entry_of(&out.data, "docProps/core.xml");
        assert!(
            !scrubbed.contains("Real Author"),
            "creator leaked: {scrubbed:?}"
        );
        assert!(
            !scrubbed.contains("Real Editor"),
            "editor leaked: {scrubbed:?}"
        );
        assert!(scrubbed.contains("creator"), "structure lost: {scrubbed:?}");
    }

    #[test]
    fn non_text_entries_are_preserved_verbatim() {
        let doc = br#"<?xml version="1.0"?><w:document xmlns:w="x"><w:body><w:p><w:r><w:t>Neo</w:t></w:r></w:p></w:body></w:document>"#;
        let media: &[u8] = &[0x89, 0x50, 0x4E, 0x47, 1, 2, 3, 4, 5];
        let bytes = zip_with(&[("word/document.xml", doc), ("word/media/image1.png", media)]);
        let plan = plan(&bytes, "x.docx").unwrap();
        let out = plan.finish(&[]).unwrap();
        let mut zip = open_zip(&out.data).unwrap();
        let mut buf = Vec::new();
        zip.by_name("word/media/image1.png")
            .unwrap()
            .read_to_end(&mut buf)
            .unwrap();
        assert_eq!(buf, media, "media corrupted");
    }

    #[test]
    fn pdf_generation_round_trips_text() {
        let pdf = generate_pdf("first line\nsecret token ZZTOKENZZ here\nthird line");
        assert!(pdf.starts_with(b"%PDF"), "not a pdf");
        let extracted = pdf_extract::extract_text_from_mem(&pdf).unwrap();
        assert!(
            extracted.contains("ZZTOKENZZ"),
            "round-trip lost token: {extracted:?}"
        );
    }

    #[test]
    fn pdf_plan_finish_stays_pdf() {
        let input = generate_pdf("contact ZZNAMEZZ now");
        let plan = plan(&input, "doc.pdf").unwrap();
        assert!(plan.text.contains("ZZNAMEZZ"), "got: {:?}", plan.text);
        let placements = vec![placement_for(&plan.text, "ZZNAMEZZ", "ZZFAKEZZ")];
        let out = plan.finish(&placements).unwrap();
        assert_eq!(out.mime, MIME_PDF);
        assert!(out.data.starts_with(b"%PDF"));
        let extracted = pdf_extract::extract_text_from_mem(&out.data).unwrap();
        assert!(extracted.contains("ZZFAKEZZ"), "got: {extracted:?}");
        assert!(!extracted.contains("ZZNAMEZZ"), "leaked: {extracted:?}");
    }

    #[test]
    fn unknown_extension_is_rejected() {
        let bytes = [0u8, 159, 146, 150, 255, 1, 2, 3];
        let err = plan(&bytes, "mystery.bin").unwrap_err();
        assert!(err.to_string().contains("unsupported"), "got: {err}");
    }

    #[test]
    fn corrupt_office_file_errors() {
        let err = plan(b"not a zip", "broken.docx").unwrap_err();
        assert!(err.to_string().contains("office"), "got: {err}");
    }
}
