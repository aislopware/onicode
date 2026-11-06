//! Viewport-local dictation. Only transcript strings cross this boundary; audio
//! never enters the engine, a document, an attachment, or the RPC transport.
use std::borrow::Cow;
use std::ops::Range;
use std::time::{Duration, Instant};

use unicode_segmentation::UnicodeSegmentation;

pub(crate) mod glass;
mod meter;
mod model;
pub(crate) mod waveform;
pub(crate) use meter::{Bar, FLOOR, Meter};
pub(crate) use model::{card, enabled, init};

pub(crate) const FINALIZE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(tag = "kind", content = "text", rename_all = "snake_case")]
pub(crate) enum Event {
    Listening,
    Finalizing,
    Partial(String),
    Final(String),
    Denied(String),
    Unavailable(String),
    Failed(String),
    Cancelled,
}

/// Implementations and their callbacks are owned by the UI thread. Tests inject
/// a fake; permission prompts and microphone access are never needed in CI.
pub(crate) trait Transcriber {
    fn poll(&mut self) -> Option<Event>;
    fn finish(&mut self);
    /// Peak input RMS since the previous call, for the live waveform.
    fn level(&mut self) -> f32 {
        0.0
    }
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn onicode_microphone_permission() -> i32;
    fn onicode_request_microphone();
    fn onicode_microphone_window() -> usize;
}
fn permission() -> i32 {
    #[cfg(target_os = "macos")]
    {
        unsafe { onicode_microphone_permission() }
    }
    #[cfg(not(target_os = "macos"))]
    {
        1
    }
}
pub(crate) fn permission_pending() -> bool {
    permission() == 0
}
fn origin_window() -> usize {
    #[cfg(target_os = "macos")]
    {
        unsafe { onicode_microphone_window() }
    }
    #[cfg(not(target_os = "macos"))]
    {
        1
    }
}
struct Native {
    pending: Option<Event>,
    origin_window: usize,
    dir: std::path::PathBuf,
    device: Option<String>,
    session: Option<onicode_voice::Session>,
    finished: bool,
}
impl Transcriber for Native {
    fn poll(&mut self) -> Option<Event> {
        if let Some(event) = self.pending.take() {
            return Some(event);
        }
        if self.finished {
            return None;
        }
        if self.session.is_none() {
            match permission() {
                0 => return None,
                -2 => {
                    self.finished = true;
                    return Some(Event::Unavailable(
                        "Dictation requires the packaged Onicode app.".into(),
                    ));
                }
                -1 => {
                    self.finished = true;
                    return Some(Event::Denied("Allow Microphone access for Onicode in System Settings → Privacy & Security, then retry.".into()));
                }
                _ if self.origin_window == 0 || origin_window() != self.origin_window => {
                    self.finished = true;
                    return Some(Event::Cancelled);
                }
                _ => match onicode_voice::Session::start(self.dir.clone(), self.device.clone()) {
                    Ok(s) => self.session = Some(s),
                    Err(e) => {
                        self.finished = true;
                        return Some(Event::Failed(e.to_string()));
                    }
                },
            }
        }
        self.session.as_mut()?.poll().map(|event| match event {
            onicode_voice::Event::Listening => Event::Listening,
            onicode_voice::Event::Finalizing => Event::Finalizing,
            onicode_voice::Event::Final(t) => Event::Final(t),
            onicode_voice::Event::Failed(e) => Event::Failed(e),
        })
    }

    fn level(&mut self) -> f32 {
        self.session.as_ref().map_or(0.0, |s| s.take_level())
    }

    fn finish(&mut self) {
        if let Some(s) = &mut self.session {
            s.finish();
        } else {
            self.finished = true;
            self.pending = Some(Event::Final(String::new()));
        }
    }
}
pub(crate) fn start(cx: &gpui::App) -> Option<Box<dyn Transcriber>> {
    if !enabled(cx) {
        return None;
    }
    let origin_window = origin_window();
    #[cfg(target_os = "macos")]
    if permission() == 0 {
        unsafe {
            onicode_request_microphone();
        }
    }
    Some(Box::new(Native {
        pending: None,
        origin_window,
        dir: model::directory(cx),
        device: crate::settings::current(cx).dictation_input.clone(),
        session: None,
        finished: false,
    }))
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) enum Phase {
    #[default]
    Idle,
    Requesting,
    Listening,
    Finalizing,
    NoSpeech,
    /// Released almost immediately: dictation is hold to talk.
    Tapped,
    Denied(String),
    Unavailable(String),
    Failed(String),
}

