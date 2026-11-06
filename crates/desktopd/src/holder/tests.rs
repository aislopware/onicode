//! The holder and a desktopd's [`Remote`] over a real Unix socket, with a fake factory in the
//! holder and tasks standing in for both main threads: nothing here makes a real display.

use std::sync::Arc;
use std::time::Duration;

use onicode_desktop_proto::screen::{DisplayKey, DisplayShape};
use onicode_desktop_vdisplay::{DisplayError, Enforced, Plan};
use parking_lot::Mutex;
use tokio::sync::mpsc;

use super::{ALONE_FOR, Dial, Ended, Holder, Need, RESTORE_TRIES, Remote, kept, restore, serve};
use crate::sized::{Displays, Factory, Job, Main, Registry, plan_for};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Did {
    Made(u32),
    Released(u32),
}

/// A factory that numbers its displays and says what it did; it refuses the first `refuse`
/// makes, as macOS does while a display's identity is still another holder's.
#[derive(Clone, Debug, Default)]
struct Fake(Arc<Mutex<(Vec<Did>, u32, u32)>>);

impl Fake {
    fn refusing(refuse: u32) -> Self {
        Self(Arc::new(Mutex::new((Vec::new(), 0, refuse))))
    }

    fn did(&self) -> Vec<Did> {
        self.0.lock().0.clone()
    }
}

impl Factory for Fake {
    type Display = u32;

    fn create(&mut self, _key: DisplayKey, _plan: &Plan) -> Result<u32, DisplayError> {
        let mut state = self.0.lock();
        if state.2 > 0 {
            state.2 = state.2.saturating_sub(1);
            drop(state);
            return Err(DisplayError::Refused);
        }
        state.1 = state.1.saturating_add(1);
        let id = state.1;
        state.0.push(Did::Made(id));
        drop(state);
        Ok(id)
    }

    fn resize(&mut self, _display: &mut u32, _plan: &Plan) -> Result<(), DisplayError> {
        Ok(())
    }

    fn enforce(&mut self, _display: &u32) -> Result<Enforced, DisplayError> {
        Ok(Enforced::Settled)
    }

    fn id(&self, display: &u32) -> u32 {
        *display
    }

    fn release(&mut self, display: u32) {
        self.0.lock().0.push(Did::Released(display));
    }
}

/// A task that owns the state and runs jobs one at a time, as a main queue does; it ends, and
/// drops the state, with its handle.
struct TaskMain<S>(mpsc::UnboundedSender<Job<S>>);

impl<S: Send + 'static> Main<S> for TaskMain<S> {
    fn run(&self, job: Job<S>) {
        let _gone = self.0.send(job);
    }

    fn run_after(&self, delay: Duration, job: Job<S>) {
        let tx = self.0.clone();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let _gone = tx.send(job);
        });
    }
}

/// `state` on a task of its own, and the handle that ends it.
fn on_task<S: Send + 'static>(mut state: S) -> (Arc<dyn Main<S>>, tokio::task::AbortHandle) {
    let (tx, mut rx) = mpsc::unbounded_channel::<Job<S>>();
    let task = tokio::spawn(async move {
        while let Some(job) = rx.recv().await {
            job(&mut state);
        }
    });
    (Arc::new(TaskMain(tx)), task.abort_handle())
}

const KEY: DisplayKey = DisplayKey(*b"oni-held-key-01!");

const SHAPE: DisplayShape = DisplayShape {
    width: 2560,
    height: 1600,
    scale: 2.0,
    refresh_hz: 60,
};

/// A desktopd's displays over the holder at `socket`, lingering ten minutes, and the handle that
/// kills desktopd: its main thread ends without releasing anything.
fn desktopd(socket: &std::path::Path) -> (Displays<Remote>, tokio::task::AbortHandle) {
    let socket = socket.to_path_buf();
    let dial: Dial = Box::new(move |need| match need {
        Need::Fresh => Err(std::io::Error::other("no holder is started in these tests")),
        Need::Adopt | Need::Make => std::os::unix::net::UnixStream::connect(&socket),
    });
    let (main, killed) = on_task(Registry::new(Remote::new(dial)));
    let displays = Displays::new(main, Duration::from_secs(5));
    displays.set_linger(Duration::from_mins(10));
    (displays, killed)
}

/// A holder serving `socket` over `fake`, its list at `store`.
fn holder(
    fake: &Fake,
    socket: &std::path::Path,
    store: &std::path::Path,
) -> Arc<dyn Main<Holder<Fake>>> {
    let (main, _ends_with_the_test) = on_task(Holder::new(fake.clone(), store.to_path_buf()));
    let listener = super::listen(socket).unwrap();
    let serving = Arc::clone(&main);
    tokio::spawn(
        async move { serve(serving, None, Some(listener), Duration::from_mins(10)).await },
    );
    main
}

