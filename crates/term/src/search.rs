//! Find in a terminal's scrollback and screen, as the file editor's Find panel reads its query:
//! case-insensitive unless asked, whole words only when asked, and a regular expression when
//! asked. A match never crosses a hard line break but runs across soft wraps: the grid is read as
//! logical lines, the rows a program's long line wrapped onto joined, and each match maps back to
//! its first and last cells. Wide characters' padding is not text, and a line's trailing blanks
//! are trimmed, as a copy trims them.
//!
//! The mirror reads the grid ([`lines`]) once per change of the grid and finds every query typed
//! against the same lines ([`find`]), so the search runs on the viewer's own copy of the terminal,
//! scrollback included, with nothing asked of the host.
use crate::select::{self, Grid, Point};

/// What the find panel asks for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    pub text: String,
    /// Match case; otherwise letters match either case.
    pub case: bool,
    /// Only matches neither end of which splits a word.
    pub word: bool,
    /// `text` is a regular expression; otherwise it is found as it is.
    pub regex: bool,
}

/// One match by its first and last cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Found {
    pub start: Point,
    pub end: Point,
}

impl Found {
    #[must_use]
    pub fn contains(&self, at: Point) -> bool {
        select::contains(self.start, self.end, at)
    }
}

/// One logical line: its text and the cell each of its characters is on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Line {
    text: String,
    /// Every character's byte offset in `text` and its cell, in order.
    chars: Vec<(usize, Point)>,
}

impl Line {
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The cells of the bytes `from..to`, which start and end on character boundaries.
    fn span(&self, from: usize, to: usize) -> Option<Found> {
        let first = self.chars.partition_point(|(at, _)| *at < from);
        let last = self
            .chars
            .partition_point(|(at, _)| *at < to)
            .checked_sub(1)?;
        Some(Found {
            start: self.chars.get(first)?.1,
            end: self.chars.get(last)?.1,
        })
    }
}

/// The grid's logical lines, top of the scrollback first.
#[must_use]
pub fn lines(grid: &impl Grid) -> Vec<Line> {
    let (columns, total) = (grid.columns(), grid.lines());
    let mut lines = Vec::new();
    let mut row = 0;
    while row < total {
        let mut line = Line::default();
        loop {
            for col in 0..columns {
                let at = Point { line: row, col };
                if let Some(ch) = grid.glyph(at) {
                    line.chars.push((line.text.len(), at));
                    line.text.push(ch);
                }
            }
            if !grid.wrapped(row) || row + 1 >= total {
                break;
            }
            row += 1;
        }
        let kept = line.text.trim_end().len();
        line.text.truncate(kept);
        line.chars.retain(|(at, _)| *at < kept);
        lines.push(line);
        row += 1;
    }
    lines
}

/// Every match of `query` in `lines`, in order. Empty matches are skipped, and a match ends
/// where the next may start. `None` is a regular expression that does not parse.
#[must_use]
pub fn find(lines: &[Line], query: &Query) -> Option<Vec<Found>> {
    if query.text.is_empty() {
        return Some(Vec::new());
    }
    let pattern = if query.regex {
        query.text.clone()
    } else {
        regex::escape(&query.text)
    };
    let compiled = regex::RegexBuilder::new(&pattern)
        .case_insensitive(!query.case)
        .build()
        .ok()?;
    let mut found = Vec::new();
    for line in lines {
        let source = line.text.as_str();
        let mut at = 0;
        while at <= source.len() {
            let Some(hit) = compiled.find_at(source, at) else {
                break;
            };
            let (from, to) = (hit.start(), hit.end());
            if to > from && (!query.word || whole_word(source, from, to)) {
                found.extend(line.span(from, to));
                at = to;
            } else {
                // Try again one character on, where a whole word may start.
                at = from + source[from..].chars().next().map_or(1, char::len_utf8);
            }
        }
    }
    Some(found)
}

/// The file editor's word test (CodeMirror's): neither end of the match splits a word.
fn whole_word(source: &str, from: usize, to: usize) -> bool {
    let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    let before = |at: usize| word(source[..at].chars().next_back());
    let after = |at: usize| word(source[at..].chars().next());
    (!before(from) || !after(from)) && (!after(to) || !before(to))
}

#[cfg(test)]
mod tests {
    //! Matching over a grid whose logical lines wrapped at any width, against the same query run
    //! on the lines themselves: the width a program's output wrapped at never changes what is
    //! found, only the cells it is found on.
    use proptest::prelude::*;

    use super::*;

    /// Logical lines laid out `columns` wide as a terminal lays them out: a line runs on over
    /// soft-wrapped rows, and a wide character that does not fit leaves a padding cell at the
    /// row's end and starts the next.
    struct Wrapped {
        columns: u16,
        /// Each row's cells (`None` pads a wide character) and whether it wraps.
        rows: Vec<(Vec<Option<char>>, bool)>,
        /// Each logical line's characters and the cell each landed on.
        placed: Vec<Vec<(char, Point)>>,
    }

    fn wide(c: char) -> bool {
        matches!(c, '漢' | '字')
    }

    impl Wrapped {
        fn new(lines: &[String], columns: u16) -> Self {
            let width = usize::from(columns);
            let mut grid = Self {
                columns,
                rows: Vec::new(),
                placed: Vec::new(),
            };
            for line in lines {
                let mut placed = Vec::new();
                let mut row: Vec<Option<char>> = Vec::new();
                for c in line.chars() {
                    let need = if wide(c) { 2 } else { 1 };
                    if row.len() + need > width {
                        while row.len() < width {
                            row.push(None);
                        }
                        grid.rows.push((std::mem::take(&mut row), true));
                    }
                    let at = Point {
                        line: u32::try_from(grid.rows.len()).unwrap(),
                        col: u16::try_from(row.len()).unwrap(),
                    };
                    placed.push((c, at));
                    row.push(Some(c));
                    if need == 2 {
                        row.push(None);
                    }
                }
                while row.len() < width {
                    row.push(Some(' '));
                }
                grid.rows.push((row, false));
                grid.placed.push(placed);
            }
            grid
        }