impl Phase {
    pub fn active(&self) -> bool {
        matches!(self, Self::Requesting | Self::Listening | Self::Finalizing)
    }

    /// Button names describe the action, independently of the live status.
    pub fn action_label(&self) -> &'static str {
        match self {
            Self::Idle | Self::Tapped => "Hold to dictate",
            Self::Requesting | Self::Listening => "Release to transcribe",
            Self::Finalizing => "Transcribing",
            Self::NoSpeech | Self::Denied(_) | Self::Unavailable(_) | Self::Failed(_) => {
                "Hold to retry dictation"
            }
        }
    }

    pub fn status(&self) -> Option<(&str, &str)> {
        Some(match self {
            Self::Idle => return None,
            Self::Requesting => ("Getting ready…", "Wait for Listening before speaking."),
            Self::Listening => ("Listening", "Release when you’re done · Up to 1 minute"),
            Self::Finalizing => ("Transcribing…", "Processing on this device."),
            Self::NoSpeech => (
                "No speech detected",
                "Check your microphone, then try again.",
            ),
            Self::Tapped => (
                "Hold to dictate",
                "Keep holding the microphone or the shortcut while you speak.",
            ),
            Self::Denied(message) | Self::Unavailable(message) => {
                ("Dictation unavailable", message)
            }
            Self::Failed(message) => ("Dictation stopped", message),
        })
    }
}

#[derive(Default)]
pub(crate) struct Dictation {
    pub phase: Phase,
    pub generation: u64,
    range: Range<usize>,
    /// Dictation began at a caret, not over a selection: the transcript gets
    /// word boundaries against the text on either side.
    caret: bool,
    expected: String,
    pub has_partial: bool,
    pending_send: bool,
    finish_started: Option<Instant>,
    pub meter: Meter,
}

impl Dictation {
    pub fn begin(&mut self, content: &str, selection: Range<usize>) {
        self.cancel();
        self.phase = Phase::Requesting;
        self.caret = selection.is_empty();
        self.range = selection;
        self.expected = content.to_owned();
        self.has_partial = false;
    }

    /// Invalidates late callbacks AND pending sends, preserving committed text.
    pub fn cancel(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.phase = Phase::Idle;
        self.pending_send = false;
        self.finish_started = None;
        self.expected.clear();
        self.meter.reset();
    }

    /// Returns true only on the transition that must finish the audio stream.
    pub fn finish(&mut self, send: bool, now: Instant) -> bool {
        if !self.phase.active() {
            return false;
        }
        self.pending_send |= send;
        if self.phase == Phase::Finalizing {
            return false;
        }
        self.phase = Phase::Finalizing;
        self.finish_started = Some(now);
        self.meter.freeze(now);
        true
    }

    pub fn timed_out(&self, now: Instant) -> bool {
        self.finish_started
            .is_some_and(|at| now.duration_since(at) >= FINALIZE_TIMEOUT)
    }

    /// Empty recognition does not erase selected text or the latest partial.
    /// A changed draft cannot be overwritten, even if a caller missed cancellation.
    pub fn replace(&mut self, content: &mut String, transcript: &str) -> Option<usize> {
        if !self.phase.active() || *content != self.expected {
            self.cancel();
            return None;
        }
        if transcript.trim().is_empty() {
            return None;
        }
        // `range` covers the previous partial with its padding, so its
        // neighbours are still the text around the original caret.
        let insertion = if self.caret {
            with_word_boundaries(
                &content[..self.range.start],
                transcript,
                &content[self.range.end..],
            )
        } else {
            Cow::Borrowed(transcript)
        };
        content.replace_range(self.range.clone(), &insertion);
        self.range.end = self.range.start + insertion.len();
        self.expected.clone_from(content);
        self.has_partial = true;
        Some(self.range.end)
    }

    pub fn complete(&mut self, phase: Phase) -> bool {
        let send = self.pending_send && phase == Phase::Idle;
        self.cancel();
        self.phase = phase;
        send
    }
}

