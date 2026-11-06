use std::collections::VecDeque;
use std::sync::Arc;

use super::{Claimant, Claims, Kept, Restore, SETTLE_MOST, Sources, Tis};

const US: &str = "com.apple.keylayout.US";
const FRENCH: &str = "com.apple.keylayout.French";
const GERMAN: &str = "com.apple.keylayout.German";
const TELEX: &str = "com.apple.inputmethod.VietnameseIM.VietnameseSimpleTelex";

type Job = Box<dyn FnOnce() + Send>;

/// A Mac's input sources as a test plays them: the current one, the ones installed but off,
/// every call made, and, when deferred, the main queue's jobs waiting for the test to run them.
#[derive(Default)]
struct State {
    current: Option<String>,
    off: Vec<String>,
    refused: Vec<String>,
    calls: Vec<String>,
    queue: Option<VecDeque<Job>>,
}

#[derive(Clone, Default)]
struct Fake(Arc<parking_lot::Mutex<State>>);

impl Fake {
    fn on(current: &str) -> Self {
        let fake = Self::default();
        fake.0.lock().current = Some(current.to_owned());
        fake
    }

    /// Hold the main queue's jobs until [`Self::run`].
    fn deferred(self) -> Self {
        self.0.lock().queue = Some(VecDeque::new());
        self
    }

    /// Run every job waiting, in order.
    fn run(&self) {
        loop {
            let job = self.0.lock().queue.as_mut().and_then(VecDeque::pop_front);
            let Some(job) = job else { break };
            job();
        }
    }

    fn calls(&self) -> Vec<String> {
        std::mem::take(&mut self.0.lock().calls)
    }

    fn selected(&self) -> Option<String> {
        self.0.lock().current.clone()
    }

    /// The person at the host picks `source` from the menu bar.
    fn picked(&self, source: &str) {
        self.0.lock().current = Some(source.to_owned());
    }
}

impl Tis for Fake {
    fn on_main(&self, job: Job) {
        let mut state = self.0.lock();
        if let Some(queue) = state.queue.as_mut() {
            queue.push_back(job);
            return;
        }
        drop(state);
        job();
    }

    fn current(&self) -> Option<String> {
        self.0.lock().current.clone()
    }

    fn select(&self, id: &str) -> Result<bool, String> {
        let mut state = self.0.lock();
        state.calls.push(format!("select {id}"));
        let refused = state.refused.iter().any(|r| r == id);
        let was_off = state.off.iter().any(|o| o == id);
        if !refused {
            state.off.retain(|o| o != id);
            state.current = Some(id.to_owned());
        }
        drop(state);
        if refused {
            Err("not selectable".to_owned())
        } else {
            Ok(was_off)
        }
    }

    fn disable(&self, id: &str) {
        let mut state = self.0.lock();
        state.calls.push(format!("disable {id}"));
        state.off.push(id.to_owned());
    }
}

fn who() -> Claimant {
    Claimant::next()
}

fn ours(source: &str) -> Restore {
    Restore {
        select: Some(source.to_owned()),
        disable: Vec::new(),
        ours: None,
    }
}

/// Every stream draws its own token: two clients' first streams, or one client's stream before
/// and after it reconnects, all share `StreamId(1)` and never share a claim.
#[test]
fn every_stream_draws_its_own_claimant() {
    let (a, b) = (who(), who());
    assert_ne!(a, b);
    let mut claims = Claims::new();
    claims.claim(a, TELEX.to_owned(), Some(US.to_owned()));
    claims.claim(b, FRENCH.to_owned(), Some(TELEX.to_owned()));
    assert_eq!(claims.of(a), Some(TELEX));
    assert_eq!(claims.of(b), Some(FRENCH));
    assert_eq!(
        claims.release(a),
        Restore::default(),
        "B still asks: nothing moves"
    );
    assert_eq!(
        claims.release(b).select.as_deref(),
        Some(US),
        "none asks: the host's own"
    );
}

