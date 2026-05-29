use super::PiiSpan;

const SENTINEL: &str = " ";

#[derive(Clone, Copy)]
struct Seg {
    masked_start: usize,

    len: usize,

    orig_start: usize,

    orig_end: usize,
    sentinel: bool,
}

pub(crate) struct Masked {
    pub(crate) text: String,
    segs: Vec<Seg>,
}

pub(crate) fn mask(text: &str, spans: &[PiiSpan]) -> Masked {
    let mut out = String::with_capacity(text.len());
    let mut segs: Vec<Seg> = Vec::with_capacity(spans.len() * 2 + 1);
    let mut cursor = 0usize;

    for span in spans {
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
    pub(crate) fn map_start(&self, pos: usize) -> usize {
        self.map(pos, false)
    }

    pub(crate) fn map_end(&self, pos: usize) -> usize {
        self.map(pos, true)
    }

    fn map(&self, pos: usize, is_end: bool) -> usize {
        if self.segs.is_empty() {
            return pos;
        }

        let idx = self.segs.partition_point(|s| s.masked_start <= pos);
        if idx == 0 {
            return self.segs[0].orig_start;
        }
        let seg = &self.segs[idx - 1];
        if seg.sentinel {
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

        assert_eq!(masked.text, "    tail");
        let mpos = masked.text.find("tail").unwrap();
        assert_eq!(
            &text[masked.map_start(mpos)..masked.map_end(mpos + 4)],
            "tail"
        );
    }
}
