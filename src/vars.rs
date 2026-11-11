//! Environment variable editing: validation and diffing edits into API calls.

use std::collections::{HashMap, HashSet};

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Changes {
    /// New or changed variables, sent with `method: "set"`.
    pub upserts: Vec<(String, String)>,
    /// Keys removed by the user.
    pub deletes: Vec<String>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.upserts.is_empty() && self.deletes.is_empty()
    }

    pub fn summary(&self) -> String {
        let changed = self.upserts.len();
        let deleted = self.deletes.len();
        match (changed, deleted) {
            (0, 0) => "No changes".into(),
            (c, 0) => format!("{c} variable(s) set"),
            (0, d) => format!("{d} variable(s) deleted"),
            (c, d) => format!("{c} set, {d} deleted"),
        }
    }
}

pub fn valid_key(k: &str) -> bool {
    let mut chars = k.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Compute the API calls turning `original` into `edited`.
pub fn diff(original: &[(String, String)], edited: &[(String, String)]) -> Result<Changes, String> {
    let mut seen = HashSet::new();
    for (k, _) in edited {
        if !valid_key(k) {
            return Err(format!(
                "Invalid key {k:?}: use letters, digits and _ and don't start with a digit"
            ));
        }
        if !seen.insert(k.as_str()) {
            return Err(format!("Duplicate key {k}"));
        }
    }
    let before: HashMap<&str, &str> = original
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let upserts = edited
        .iter()
        .filter(|(k, v)| before.get(k.as_str()) != Some(&v.as_str()))
        .cloned()
        .collect();
    let deletes = original
        .iter()
        .filter(|(k, _)| !seen.contains(k.as_str()))
        .map(|(k, _)| k.clone())
        .collect();
    Ok(Changes { upserts, deletes })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kv(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn detects_upserts_and_deletes() {
        let orig = kv(&[("A", "1"), ("B", "2"), ("C", "3")]);
        let edit = kv(&[("A", "1"), ("B", "20"), ("D", "4")]);
        let c = diff(&orig, &edit).unwrap();
        assert_eq!(c.upserts, kv(&[("B", "20"), ("D", "4")]));
        assert_eq!(c.deletes, vec!["C".to_string()]);
        assert_eq!(c.summary(), "2 set, 1 deleted");
    }

    #[test]
    fn unchanged_is_empty() {
        let orig = kv(&[("A", "1")]);
        assert!(diff(&orig, &orig).unwrap().is_empty());
    }

    #[test]
    fn rename_is_delete_plus_insert() {
        let c = diff(&kv(&[("OLD", "x")]), &kv(&[("NEW", "x")])).unwrap();
        assert_eq!(c.upserts, kv(&[("NEW", "x")]));
        assert_eq!(c.deletes, vec!["OLD".to_string()]);
    }

    #[test]
    fn rejects_bad_and_duplicate_keys() {
        assert!(diff(&[], &kv(&[("1A", "x")])).is_err());
        assert!(diff(&[], &kv(&[("A-B", "x")])).is_err());
        assert!(diff(&[], &kv(&[("", "x")])).is_err());
        assert!(
            diff(&[], &kv(&[("A", "1"), ("A", "2")]))
                .unwrap_err()
                .contains("Duplicate")
        );
        assert!(valid_key("_PRIVATE_1"));
    }
}
