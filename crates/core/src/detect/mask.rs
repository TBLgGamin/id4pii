//! Masking: rewrite the text so the regex-found PII is gone before the model sees it, and
//! remember how to translate model span offsets back to the original text.
//!
//! Each regex match is replaced by a single-space [`SENTINEL`]. This shrinks the token count
//! the transformer must process (the whole point — the model is the expensive stage) while
//! keeping word boundaries intact, and the model tags the lone space as "outside", so model
//! spans never land on a sentinel. The original `gap → sentinel → gap …` layout is recorded as
//! a segment list so [`Masked::map_start`] / [`Masked::map_end`] can map a masked-coordinate
//! offset back to the original document in `O(log segments)`.

use super::PiiSpan;

/// Replacement for one masked span. A single space: minimal tokens, preserves word boundaries,
/// and is reliably classified "outside" by the model.
const SENTINEL: &str = " ";

#[derive(Clone, Copy)]
struct Seg {
    /// Start offset of this segment within the masked text.
    masked_start: usize,
    /// Byte length of this segment within the masked text.
    len: usize,
    /// For a copied gap: the original offset the gap begins at (mapping is `+ within`).
    /// For a sentinel: the original start of the masked-out span.
    orig_start: usize,
    /// For a sentinel: the original end of the masked-out span. Unused for gaps.
    orig_end: usize,
    sentinel: bool,
}

/// The masked text plus the segment map needed to undo the coordinate shift.
pub(crate) struct Masked {
    pub(crate) text: String,
    segs: Vec<Seg>,
}

/// Replace each span in `spans` (assumed sorted by `start` and non-overlapping, as produced by
/// the regex detector) with a [`SENTINEL`], returning the masked text and its segment map.
pub(crate) fn mask(text: &str, spans: &[PiiSpan]) -> Masked {
    let mut out = String::with_capacity(text.len());
    let mut segs: Vec<Seg> = Vec::with_capacity(spans.len() * 2 + 1);
    let mut cursor = 0usize;

    for span in spans {
        // Defensive: skip anything that would overlap already-consumed text or is out of range.
        if span.start < cursor || span.end > text.len() || span.start >= span.end {
            continue;
        }
        if span.start > cursor {
            let gap = &text[cursor..span.start];
            segs.push(Seg {
                masked_start: out.len(),
                len: gap.len(),
                orig_start: cursor,
                orig_end: cursor,
                sentinel: false,
            });
            out.push_str(gap);
        }
        segs.push(Seg {
            masked_start: out.len(),
            len: SENTINEL.len(),
            orig_start: span.start,
            orig_end: span.end,
            sentinel: true,
        });
        out.push_str(SENTINEL);
        cursor = span.end;
    }

    if cursor < text.len() {
        let gap = &text[cursor..];
        segs.push(Seg {
            masked_start: out.len(),
            len: gap.len(),
            orig_start: cursor,
            orig_end: cursor,
            sentinel: false,
        });
        out.push_str(gap);
    }

    Masked { text: out, segs }
}

impl Masked {
    /// Map a span-start offset in masked coordinates back to the original text.
    pub(crate) fn map_start(&self, pos: usize) -> usize {
        self.map(pos, false)
    }

    /// Map a span-end offset in masked coordinates back to the original text.
    pub(crate) fn map_end(&self, pos: usize) -> usize {
        self.map(pos, true)
    }

    fn map(&self, pos: usize, is_end: bool) -> usize {
        if self.segs.is_empty() {
            return pos;
        }
        // Largest segment whose masked_start <= pos.
        let idx = self.segs.partition_point(|s| s.masked_start <= pos);
        if idx == 0 {
            return self.segs[0].orig_start;
        }
        let seg = &self.segs[idx - 1];
        if seg.sentinel {
            // Model spans are trimmed of whitespace and never include the lone sentinel space,
            // so this is a defensive fallback: collapse onto the original masked-out range.
            if is_end { seg.orig_end } else { seg.orig_start }
        } else {
            let within = (pos - seg.masked_start).min(seg.len);
            seg.orig_start + within
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::labels::Category;

    fn span(start: usize, end: usize, text: &str) -> PiiSpan {
        PiiSpan {
            category: Category::PrivateEmail,
            start,
            end,
            text: text.to_string(),
            score: 1.0,
        }
    }

    #[test]
    fn masks_span_and_shrinks_text() {
        let text = "mail a@b.com now";
        let masked = mask(text, &[span(5, 12, "a@b.com")]);
        assert_eq!(masked.text, "mail   now");
    }

    #[test]
    fn remaps_offsets_back_to_original() {
        let text = "mail a@b.com then ping bob now";
        let spans = [span(5, 12, "a@b.com")];
        let masked = mask(text, &spans);
        // "mail   then ping bob now" — locate "bob" in the masked text and map it back.
        let mpos = masked.text.find("bob").unwrap();
        let start = masked.map_start(mpos);
        let end = masked.map_end(mpos + 3);
        assert_eq!(&text[start..end], "bob");
    }

    #[test]
    fn no_spans_is_identity() {
        let text = "nothing to mask here";
        let masked = mask(text, &[]);
        assert_eq!(masked.text, text);
        assert_eq!(masked.map_start(4), 4);
        assert_eq!(masked.map_end(7), 7);
    }

    #[test]
    fn handles_adjacent_and_trailing_spans() {
        let text = "x@y.com a@b.com tail";
        let spans = [span(0, 7, "x@y.com"), span(8, 15, "a@b.com")];
        let masked = mask(text, &spans);
        // sentinel + preserved inter-span space + sentinel + " tail".
        assert_eq!(masked.text, "    tail");
        let mpos = masked.text.find("tail").unwrap();
        assert_eq!(
            &text[masked.map_start(mpos)..masked.map_end(mpos + 4)],
            "tail"
        );
    }
}