/// A client reconnects: its new stream asks before the old connection's stream has ended. The
/// old stream's release, queued last, leaves the new stream's claim alone. Keyed by the client
/// and its stream id, both were `(client, StreamId(1))`, and the release took the new claim.
#[tokio::test]
async fn a_reconnected_streams_claim_outlives_the_old_streams_release() {
    let fake = Fake::on(US);
    let sources = Sources::new(fake.clone());
    let old = sources.claimant();
    assert!(old.ask(TELEX.to_owned()).await);
    let new = sources.claimant();
    assert!(new.ask(TELEX.to_owned()).await);
    fake.calls();
    drop(old);
    assert!(fake.calls().is_empty(), "nothing moves");
    assert_eq!(
        fake.selected().as_deref(),
        Some(TELEX),
        "the new stream types under Telex"
    );
    drop(new);
    assert_eq!(fake.selected().as_deref(), Some(US));
}

/// Two clients: the last to ask wins; the first leaving changes nothing; the last leaving
/// brings the one still asking back, then the host's own once none asks.
#[test]
fn the_last_to_ask_wins_and_the_original_comes_back() {
    let (a, b, c) = (who(), who(), who());
    let mut claims = Claims::new();
    assert_eq!(
        claims.claim(a, TELEX.to_owned(), Some(US.to_owned())),
        TELEX
    );
    claims.selected(TELEX.to_owned(), false);
    assert_eq!(
        claims.claim(b, FRENCH.to_owned(), Some(TELEX.to_owned())),
        FRENCH
    );
    claims.selected(FRENCH.to_owned(), false);
    assert_eq!(claims.release(a), Restore::default(), "not the latest");
    assert_eq!(
        claims.claim(c, TELEX.to_owned(), Some(FRENCH.to_owned())),
        TELEX
    );
    claims.selected(TELEX.to_owned(), false);
    let back = claims.release(c);
    assert_eq!(
        back,
        Restore {
            ours: Some(TELEX.to_owned()),
            ..ours(FRENCH)
        },
        "the one asking"
    );
    claims.selected(FRENCH.to_owned(), false);
    let back = claims.release(b);
    assert_eq!(
        back,
        Restore {
            ours: Some(FRENCH.to_owned()),
            ..ours(US)
        },
        "the host's own"
    );
    assert_eq!(claims.release(b), Restore::default(), "released once");
}

/// A claimant asking again moves to the front, and the original is the one from before the
/// first claim, not a claimed one.
#[test]
fn asking_again_moves_to_the_front() {
    let (a, b) = (who(), who());
    let mut claims = Claims::new();
    claims.claim(a, "jp".to_owned(), Some("us".to_owned()));
    claims.claim(b, "fr".to_owned(), Some("jp".to_owned()));
    claims.claim(a, "jp".to_owned(), Some("fr".to_owned()));
    assert_eq!(claims.release(b), Restore::default());
    assert_eq!(claims.release(a).select.as_deref(), Some("us"));
}

/// A source the host already has is answered at once, with nothing selected and no wait for
/// a switch that will never be heard.
#[tokio::test(start_paused = true)]
async fn a_source_already_current_is_answered_at_once() {
    let fake = Fake::on(US);
    let sources = Sources::new(fake.clone());
    sources.hearing();
    let started = tokio::time::Instant::now();
    assert!(sources.claim(who(), US.to_owned()).await);
    assert_eq!(
        started.elapsed(),
        std::time::Duration::ZERO,
        "no settle wait"
    );
    assert!(fake.calls().is_empty(), "nothing selected");
}

/// A switch is answered once the platform reports it, not after a fixed delay; one never
/// reported is answered at the bound.
#[tokio::test(start_paused = true)]
async fn a_switch_is_answered_once_heard() {
    let fake = Fake::on(US);
    let sources = Sources::new(fake.clone());
    sources.hearing();
    let a = who();
    let answer = tokio::spawn(sources.claim(a, FRENCH.to_owned()));
    tokio::time::sleep(SETTLE_MOST / 4).await;
    assert!(!answer.is_finished(), "waits for the switch to be heard");
    sources.heard(Some(FRENCH.to_owned()));
    assert!(answer.await.unwrap());
    assert_eq!(fake.calls(), [format!("select {FRENCH}")]);

    let started = tokio::time::Instant::now();
    assert!(
        sources.claim(a, TELEX.to_owned()).await,
        "selected, never heard"
    );
    assert_eq!(started.elapsed(), SETTLE_MOST);
}

