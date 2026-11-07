//! Selection as monocode's terminal made it, which was alacritty's: a click lands on the left or
//! right half of a cell, and a drag selects the cells between the two halves; a double click takes
//! the word between alacritty's separators (or the span to a matching bracket), a triple click the
//! logical lines. The rules here are alacritty_terminal 0.26's `Selection::to_range` and its
//! searches, over any [`Grid`]; the mirror hands libghostty the range for painting and copying.

/// alacritty's `SEMANTIC_ESCAPE_CHARS`: what a double click stops at.
pub const SEPARATORS: &str = ",│`|:\"' ()[]{}<>\t";

const BRACKETS: [(char, char); 4] = [('(', ')'), ('[', ']'), ('{', '}'), ('<', '>')];

/// A cell in screen space: `line` counts from the top of the scrollback.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Point {
    pub line: u32,
    pub col: u16,
}

/// The half of a cell the pointer is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

/// How a selection grows from where it started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Cell by cell (a click and drag).
    Simple,
    /// Whole words (a double click).
    Word,
    /// Whole logical lines (a triple click).
    Line,
}

/// What the selection rules read of the grid.
pub trait Grid {
    /// Columns per line; at least one.
    fn columns(&self) -> u16;
    /// Lines of scrollback and screen together; at least one.
    fn lines(&self) -> u32;
    /// The cell's first codepoint; a space when it is empty.
    fn char_at(&self, at: Point) -> char;
    /// The cell pads a wide character (after it, or at a line end before one that wrapped).
    fn spacer(&self, at: Point) -> bool;
    /// The line continues on the next (soft wrap).
    fn wrapped(&self, line: u32) -> bool;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Anchor {
    pub point: Point,
    pub side: Side,
}

/// A selection by its two ends, in the order the pointer made them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub kind: Kind,
    pub start: Anchor,
    pub end: Anchor,
}

impl Selection {
    #[must_use]
    pub fn new(kind: Kind, point: Point, side: Side) -> Self {
        let anchor = Anchor { point, side };
        Self {
            kind,
            start: anchor,
            end: anchor,
        }
    }

    /// The pointer moved: the end follows it.
    pub fn update(&mut self, point: Point, side: Side) {
        self.end = Anchor { point, side };
    }

    fn ordered(&self) -> (Anchor, Anchor) {
        if self.start.point > self.end.point {
            (self.end, self.start)
        } else {
            (self.start, self.end)
        }
    }

    /// A simple selection is empty until it covers a cell: its ends are the same, or the right
    /// half of one cell and the left half of the next.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        match self.kind {
            Kind::Simple => {
                let (start, end) = self.ordered();
                start == end
                    || (start.side == Side::Right
                        && end.side == Side::Left
                        && start.point.line == end.point.line
                        && start.point.col + 1 == end.point.col)
            }
            Kind::Word | Kind::Line => false,
        }
    }

    /// The cells selected, first and last inclusive, or `None` when it is empty.
    #[must_use]
    pub fn range(&self, grid: &impl Grid) -> Option<(Point, Point)> {
        let (mut start, mut end) = self.ordered();
        start.point = clamp(grid, start.point);
        end.point = clamp(grid, end.point);
        match self.kind {
            Kind::Simple => self.simple(grid, start, end),
            Kind::Word => Some(word(grid, start.point, end.point)),
            Kind::Line => Some((line_start(grid, start.point), line_end(grid, end.point))),
        }
    }

    fn simple(
        &self,
        grid: &impl Grid,
        mut start: Anchor,
        mut end: Anchor,
    ) -> Option<(Point, Point)> {
        if self.is_empty() {
            return None;
        }
        let columns = grid.columns();
        // Ending on a cell's left half leaves that cell out.
        if end.side == Side::Left && start.point != end.point {
            if end.point.col == 0 {
                end.point.col = columns - 1;
                end.point.line = end.point.line.saturating_sub(1);
            } else {
                end.point.col -= 1;
            }
        }
        // Starting on a cell's right half leaves that cell out.
        if start.side == Side::Right && start.point != end.point {
            start.point.col += 1;
            if start.point.col == columns {
                start.point.col = 0;
                start.point.line += 1;
            }
        }
        // From a line's last right half to the next line's first left half crosses no cell:
        // alacritty ends up with an inverted range that paints nothing.
        (start.point <= end.point).then_some((start.point, end.point))
    }
}

