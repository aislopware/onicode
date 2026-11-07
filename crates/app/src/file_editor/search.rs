//! editorSearch.ts's two-row Find/Replace panel, and its query engine:
//! CodeMirror's `SearchQuery` semantics on Rust regexes.
use super::*;

/// A match of the Find query and the text Replace puts in its place.
#[derive(Clone, Debug)]
pub(super) struct Match {
    pub from: usize,
    pub to: usize,
    pub insert: String,
}

/// Finds `query` in `source` as CodeMirror's `SearchQuery` does with
/// `literal` set: case-insensitive unless `case`, whole words only with
/// `word`, and with `regex` a multiline regular expression whose
/// replacement expands `$1`, `$&` and `$$`. Empty matches are skipped.
/// `None` is a regex that does not parse.
pub(super) fn find(
    source: &str,
    query: &str,
    replacement: &str,
    case: bool,
    word: bool,
    regex: bool,
) -> Option<Vec<Match>> {
    if query.is_empty() {
        return Some(Vec::new());
    }
    let pattern = if regex {
        query.to_owned()
    } else {
        regex::escape(query)
    };
    let compiled = regex::RegexBuilder::new(&pattern)
        .case_insensitive(!case)
        .multi_line(true)
        .build()
        .ok()?;
    let mut matches = Vec::new();
    let mut at = 0;
    while at <= source.len() {
        let Some(captures) = compiled.captures_at(source, at) else {
            break;
        };
        let Some(found) = captures.get(0) else {
            break;
        };
        let (from, to) = (found.start(), found.end());
        if to > from && (!word || whole_word(source, from, to)) {
            matches.push(Match {
                from,
                to,
                insert: if regex {
                    expand(&captures, replacement)
                } else {
                    replacement.to_owned()
                },
            });
            at = to;
        } else {
            // Try again one character on, where a whole word may start.
            at = from + source[from..].chars().next().map_or(1, char::len_utf8);
        }
    }
    Some(matches)
}

/// CodeMirror's word test: neither end of the match splits a word.
fn whole_word(source: &str, from: usize, to: usize) -> bool {
    let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    let before = |at: usize| word(source[..at].chars().next_back());
    let after = |at: usize| word(source[at..].chars().next());
    (!before(from) || !after(from)) && (!after(to) || !before(to))
}

