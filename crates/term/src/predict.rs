//! Local echo for a terminal on a far engine.
//!
//! A keystroke to a remote shell cannot show until its echo has crossed the link twice. The
//! predictor runs in the viewer, over its mirror: it guesses where typed characters will land and
//! the view paints them there at once, while the screen stays the host's.
//!
//! - **Trust is earned per run of typing.** A run starts after Enter, a control key, a paste or a
//!   misprediction, and only once the host has drawn something since. Its first character is held
//!   back until the host echoes it (the right character in the predicted cell, with the cursor past
//!   it; a blank cell "matching" a space proves nothing), and only then is the run painted ahead of
//!   its echo. A password prompt never echoes, so nothing typed into one is ever painted.
//! - **Trust is lost the moment the host stops echoing.** Output that confirms none of the run's
//!   pending characters (a `Passphrase:` appearing mid-run), or a cursor that left the run's row,
//!   ends it.
//! - **The host always wins.** Each output batch checks every prediction against its cell: a match
//!   retires it, an older one left unmatched behind a match, or one past its echo deadline, wipes
//!   them all and a new run must earn trust again.
//! - **Only where it is safe.** Predictions live on the primary screen with a visible cursor and
//!   paint only with the view at the live bottom; full-screen programs draw what they like.
//! - **Only when it helps.** They show once the measured echo time, from a keystroke to the output
//!   that confirms it, rises above [`SHOW_ABOVE`], and stop below [`HIDE_BELOW`]. A fast link, the
//!   engine on the viewer's own machine included, never shows one.
//!
//! The clock is the caller's: every call takes `now`, so tests replay a schedule exactly.
use std::time::{Duration, Instant};

use onicode_protocol::terminal::Input;

/// Echo time above which predictions start showing...
pub const SHOW_ABOVE: Duration = Duration::from_millis(30);
/// ...and below which they stop.
pub const HIDE_BELOW: Duration = Duration::from_millis(15);
/// An echo this much later than the measured echo time means the prediction was wrong.
const ECHO_SLACK: Duration = Duration::from_millis(150);

/// What the predictor reads of the viewer's terminal, in active-area cells (the live screen,
/// whatever the view's scroll).
pub trait Surface {
    /// Columns.
    fn columns(&self) -> usize;
    /// The cursor's row and column when typing there can be predicted: the primary screen with
    /// the cursor shown.
    fn typing_cursor(&self) -> Option<(usize, usize)>;
    /// The character in a cell, a space when it is empty.
    fn char_at(&self, row: usize, col: usize) -> Option<char>;
    /// The view shows the live bottom, where active rows are the viewport's.
    fn at_bottom(&self) -> bool;
}

/// What the view paints over the mirror: predicted characters by viewport row and column, and
/// the cursor past the last of them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Overlay {
    pub cells: Vec<(usize, usize, char)>,
    pub cursor: Option<(usize, usize)>,
}

impl Overlay {
    /// Nothing to paint.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty() && self.cursor.is_none()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Prediction {
    row: usize,
    col: usize,
    ch: char,
    /// When the keystroke left.
    at: Instant,
}

/// What a keystroke does to the line being typed.
enum Typing<'a> {
    Text(&'a str),
    Erase,
    Other,
}

fn typing(input: &Input) -> Typing<'_> {
    match input {
        Input::Text(text) => Typing::Text(text),
        Input::Key(key) if key.control || key.alt || key.platform => Typing::Other,
        Input::Key(key) if key.key == "backspace" => Typing::Erase,
        Input::Key(key) => match key.key_char.as_deref() {
            Some(text) if !text.is_empty() => Typing::Text(text),
            _ => Typing::Other,
        },
        Input::Paste(_) | Input::Wheel { .. } => Typing::Other,
    }
}