fn last(grid: &impl Grid) -> Point {
    Point {
        line: grid.lines().saturating_sub(1),
        col: grid.columns().saturating_sub(1),
    }
}

fn clamp(grid: &impl Grid, point: Point) -> Point {
    let last = last(grid);
    if point.line > last.line {
        last
    } else {
        Point {
            line: point.line,
            col: point.col.min(last.col),
        }
    }
}

pub(crate) fn next(grid: &impl Grid, at: Point) -> Option<Point> {
    let last = last(grid);
    if at >= last {
        return None;
    }
    Some(if at.col >= last.col {
        Point {
            line: at.line + 1,
            col: 0,
        }
    } else {
        Point {
            line: at.line,
            col: at.col + 1,
        }
    })
}

pub(crate) fn prev(grid: &impl Grid, at: Point) -> Option<Point> {
    if at == Point::default() {
        return None;
    }
    Some(if at.col == 0 {
        Point {
            line: at.line - 1,
            col: grid.columns().saturating_sub(1),
        }
    } else {
        Point {
            line: at.line,
            col: at.col - 1,
        }
    })
}

/// The line ends here, at its last column without a wrap.
fn hard_end(grid: &impl Grid, at: Point) -> bool {
    at.col == grid.columns().saturating_sub(1) && !grid.wrapped(at.line)
}

fn separator(grid: &impl Grid, at: Point) -> bool {
    !grid.spacer(at) && SEPARATORS.contains(grid.char_at(at))
}

/// alacritty's `range_semantic`: on a bracket, the span to its match; else the words under both
/// ends.
fn word(grid: &impl Grid, start: Point, end: Point) -> (Point, Point) {
    if start == end
        && let Some(matching) = bracket(grid, start)
    {
        return if matching < start {
            (matching, end)
        } else {
            (start, matching)
        };
    }
    (word_start(grid, start), word_end(grid, end))
}

fn word_start(grid: &impl Grid, at: Point) -> Point {
    let mut point = at;
    let mut cursor = at;
    while let Some(cell) = prev(grid, cursor) {
        cursor = cell;
        if hard_end(grid, cell) {
            return point;
        }
        point = cell;
        if separator(grid, cell) {
            // One past the separator, over any wide-character padding.
            let mut after = cell;
            while let Some(cell) = next(grid, after) {
                if !grid.spacer(cell) {
                    return cell;
                }
                after = cell;
            }
            return cell;
        }
    }
    point
}

fn word_end(grid: &impl Grid, at: Point) -> Point {
    if hard_end(grid, at) {
        return at;
    }
    let mut point = at;
    let mut cursor = at;
    while let Some(cell) = next(grid, cursor) {
        cursor = cell;
        point = cell;
        if separator(grid, cell) {
            return prev(grid, cell).unwrap_or(cell);
        }
        if hard_end(grid, cell) {
            break;
        }
    }
    point
}

/// The bracket matching the one at `at`, nesting counted, anywhere in the grid.
fn bracket(grid: &impl Grid, at: Point) -> Option<Point> {
    let open = grid.char_at(at);
    let (forward, close) = BRACKETS.iter().find_map(|&(left, right)| {
        if left == open {
            Some((true, right))
        } else if right == open {
            Some((false, left))
        } else {
            None
        }
    })?;
    let mut skip = 0i64;
    let mut cursor = at;
    loop {
        cursor = if forward {
            next(grid, cursor)?
        } else {
            prev(grid, cursor)?
        };
        let c = grid.char_at(cursor);
        if c == close && skip == 0 {
            return Some(cursor);
        } else if c == open {
            skip += 1;
        } else if c == close {
            skip -= 1;
        }
    }
}

