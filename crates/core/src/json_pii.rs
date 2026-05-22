use serde_json::Value;

use crate::anonymize::{Rng, Vault, anonymize_into, deanonymize};
use crate::detector::Detector;

const CONTENT_KEYS: &[&str] = &[
    "content",
    "text",
    "value",
    "reasoning",
    "input",
    "prompt",
    "query",
];

const MAX_CARRY: usize = 48;

fn is_functional_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    if lower.ends_with("_id") {
        return true;
    }
    matches!(
        lower.as_str(),
        "model"
            | "id"
            | "object"
            | "type"
            | "role"
            | "stream"
            | "mime_type"
            | "media_type"
            | "encoding_format"
            | "format"
            | "finish_reason"
            | "stop_reason"
            | "index"
            | "logprobs"
    )
}

fn is_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    bytes.iter().enumerate().all(|(index, byte)| {
        if matches!(index, 8 | 13 | 18 | 23) {
            *byte == b'-'
        } else {
            byte.is_ascii_hexdigit()
        }
    })
}

fn is_prose(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.len() < 2 {
        return false;
    }
    if is_uuid(trimmed) {
        return false;
    }
    if trimmed.len() > 96 && !trimmed.contains(char::is_whitespace) {
        return false;
    }
    true
}

fn transform_strings(value: &mut Value, transform: &mut impl FnMut(&str) -> Option<String>) {
    match value {
        Value::String(text) if is_prose(text) => {
            if let Some(replaced) = transform(text) {
                *text = replaced;
            }
        }
        Value::Array(items) => {
            for item in items {
                transform_strings(item, transform);
            }
        }
        Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                if !is_functional_key(key) {
                    transform_strings(child, transform);
                }
            }
        }
        _ => {}
    }
}

pub fn anonymize_json(
    value: &mut Value,
    detector: &mut Detector,
    rng: &mut Rng,
    vault: &mut Vault,
) {
    transform_strings(value, &mut |text| match detector.detect(text) {
        Ok(spans) if !spans.is_empty() => Some(anonymize_into(text, &spans, rng, vault)),
        _ => None,
    });
}

pub fn deanonymize_json(value: &mut Value, vault: &Vault) {
    transform_strings(value, &mut |text| {
        let restored = deanonymize(text, vault);
        if restored == text {
            None
        } else {
            Some(restored)
        }
    });
}

fn sorted_pairs(vault: &Vault) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = vault
        .entries
        .iter()
        .filter(|entry| !entry.fake.is_empty())
        .map(|entry| (entry.fake.clone(), entry.real.clone()))
        .collect();
    pairs.sort_by_key(|pair| std::cmp::Reverse(pair.0.len()));
    pairs
}

fn replace_all(text: &str, pairs: &[(String, String)]) -> String {
    let mut result = String::with_capacity(text.len());
    let mut rest = text;
    while !rest.is_empty() {
        let mut matched = false;
        for (fake, real) in pairs {
            if let Some(stripped) = rest.strip_prefix(fake.as_str()) {
                result.push_str(real);
                rest = stripped;
                matched = true;
                break;
            }
        }
        if !matched {
            let mut chars = rest.chars();
            if let Some(character) = chars.next() {
                result.push(character);
                rest = chars.as_str();
            }
        }
    }
    result
}

fn split_streaming(combined: &str, pairs: &[(String, String)]) -> (String, String) {
    let char_count = combined.chars().count();
    if char_count <= MAX_CARRY {
        return (String::new(), combined.to_string());
    }
    let boundary = combined
        .char_indices()
        .nth(char_count - MAX_CARRY)
        .map_or(combined.len(), |(index, _)| index);

    let mut emitted = String::new();
    let mut rest = combined;
    let mut consumed = 0;
    while !rest.is_empty() && consumed < boundary {
        let mut matched = false;
        for (fake, real) in pairs {
            if let Some(stripped) = rest.strip_prefix(fake.as_str()) {
                emitted.push_str(real);
                consumed += fake.len();
                rest = stripped;
                matched = true;
                break;
            }
        }
        if !matched {
            let mut chars = rest.chars();
            if let Some(character) = chars.next() {
                emitted.push(character);
                consumed += character.len_utf8();
                rest = chars.as_str();
            }
        }
    }
    (emitted, rest.to_string())
}

