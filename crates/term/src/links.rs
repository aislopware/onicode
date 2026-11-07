//! Plain URLs in a line of text: the web build's WebLinksAddon pattern,
//! `` (https?|HTTPS?)://[^\s"'!*(){}|\\^<>`]*[^\s"':,.!?{}|\\^~\[\]`()<>] ``, matched as a regex
//! engine would (leftmost, greedy, then backing off until the last character may end a URL), and
//! without one, since the line is short and the pattern fixed.

/// Every match in `text`, left to right and not overlapping, as inclusive index ranges.
#[must_use]
pub fn urls(text: &[char]) -> Vec<(usize, usize)> {
    let mut found = Vec::new();
    let mut at = 0;
    while at < text.len() {
        match url_at(text, at) {
            Some(end) => {
                found.push((at, end));
                at = end + 1;
            }
            None => at += 1,
        }
    }
    found
}

/// Only HTTP(S) opens, as TerminalView's `openTerminalLink` allows.
#[must_use]
pub fn is_web_link(uri: &str) -> bool {
    let lower = uri.get(..8).unwrap_or(uri).to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// The last index of the URL that starts at `start`, if one does.
fn url_at(text: &[char], start: usize) -> Option<usize> {
    let body = start + scheme(text, start)?;
    let mut run = body;
    while text.get(run).copied().is_some_and(in_body) {
        run += 1;
    }
    // The body takes all it can, then gives back one character at a time until the next one
    // may end the URL.
    let mut end = run;
    loop {
        if text.get(end).copied().is_some_and(ends) {
            return Some(end);
        }
        if end == body {
            return None;
        }
        end -= 1;
    }
}

/// The length of `http://`, `https://`, `HTTP://` or `HTTPS://` at `start`.
fn scheme(text: &[char], start: usize) -> Option<usize> {
    for (word, secure) in [(['h', 't', 't', 'p'], 's'), (['H', 'T', 'T', 'P'], 'S')] {
        if text.get(start..start + 4) != Some(&word[..]) {
            continue;
        }
        let mut at = start + 4;
        if text.get(at) == Some(&secure) {
            at += 1;
        }
        if text.get(at..at + 3) == Some(&[':', '/', '/'][..]) {
            return Some(at + 3 - start);
        }
    }
    None
}

/// `[^\s"'!*(){}|\\^<>`]`
fn in_body(c: char) -> bool {
    !c.is_whitespace()
        && !matches!(
            c,
            '"' | '\'' | '!' | '*' | '(' | ')' | '{' | '}' | '|' | '\\' | '^' | '<' | '>' | '`'
        )
}

/// `[^\s"':,.!?{}|\\^~\[\]`()<>]`
fn ends(c: char) -> bool {
    !c.is_whitespace()
        && !matches!(
            c,
            '"' | '\''
                | ':'
                | ','
                | '.'
                | '!'
                | '?'
                | '{'
                | '}'
                | '|'
                | '\\'
                | '^'
                | '~'
                | '['
                | ']'
                | '`'
                | '('
                | ')'
                | '<'
                | '>'
        )
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn found(text: &str) -> Vec<String> {
        let chars: Vec<char> = text.chars().collect();
        urls(&chars)
            .into_iter()
            .map(|(start, end)| chars[start..=end].iter().collect())
            .collect()
    }

    #[test]
    fn urls_stop_before_trailing_punctuation() {
        assert_eq!(
            found("see https://example.com/a/b. ok"),
            ["https://example.com/a/b"]
        );
        assert_eq!(found("(http://x.dev/a)"), ["http://x.dev/a"]);
        assert_eq!(found("HTTPS://X.DEV, then"), ["HTTPS://X.DEV"]);
        assert_eq!(found("go https://a.b/c*d!"), ["https://a.b/c*"]);
        assert_eq!(
            found("HTTPs://x.dev ftp://a.b https://"),
            Vec::<String>::new()
        );
        assert_eq!(
            found("xhttps://a.b https://c.d"),
            ["https://a.b", "https://c.d"]
        );
        assert!(is_web_link("HTTPS://x") && !is_web_link("file:///tmp"));
    }

    proptest! {
        /// Every match starts with a scheme, holds only body characters before its last one, and
        /// ends on a character that may end a URL; matches never overlap.
        #[test]
        fn matches_follow_the_pattern(
            text in proptest::collection::vec(
                prop::sample::select(vec![
                    'h', 't', 'p', 's', 'H', 'T', 'P', 'S', ':', '/', '.', ',', 'a', ' ', '(',
                    ')', '*', '!', '?', '~', '-',
                ]),
                0..48,
            ),
        ) {
            let mut last = None;
            for (start, end) in urls(&text) {
                prop_assert!(last.is_none_or(|last| start > last));
                last = Some(end);
                let body = start + scheme(&text, start).expect("a scheme");
                prop_assert!(end >= body);
                prop_assert!(text[body..end].iter().all(|c| in_body(*c)));
                prop_assert!(ends(text[end]));
                let uri: String = text[start..=end].iter().collect();
                prop_assert!(is_web_link(&uri));
            }
        }
    }
}