/// A report of the wanted source from before the switch (the host under French a while ago,
/// the report of the switch back to US lost) does not answer it: only one made after the
/// selection does. Read off the channel's latest value, the answer went at once.
#[tokio::test(start_paused = true)]
async fn a_stale_report_of_the_source_does_not_answer_a_switch() {
    let fake = Fake::on(US);
    let sources = Sources::new(fake);
    sources.hearing();
    sources.heard(Some(FRENCH.to_owned()));
    let answer = tokio::spawn(sources.claim(who(), FRENCH.to_owned()));
    tokio::time::sleep(SETTLE_MOST / 4).await;
    assert!(
        !answer.is_finished(),
        "the report from before the switch is not its answer"
    );
    sources.heard(Some(FRENCH.to_owned()));
    assert!(answer.await.unwrap());
}

/// A source the host turned on for a claim goes off again once none asks, after the host's
/// own is selected back; one it cannot select is answered no, and the one before comes back.
#[tokio::test]
async fn sources_turned_on_go_off_once_none_asks() {
    let fake = Fake::on(US);
    fake.0.lock().off.push(TELEX.to_owned());
    fake.0.lock().refused.push(FRENCH.to_owned());
    let sources = Sources::new(fake.clone());
    let (a, b) = (who(), who());
    assert!(sources.claim(a, TELEX.to_owned()).await);
    assert!(!sources.claim(b, FRENCH.to_owned()).await, "not selectable");
    assert_eq!(
        fake.calls(),
        [
            format!("select {TELEX}"),
            format!("select {FRENCH}"),
            format!("select {TELEX}")
        ]
    );
    sources.release(a);
    assert_eq!(
        fake.calls(),
        [format!("select {US}"), format!("disable {TELEX}")]
    );
    assert_eq!(fake.selected().as_deref(), Some(US));
}

/// The person at the host picks German by hand while a client's French is selected: when
/// the claim goes, German stays, and the source they are under is never turned off, though
/// the host turned it on for a claim.
#[tokio::test]
async fn a_source_picked_by_hand_is_left_as_it_is() {
    let fake = Fake::on(US);
    fake.0
        .lock()
        .off
        .extend([FRENCH.to_owned(), GERMAN.to_owned()]);
    let sources = Sources::new(fake.clone());
    let (a, b) = (who(), who());
    assert!(sources.claim(a, FRENCH.to_owned()).await);
    assert!(sources.claim(b, GERMAN.to_owned()).await);
    assert!(sources.claim(a, FRENCH.to_owned()).await);
    fake.calls();
    fake.picked(GERMAN);
    sources.release(a);
    assert!(fake.calls().is_empty(), "German stays, B's claim or not");
    sources.release(b);
    assert_eq!(
        fake.calls(),
        [format!("disable {FRENCH}")],
        "German, current, stays on"
    );
    assert_eq!(fake.selected().as_deref(), Some(GERMAN));
}

/// A release queued after a claim runs after it, whenever the main queue gets to them: the claim
/// is never left behind with nobody to release it.
#[tokio::test]
async fn a_release_runs_after_the_claim_before_it() {
    let fake = Fake::on(US).deferred();
    let sources = Sources::new(fake.clone());
    let a = who();
    let answer = sources.claim(a, FRENCH.to_owned());
    sources.release(a);
    fake.run();
    assert_eq!(
        fake.calls(),
        [format!("select {FRENCH}"), format!("select {US}")]
    );
    assert!(sources.0.claims.lock().kept().is_none(), "no claim left");
    assert!(answer.await, "answered for the stream that asked");
}

