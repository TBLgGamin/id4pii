use std::sync::OnceLock;

use regex::Regex;

use super::PiiSpan;
use crate::labels::Category;

struct Spec {
    category: Category,

    luhn: bool,
}

const PATTERNS: &[(&str, Category, bool)] = &[
    (
        r"(?s:-----BEGIN (?:RSA |EC |DSA |OPENSSH |PGP )?PRIVATE KEY-----.*?-----END (?:RSA |EC |DSA |OPENSSH |PGP )?PRIVATE KEY-----)",
        Category::Secret,
        false,
    ),
    (
        r"https://hooks\.slack\.com/services/T[A-Za-z0-9_]+/B[A-Za-z0-9_]+/[A-Za-z0-9_]+",
        Category::Secret,
        false,
    ),
    (
        r"eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}",
        Category::Secret,
        false,
    ),
    (
        r"(?:A3T[A-Z0-9]|AKIA|AGPA|AIDA|AROA|AIPA|ANPA|ANVA|ASIA)[A-Z0-9]{16}",
        Category::Secret,
        false,
    ),
    (
        r"github_pat_[A-Za-z0-9]{22}_[A-Za-z0-9]{59}",
        Category::Secret,
        false,
    ),
    (r"gh[oprsu]_[A-Za-z0-9]{36}", Category::Secret, false),
    (r"AIza[0-9A-Za-z_-]{35}", Category::Secret, false),
    (r"GOCSPX-[A-Za-z0-9_-]{20,40}", Category::Secret, false),
    (r"xox[baprs]-[A-Za-z0-9-]{10,48}", Category::Secret, false),
    (
        r"(?:sk|rk|pk)_(?:live|test)_[0-9A-Za-z]{16,99}",
        Category::Secret,
        false,
    ),
    (r"sk-[A-Za-z0-9_-]{20,}", Category::Secret, false),
    (r"pypi-[A-Za-z0-9_-]{16,}", Category::Secret, false),
    (
        r"(?i:bearer)\s+[A-Za-z0-9._~+/-]{16,}=*",
        Category::Secret,
        false,
    ),
    (
        r"(?:https?|ftp)://[^\s/$.?#][^\s]*",
        Category::PrivateUrl,
        false,
    ),
    (
        r"www\.[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)+(?:/[^\s]*)?",
        Category::PrivateUrl,
        false,
    ),
    (
        r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,24}",
        Category::PrivateEmail,
        false,
    ),
    (
        r"\b[A-Z]{2}[0-9]{2}(?:[ ]?[A-Z0-9]{4}){2,7}(?:[ ]?[A-Z0-9]{1,3})?\b",
        Category::AccountNumber,
        false,
    ),
    (
        r"\b[0-9](?:[ -]?[0-9]){12,18}\b",
        Category::AccountNumber,
        true,
    ),
    (
        r"\b[0-9]{3}-[0-9]{2}-[0-9]{4}\b",
        Category::AccountNumber,
        false,
    ),
    (
        r"\+[0-9]{1,3}[ .-]?(?:\([0-9]{1,4}\)[ .-]?)?[0-9]{2,4}(?:[ .-][0-9]{2,4}){1,4}",
        Category::PrivatePhone,
        false,
    ),
    (
        r"\([0-9]{3}\)[ .-]?[0-9]{3}[ .-]?[0-9]{4}",
        Category::PrivatePhone,
        false,
    ),
    (
        r"\b[0-9]{3}[ .-][0-9]{3}[ .-][0-9]{4}\b",
        Category::PrivatePhone,
        false,
    ),
    (
        r"\b[0-9]{4}-[0-9]{2}-[0-9]{2}\b",
        Category::PrivateDate,
        false,
    ),
    (
        r"\b[0-9]{1,2}[/.][0-9]{1,2}[/.][0-9]{2,4}\b",
        Category::PrivateDate,
        false,
    ),
    (
        r"\b(?i:jan(?:uary)?|feb(?:ruary)?|mar(?:ch)?|apr(?:il)?|may|jun(?:e)?|jul(?:y)?|aug(?:ust)?|sep(?:t(?:ember)?)?|oct(?:ober)?|nov(?:ember)?|dec(?:ember)?)\b\.?\s+[0-9]{1,2}(?:st|nd|rd|th)?(?:,?\s+[0-9]{2,4})?",
        Category::PrivateDate,
        false,
    ),
    (
        r"\b[0-9]{1,2}(?:st|nd|rd|th)?\s+(?i:jan(?:uary)?|feb(?:ruary)?|mar(?:ch)?|apr(?:il)?|may|jun(?:e)?|jul(?:y)?|aug(?:ust)?|sep(?:t(?:ember)?)?|oct(?:ober)?|nov(?:ember)?|dec(?:ember)?)\b(?:,?\s+[0-9]{2,4})?",
        Category::PrivateDate,
        false,
    ),
];