fn content_leaf_mut(value: &mut Value) -> Option<&mut String> {
    match value {
        Value::Object(map) => {
            let direct = map.iter().find_map(|(key, child)| {
                if CONTENT_KEYS.contains(&key.as_str()) && child.is_string() {
                    Some(key.clone())
                } else {
                    None
                }
            });
            if let Some(key) = direct {
                if let Some(Value::String(text)) = map.get_mut(&key) {
                    return Some(text);
                }
                return None;
            }
            for child in map.values_mut() {
                if let Some(found) = content_leaf_mut(child) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(items) => {
            for item in items {
                if let Some(found) = content_leaf_mut(item) {
                    return Some(found);
                }
            }
            None
        }
        _ => None,
    }
}

struct HeldBlock {
    prelude: Vec<String>,
    json: Value,
}

fn serialize_block(block: &HeldBlock) -> String {
    let mut out = String::new();
    for line in &block.prelude {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("data: ");
    out.push_str(&block.json.to_string());
    out.push_str("\n\n");
    out
}

#[derive(Debug)]
pub struct SseDeanonymizer {
    pairs: Vec<(String, String)>,
    raw: String,
    carry: String,
    seen_content: bool,
    held: Option<HeldBlock>,
    tail: Vec<String>,
}

impl std::fmt::Debug for HeldBlock {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("HeldBlock").finish_non_exhaustive()
    }
}

impl SseDeanonymizer {
    #[must_use]
    pub fn new(vault: &Vault) -> Self {
        Self {
            pairs: sorted_pairs(vault),
            raw: String::new(),
            carry: String::new(),
            seen_content: false,
            held: None,
            tail: Vec::new(),
        }
    }

    #[must_use]
    pub fn push(&mut self, chunk: &[u8]) -> Vec<u8> {
        self.raw
            .push_str(&String::from_utf8_lossy(chunk).replace('\r', ""));
        let mut output = String::new();
        while let Some(index) = self.raw.find("\n\n") {
            let block = self.raw[..index].to_string();
            self.raw = self.raw[index + 2..].to_string();
            output.push_str(&self.handle_block(&block));
        }
        output.into_bytes()
    }

    #[must_use]
    pub fn finish(&mut self) -> Vec<u8> {
        let mut output = String::new();
        if let Some(mut held) = self.held.take() {
            if !self.carry.is_empty() {
                let flushed = replace_all(&self.carry, &self.pairs);
                if let Some(leaf) = content_leaf_mut(&mut held.json) {
                    leaf.push_str(&flushed);
                }
            }
            output.push_str(&serialize_block(&held));
        }
        for block in self.tail.drain(..) {
            output.push_str(&block);
            output.push_str("\n\n");
        }
        if !self.raw.is_empty() {
            output.push_str(&self.raw);
            self.raw.clear();
        }
        output.into_bytes()
    }

    fn handle_block(&mut self, block: &str) -> String {
        let mut prelude: Vec<String> = Vec::new();
        let mut data = String::new();
        for line in block.lines() {
            if let Some(rest) = line.strip_prefix("data:") {
                data.push_str(rest.strip_prefix(' ').unwrap_or(rest));
            } else {
                prelude.push(line.to_string());
            }
        }

        let parsed: Option<Value> = if data.is_empty() || data == "[DONE]" {
            None
        } else {
            serde_json::from_str(&data).ok()
        };
        let Some(mut json) = parsed else {
            return self.emit_non_content(block);
        };
        if content_leaf_mut(&mut json).is_none() {
            return self.emit_non_content(block);
        }

        self.seen_content = true;
        let raw_content = content_leaf_mut(&mut json)
            .map(|leaf| leaf.clone())
            .unwrap_or_default();
        let combined = format!("{}{}", self.carry, raw_content);
        let (emittable, new_carry) = split_streaming(&combined, &self.pairs);
        self.carry = new_carry;
        if let Some(leaf) = content_leaf_mut(&mut json) {
            *leaf = emittable;
        }

        let mut output = String::new();
        if let Some(previous) = self.held.take() {
            output.push_str(&serialize_block(&previous));
        }
        self.held = Some(HeldBlock { prelude, json });
        output
    }

    fn emit_non_content(&mut self, block: &str) -> String {
        if self.seen_content {
            self.tail.push(block.to_string());
            String::new()
        } else {
            format!("{block}\n\n")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anonymize::VaultEntry;
    use crate::labels::Category;

    fn vault_with(fake: &str, real: &str) -> Vault {
        Vault {
            entries: vec![VaultEntry {
                category: Category::PrivatePerson,
                real: real.to_string(),
                fake: fake.to_string(),
            }],
        }
    }

    #[test]
    fn deanonymize_json_restores_string_leaves() {
        let vault = vault_with("Paul Organa", "Sarah Connor");
        let mut value: Value = serde_json::from_str(
            r#"{"choices":[{"message":{"content":"hi Paul Organa","role":"assistant"}}]}"#,
        )
        .unwrap();
        deanonymize_json(&mut value, &vault);
        assert_eq!(value["choices"][0]["message"]["content"], "hi Sarah Connor");
    }

    #[test]
    fn functional_keys_are_left_untouched() {
        let vault = vault_with("Paul Organa", "Sarah Connor");
        let mut value: Value =
            serde_json::from_str(r#"{"model":"Paul Organa","note":"Paul Organa"}"#).unwrap();
        deanonymize_json(&mut value, &vault);
        assert_eq!(value["model"], "Paul Organa");
        assert_eq!(value["note"], "Sarah Connor");
    }

    #[test]
    fn streaming_reassembles_surrogate_split_across_events() {
        let vault = vault_with("Paul Organa", "Sarah Connor");
        let mut deanon = SseDeanonymizer::new(&vault);
        let mut output = Vec::new();
        output.extend(deanon.push(b"data: {\"delta\":{\"content\":\"Hi Paul\"}}\n\n"));
        output.extend(deanon.push(b"data: {\"delta\":{\"content\":\" Organa there\"}}\n\n"));
        output.extend(deanon.push(b"data: [DONE]\n\n"));
        output.extend(deanon.finish());

        let text = String::from_utf8(output).unwrap();
        let mut restored = String::new();
        for line in text.lines() {
            if let Some(payload) = line.strip_prefix("data: ")
                && let Ok(value) = serde_json::from_str::<Value>(payload)
                && let Some(content) = value["delta"]["content"].as_str()
            {
                restored.push_str(content);
            }
        }
        assert_eq!(restored, "Hi Sarah Connor there");
        assert!(text.contains("[DONE]"));
    }

    #[test]
    fn streaming_restores_surrogate_before_finish() {
        let vault = vault_with("Paul Organa", "Sarah Connor");
        let mut deanon = SseDeanonymizer::new(&vault);
        let mut output = Vec::new();
        output.extend(deanon.push(b"data: {\"delta\":{\"content\":\"Hi Paul\"}}\n\n"));
        output.extend(deanon.push(b"data: {\"delta\":{\"content\":\" Organa, \"}}\n\n"));
        output.extend(deanon.push(
            b"data: {\"delta\":{\"content\":\"here is a long trailing passage of filler beyond the carry window\"}}\n\n",
        ));
        output.extend(deanon.push(b"data: {\"delta\":{\"content\":\" end\"}}\n\n"));

        let before_finish = String::from_utf8(output).unwrap();
        assert!(
            before_finish.contains("Sarah Connor"),
            "surrogate must be restored mid-stream, before finish(): {before_finish}"
        );
        assert!(!before_finish.contains("Paul Organa"));
    }
}