/// Pads a transcript dictated at a caret so it does not fuse with the word
/// beside it: `Hello|` + `world` gives `Hello world`, `|world` + `Hello` gives
/// `Hello world`. A side is padded only when the transcript does not already
/// bring whitespace there, the neighbour ends a word (a letter, mark or number
/// in any script, or closing punctuation) or starts one, and the far side of
/// the caret is whitespace or the draft's edge, so a caret inside a word stays
/// a plain insertion. Parakeet only speaks word-spaced languages, but the draft
/// may not be: Han, kana, Thai, Lao, Khmer and Myanmar neighbours stay joined.
fn with_word_boundaries<'a>(before: &str, transcript: &'a str, after: &str) -> Cow<'a, str> {
    let ends_with_space = |text: &str| text.chars().next_back().is_none_or(char::is_whitespace);
    let starts_with_space = |text: &str| text.chars().next().is_none_or(char::is_whitespace);
    // A grapheme's first char is its base, so a word ending in a combining
    // mark (Hindi `नमस्ते`) still reads as a letter.
    let word = |grapheme: &str| {
        grapheme
            .chars()
            .next()
            .is_some_and(|base| base.is_alphanumeric() && !spaceless(base))
    };
    let left = !starts_with_space(transcript)
        && starts_with_space(after)
        && before.graphemes(true).next_back().is_some_and(|last| {
            word(last)
                || matches!(
                    last,
                    "." | "!" | "?" | "," | ":" | ";" | ")" | "]" | "}" | "'" | "\""
                )
        });
    let right = !ends_with_space(transcript)
        && ends_with_space(before)
        && after
            .graphemes(true)
            .next()
            .is_some_and(|first| word(first) || matches!(first, "(" | "[" | "{" | "'" | "\""));
    match (left, right) {
        (false, false) => Cow::Borrowed(transcript),
        (left, right) => Cow::Owned(format!(
            "{}{transcript}{}",
            if left { " " } else { "" },
            if right { " " } else { "" },
        )),
    }
}

