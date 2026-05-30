use std::path::Path;

use crate::detect::PiiSpan;
use crate::error::{Error, Result};
use crate::labels::Category;

pub const CATEGORIES: [Category; 8] = [
    Category::AccountNumber,
    Category::PrivateAddress,
    Category::PrivateDate,
    Category::PrivateEmail,
    Category::PrivatePerson,
    Category::PrivatePhone,
    Category::PrivateUrl,
    Category::Secret,
];

fn cat_index(category: Category) -> usize {
    match category {
        Category::AccountNumber => 0,
        Category::PrivateAddress => 1,
        Category::PrivateDate => 2,
        Category::PrivateEmail => 3,
        Category::PrivatePerson => 4,
        Category::PrivatePhone => 5,
        Category::PrivateUrl => 6,
        Category::Secret => 7,
    }
}

#[derive(Debug, Clone)]
pub struct Label {
    pub category: Option<Category>,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone)]
pub struct Example {
    pub text: String,
    pub labels: Vec<Label>,
}

pub fn load_tsv(path: &Path) -> Result<Vec<Example>> {
    let raw = std::fs::read_to_string(path)?;
    let mut out = Vec::with_capacity(2048);
    for line in raw.lines() {
        if line.is_empty() {
            continue;
        }
        let (text_esc, spans_str) = line
            .split_once('\t')
            .ok_or_else(|| Error::Model("dataset line missing tab separator".into()))?;
        let text = unescape(text_esc);
        let mut labels = Vec::new();
        if !spans_str.is_empty() {
            for tok in spans_str.split('|') {
                let mut parts = tok.splitn(3, ':');
                let start = parts.next().and_then(|s| s.parse::<usize>().ok());
                let end = parts.next().and_then(|s| s.parse::<usize>().ok());
                let cat = parts.next();
                match (start, end, cat) {
                    (Some(start), Some(end), Some(cat)) => labels.push(Label {
                        category: Category::from_snake(cat),
                        start,
                        end,
                    }),
                    _ => return Err(Error::Model(format!("malformed span '{tok}'"))),
                }
            }
        }
        out.push(Example { text, labels });
    }
    Ok(out)
}

