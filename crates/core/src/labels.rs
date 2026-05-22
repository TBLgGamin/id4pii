use std::collections::BTreeMap;

use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    AccountNumber,
    PrivateAddress,
    PrivateDate,
    PrivateEmail,
    PrivatePerson,
    PrivatePhone,
    PrivateUrl,
    Secret,
}

impl Category {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AccountNumber => "account_number",
            Self::PrivateAddress => "private_address",
            Self::PrivateDate => "private_date",
            Self::PrivateEmail => "private_email",
            Self::PrivatePerson => "private_person",
            Self::PrivatePhone => "private_phone",
            Self::PrivateUrl => "private_url",
            Self::Secret => "secret",
        }
    }

    fn from_raw(raw: &str) -> Option<Self> {
        Some(match raw {
            "account_number" => Self::AccountNumber,
            "private_address" => Self::PrivateAddress,
            "private_date" => Self::PrivateDate,
            "private_email" => Self::PrivateEmail,
            "private_person" => Self::PrivatePerson,
            "private_phone" => Self::PrivatePhone,
            "private_url" => Self::PrivateUrl,
            "secret" => Self::Secret,
            _ => return None,
        })
    }
}

pub(crate) fn load_label_map(id2label: &BTreeMap<String, String>) -> Result<Vec<Option<Category>>> {
    let mut entries: Vec<(usize, Option<Category>)> = Vec::with_capacity(id2label.len());
    for (key, value) in id2label {
        let index: usize = key
            .parse()
            .map_err(|_| Error::Model(format!("invalid label id: {key}")))?;
        entries.push((index, parse_label(value)?));
    }
    entries.sort_by_key(|(index, _)| *index);
    for (expected, (index, _)) in entries.iter().enumerate() {
        if expected != *index {
            return Err(Error::Model("label ids are not contiguous".into()));
        }
    }
    Ok(entries.into_iter().map(|(_, category)| category).collect())
}

fn parse_label(label: &str) -> Result<Option<Category>> {
    if label == "O" {
        return Ok(None);
    }
    let raw = label.split_once('-').map_or(label, |(_, rest)| rest);
    Category::from_raw(raw)
        .map(Some)
        .ok_or_else(|| Error::Model(format!("unknown label: {label}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bioes_and_outside() {
        assert_eq!(parse_label("O").unwrap(), None);
        assert_eq!(
            parse_label("B-private_email").unwrap(),
            Some(Category::PrivateEmail)
        );
        assert_eq!(
            parse_label("S-account_number").unwrap(),
            Some(Category::AccountNumber)
        );
        assert!(parse_label("B-mystery").is_err());
    }

    #[test]
    fn builds_contiguous_label_vector() {
        let mut map = BTreeMap::new();
        map.insert("0".to_string(), "O".to_string());
        map.insert("1".to_string(), "B-secret".to_string());
        map.insert("2".to_string(), "E-secret".to_string());
        let labels = load_label_map(&map).unwrap();
        assert_eq!(
            labels,
            vec![None, Some(Category::Secret), Some(Category::Secret)]
        );
    }

    #[test]
    fn rejects_non_contiguous_ids() {
        let mut map = BTreeMap::new();
        map.insert("0".to_string(), "O".to_string());
        map.insert("2".to_string(), "B-secret".to_string());
        assert!(load_label_map(&map).is_err());
    }
}