fn line_start(grid: &impl Grid, at: Point) -> Point {
    let mut line = at.line;
    while line > 0 && grid.wrapped(line - 1) {
        line -= 1;
    }
    Point { line, col: 0 }
}

fn line_end(grid: &impl Grid, at: Point) -> Point {
    let mut line = at.line;
    while line + 1 < grid.lines() && grid.wrapped(line) {
        line += 1;
    }
    Point {
        line,
        col: grid.columns().saturating_sub(1),
    }
}

/// The logical line holding `at`: its first and last cells.
#[must_use]
pub fn logical_line(grid: &impl Grid, at: Point) -> (Point, Point) {
    (line_start(grid, at), line_end(grid, at))
}

/// `at` is in the inclusive span `start..=end`.
#[must_use]
pub fn contains(start: Point, end: Point, at: Point) -> bool {
    start <= at && at <= end
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    /// Lines of text; a line ending in `\` wraps onto the next. `_` pads a wide `W`.
    #[derive(Debug)]
    struct Text {
        columns: u16,
        lines: Vec<(Vec<char>, bool)>,
    }

    impl Text {
        fn new(columns: u16, text: &[&str]) -> Self {
            let lines = text
                .iter()
                .map(|line| {
                    let (line, wrapped) = match line.strip_suffix('\\') {
                        Some(line) => (line, true),
                        None => (*line, false),
                    };
                    (line.chars().collect(), wrapped)
                })
                .collect();
            Self { columns, lines }
        }
    }

    impl Grid for Text {
        fn columns(&self) -> u16 {
            self.columns
        }

        fn lines(&self) -> u32 {
            u32::try_from(self.lines.len()).unwrap()
        }

        fn char_at(&self, at: Point) -> char {
            self.lines[at.line as usize]
                .0
                .get(usize::from(at.col))
                .copied()
                .unwrap_or(' ')
        }

        fn spacer(&self, at: Point) -> bool {
            self.char_at(at) == '_'
        }

        fn wrapped(&self, line: u32) -> bool {
            self.lines[line as usize].1
        }
    }

    fn p(line: u32, col: u16) -> Point {
        Point { line, col }
    }

    fn text(grid: &Text, (start, end): (Point, Point)) -> String {
        let mut out = String::new();
        let mut at = start;
        loop {
            if !grid.spacer(at) {
                out.push(grid.char_at(at));
            }
            if at >= end {
                break;
            }
            if hard_end(grid, at) {
                out.push('\n');
            }
            at = next(grid, at).unwrap();
        }
        out
    }

    fn drag(grid: &Text, from: (Point, Side), to: (Point, Side)) -> Option<String> {
        let mut selection = Selection::new(Kind::Simple, from.0, from.1);
        selection.update(to.0, to.1);
        selection.range(grid).map(|range| text(grid, range))
    }

    #[test]
    fn a_drag_selects_between_the_halves_it_crossed() {
        let grid = Text::new(8, &["abcdef", "ghij"]);
        assert_eq!(
            drag(&grid, (p(0, 1), Side::Left), (p(0, 1), Side::Left)),
            None
        );
        assert_eq!(
            drag(&grid, (p(0, 1), Side::Right), (p(0, 2), Side::Left)),
            None
        );
        assert_eq!(
            drag(&grid, (p(0, 1), Side::Left), (p(0, 1), Side::Right)).as_deref(),
            Some("b")
        );
        assert_eq!(
            drag(&grid, (p(0, 1), Side::Right), (p(0, 4), Side::Left)).as_deref(),
            Some("cd")
        );
        assert_eq!(
            drag(&grid, (p(0, 4), Side::Left), (p(0, 1), Side::Right)).as_deref(),
            Some("cd")
        );
        // Ending on the left half of a line's first cell stops at the line above's end.
        assert_eq!(
            drag(&grid, (p(0, 3), Side::Left), (p(1, 0), Side::Left)).as_deref(),
            Some("def  ")
        );
        // Starting on the right half of a line's last cell starts on the next line.
        assert_eq!(
            drag(&grid, (p(0, 7), Side::Right), (p(1, 1), Side::Right)).as_deref(),
            Some("gh")
        );
    }

    fn double_click(grid: &Text, at: Point) -> String {
        text(
            grid,
            Selection::new(Kind::Word, at, Side::Left)
                .range(grid)
                .unwrap(),
        )
    }

    #[test]
    fn a_double_click_takes_the_word_between_separators() {
        let grid = Text::new(16, &["ls foo/bar.rs:12", "x"]);
        assert_eq!(double_click(&grid, p(0, 4)), "foo/bar.rs");
        assert_eq!(double_click(&grid, p(0, 0)), "ls");
        assert_eq!(double_click(&grid, p(0, 15)), "12");
        // On a separator, alacritty spans the words on both sides of it.
        assert_eq!(double_click(&grid, p(0, 2)), "ls foo/bar.rs");
        // Words run on across a soft wrap, and stop at a hard line end.
        let grid = Text::new(4, &["ab c\\", "def", "gh"]);
        assert_eq!(double_click(&grid, p(0, 3)), "cdef");
        assert_eq!(double_click(&grid, p(2, 0)), "gh");
    }

    #[test]
    fn a_double_click_on_a_bracket_takes_the_span_to_its_match() {
        let grid = Text::new(16, &["f(a, (b)) [x", "y]"]);
        assert_eq!(double_click(&grid, p(0, 1)), "(a, (b))");
        assert_eq!(double_click(&grid, p(0, 8)), "(a, (b))");
        assert_eq!(
            double_click(&grid, p(0, 10)),
            format!("[x{}\ny]", " ".repeat(4))
        );
    }

    #[test]
    fn a_triple_click_takes_logical_lines() {
        let grid = Text::new(4, &["ab\\", "cd", "ef"]);
        let range = Selection::new(Kind::Line, p(1, 1), Side::Left)
            .range(&grid)
            .unwrap();
        assert_eq!(range, (p(0, 0), p(1, 3)));
        assert_eq!(logical_line(&grid, p(2, 0)), (p(2, 0), p(2, 3)));
    }

    #[test]
    fn word_search_steps_over_wide_padding() {
        let grid = Text::new(8, &["a W_b c"]);
        assert_eq!(double_click(&grid, p(0, 3)), "W_b".replace('_', ""));
    }

    fn arb_grid() -> impl Strategy<Value = Text> {
        (1u16..6, 1usize..5).prop_flat_map(|(columns, lines)| {
            proptest::collection::vec(
                (
                    proptest::collection::vec(
                        prop::sample::select(vec!['a', 'b', ' ', '(', ')', ',', '[', ']']),
                        0..=usize::from(columns),
                    ),
                    any::<bool>(),
                ),
                lines,
            )
            .prop_map(move |lines| Text { columns, lines })
        })
    }

    proptest! {
        /// Whatever the ends, a selection's range is ordered and inside the grid, and a word or
        /// line selection covers the cells under both ends.
        #[test]
        fn ranges_are_ordered_and_in_the_grid(
            grid in arb_grid(),
            a in (0u32..6, 0u16..7, any::<bool>()),
            b in (0u32..6, 0u16..7, any::<bool>()),
            kind in prop::sample::select(vec![Kind::Simple, Kind::Word, Kind::Line]),
        ) {
            let side = |right: bool| if right { Side::Right } else { Side::Left };
            let mut selection = Selection::new(kind, p(a.0, a.1), side(a.2));
            selection.update(p(b.0, b.1), side(b.2));
            let Some((start, end)) = selection.range(&grid) else {
                prop_assert_eq!(kind, Kind::Simple);
                return Ok(());
            };
            prop_assert!(start <= end);
            prop_assert!(end <= last(&grid));
            if kind != Kind::Simple {
                let (first, second) = selection.ordered();
                let first = clamp(&grid, first.point);
                let second = clamp(&grid, second.point);
                if kind == Kind::Line || first != second || bracket(&grid, first).is_none() {
                    prop_assert!(contains(start, end, first) && contains(start, end, second));
                }
            }
        }
    }
}