fn unescape(s: &str) -> String {
    if !s.contains('\\') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('\\') | None => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Counts {
    pub gold: u32,

    pub matched: u32,

    pub good_preds: u32,

    pub fp: u32,
}

impl Counts {
    #[must_use]
    pub fn precision(&self) -> f64 {
        let denom = self.good_preds + self.fp;
        if denom == 0 {
            f64::NAN
        } else {
            f64::from(self.good_preds) / f64::from(denom)
        }
    }

    #[must_use]
    pub fn recall(&self) -> f64 {
        if self.gold == 0 {
            f64::NAN
        } else {
            f64::from(self.matched) / f64::from(self.gold)
        }
    }

    #[must_use]
    pub fn f1(&self) -> f64 {
        let (p, r) = (self.precision(), self.recall());
        if p.is_nan() || r.is_nan() || p + r == 0.0 {
            f64::NAN
        } else {
            2.0 * p * r / (p + r)
        }
    }

    fn add(&mut self, other: Counts) {
        self.gold += other.gold;
        self.matched += other.matched;
        self.good_preds += other.good_preds;
        self.fp += other.fp;
    }
}

#[derive(Debug, Clone, Default)]
pub struct Report {
    pub per_category: [Counts; 8],
}

impl Report {
    #[must_use]
    pub fn overall(&self) -> Counts {
        let mut total = Counts::default();
        for c in &self.per_category {
            total.add(*c);
        }
        total
    }

    #[must_use]
    pub fn format_table(&self) -> String {
        use std::fmt::Write as _;
        let mut s = String::new();
        let _ = writeln!(
            s,
            "{:<16} {:>6} {:>7} {:>4} {:>9} {:>7} {:>6}",
            "category", "gold", "matched", "fp", "precision", "recall", "f1"
        );
        let _ = writeln!(s, "{}", "-".repeat(60));
        for (i, category) in CATEGORIES.iter().enumerate() {
            let c = self.per_category[i];
            if c.gold == 0 && c.good_preds == 0 && c.fp == 0 {
                continue;
            }
            let _ = writeln!(
                s,
                "{:<16} {:>6} {:>7} {:>4} {:>9} {:>7} {:>6}",
                category.as_str(),
                c.gold,
                c.matched,
                c.fp,
                fmt_pct(c.precision()),
                fmt_pct(c.recall()),
                fmt_pct(c.f1()),
            );
        }
        let o = self.overall();
        let _ = writeln!(s, "{}", "-".repeat(60));
        let _ = writeln!(
            s,
            "{:<16} {:>6} {:>7} {:>4} {:>9} {:>7} {:>6}",
            "OVERALL",
            o.gold,
            o.matched,
            o.fp,
            fmt_pct(o.precision()),
            fmt_pct(o.recall()),
            fmt_pct(o.f1()),
        );
        s
    }
}

fn fmt_pct(v: f64) -> String {
    if v.is_nan() {
        "  -  ".to_string()
    } else {
        format!("{:.1}%", v * 100.0)
    }
}

fn overlaps(a_start: usize, a_end: usize, b_start: usize, b_end: usize) -> bool {
    a_start < b_end && b_start < a_end
}

pub fn accumulate(predicted: &[PiiSpan], labels: &[Label], report: &mut Report) {
    for category in CATEGORIES {
        let idx = cat_index(category);

        for label in labels.iter().filter(|l| l.category == Some(category)) {
            report.per_category[idx].gold += 1;
            if predicted
                .iter()
                .any(|p| p.category == category && overlaps(p.start, p.end, label.start, label.end))
            {
                report.per_category[idx].matched += 1;
            }
        }

        for pred in predicted.iter().filter(|p| p.category == category) {
            let hits_gold = labels.iter().any(|l| {
                l.category == Some(category) && overlaps(pred.start, pred.end, l.start, l.end)
            });
            if hits_gold {
                report.per_category[idx].good_preds += 1;
                continue;
            }

            let hits_ignore = labels
                .iter()
                .any(|l| l.category.is_none() && overlaps(pred.start, pred.end, l.start, l.end));
            if !hits_ignore {
                report.per_category[idx].fp += 1;
            }
        }
    }
}

pub fn evaluate<F>(examples: &[Example], mut detect: F) -> Report
where
    F: FnMut(&str) -> Vec<PiiSpan>,
{
    let mut report = Report::default();
    for example in examples {
        let predicted = detect(&example.text);
        accumulate(&predicted, &example.labels, &mut report);
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pred(category: Category, start: usize, end: usize) -> PiiSpan {
        PiiSpan {
            category,
            start,
            end,
            text: String::new(),
            score: 1.0,
        }
    }

    #[test]
    fn unescape_roundtrips_control_chars() {
        assert_eq!(unescape(r"a\nb\tc\\d"), "a\nb\tc\\d");
        assert_eq!(unescape("plain text"), "plain text");
    }

    #[test]
    fn exact_match_is_true_positive() {
        let labels = vec![Label {
            category: Some(Category::PrivateEmail),
            start: 0,
            end: 7,
        }];
        let mut report = Report::default();
        accumulate(&[pred(Category::PrivateEmail, 0, 7)], &labels, &mut report);
        let c = report.per_category[cat_index(Category::PrivateEmail)];
        assert_eq!((c.gold, c.matched, c.good_preds, c.fp), (1, 1, 1, 0));
        assert!((c.f1() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn prediction_over_other_region_is_not_a_false_positive() {
        let labels = vec![Label {
            category: None,
            start: 0,
            end: 10,
        }];
        let mut report = Report::default();
        accumulate(&[pred(Category::PrivatePerson, 2, 6)], &labels, &mut report);
        let c = report.per_category[cat_index(Category::PrivatePerson)];
        assert_eq!((c.good_preds, c.fp), (0, 0));
    }

    #[test]
    fn missed_gold_is_false_negative_and_wrong_pred_is_false_positive() {
        let labels = vec![Label {
            category: Some(Category::PrivatePhone),
            start: 0,
            end: 5,
        }];
        let mut report = Report::default();

        accumulate(&[pred(Category::PrivateUrl, 20, 25)], &labels, &mut report);
        let phone = report.per_category[cat_index(Category::PrivatePhone)];
        let url = report.per_category[cat_index(Category::PrivateUrl)];
        assert_eq!((phone.gold, phone.matched), (1, 0));
        assert_eq!(url.fp, 1);
    }
}
