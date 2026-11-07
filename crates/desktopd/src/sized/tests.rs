//! The displays' lifecycle over a fake factory and a task standing in for the main thread:
//! nothing here makes a real display.

use std::sync::Arc;
use std::time::Duration;

use onicode_desktop_proto::screen::{DisplayKey, DisplayShape, NoVirtualDisplay};
use onicode_desktop_vdisplay::{DisplayError, Enforced, Plan};
use parking_lot::Mutex;
use tokio::sync::mpsc;

use super::{Displays, Factory, Job, Main, Registry, Resized};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Did {
    Made(u32),
    Resized(u32),
    Enforced(u32),
    Released(u32),
}

/// What the fake does next, and what it did.
#[derive(Debug, Default)]
struct Script {
    refuse: Option<DisplayError>,
    /// Enforce calls still to answer `Pending`; `None` never settles.
    pending: Option<u32>,
    next_id: u32,
    did: Vec<Did>,
}

#[derive(Clone, Debug, Default)]
struct Fake(Arc<Mutex<Script>>);

#[derive(Debug)]
struct Shown {
    id: u32,
    max: (u32, u32),
    script: Arc<Mutex<Script>>,
}

impl Drop for Shown {
    fn drop(&mut self) {
        self.script.lock().did.push(Did::Released(self.id));
    }
}

impl Factory for Fake {
    type Display = Shown;

    fn create(&mut self, plan: &Plan) -> Result<Shown, DisplayError> {
        let id = {
            let mut script = self.0.lock();
            if let Some(refused) = script.refuse.clone() {
                return Err(refused);
            }
            script.next_id = script.next_id.saturating_add(1);
            let id = script.next_id;
            script.did.push(Did::Made(id));
            id
        };
        Ok(Shown {
            id,
            max: plan.descriptor.max_pixels,
            script: Arc::clone(&self.0),
        })
    }

    fn resize(&mut self, display: &mut Shown, plan: &Plan) -> Result<(), DisplayError> {
        if !plan.mode.fits(display.max) {
            return Err(DisplayError::Outgrown {
                wanted: plan.mode.pixels,
                max: display.max,
            });
        }
        self.0.lock().did.push(Did::Resized(display.id));
        Ok(())
    }

    fn enforce(&mut self, display: &Shown) -> Result<Enforced, DisplayError> {
        let mut script = self.0.lock();
        script.did.push(Did::Enforced(display.id));
        match &mut script.pending {
            None => Ok(Enforced::Pending),
            Some(0) => Ok(Enforced::Settled),
            Some(n) => {
                *n = n.saturating_sub(1);
                Ok(Enforced::Applied)
            }
        }
    }

    fn id(&self, display: &Shown) -> u32 {
        display.id
    }
}

/// A task that owns the state and runs jobs one at a time, as the main queue does.
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

fn displays(fake: &Fake, settle_within: Duration) -> Displays<Fake> {
    let (tx, mut rx) = mpsc::unbounded_channel::<Job<Registry<Fake>>>();
    let mut registry = Registry::new(fake.clone());
    tokio::spawn(async move {
        while let Some(job) = rx.recv().await {
            job(&mut registry);
        }
    });
    Displays::new(Arc::new(TaskMain(tx)), settle_within)
}

const KEY: DisplayKey = DisplayKey(*b"oni-test-key-01!");

fn shape(width: u32, height: u32, scale: f32) -> DisplayShape {
    DisplayShape {
        width,
        height,
        scale,
        refresh_hz: 120,
    }
}

impl Fake {
    fn scripted(pending: Option<u32>) -> Self {
        Self(Arc::new(Mutex::new(Script {
            pending,
            ..Script::default()
        })))
    }

    fn did(&self) -> Vec<Did> {
        self.0.lock().did.clone()
    }

    /// Until the main-thread task has run everything handed to it so far.
    async fn drained(&self, displays: &Displays<Self>) {
        displays.ask(|_registry, _this| ()).await;
    }
}