/// One terminal's predictions.
#[derive(Debug, Default)]
pub struct Predictor {
    /// The current run's predictions, in typing order.
    pending: Vec<Prediction>,
    /// A trusted run's predictions still painting past the key that ended it (Enter after fast
    /// typing), until the host's next output.
    held: Vec<Prediction>,
    /// Characters typed before the host redrew after a run ended, placed at its cursor once it
    /// has.
    blind: Vec<(char, Instant)>,
    /// The host confirmed a character of the current run.
    trusted: bool,
    /// A run ended and the host has drawn nothing since: the cursor may still be on the old line.
    awaiting_redraw: bool,
    /// Smoothed echo time.
    srtt: Option<Duration>,
    showing: bool,
}

impl Predictor {
    /// The viewer sent `input`. `surface` is the mirror as of the host's last output.
    pub fn typed(&mut self, input: &Input, surface: &impl Surface, now: Instant) {
        let Some(cursor) = surface.typing_cursor() else {
            self.reset();
            return;
        };
        let columns = surface.columns();
        match typing(input) {
            Typing::Text(text) => {
                for ch in text.chars() {
                    if !(ch.is_ascii_graphic() || ch == ' ') {
                        self.interrupt();
                        return;
                    }
                    self.push(ch, cursor, columns, now);
                }
            }
            Typing::Erase => self.erase(),
            Typing::Other => self.interrupt(),
        }
    }

    fn push(&mut self, ch: char, cursor: (usize, usize), columns: usize, now: Instant) {
        if self.awaiting_redraw {
            if self.blind.len() < columns {
                self.blind.push((ch, now));
            }
            return;
        }
        let (row, col) = self
            .pending
            .last()
            .map_or(cursor, |last| (last.row, last.col + 1));
        // The shell decides how a line wraps: stop at the edge.
        if col + 1 >= columns {
            return;
        }
        self.pending.push(Prediction {
            row,
            col,
            ch,
            at: now,
        });
    }

    /// Backspace takes back the run's newest prediction; with none of ours to take back, what it
    /// erases is the shell's business.
    fn erase(&mut self) {
        let took = if self.awaiting_redraw {
            self.blind.pop().is_some()
        } else {
            self.pending.pop().is_some()
        };
        if !took {
            self.interrupt();
        }
    }

    /// A key that may change the line or the program: the next run earns trust again. A trusted
    /// run keeps painting until the host answers, so Enter after fast typing does not blank it.
    fn interrupt(&mut self) {
        let run = std::mem::take(&mut self.pending);
        if self.trusted {
            self.held.extend(run);
        }
        self.blind.clear();
        self.trusted = false;
        self.awaiting_redraw = true;
    }

    /// Forgets every prediction (a resize, a new snapshot, a misprediction), keeping the
    /// measured echo time.
    pub fn reset(&mut self) {
        self.pending.clear();
        self.held.clear();
        self.blind.clear();
        self.trusted = false;
        self.awaiting_redraw = true;
    }

    /// The host's output landed in the mirror: measure, then confirm or refute.
    pub fn output(&mut self, surface: &impl Surface, now: Instant) {
        let Some(cursor) = surface.typing_cursor() else {
            self.reset();
            return;
        };
        let redraw = std::mem::take(&mut self.awaiting_redraw);
        let echoed = |p: &Prediction| {
            surface.char_at(p.row, p.col) == Some(p.ch)
                && (cursor.0 > p.row || (cursor.0 == p.row && cursor.1 > p.col))
        };
        // The host answered the key that ended the held run: its echoes are on screen, or the
        // screen moved under them.
        for held in std::mem::take(&mut self.held) {
            if echoed(&held) {
                self.sample(now.saturating_duration_since(held.at));
            }
        }
        let deadline = self.echo_deadline();
        let confirmed: Vec<bool> = self.pending.iter().map(echoed).collect();
        let newest = confirmed.iter().rposition(|&c| c);
        let mut gained = false;
        let mut wrong = false;
        for (ix, (p, &c)) in self.pending.iter().zip(&confirmed).enumerate() {
            if c {
                gained |= p.ch != ' ';
            } else if newest.is_some_and(|newest| ix < newest)
                || now.saturating_duration_since(p.at) >= deadline
            {
                // Echoes come in order: one left behind a confirmed echo, or one long overdue,
                // was a wrong guess.
                wrong = true;
            }
        }
        if wrong {
            self.reset();
            return;
        }
        let samples: Vec<Duration> = self
            .pending
            .iter()
            .zip(&confirmed)
            .filter(|&(_, &c)| c)
            .map(|(p, _)| now.saturating_duration_since(p.at))
            .collect();
        for sample in samples {
            self.sample(sample);
        }
        let had = !self.pending.is_empty();
        let mut kept = confirmed.iter();
        self.pending.retain(|_| kept.next().is_some_and(|&c| !c));
        // Output that echoes nothing we wait on: the host may have stopped echoing.
        self.trusted = (self.trusted || gained) && (newest.is_some() || !had);
        if self
            .pending
            .first()
            .is_some_and(|first| first.row != cursor.0)
        {
            // The host moved to another line: what we guessed for this one no longer applies.
            self.reset();
            return;
        }
        if redraw {
            self.place_blind(cursor, surface.columns(), now);
        }
    }