/// A regex replacement as JavaScript's `String.replace` expands it: `$$` is
/// a dollar, `$&` the match, and `$n` the longest prefix of the digits that
/// names a group.
fn expand(captures: &regex::Captures<'_>, replacement: &str) -> String {
    let group = |n: usize| captures.get(n).map_or("", |m| m.as_str());
    let mut out = String::with_capacity(replacement.len());
    let mut rest = replacement;
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        let tail = &rest[at + 1..];
        if let Some(after) = tail.strip_prefix('$') {
            out.push('$');
            rest = after;
        } else if let Some(after) = tail.strip_prefix('&') {
            out.push_str(group(0));
            rest = after;
        } else {
            let digits = tail.bytes().take_while(u8::is_ascii_digit).count();
            let named = (1..=digits).rev().find_map(|len| {
                let n: usize = tail[..len].parse().ok()?;
                (n > 0 && n < captures.len()).then_some((n, len))
            });
            match named {
                Some((n, len)) => {
                    out.push_str(group(n));
                    rest = &tail[len..];
                }
                None => {
                    out.push('$');
                    rest = tail;
                }
            }
        }
    }
    out.push_str(rest);
    out
}
impl FileEditor {
    pub(super) fn search_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let count = if self.search_invalid {
            "Invalid regex".into()
        } else if self.search.read(cx).content.is_empty() {
            String::new()
        } else if self.matches.is_empty() {
            "No results".into()
        } else {
            format!(
                "{} of {}{}",
                self.match_index + 1,
                self.matches.len().min(999),
                if self.matches.len() > 999 { "+" } else { "" }
            )
        };
        let bad = self.search_invalid
            || (!self.search.read(cx).content.is_empty() && self.matches.is_empty());
        let query = div()
            .h(px(26.))
            .min_w_0()
            .flex_1()
            .flex()
            .items_center()
            .px_2()
            .rounded_md()
            .border_1()
            .border_color(if bad {
                rgb(0xf87171).opacity(0.55).into()
            } else {
                content(0.1)
            })
            .bg(content(0.06))
            .child(div().flex_1().min_w_0().child(self.search.clone()))
            .child(
                div()
                    .min_w(px(85.))
                    .text_right()
                    .text_size(px(11.))
                    .text_color(if bad {
                        rgb(0xf87171).into()
                    } else {
                        content(0.45)
                    })
                    .child(count),
            );
        let toggle = |id, label: &'static str, title: &'static str, on| {
            div()
                .id(id)
                .role(Role::Button)
                .aria_label(title)
                .aria_toggled(if on {
                    gpui::Toggled::True
                } else {
                    gpui::Toggled::False
                })
                .size(px(24.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_md()
                .text_size(px(11.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(content(if on { 1. } else { 0.6 }))
                .when(on, |d| d.bg(accent_color().opacity(0.25)))
                .hover(|s| s.bg(content(0.1)))
                .child(label)
        };
        div()
            .flex_shrink_0()
            .flex()
            .items_start()
            .gap_1()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(stroke())
            .font_family(crate::fonts::mono())
            .child(
                icon_action(
                    "toggle-replace",
                    "Toggle Replace",
                    if self.replace_open {
                        "chevron-down"
                    } else {
                        "chevron-right"
                    },
                    true,
                )
                .mt(px(3.))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.replace_open = !this.replace_open;
                    cx.notify();
                })),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .h(px(26.))
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(query)
                            .child(
                                toggle("match-case", "Aa", "Match Case", self.match_case).on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.match_case = !this.match_case;
                                        this.find_matches(cx);
                                    }),
                                ),
                            )
                            .child(
                                toggle("whole-word", "ab", "Match Whole Word", self.whole_word)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.whole_word = !this.whole_word;
                                        this.find_matches(cx);
                                    })),
                            )
                            .child(
                                toggle("use-regex", ".*", "Use Regular Expression", self.regex)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.regex = !this.regex;
                                        this.find_matches(cx);
                                    })),
                            ),
                    )
                    .when(self.replace_open, |d| {
                        d.child(
                            div()
                                .h(px(26.))
                                .flex()
                                .items_center()
                                .gap_1()
                                .child(
                                    div()
                                        .h(px(26.))
                                        .flex_1()
                                        .min_w_0()
                                        .px_2()
                                        .flex()
                                        .items_center()
                                        .rounded_md()
                                        .border_1()
                                        .border_color(content(0.1))
                                        .bg(content(0.06))
                                        .child(self.replacement.clone()),
                                )
                                .child(
                                    div()
                                        .id("replace-next")
                                        .role(Role::Button)
                                        .aria_label("Replace")
                                        .px_2()
                                        .py_1()
                                        .rounded_md()
                                        .text_size(px(11.))
                                        .text_color(content(0.6))
                                        .hover(|s| s.bg(content(0.1)))
                                        .child("Replace")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.replace_matches(false, cx)
                                        })),
                                )
                                .child(
                                    div()
                                        .id("replace-all")
                                        .role(Role::Button)
                                        .aria_label("Replace All")
                                        .px_2()
                                        .py_1()
                                        .rounded_md()
                                        .text_size(px(11.))
                                        .text_color(content(0.6))
                                        .hover(|s| s.bg(content(0.1)))
                                        .child("All")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.replace_matches(true, cx)
                                        })),
                                ),
                        )
                    }),
            )
            .child(
                div()
                    .h(px(26.))
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        icon_action(
                            "find-prev",
                            "Previous Match",
                            "chevron-up",
                            !self.matches.is_empty(),
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.step_match(true, cx))),
                    )
                    .child(
                        icon_action(
                            "find-next",
                            "Next Match",
                            "chevron-down",
                            !self.matches.is_empty(),
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.step_match(false, cx))),
                    )
                    .child(
                        icon_action("close-find", "Close", "x", true)
                            .size(px(24.))
                            .on_click(cx.listener(|this, _, w, cx| this.dismiss_search(w, cx))),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod query_tests {
    use super::find;

    #[test]
    fn search_and_replace_share_codemirror_unicode_captures_and_word_boundaries() {
        let source = "🙂 café CAFE caféine\nCAFÉ";
        let found = find(source, "café", "tea", false, true, false).unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(&source[found[0].from..found[0].to], "café");
        assert_eq!(&source[found[1].from..found[1].to], "CAFÉ");
        assert_eq!(found[0].insert, "tea");
        let replace = "$2$1 $& $$ $9";
        let found = find(source, "(caf)(é)", replace, false, true, true).unwrap(); // spellchecker:disable-line
        assert_eq!(found[0].insert, "écaf café $ $9");
        assert!(find(source, "[", "", false, false, true).is_none());
        assert!(
            find(source, "^", "x", false, false, true)
                .unwrap()
                .is_empty()
        );
        assert!(
            find(source, "", "x", false, false, true)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            find(source, "café", "", true, false, false).unwrap().len(),
            2
        );
        assert_eq!(
            find("a\\nb", "a\\nb", "$1", true, false, false).unwrap()[0].insert,
            "$1"
        );
        // Line anchors hold at every line, as JavaScript's `m` flag has them.
        let lines = find("one\ntwo\n", "^t\\w+$", "", true, false, true).unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].from, 4);
        // A rejected partial word does not hide a whole word that overlaps it.
        let words = find("aa a", "a", "", true, true, false).unwrap();
        assert_eq!(words.iter().map(|m| m.from).collect::<Vec<_>>(), [3]);
    }
}
