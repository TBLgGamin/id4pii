use crate::detect::PiiSpan;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedactStyle {
    Label,
    Block,
    Char,
}

#[must_use]
pub fn redact(text: &str, spans: &[PiiSpan], style: RedactStyle) -> String {
    let mut ordered: Vec<&PiiSpan> = spans.iter().collect();
    ordered.sort_by_key(|span| span.start);

    let mut result = String::with_capacity(text.len());
    let mut cursor = 0;
    for span in ordered {
        if span.start < cursor || span.end > text.len() || span.start > span.end {
            continue;
        }
        if let Some(prefix) = text.get(cursor..span.start) {
            result.push_str(prefix);
        }
        result.push_str(&placeholder(span, style));
        cursor = span.end;
    }
    if let Some(rest) = text.get(cursor..) {
        result.push_str(rest);
    }
    result
}

fn placeholder(span: &PiiSpan, style: RedactStyle) -> String {
    match style {
        RedactStyle::Label => format!("[{}]", span.category.as_str().to_uppercase()),
        RedactStyle::Block => "\u{2588}".repeat(span.text.chars().count()),
        RedactStyle::Char => "*".repeat(span.text.chars().count()),
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
    fn replaces_span_with_label() {
        let text = "mail me at a@b.com please";
        let spans = vec![span(11, 18, "a@b.com")];
        assert_eq!(
            redact(text, &spans, RedactStyle::Label),
            "mail me at [PRIVATE_EMAIL] please"
        );
    }

    #[test]
    fn char_style_preserves_length() {
        let text = "id a@b.com";
        let spans = vec![span(3, 10, "a@b.com")];
        assert_eq!(redact(text, &spans, RedactStyle::Char), "id *******");
    }

    #[test]
    fn empty_spans_return_original() {
        assert_eq!(
            redact("nothing here", &[], RedactStyle::Label),
            "nothing here"
        );
    }
}