        fn cell(&self, at: Point) -> Option<char> {
            self.rows[at.line as usize].0[usize::from(at.col)]
        }
    }

    impl Grid for Wrapped {
        fn columns(&self) -> u16 {
            self.columns
        }

        fn lines(&self) -> u32 {
            u32::try_from(self.rows.len()).unwrap()
        }

        fn char_at(&self, at: Point) -> char {
            self.cell(at).unwrap_or(' ')
        }

        fn spacer(&self, at: Point) -> bool {
            self.cell(at).is_none()
        }

        fn wrapped(&self, line: u32) -> bool {
            self.rows[line as usize].1
        }
    }

    fn query(text: &str, case: bool, word: bool, regex: bool) -> Query {
        Query {
            text: text.to_owned(),
            case,
            word,
            regex,
        }
    }

    #[test]
    fn a_match_runs_across_a_soft_wrap_but_not_a_hard_break() {
        let grid = Wrapped::new(&["say magic".into(), "mag".into(), "ic".into()], 6);
        let lines = lines(&grid);
        assert_eq!(
            lines.iter().map(Line::text).collect::<Vec<_>>(),
            ["say magic", "mag", "ic"]
        );
        let found = find(&lines, &query("magic", false, false, false)).unwrap();
        assert_eq!(
            found,
            [Found {
                start: Point { line: 0, col: 4 },
                end: Point { line: 1, col: 2 },
            }]
        );
    }

    #[test]
    fn case_words_and_regexes_follow_the_editors_find() {
        let grid = Wrapped::new(&["Error: an error, errors".into()], 80);
        let lines = lines(&grid);
        let count = |q: Query| find(&lines, &q).map(|found| found.len());
        assert_eq!(count(query("error", false, false, false)), Some(3));
        assert_eq!(count(query("error", true, false, false)), Some(2));
        assert_eq!(count(query("error", false, true, false)), Some(2));
        assert_eq!(count(query("e[a-z]+s$", false, false, true)), Some(1));
        assert_eq!(count(query("e.r", false, false, false)), Some(0), "literal");
        assert_eq!(count(query("[", false, false, true)), None, "invalid");
        assert_eq!(count(query("", false, false, true)), Some(0));
        assert_eq!(count(query("^", false, false, true)), Some(0), "empty");
    }

    #[test]
    fn wide_characters_match_by_their_first_cell() {
        let grid = Wrapped::new(&["a漢字b".into()], 4);
        let lines = lines(&grid);
        assert_eq!(lines[0].text(), "a漢字b");
        let found = find(&lines, &query("字b", true, false, false)).unwrap();
        assert_eq!(
            found,
            [Found {
                start: Point { line: 1, col: 0 },
                end: Point { line: 1, col: 2 },
            }]
        );
    }

    fn line() -> impl Strategy<Value = String> {
        proptest::collection::vec(
            prop_oneof![
                6 => prop::sample::select(vec!['a', 'b', 'A', 'B', '_', '-']),
                2 => Just(' '),
                1 => prop::sample::select(vec!['漢', '字', 'é']),
            ],
            0..30,
        )
        .prop_map(|chars| chars.into_iter().collect::<String>().trim_end().to_owned())
    }

    fn any_query() -> impl Strategy<Value = Query> {
        (
            prop_oneof![
                Just("a"),
                Just("ab"),
                Just("b a"),
                Just("A_"),
                Just("漢字"),
                Just("a+b?"),
                Just("[ab]{2}"),
                Just("^b"),
                Just("a$"),
                Just("é"),
            ],
            any::<bool>(),
            any::<bool>(),
            any::<bool>(),
        )
            .prop_map(|(text, case, word, regex)| query(text, case, word, regex))
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// Whatever width the lines wrapped at, the grid finds what the lines themselves hold,
        /// each match on the cells its characters landed on.
        #[test]
        fn matches_over_wrapped_lines_are_the_lines_own(
            source in proptest::collection::vec(line(), 1..6),
            columns in 2u16..12,
            query in any_query(),
        ) {
            let grid = Wrapped::new(&source, columns);
            let read = lines(&grid);
            prop_assert_eq!(
                read.iter().map(Line::text).collect::<Vec<_>>(),
                source.iter().map(String::as_str).collect::<Vec<_>>()
            );
            // The same query over each logical line laid out on one row of its own.
            let mut expected = Vec::new();
            for (line, placed) in source.iter().zip(&grid.placed) {
                let alone = Wrapped::new(std::slice::from_ref(line), 64);
                let Some(found) = find(&lines(&alone), &query) else {
                    prop_assert!(find(&read, &query).is_none());
                    return Ok(());
                };
                // A match's ends as character indexes, then as the cells the wrapped grid put
                // those characters on.
                let index = |at: Point| {
                    alone.placed[0].iter().position(|(_, cell)| *cell == at).unwrap()
                };
                for hit in found {
                    expected.push(Found {
                        start: placed[index(hit.start)].1,
                        end: placed[index(hit.end)].1,
                    });
                }
            }
            prop_assert_eq!(find(&read, &query), Some(expected));
        }
    }
}