#[tokio::test]
async fn a_display_is_enforced_until_it_settles_shared_by_key_and_released_with_its_last_lease() {
    let fake = Fake::scripted(Some(3));
    let displays = displays(&fake, Duration::from_secs(5));
    let first = displays
        .acquire(KEY, &shape(2752, 2064, 2.0))
        .await
        .unwrap();
    assert_eq!(first.settled().await, Ok(1));
    let enforced = fake
        .did()
        .iter()
        .filter(|d| matches!(d, Did::Enforced(1)))
        .count();
    assert_eq!(enforced, 4, "three moves, then settled: {:?}", fake.did());

    let second = displays
        .acquire(KEY, &shape(2752, 2064, 2.0))
        .await
        .unwrap();
    assert_eq!(
        second.settled().await,
        Ok(1),
        "the same key shares the display"
    );
    drop(first);
    fake.drained(&displays).await;
    assert!(
        !fake.did().contains(&Did::Released(1)),
        "a lease still holds it"
    );
    drop(second);
    fake.drained(&displays).await;
    assert_eq!(
        fake.did()
            .iter()
            .filter(|d| matches!(d, Did::Made(_)))
            .count(),
        1
    );
    assert_eq!(fake.did().last(), Some(&Did::Released(1)));
}

#[tokio::test]
async fn a_desktopd_that_cannot_make_one_says_unavailable_and_a_refusal_says_refused() {
    let fake = Fake::scripted(Some(0));
    let displays = displays(&fake, Duration::from_secs(5));
    fake.0.lock().refuse = Some(DisplayError::Unavailable("no CGVirtualDisplay".to_owned()));
    let unavailable = displays.acquire(KEY, &shape(1920, 1080, 1.0)).await;
    assert_eq!(
        unavailable.map(|_lease| ()),
        Err(NoVirtualDisplay::Unavailable)
    );
    fake.0.lock().refuse = Some(DisplayError::Refused);
    let refused = displays.acquire(KEY, &shape(1920, 1080, 1.0)).await;
    assert_eq!(refused.map(|_lease| ()), Err(NoVirtualDisplay::Refused));
    fake.0.lock().refuse = Some(DisplayError::NotMainThread);
    let off_main = displays.acquire(KEY, &shape(1920, 1080, 1.0)).await;
    assert_eq!(
        off_main.map(|_lease| ()),
        Err(NoVirtualDisplay::Unavailable)
    );
    assert!(fake.did().is_empty(), "nothing was made: {:?}", fake.did());
}

#[tokio::test]
async fn a_display_that_never_settles_is_given_up_on_and_released_with_its_lease() {
    let fake = Fake::scripted(None);
    let displays = displays(&fake, Duration::from_millis(300));
    let lease = displays
        .acquire(KEY, &shape(1920, 1080, 1.0))
        .await
        .unwrap();
    let started = std::time::Instant::now();
    assert_eq!(lease.settled().await, Err(NoVirtualDisplay::Unsettled));
    assert!(
        started.elapsed() >= Duration::from_millis(250),
        "{:?}",
        started.elapsed()
    );
    drop(lease);
    fake.drained(&displays).await;
    assert_eq!(fake.did().last(), Some(&Did::Released(1)));
    // The ticks stopped once nobody waited past the deadline.
    let enforced = fake.did().len();
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(
        fake.did().len(),
        enforced,
        "enforcing went on after the release"
    );
}

#[tokio::test]
async fn a_resize_that_fits_stays_in_place_and_one_that_outgrows_remakes_the_display() {
    let fake = Fake::scripted(Some(0));
    let displays = displays(&fake, Duration::from_secs(5));
    let lease = displays
        .acquire(KEY, &shape(2560, 1600, 2.0))
        .await
        .unwrap();
    assert_eq!(lease.settled().await, Ok(1));
    assert_eq!(
        lease.resize(&shape(2560, 1600, 2.0)).await,
        Ok(Resized::Unchanged)
    );
    assert_eq!(
        lease.resize(&shape(1600, 2560, 2.0)).await,
        Ok(Resized::InPlace { rescaled: false })
    );
    assert_eq!(lease.settled().await, Ok(1), "rotated in place");
    assert_eq!(
        lease.resize(&shape(1280, 800, 1.0)).await,
        Ok(Resized::InPlace { rescaled: true })
    );
    assert_eq!(
        lease.resize(&shape(6016, 3384, 2.0)).await,
        Ok(Resized::Remade)
    );
    assert_eq!(lease.settled().await, Ok(2), "a new display");
    let did = fake.did();
    let released = did.iter().position(|d| *d == Did::Released(1)).unwrap();
    let remade = did.iter().position(|d| *d == Did::Made(2)).unwrap();
    assert!(
        released < remade,
        "the old display goes before its identity is reused: {did:?}"
    );
    drop(lease);
    fake.drained(&displays).await;
    assert_eq!(fake.did().last(), Some(&Did::Released(2)));
}

