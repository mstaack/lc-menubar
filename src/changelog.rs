//! `CHANGELOG.md`, embedded at build time and parsed for the "What's new" page.

use serde::Serialize;

const SOURCE: &str = include_str!("../CHANGELOG.md");

#[derive(Debug, Serialize, PartialEq)]
pub struct Release {
    /// "0.1.0", or "Unreleased".
    pub version: String,
    pub date: Option<String>,
    /// ("Added", items), ("Fixed", items), …
    pub groups: Vec<(String, Vec<String>)>,
}

/// Releases with at least one entry, newest first.
pub fn releases() -> Vec<Release> {
    parse(SOURCE)
}

/// Release notes (Markdown, as on GitHub) as plain text for a dialog: headings
/// become labels, list items bullets, and links and code marks are dropped.
pub fn plain(md: &str) -> String {
    let mut out: Vec<String> = vec![];
    for line in md.lines() {
        let line = line.trim_end();
        let text = if let Some(h) = line.trim_start_matches('#').strip_prefix(' ')
            && line.starts_with('#')
        {
            format!("{}:", h.trim())
        } else if let Some(item) = line.trim_start().strip_prefix("- ") {
            format!("• {item}")
        } else {
            line.trim().to_string()
        };
        let text = strip_inline(&text);
        if text.is_empty() && out.last().is_none_or(|l| l.is_empty()) {
            continue;
        }
        out.push(text);
    }
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out.join("\n")
}

/// `code` → code, [text](url) → text, **bold** → bold.
fn strip_inline(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('[') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        match after
            .find("](")
            .and_then(|j| after[j..].find(')').map(|k| (j, j + k)))
        {
            Some((j, end)) => {
                out.push_str(&after[..j]);
                rest = &after[end + 1..];
            }
            None => {
                out.push('[');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out.replace('`', "").replace("**", "")
}

fn parse(src: &str) -> Vec<Release> {
    let mut out: Vec<Release> = vec![];
    for line in src.lines() {
        if let Some(rest) = line.strip_prefix("## [") {
            let Some((version, tail)) = rest.split_once(']') else {
                continue;
            };
            let date = tail.trim_start_matches([' ', '-']).trim().to_string();
            out.push(Release {
                version: version.to_string(),
                date: (!date.is_empty()).then_some(date),
                groups: vec![],
            });
        } else if let Some(name) = line.strip_prefix("### ") {
            if let Some(r) = out.last_mut() {
                r.groups.push((name.trim().to_string(), vec![]));
            }
        } else if let Some(item) = line.strip_prefix("- ") {
            if let Some((_, items)) = out.last_mut().and_then(|r| r.groups.last_mut()) {
                items.push(item.trim().to_string());
            }
        } else if line.starts_with("  ") && !line.trim().is_empty() {
            // Continuation of a wrapped list item.
            if let Some(last) = out
                .last_mut()
                .and_then(|r| r.groups.last_mut())
                .and_then(|(_, items)| items.last_mut())
            {
                last.push(' ');
                last.push_str(line.trim());
            }
        }
    }
    out.retain(|r| r.groups.iter().any(|(_, items)| !items.is_empty()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sections_and_skips_empty_releases() {
        let src = "# Changelog\n\n## [Unreleased]\n\n## [0.2.0] - 2026-11-12\n\n### Fixed\n\n- One\n  continued\n- Two\n\n### Added\n\n- Three\n\n[0.2.0]: https://x\n";
        assert_eq!(
            parse(src),
            vec![Release {
                version: "0.2.0".into(),
                date: Some("2026-11-12".into()),
                groups: vec![
                    ("Fixed".into(), vec!["One continued".into(), "Two".into()]),
                    ("Added".into(), vec!["Three".into()]),
                ],
            }]
        );
    }

    #[test]
    fn notes_become_plain_text() {
        let md = "### Added\n\n- Show `brew` [docs](https://x.y) **now**\n\n### Fixed\n\n- Blank cards\n";
        assert_eq!(
            plain(md),
            "Added:\n\n• Show brew docs now\n\nFixed:\n\n• Blank cards"
        );
    }

    /// Every released version must have its changelog section.
    #[test]
    fn current_version_has_notes() {
        let v = env!("CARGO_PKG_VERSION");
        assert!(
            releases().iter().any(|r| r.version == v),
            "CHANGELOG.md has no entries for {v}"
        );
    }
}