    /// The characters typed while the host had not redrawn, from its cursor now.
    fn place_blind(&mut self, cursor: (usize, usize), columns: usize, now: Instant) {
        let deadline = self.echo_deadline();
        let blind = std::mem::take(&mut self.blind);
        if blind
            .iter()
            .any(|(_, at)| now.saturating_duration_since(*at) >= deadline)
        {
            // Old enough to have landed already: where is anyone's guess.
            return;
        }
        for (ch, at) in blind {
            self.push(ch, cursor, columns, at);
        }
    }

    fn sample(&mut self, sample: Duration) {
        let srtt = self.srtt.map_or(sample, |srtt| (srtt * 7 + sample) / 8);
        self.srtt = Some(srtt);
        if srtt > SHOW_ABOVE {
            self.showing = true;
        } else if srtt < HIDE_BELOW {
            self.showing = false;
        }
    }

    fn echo_deadline(&self) -> Duration {
        self.srtt.unwrap_or(SHOW_ABOVE) * 2 + ECHO_SLACK
    }

    /// When the oldest prediction falls due, so a silent host still has it withdrawn.
    #[must_use]
    pub fn next_deadline(&self) -> Option<Instant> {
        let deadline = self.echo_deadline();
        self.pending
            .iter()
            .chain(&self.held)
            .map(|p| p.at + deadline)
            .min()
    }

    /// Withdraws everything once a prediction is past its deadline with no output to settle it;
    /// true when something was withdrawn.
    pub fn expire(&mut self, now: Instant) -> bool {
        let deadline = self.echo_deadline();
        let due = self
            .pending
            .iter()
            .chain(&self.held)
            .any(|p| now.saturating_duration_since(p.at) >= deadline);
        if due {
            self.reset();
        }
        due
    }

    /// What to paint over the mirror now.
    #[must_use]
    pub fn overlay(&self, surface: &impl Surface) -> Overlay {
        if !self.showing || !surface.at_bottom() || surface.typing_cursor().is_none() {
            return Overlay::default();
        }
        let run: &[Prediction] = if self.trusted { &self.pending } else { &[] };
        let cells: Vec<_> = self
            .held
            .iter()
            .chain(run)
            .map(|p| (p.row, p.col, p.ch))
            .collect();
        let cursor = cells.last().map(|&(row, col, _)| (row, col + 1));
        Overlay { cells, cursor }
    }

    /// The smoothed echo time, once measured.
    #[must_use]
    pub const fn echo_time(&self) -> Option<Duration> {
        self.srtt
    }
}

#[cfg(test)]
mod tests {
    use onicode_protocol::terminal::{Key, TerminalSize};
    use proptest::prelude::*;

    use super::*;
    use crate::mirror::Mirror;

    const COLS: u16 = 20;
    const RTT: Duration = Duration::from_millis(80);

    fn key(ch: char) -> Input {
        Input::Key(Key {
            key: ch.to_string(),
            key_char: Some(ch.to_string()),
            ..Key::default()
        })
    }

    fn named(name: &str) -> Input {
        Input::Key(Key {
            key: name.to_owned(),
            ..Key::default()
        })
    }