/// Scripts written without spaces between words.
fn spaceless(c: char) -> bool {
    matches!(
        u32::from(c),
        0x0e00..=0x0eff // Thai, Lao
            | 0x1000..=0x109f // Myanmar
            | 0x1780..=0x17ff // Khmer
            | 0x19e0..=0x19ff // Khmer symbols
            | 0x2e80..=0x2fdf // CJK and Kangxi radicals
            | 0x3005..=0x3007 // 々 〆 〇
            | 0x3040..=0x30ff // Hiragana, Katakana
            | 0x3100..=0x312f // Bopomofo
            | 0x31f0..=0x31ff // Katakana phonetic extensions
            | 0x3400..=0x4dbf // CJK extension A
            | 0x4e00..=0x9fff // CJK unified ideographs
            | 0xa9e0..=0xa9ff // Myanmar extended-B
            | 0xaa60..=0xaa7f // Myanmar extended-A
            | 0xf900..=0xfaff // CJK compatibility ideographs
            | 0xff66..=0xff9f // Halfwidth Katakana
            | 0x1b000..=0x1b16f // Kana supplement and extensions
            | 0x20000..=0x3ffff // CJK extensions B onward
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finishing_before_permission_does_not_wait_or_start_capture() {
        let mut native = Native {
            pending: None,
            origin_window: 0,
            dir: std::path::PathBuf::new(),
            device: None,
            session: None,
            finished: false,
        };
        native.finish();
        assert_eq!(native.poll(), Some(Event::Final(String::new())));
        assert_eq!(native.poll(), None);
        assert!(native.session.is_none());
    }

    #[test]
    fn finalization_deadline_does_not_restart_when_send_follows_stop() {
        let mut state = Dictation::default();
        state.begin("draft", 5..5);
        let start = Instant::now();
        assert!(state.finish(false, start));
        assert!(!state.finish(true, start + Duration::from_secs(1)));
        assert!(!state.timed_out(start + FINALIZE_TIMEOUT - Duration::from_millis(1)));
        assert!(state.timed_out(start + FINALIZE_TIMEOUT));
        assert!(state.complete(Phase::Idle));
        assert!(!state.complete(Phase::Idle));
    }

    #[test]
    fn unexpected_draft_change_rejects_late_partial_and_send() {
        let mut state = Dictation::default();
        state.begin("old", 0..3);
        state.finish(true, Instant::now());
        let mut current = "new draft".to_owned();
        assert_eq!(state.replace(&mut current, "late"), None);
        assert_eq!(current, "new draft");
        assert!(!state.complete(Phase::Idle));
    }

    #[test]
    fn caret_insertion_adds_word_boundaries_in_any_word_spaced_script() {
        for (case, before, transcript, after, expected) in [
            ("after a word", "Hello", "world", "", "Hello world"),
            (
                "before a word",
                "",
                "Weitere",
                "Änderungen",
                "Weitere Änderungen",
            ),
            (
                "between words",
                "Hello ",
                "big",
                " world",
                "Hello big world",
            ),
            ("after a space", "Hello ", "world", "", "Hello world"),
            (
                "before a space",
                "Hello",
                "big",
                " world",
                "Hello big world",
            ),
            ("after a sentence", "Done.", "Next", "", "Done. Next"),
            (
                "before an opening bracket",
                "",
                "see",
                "(below)",
                "see (below)",
            ),
            (
                "Korean",
                "코드를",
                "검토해 주세요",
                "",
                "코드를 검토해 주세요",
            ),
            ("trailing combining mark", "नमस्ते", "दुनिया", "", "नमस्ते दुनिया"),
            ("supplementary letter", "𞤀", "𞤦", "", "𞤀 𞤦"),
            ("supplementary digit", "Hello", "𝟚", "", "Hello 𝟚"),
            (
                "Cyrillic before Latin",
                "",
                "привет",
                "world",
                "привет world",
            ),
            ("inside a word", "wor", "l", "d", "world"),
            (
                "Japanese before",
                "修正キャッシュ",
                "tests",
                "",
                "修正キャッシュtests",
            ),
            ("Thai after", "", "hello", "โลก", "helloโลก"),
            ("Chinese before", "你好", "hello", "", "你好hello"),
            ("emoji before", "🧪", "test", "", "🧪test"),
            (
                "transcript brings its space",
                "draft",
                " words",
                "",
                "draft words",
            ),
            (
                "transcript brings its trailing space",
                "before ",
                "speech ",
                "after",
                "before speech after",
            ),
            ("empty draft", "", "Hello", "", "Hello"),
            ("after a newline", "line\n", "next", "", "line\nnext"),
        ] {
            let inserted = with_word_boundaries(before, transcript, after);
            assert_eq!(format!("{before}{inserted}{after}"), expected, "{case}");
        }
    }

    /// Every combination of short neighbours and transcripts over an alphabet
    /// of letters, marks, digits, punctuation, whitespace and spaceless
    /// scripts: padding is at most one space a side, never doubles existing
    /// whitespace, and never splits a word the caret sits inside.
    #[test]
    fn caret_insertion_boundaries_hold_for_every_short_draft() {
        let alphabet = ["", "a", "é", "न्", "7", ".", "(", " ", "\n", "日", "ไ", "🧪"];
        let pairs: Vec<String> = alphabet
            .iter()
            .flat_map(|a| alphabet.iter().map(move |b| format!("{a}{b}")))
            .collect();
        for before in &pairs {
            for after in &pairs {
                for transcript in ["word", " word", "word ", "слово", "1.5"] {
                    let inserted = with_word_boundaries(before, transcript, after);
                    let left = inserted.len()
                        - inserted.trim_start_matches(' ').len()
                        - (transcript.len() - transcript.trim_start_matches(' ').len());
                    let right = inserted.len()
                        - inserted.trim_end_matches(' ').len()
                        - (transcript.len() - transcript.trim_end_matches(' ').len());
                    let case = format!("{before:?} + {transcript:?} + {after:?}");
                    assert!(left <= 1 && right <= 1, "{case}");
                    assert_eq!(
                        &inserted[left..inserted.len() - right],
                        transcript,
                        "{case}"
                    );
                    if left == 1 {
                        assert!(!before.ends_with(char::is_whitespace), "{case}");
                        assert!(!transcript.starts_with(char::is_whitespace), "{case}");
                    }
                    if right == 1 {
                        assert!(!after.starts_with(char::is_whitespace), "{case}");
                        assert!(!transcript.ends_with(char::is_whitespace), "{case}");
                    }
                    let inside_word =
                        |text: &str| !text.is_empty() && !text.ends_with(char::is_whitespace);
                    if inside_word(before)
                        && !after.is_empty()
                        && !after.starts_with(char::is_whitespace)
                    {
                        assert_eq!(inserted, transcript, "{case}");
                    }
                }
            }
        }
    }

    #[test]
    fn partials_keep_the_caret_boundaries_and_selections_get_none() {
        let mut state = Dictation::default();
        let mut draft = "Hello".to_owned();
        state.begin(&draft, 5..5);
        assert_eq!(state.replace(&mut draft, "wor"), Some(9));
        assert_eq!(draft, "Hello wor");
        assert_eq!(state.replace(&mut draft, "world"), Some(11));
        assert_eq!(draft, "Hello world");

        let mut draft = "keep selected suffix".to_owned();
        state.begin(&draft, 5..13);
        assert_eq!(state.replace(&mut draft, "speech"), Some(11));
        assert_eq!(draft, "keep speech suffix");
        let mut draft = "keepsuffix".to_owned();
        state.begin(&draft, 4..4);
        state.replace(&mut draft, "x");
        assert_eq!(draft, "keepxsuffix");
    }
}
