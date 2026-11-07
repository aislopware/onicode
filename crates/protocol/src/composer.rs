//! Composer tokens, independent of the UI and the disk.
use std::ops::Range;
use std::path::Path;

use crate::files::ProjectFile;

pub const MAX_ATTACHMENTS: usize = 20;
pub const MAX_EMBED_BYTES: u64 = 20 * 1024 * 1024;

/// quoteDraft.ts: append selected rendered text without replacing a live draft.
pub fn append_selection_quote(draft: &str, text: &str) -> String {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let text = normalized.trim();
    if text.is_empty() {
        return draft.into();
    }
    let quote = text
        .split('\n')
        .map(|line| {
            if line.is_empty() {
                ">".to_owned()
            } else {
                format!("> {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let separator = if draft.is_empty() || draft.ends_with("\n\n") {
        ""
    } else if draft.ends_with('\n') {
        "\n"
    } else {
        "\n\n"
    };
    format!("{draft}{separator}{quote}\n\n")
}

#[cfg(test)]
mod quote_tests {
    use super::*;
    #[test]
    fn quote_preserves_existing_draft_blank_lines_and_unicode() {
        assert_eq!(
            append_selection_quote("draft", " one\r\n\r\ncafé 🙂 "),
            "draft\n\n> one\n>\n> café 🙂\n\n"
        );
        assert_eq!(append_selection_quote("draft\n", "x"), "draft\n\n> x\n\n");
        assert_eq!(append_selection_quote("draft\n\n", "x"), "draft\n\n> x\n\n");
        assert_eq!(append_selection_quote("unchanged", " \r\n "), "unchanged");
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    Skill,
    Mention,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub trigger: Trigger,
    pub range: Range<usize>,
    pub query: String,
}

/// Byte offsets match the native editor. A trigger must start on a word boundary;
/// paths, URLs, emails, selections and IME marked text never become commands.
pub fn token_at(text: &str, cursor: usize) -> Option<Token> {
    if !text.is_char_boundary(cursor) {
        return None;
    }
    let start = text[..cursor]
        .char_indices()
        .rev()
        .find(|(_, c)| c.is_whitespace())
        .map_or(0, |(i, c)| i + c.len_utf8());
    let fragment = &text[start..cursor];
    let trigger = match fragment.chars().next()? {
        '/' => Trigger::Skill,
        '@' => Trigger::Mention,
        _ => return None,
    };
    let query = &fragment[1..];
    if query.contains(['@', '\n', '\r'])
        || (trigger == Trigger::Skill && query.contains(['/', '.', '\\']))
    {
        return None;
    }
    if text[..start]
        .lines()
        .last()
        .is_some_and(|line| line.trim_start().starts_with('>'))
    {
        return None;
    }
    let end = text[cursor..]
        .find(char::is_whitespace)
        .map_or(text.len(), |i| cursor + i);
    Some(Token {
        trigger,
        range: start..end,
        query: query.to_owned(),
    })
}

pub fn consume_command<'a>(text: &'a str, command: &str) -> Option<&'a str> {
    let text = text.trim_start();
    let end = text.find(char::is_whitespace).unwrap_or(text.len());
    text[..end]
        .eq_ignore_ascii_case(&format!("/{command}"))
        .then(|| text[end..].trim_start())
}

/// Prefer leaf and consecutive matches, then a compact fuzzy subsequence.
pub fn score(query: &str, value: &str) -> Option<i64> {
    let query = query.to_lowercase();
    let value = value.to_lowercase();
    if query.is_empty() {
        return Some(0);
    }
    if value == query {
        return Some(10_000);
    }
    let leaf = value.rsplit('/').next().unwrap_or(&value);
    if leaf.starts_with(&query) {
        return Some(5_000 - value.len() as i64);
    }
    if let Some(at) = value.find(&query) {
        return Some(3_000 - at as i64 - value.len() as i64);
    }
    let mut rest = value.as_str();
    let mut gaps = 0;
    for ch in query.chars() {
        let at = rest.find(ch)?;
        gaps += at;
        rest = &rest[at + ch.len_utf8()..];
    }
    Some(1_000 - gaps as i64 - value.len() as i64)
}
pub fn rank_files<'a>(files: &'a [ProjectFile], query: &str, limit: usize) -> Vec<&'a ProjectFile> {
    let mut matches: Vec<_> = files
        .iter()
        .filter_map(|f| score(query, &f.relative).map(|s| (s, f)))
        .collect();
    matches.sort_unstable_by(|(a, x), (b, y)| b.cmp(a).then_with(|| x.relative.cmp(&y.relative)));
    matches.truncate(limit);
    matches.into_iter().map(|(_, file)| file).collect()
}
pub fn image_mime(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}
pub fn mention_text(relative: &str) -> String {
    if relative.chars().any(char::is_whitespace) {
        format!("@\"{}\"", relative.replace('"', "\\\""))
    } else {
        format!("@{relative}")
    }
}
pub fn highlight_ranges(
    text: &str,
    known: &std::collections::HashSet<String>,
) -> Vec<Range<usize>> {
    static TOKENS: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let pattern = TOKENS.get_or_init(|| {
        regex::Regex::new(r#"(?m)(?:^|\s)(/[a-z0-9][a-z0-9:-]*|@"(?:\\.|[^"\r\n])*"|@[^\s]+)"#)
            .expect("composer token pattern")
    });
    pattern
        .captures_iter(text)
        .filter_map(|capture| {
            let token = capture.get(1)?;
            if !known.contains(token.as_str()) {
                return None;
            }
            let line_start = text[..token.start()].rfind('\n').map_or(0, |i| i + 1);
            (!text[line_start..token.start()]
                .trim_start()
                .starts_with('>'))
            .then(|| token.range())
        })
        .collect()
}
pub fn mentions(text: &str) -> Vec<String> {
    text.lines()
        .filter(|line| !line.trim_start().starts_with('>'))
        .flat_map(mentions_in_line)
        .collect()
}
fn mentions_in_line(text: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut chars = text.chars().peekable();
    let mut boundary = true;
    while let Some(c) = chars.next() {
        if c == '@' && boundary {
            let quoted = chars.peek() == Some(&'"');
            if quoted {
                chars.next();
            }
            let mut path = String::new();
            while let Some(&next) = chars.peek() {
                if (quoted && next == '"') || (!quoted && next.is_whitespace()) {
                    break;
                }
                path.push(next);
                chars.next();
            }
            if quoted {
                chars.next();
            }
            result.push(path);
        }
        boundary = c.is_whitespace();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn token_boundaries_and_unicode() {
        let text = "漢字 /rev tail";
        assert_eq!(token_at(text, 11).unwrap().range, 7..11);
        assert!(token_at("https://host", 12).is_none());
        assert!(token_at("me@host", 7).is_none());
        assert!(token_at("> /review", 9).is_none());
        assert!(token_at("/tmp/file", 9).is_none());
        assert_eq!(token_at("@src/ma", 7).unwrap().trigger, Trigger::Mention);
    }
    #[test]
    fn replacement_and_commands_have_exact_boundaries() {
        assert_eq!(consume_command(" /PLAN fix this", "plan"), Some("fix this"));
        assert_eq!(consume_command("/plan-extra", "plan"), None);
        assert_eq!(
            mentions("Use @\"src/my file.rs\" and @src/lib.rs"),
            vec!["src/my file.rs", "src/lib.rs"]
        );
        assert_eq!(
            mentions("> @quoted.rs\n  > @also-quoted.rs\nUse @actual.rs"),
            vec!["actual.rs"]
        );
        assert!(score("cpr", "src/composer.rs").is_some());
        assert!(score("xyz", "src/composer.rs").is_none());
    }
}