    fn mirror(rows: u16) -> Mirror {
        Mirror::new(TerminalSize {
            cols: COLS,
            rows,
            cell_width: 8,
            cell_height: 16,
        })
    }

    /// A host 80 ms away whose shell echoes what it is sent.
    struct Remote {
        mirror: Mirror,
        predictor: Predictor,
        now: Instant,
    }

    impl Remote {
        fn new() -> Self {
            let mut mirror = mirror(4);
            mirror.feed(b"$ ");
            Self {
                mirror,
                predictor: Predictor::default(),
                now: Instant::now(),
            }
        }

        fn send(&mut self, input: &Input) {
            self.predictor.typed(input, &self.mirror, self.now);
        }

        fn type_text(&mut self, text: &str) {
            for ch in text.chars() {
                self.send(&key(ch));
            }
        }

        fn host_writes(&mut self, bytes: &[u8]) {
            self.now += RTT;
            self.mirror.feed(bytes);
            self.predictor.output(&self.mirror, self.now);
        }

        fn shown(&self) -> String {
            self.predictor
                .overlay(&self.mirror)
                .cells
                .iter()
                .map(|c| c.2)
                .collect()
        }
    }

    #[test]
    fn a_run_shows_predictions_only_after_its_first_echo() {
        let mut r = Remote::new();
        r.type_text("l");
        assert_eq!(
            r.shown(),
            "",
            "nothing painted before the host proves it echoes"
        );
        r.host_writes(b"l");
        r.type_text("s");
        assert_eq!(r.shown(), "s", "a trusted run paints ahead of the echo");
        assert_eq!(r.predictor.overlay(&r.mirror).cursor, Some((0, 4)));
        r.host_writes(b"s");
        assert_eq!(r.shown(), "", "the echo retires it");
    }

    #[test]
    fn a_password_prompt_never_paints_what_is_typed() {
        let mut r = Remote::new();
        r.type_text("x");
        r.host_writes(b"x");
        r.send(&named("enter"));
        r.host_writes(b"\r\nPassword: ");
        for ch in "hunter2".chars() {
            r.type_text(&ch.to_string());
            assert_eq!(r.shown(), "", "a silent prompt never earns trust");
        }
        r.now += Duration::from_secs(1);
        assert!(
            r.predictor.expire(r.now),
            "unechoed predictions are withdrawn"
        );
        assert_eq!(r.predictor.next_deadline(), None);
    }

    #[test]
    fn a_wrong_echo_wipes_every_prediction() {
        let mut r = Remote::new();
        r.type_text("a");
        r.host_writes(b"a");
        r.type_text("bc");
        assert_eq!(r.shown(), "bc");
        // The host draws something else where "b" was predicted, then echoes "c" past it.
        r.host_writes(b"Zc");
        assert_eq!(r.shown(), "");
        r.type_text("d");
        assert_eq!(r.shown(), "", "trust must be earned again");
    }

    #[test]
    fn backspace_takes_back_our_own_prediction_only() {
        let mut r = Remote::new();
        r.type_text("a");
        r.host_writes(b"a");
        r.type_text("bc");
        r.send(&named("backspace"));
        assert_eq!(r.shown(), "b");
        r.host_writes(b"b");
        r.send(&named("backspace")); // erases the host's text: not ours to predict
        r.type_text("x");
        assert_eq!(r.shown(), "", "a new run after an unpredicted erase");
    }

    #[test]
    fn a_fast_link_never_paints_predictions() {
        let mut r = Remote::new();
        for ch in ["a", "b", "c", "d"] {
            r.type_text(ch);
            r.now += Duration::from_millis(2);
            r.mirror.feed(ch.as_bytes());
            r.predictor.output(&r.mirror, r.now);
        }
        r.type_text("e");
        assert_eq!(r.shown(), "", "2 ms echoes need no prediction");
    }

    #[test]
    fn a_prompt_appearing_mid_run_stops_the_painting() {
        let mut r = Remote::new();
        r.type_text("g");
        r.host_writes(b"g");
        r.type_text("i");
        assert_eq!(r.shown(), "i");
        // Output that echoes nothing we typed (a passphrase prompt redraws the line).
        r.host_writes(b"\r\x1b[KPassphrase: ");
        r.type_text("s");
        assert_eq!(r.shown(), "", "untrusted after a non-echo");
    }