/// A stream's claim, dropped however the stream ends (a panic unwinding its task included),
/// queues its release.
#[tokio::test]
async fn a_claim_dropped_by_a_panic_is_released() {
    let fake = Fake::on(US);
    let sources = Sources::new(fake.clone());
    let claim = sources.claimant();
    assert!(claim.ask(FRENCH.to_owned()).await);
    let task = tokio::spawn(async move {
        let _claim = claim;
        panic!("a stream's task fell over");
    });
    assert!(task.await.is_err());
    assert_eq!(
        fake.selected().as_deref(),
        Some(US),
        "the host's own is back"
    );
}

/// While a claim holds, the host's own source, what it turned on and what it selected are on
/// disk; a run that ends without letting go (a crash, `SIGKILL`) has them brought back at the
/// next start, and the file goes.
#[tokio::test]
async fn the_original_source_is_kept_and_restored_at_the_next_start() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("input-source");
    let fake = Fake::on(US);
    fake.0.lock().off.push(TELEX.to_owned());
    let crashed = Sources::new(fake.clone());
    crashed.keep_at(path.clone());
    assert!(crashed.claim(who(), TELEX.to_owned()).await);
    let kept = Kept::parse(&std::fs::read_to_string(&path).unwrap());
    let want = Kept {
        original: Some(US.to_owned()),
        enabled: vec![TELEX.to_owned()],
        selected: Some(TELEX.to_owned()),
    };
    assert_eq!(kept, want);
    drop(crashed);
    fake.calls();

    let next = Sources::new(fake.clone());
    next.keep_at(path.clone());
    assert_eq!(
        fake.calls(),
        [format!("select {US}"), format!("disable {TELEX}")]
    );
    assert!(!path.exists(), "restored: nothing kept");
}

/// A start after a crash finds the person under a source they picked since: it stays, and so
/// does whatever they are under; only the other sources the host turned on go off.
#[tokio::test]
async fn a_restore_at_start_leaves_a_source_picked_since() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("input-source");
    let kept = Kept {
        original: Some(US.to_owned()),
        enabled: vec![TELEX.to_owned(), GERMAN.to_owned()],
        selected: Some(TELEX.to_owned()),
    };
    std::fs::write(&path, kept.to_text()).unwrap();
    let fake = Fake::on(GERMAN);
    Sources::new(fake.clone()).keep_at(path.clone());
    assert_eq!(fake.calls(), [format!("disable {TELEX}")]);
    assert_eq!(fake.selected().as_deref(), Some(GERMAN));
    assert!(!path.exists());
}

/// The host stopping lets every claim go: its own source back, the file gone.
#[tokio::test]
async fn stopping_lets_every_claim_go() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("input-source");
    let fake = Fake::on(US);
    let sources = Sources::new(fake.clone());
    sources.keep_at(path.clone());
    assert!(sources.claim(who(), TELEX.to_owned()).await);
    assert!(sources.claim(who(), FRENCH.to_owned()).await);
    assert!(path.exists());
    fake.calls();
    sources.release_all().await;
    assert_eq!(fake.calls(), [format!("select {US}")]);
    assert!(!path.exists());
}

/// A stream hears the source the host is under at each switch: the one whose claim another
/// client took learns it there.
#[tokio::test]
async fn each_switch_is_heard_by_every_stream() {
    let fake = Fake::on(US);
    let sources = Sources::new(fake);
    let mut heard = sources.subscribe();
    let (a, b) = (who(), who());
    assert!(sources.claim(a, TELEX.to_owned()).await);
    assert_eq!(heard.borrow_and_update().as_deref(), Some(TELEX));
    assert!(sources.claim(b, FRENCH.to_owned()).await);
    assert!(heard.has_changed().unwrap());
    assert_eq!(heard.borrow_and_update().as_deref(), Some(FRENCH));
    sources.release(b);
    assert_eq!(
        heard.borrow_and_update().as_deref(),
        Some(TELEX),
        "A's comes back"
    );
}

/// What is kept reads back as written, and a line it does not know is skipped.
#[test]
fn kept_reads_back() {
    let kept = Kept {
        original: Some(US.to_owned()),
        enabled: vec![TELEX.to_owned()],
        selected: Some(TELEX.to_owned()),
    };
    assert_eq!(Kept::parse(&kept.to_text()), kept);
    assert_eq!(Kept::parse("junk\noriginal \n"), Kept::default());
}