#[tokio::test]
async fn a_reconfiguration_enforces_every_display_again() {
    let fake = Fake::scripted(Some(0));
    let displays = displays(&fake, Duration::from_secs(5));
    let lease = displays
        .acquire(KEY, &shape(1920, 1080, 1.0))
        .await
        .unwrap();
    assert_eq!(lease.settled().await, Ok(1));
    let before = fake.did().len();
    // macOS put it back in a mode of its own: one enforce moves it, the next finds it settled.
    fake.0.lock().pending = Some(1);
    displays.reconfigured();
    assert_eq!(lease.settled().await, Ok(1));
    let enforced: Vec<Did> = fake.did().split_off(before);
    assert_eq!(enforced, [Did::Enforced(1), Did::Enforced(1)]);
}

fn released(fake: &Fake, id: u32) -> bool {
    fake.did().contains(&Did::Released(id))
}

/// A client that drops off and comes back within the linger gets its display back, windows and
/// all; one that stays away loses it once the linger runs out, counted from its last let-go.
#[tokio::test(start_paused = true)]
async fn a_display_let_go_lingers_for_its_client_and_goes_when_the_linger_runs_out() {
    let fake = Fake::scripted(Some(0));
    let linger = Duration::from_secs(600);
    let displays = displays(&fake, Duration::from_secs(5));
    displays.set_linger(linger);
    let lease = displays
        .acquire(KEY, &shape(2752, 2064, 2.0))
        .await
        .unwrap();
    assert_eq!(lease.settled().await, Ok(1));
    drop(lease);
    fake.drained(&displays).await;
    assert!(
        !released(&fake, 1),
        "kept through the disconnect: {:?}",
        fake.did()
    );

    tokio::time::sleep(Duration::from_secs(400)).await;
    // The phone is back, rotated: the same display takes the new shape.
    let back = displays
        .acquire(KEY, &shape(2064, 2752, 2.0))
        .await
        .unwrap();
    assert_eq!(back.settled().await, Ok(1), "the same display, taken back");
    assert_eq!(
        fake.did()
            .iter()
            .filter(|d| matches!(d, Did::Made(_)))
            .count(),
        1
    );
    assert!(fake.did().contains(&Did::Resized(1)), "{:?}", fake.did());
    drop(back);
    fake.drained(&displays).await;

    // The first linger's timer comes due while the second runs: it releases nothing.
    tokio::time::sleep(Duration::from_secs(300)).await;
    fake.drained(&displays).await;
    assert!(
        !released(&fake, 1),
        "an outlived linger released it: {:?}",
        fake.did()
    );
    tokio::time::sleep(Duration::from_secs(301)).await;
    fake.drained(&displays).await;
    assert_eq!(fake.did().last(), Some(&Did::Released(1)));

    let fresh = displays
        .acquire(KEY, &shape(2752, 2064, 2.0))
        .await
        .unwrap();
    assert_eq!(
        fresh.settled().await,
        Ok(2),
        "past the linger a new display is made"
    );
}

/// A display a stream could not use is released with its last lease, linger or not; another
/// client's key never takes a lingering display.
#[tokio::test(start_paused = true)]
async fn a_lost_display_is_released_at_once_and_a_linger_is_its_own_keys() {
    let fake = Fake::scripted(Some(0));
    let displays = displays(&fake, Duration::from_secs(5));
    displays.set_linger(Duration::from_secs(600));
    let lease = displays
        .acquire(KEY, &shape(1920, 1080, 1.0))
        .await
        .unwrap();
    assert_eq!(lease.settled().await, Ok(1));
    lease.lost();
    drop(lease);
    fake.drained(&displays).await;
    assert_eq!(
        fake.did().last(),
        Some(&Did::Released(1)),
        "unusable, so not kept"
    );

    let lease = displays
        .acquire(KEY, &shape(1920, 1080, 1.0))
        .await
        .unwrap();
    assert_eq!(lease.settled().await, Ok(2));
    drop(lease);
    let other = displays
        .acquire(DisplayKey(*b"another-client!!"), &shape(1920, 1080, 1.0))
        .await
        .unwrap();
    assert_eq!(other.settled().await, Ok(3), "a key of its own");
    assert!(
        !released(&fake, 2),
        "the first client's display still waits for it"
    );
}