    #[test]
    fn enter_keeps_a_trusted_run_painted_until_the_host_answers() {
        let mut r = Remote::new();
        r.type_text("l");
        r.host_writes(b"l");
        r.type_text("s");
        r.send(&named("enter"));
        assert_eq!(r.shown(), "s", "Enter does not blank what was typed");
        r.host_writes(b"s\r\n$ ");
        assert_eq!(r.shown(), "");
    }

    #[test]
    fn typing_before_the_redraw_lands_at_the_new_prompt() {
        let mut r = Remote::new();
        r.type_text("a");
        r.host_writes(b"a");
        r.send(&named("enter"));
        r.type_text("xy");
        assert_eq!(
            r.predictor.next_deadline(),
            None,
            "no guesses from a stale cursor"
        );
        r.host_writes(b"\r\n$ ");
        assert_eq!(
            r.predictor.pending.first().map(|p| (p.row, p.col)),
            Some((1, 2))
        );
    }

    #[test]
    fn a_space_over_a_blank_cell_is_not_an_echo() {
        let mut r = Remote::new();
        r.type_text(" ");
        // Unrelated output on another row: the blank cell after the prompt "matches" the space,
        // but the cursor never passed it.
        r.now += RTT;
        r.predictor.output(&r.mirror, r.now);
        r.type_text("p");
        assert_eq!(r.shown(), "", "no trust from a blank cell");
    }

    #[test]
    fn predictions_stop_at_the_line_edge() {
        let mut r = Remote::new();
        r.type_text("a");
        r.host_writes(b"a");
        r.type_text(&"x".repeat(40));
        assert_eq!(
            r.shown().len(),
            usize::from(COLS) - 3 - 1,
            "no guesses about wrapping"
        );
    }

    #[test]
    fn nothing_paints_on_the_alternate_screen_or_scrolled_back() {
        let mut r = Remote::new();
        r.type_text("a");
        r.host_writes(b"a");
        r.type_text("b");
        assert_eq!(r.shown(), "b");
        r.host_writes(b"\x1b[?1049h");
        assert_eq!(r.shown(), "");
        r.type_text("c");
        assert_eq!(r.predictor.next_deadline(), None);
    }

    /// A line-editing shell: echoes printable keys, erases with `BS SP BS`, and after `pw` and
    /// Enter asks for a password it never echoes.
    #[derive(Default)]
    struct Shell {
        line: String,
        password: bool,
    }