pub(crate) struct RegexDetector {
    combined: Regex,
    specs: Vec<Spec>,
}

static GLOBAL: OnceLock<RegexDetector> = OnceLock::new();

impl RegexDetector {
    pub(crate) fn global() -> &'static RegexDetector {
        GLOBAL.get_or_init(RegexDetector::build)
    }

    fn build() -> Self {
        let mut combined = String::new();
        let mut specs = Vec::with_capacity(PATTERNS.len());
        for (i, (pattern, category, luhn)) in PATTERNS.iter().enumerate() {
            if i > 0 {
                combined.push('|');
            }

            combined.push_str("(?P<g");
            combined.push_str(&i.to_string());
            combined.push('>');
            combined.push_str(pattern);
            combined.push(')');
            specs.push(Spec {
                category: *category,
                luhn: *luhn,
            });
        }

        let combined = Regex::new(&combined)
            .unwrap_or_else(|e| panic!("id4pii built-in PII regex failed to compile: {e}"));
        Self { combined, specs }
    }

    pub(crate) fn detect(&self, text: &str) -> Vec<PiiSpan> {
        let mut spans = Vec::new();
        for caps in self.combined.captures_iter(text) {
            let Some(whole) = caps.get(0) else { continue };

            let Some(group) = (1..caps.len()).find(|&i| caps.get(i).is_some()) else {
                continue;
            };
            let spec = &self.specs[group - 1];
            let (start, end) = (whole.start(), whole.end());
            let slice = &text[start..end];
            if spec.luhn && !luhn_ok(slice) {
                continue;
            }
            spans.push(PiiSpan {
                category: spec.category,
                start,
                end,
                text: slice.to_string(),
                score: 1.0,
            });
        }
        spans
    }
}

fn luhn_ok(s: &str) -> bool {
    let digits: Vec<u8> = s
        .bytes()
        .filter(u8::is_ascii_digit)
        .map(|b| b - b'0')
        .collect();
    if !(13..=19).contains(&digits.len()) {
        return false;
    }
    let mut sum = 0u32;
    for (i, &d) in digits.iter().rev().enumerate() {
        let mut v = u32::from(d);
        if !i.is_multiple_of(2) {
            v *= 2;
            if v > 9 {
                v -= 9;
            }
        }
        sum += v;
    }
    sum.is_multiple_of(10)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cats(text: &str) -> Vec<(Category, &str)> {
        RegexDetector::global()
            .detect(text)
            .into_iter()
            .map(|s| (s.category, &text[s.start..s.end]))
            .collect()
    }

    #[test]
    fn finds_email_url_phone() {
        let text =
            "mail me at a.b+x@corp.co.uk or visit https://example.com/p?q=1 or call 555-123-4567";
        let found = cats(text);
        assert!(found.contains(&(Category::PrivateEmail, "a.b+x@corp.co.uk")));
        assert!(found.contains(&(Category::PrivateUrl, "https://example.com/p?q=1")));
        assert!(found.contains(&(Category::PrivatePhone, "555-123-4567")));
    }

    #[test]
    fn validates_credit_card_with_luhn() {
        let good = RegexDetector::global().detect("card 4111 1111 1111 1111 ok");
        assert!(good.iter().any(|s| s.category == Category::AccountNumber));

        let bad = RegexDetector::global().detect("card 4111 1111 1111 1112 no");
        assert!(!bad.iter().any(|s| s.category == Category::AccountNumber));
    }

    #[test]
    fn finds_iban_and_ssn() {
        let found = cats("acct DE89 3704 0044 0532 0130 00 ssn 123-45-6789");
        assert!(found.iter().any(|(c, _)| *c == Category::AccountNumber));
        assert!(found.iter().any(|(_, t)| *t == "123-45-6789"));
    }

    #[test]
    fn finds_secrets() {
        let found = cats("key AKIAIOSFODNN7EXAMPLE token ghp_0123456789012345678901234567890123ab");
        let secrets: Vec<_> = found
            .iter()
            .filter(|(c, _)| *c == Category::Secret)
            .collect();
        assert_eq!(secrets.len(), 2);
    }

    #[test]
    fn finds_dates_without_eating_words() {
        let found = cats("born 1987-06-15 and on March 3, 2020 but Marketing 5 is not a date");
        let dates: Vec<_> = found
            .iter()
            .filter(|(c, _)| *c == Category::PrivateDate)
            .map(|(_, t)| *t)
            .collect();
        assert!(dates.contains(&"1987-06-15"));
        assert!(dates.iter().any(|d| d.starts_with("March 3")));
        assert!(!dates.iter().any(|d| d.contains("Marketing")));
    }

    #[test]
    fn empty_and_clean_text_yield_nothing() {
        assert!(RegexDetector::global().detect("").is_empty());
        assert!(
            RegexDetector::global()
                .detect("just some perfectly ordinary words here")
                .is_empty()
        );
    }
}