/// A display made through the holder outlives desktopd that asked for it: the next desktopd
/// adopts it, and its client takes the same display back. Released, it ends in the holder and
/// leaves its list.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_held_display_outlives_its_desktopd_and_its_client_takes_it_back() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, store) = (
        dir.path().join("displays.sock"),
        dir.path().join("displays.json"),
    );
    let fake = Fake::default();
    let _holder = holder(&fake, &socket, &store);

    let (first, kill_first) = desktopd(&socket);
    let lease = first.acquire(KEY, &SHAPE).await.unwrap();
    let id = lease.settled().await.unwrap();
    assert_eq!(
        kept(&store),
        [(KEY, plan_for(KEY, &SHAPE))],
        "listed on disk"
    );
    // desktopd dies: its main thread goes, and the lease's let-go with it.
    kill_first.abort();
    drop(lease);
    drop(first);

    let (second, _kill_second) = desktopd(&socket);
    second.adopt();
    let lease = second.acquire(KEY, &SHAPE).await.unwrap();
    assert_eq!(
        lease.settled().await.unwrap(),
        id,
        "the same display, taken back"
    );
    assert_eq!(fake.did(), [Did::Made(id)], "made once, never released");

    second.set_linger(Duration::ZERO);
    drop(lease);
    let released = async {
        while fake.did().len() < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(10), released)
        .await
        .expect("released");
    assert_eq!(fake.did(), [Did::Made(id), Did::Released(id)]);
    assert!(
        kept(&store).is_empty() && !store.exists(),
        "an empty list is no file"
    );
}

/// A holder started after one that went makes its list's displays again, trying again a second
/// apart while macOS still holds an identity for the holder before; one macOS refuses past its
/// tries is given up, and stays on the list for the next holder. On tokio's paused clock: the
/// seconds between tries pass at once, and each is exact.
#[tokio::test(start_paused = true)]
async fn a_new_holder_makes_the_kept_displays_again() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("displays.json");
    let plan = plan_for(KEY, &SHAPE);
    let kept_list = [super::Kept {
        key: KEY,
        plan: (&plan).into(),
    }];
    super::write_whole(&store, &kept_list).unwrap();

    let fake = Fake::refusing(2);
    let (main, _ends) = on_task(Holder::new(fake.clone(), store.clone()));
    let started = tokio::time::Instant::now();
    restore(Arc::clone(&main), &store).await;
    assert_eq!(
        started.elapsed(),
        Duration::from_secs(2),
        "a second after each refusal"
    );
    assert_eq!(fake.did(), [Did::Made(1)], "made again after two refusals");
    assert_eq!(kept(&store), [(KEY, plan)], "the list stands");

    let fake = Fake::refusing(u32::MAX);
    let (main, _ends) = on_task(Holder::new(fake.clone(), store.clone()));
    let started = tokio::time::Instant::now();
    restore(Arc::clone(&main), &store).await;
    let tries = Duration::from_secs(RESTORE_TRIES.into());
    assert_eq!(
        started.elapsed(),
        tries,
        "given up after its tries, a second apart"
    );
    assert_eq!(fake.did(), [], "never made");
    assert_eq!(
        kept(&store),
        [(KEY, plan)],
        "still on the list for the next holder"
    );
}

/// A holder no desktopd is linked to ends once its minute alone has passed, counted from when the
/// last desktopd went, not from its start. On tokio's paused clock, so the minute is exact.
#[tokio::test(start_paused = true)]
async fn a_holder_ends_a_minute_after_its_last_desktopd_went() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Fake::default();
    let (main, _ends) = on_task(Holder::new(fake, dir.path().join("displays.json")));
    let started = tokio::time::Instant::now();
    assert_eq!(
        serve(Arc::clone(&main), None, None, ALONE_FOR).await,
        Ended::Alone
    );
    assert_eq!(started.elapsed(), ALONE_FOR, "alone from the start");

    let (ours, theirs) = tokio::net::UnixStream::pair().unwrap();
    let serving = tokio::spawn(serve(main, Some(theirs), None, ALONE_FOR));
    let linked_for = Duration::from_secs(90);
    tokio::time::sleep(linked_for).await;
    assert!(!serving.is_finished(), "a holder with a desktopd stays");
    let went = tokio::time::Instant::now();
    drop(ours);
    assert_eq!(serving.await.unwrap(), Ended::Alone);
    assert_eq!(
        went.elapsed(),
        ALONE_FOR,
        "a minute after its desktopd went"
    );
}

/// A holder asked to quit ends at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_holder_asked_to_quit_ends_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Fake::default();
    let (main, _ends) = on_task(Holder::new(fake, dir.path().join("displays.json")));
    let (ours, theirs) = tokio::net::UnixStream::pair().unwrap();
    let serving = tokio::spawn(serve(main, Some(theirs), None, Duration::from_mins(10)));
    let ours = ours.into_std().unwrap();
    ours.set_nonblocking(false).unwrap();
    let mut link = std::io::BufReader::new(ours);
    let quit =
        tokio::task::spawn_blocking(move || super::exchange(&mut link, &super::wire::Ask::Quit));
    assert_eq!(quit.await.unwrap().unwrap(), super::wire::Answer::Done);
    let ended = tokio::time::timeout(Duration::from_secs(10), serving).await;
    assert_eq!(ended.ok().and_then(Result::ok), Some(Ended::Quit));
}