    impl Shell {
        fn key(&mut self, step: Step) -> Vec<u8> {
            match (step, self.password) {
                (Step::Char(_) | Step::Erase, true) => Vec::new(),
                (Step::Char(ch), false) if 2 + self.line.len() + 1 < usize::from(COLS) => {
                    self.line.push(ch);
                    vec![u8::try_from(ch).unwrap()]
                }
                (Step::Erase, false) if self.line.pop().is_some() => b"\x08 \x08".to_vec(),
                (Step::Char(_) | Step::Erase, false) => b"\x07".to_vec(),
                (Step::Enter, _) => {
                    let password = !self.password && self.line == "pw";
                    self.password = password;
                    self.line.clear();
                    if password {
                        b"\r\nPassword: ".to_vec()
                    } else {
                        b"\r\n$ ".to_vec()
                    }
                }
                (Step::Wait(_), _) => unreachable!("waits are not keys"),
            }
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum Step {
        Char(char),
        Erase,
        Enter,
        Wait(u64),
    }

    fn step() -> impl Strategy<Value = Step> {
        prop_oneof![
            8 => prop::sample::select(vec!['p', 'w', 'a', ' ', '1']).prop_map(Step::Char),
            1 => Just(Step::Erase),
            2 => Just(Step::Enter),
            4 => (1u64..120).prop_map(Step::Wait),
        ]
    }

    /// The viewer's grid as painted: the mirror's text with the overlay on it.
    fn painted(mirror: &Mirror, overlay: &Overlay) -> Vec<String> {
        let (cols, rows) = mirror.size();
        (0..usize::from(rows))
            .map(|row| {
                (0..usize::from(cols))
                    .map(|col| {
                        overlay
                            .cells
                            .iter()
                            .find(|c| (c.0, c.1) == (row, col))
                            .map_or_else(|| mirror.char_at(row, col).unwrap_or(' '), |c| c.2)
                    })
                    .collect()
            })
            .collect()
    }

    fn grid(mirror: &Mirror) -> Vec<String> {
        painted(mirror, &Overlay::default())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        /// For any seeded typing, echo and latency schedule over a FIFO link: a password typed
        /// into a silent prompt is never painted, a fast link never paints, and once the host's
        /// output has settled and its echo deadline passed, the painted grid is the mirror's.
        #[test]
        fn predictions_never_outlive_the_hosts_truth(
            steps in proptest::collection::vec(step(), 1..120),
            latencies in proptest::collection::vec(1u64..200, 1..16),
            fast in any::<bool>(),
        ) {
            let start = Instant::now();
            let mut now = start;
            let mut shell = Shell::default();
            let mut mirror = mirror(6);
            mirror.feed(b"$ ");
            let mut predictor = Predictor::default();
            // Output in flight: when it lands, and the bytes.
            let mut link: Vec<(Instant, Vec<u8>)> = Vec::new();
            let mut sent = 0usize;
            let deliver = |until: Instant,
                           link: &mut Vec<(Instant, Vec<u8>)>,
                           mirror: &mut Mirror,
                           predictor: &mut Predictor| {
                let ready = link.iter().take_while(|(at, _)| *at <= until).count();
                for (at, bytes) in link.drain(..ready) {
                    mirror.feed(&bytes);
                    predictor.output(&*mirror, at);
                }
            };
            for step in steps {
                if let Step::Wait(ms) = step {
                    let until = now + Duration::from_millis(ms);
                    // The app's deadline timer, between deliveries.
                    while let Some(due) = predictor.next_deadline().filter(|due| *due <= until) {
                        deliver(due, &mut link, &mut mirror, &mut predictor);
                        predictor.expire(due);
                        if predictor.next_deadline() == Some(due) {
                            break;
                        }
                    }
                    deliver(until, &mut link, &mut mirror, &mut predictor);
                    now = until;
                } else {
                    let input = match step {
                        Step::Char(ch) => key(ch),
                        Step::Erase => named("backspace"),
                        _ => named("enter"),
                    };
                    predictor.typed(&input, &mirror, now);
                    let latency = if fast {
                        latencies[sent % latencies.len()] % 8 + 1
                    } else {
                        latencies[sent % latencies.len()] + 40
                    };
                    sent += 1;
                    let lands = (now + Duration::from_millis(latency))
                        .max(link.last().map_or(now, |(at, _)| *at));
                    let echo = shell.key(step);
                    if !echo.is_empty() {
                        link.push((lands, echo));
                    }
                }
                let overlay = predictor.overlay(&mirror);
                if fast {
                    prop_assert!(overlay.is_empty(), "a fast link painted {:?}", overlay);
                }
                for (row, _, _) in &overlay.cells {
                    let line: String = (0..usize::from(COLS))
                        .map(|col| mirror.char_at(*row, col).unwrap_or(' '))
                        .collect();
                    prop_assert!(
                        !line.starts_with("Password:"),
                        "painted a password at row {}: {:?}",
                        row,
                        overlay
                    );
                }
            }
            // The host's output settles, then its echo deadline passes.
            let last = link.last().map_or(now, |(at, _)| *at).max(now);
            deliver(last, &mut link, &mut mirror, &mut predictor);
            let settled = last + Duration::from_secs(2);
            predictor.expire(settled);
            let overlay = predictor.overlay(&mirror);
            prop_assert_eq!(painted(&mirror, &overlay), grid(&mirror));
            prop_assert_eq!(overlay.cursor, None);
        }
    }
}
