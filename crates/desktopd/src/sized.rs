//! Displays made for clients, sized to them ([`ScreenRequest::OpenDisplay`]).
//!
//! A display is created, changed and released on the main thread (`CGVirtualDisplay` refuses
//! every other), so the displays live there, in a [`Registry`] keyed by the client's
//! [`DisplayKey`], and a stream's task hands its asks over as jobs ([`Main`]) and waits for the
//! answer. One key has one display; a second stream with the same key shares it.
//!
//! The last stream letting go keeps the display for its linger ([`LINGER`], ten minutes), and
//! the same key asking again within it takes the display back, windows where they were. A
//! phone that roams between networks, or a laptop whose lid closes, would otherwise have macOS
//! move every window on it to a physical display and leave them there.
//!
//! macOS picks its own mode for a new display and may restore a saved one seconds later, so a
//! display is enforced ([`Factory::enforce`]) every [`ENFORCE_EVERY`] until it settles, and again
//! whenever the displays are reconfigured ([`Displays::reconfigured`]). One that does not
//! settle within the deadline is given up on and the stream streams a physical display, as it
//! does when no display can be made at all.
//!
//! [`ScreenRequest::OpenDisplay`]: onicode_desktop_proto::screen::ScreenRequest::OpenDisplay

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use onicode_desktop_proto::DisplayId;
use onicode_desktop_proto::screen::{
    CaptureTarget, DisplayKey, DisplayShape, NoVirtualDisplay, Quality, ScreenEvent, VirtualDisplay,
};
use onicode_desktop_vdisplay::{ClientKey, Request, plan};
pub use onicode_desktop_vdisplay::{DisplayError, Enforced, Plan, park_main};
use tokio::sync::{mpsc, oneshot};

use crate::pipeline::{Opening, Pipeline, ScreenError};
use crate::platform::Platform;

/// How often a display not yet in its mode is enforced again.
pub const ENFORCE_EVERY: Duration = Duration::from_millis(100);

/// How long a new or changed display has to settle in its mode before the stream gives up on it.
pub const SETTLE_WITHIN: Duration = Duration::from_secs(5);

/// How long a display made for a client is kept after its last stream ends: ten minutes.
///
/// The same client takes it back with its windows in place, after roaming between networks or
/// with its lid closed for a while. desktopd sets it at start ([`Displays::set_linger`]).
pub const LINGER: Duration = Duration::from_mins(10);

/// What makes, changes and checks displays: CoreGraphics on a desktopd ([`Cg`]), a fake in tests.
/// Called on the main thread only.
pub trait Factory: 'static {
    /// A display, alive as long as the value.
    type Display;
    /// Make the display `plan` describes, for the client that asked with `key`.
    ///
    /// # Errors
    ///
    /// Why it could not be made.
    fn create(&mut self, key: DisplayKey, plan: &Plan) -> Result<Self::Display, DisplayError>;
    /// Put `display` in `plan`'s mode, keeping it.
    ///
    /// # Errors
    ///
    /// [`DisplayError::Outgrown`] when it has to be made anew, or why it was refused.
    fn resize(&mut self, display: &mut Self::Display, plan: &Plan) -> Result<(), DisplayError>;
    /// Put `display` back in its mode and out of any mirror set, if macOS moved it.
    ///
    /// # Errors
    ///
    /// When the configuration failed.
    fn enforce(&mut self, display: &Self::Display) -> Result<Enforced, DisplayError>;
    /// Its `CGDirectDisplayID`.
    fn id(&self, display: &Self::Display) -> u32;
    /// End `display`. A display only dropped is let go of without ending, where the factory
    /// can keep it: the display holder's outlive desktopd ([`crate::holder`]).
    fn release(&mut self, display: Self::Display) {
        drop(display);
    }
    /// The displays the factory holds already, each with its key and plan: those a display
    /// holder kept through the last desktopd's end ([`Displays::adopt`]).
    fn held(&mut self) -> Vec<(DisplayKey, Plan, Self::Display)> {
        Vec::new()
    }
}

/// A job for the main thread, handed the state that lives there.
pub type Job<S> = Box<dyn FnOnce(&mut S) + Send>;

/// The main thread, where the displays live: the main dispatch queue on a desktopd
/// (`MainQueue`), a task in tests.
pub trait Main<S>: Send + Sync + 'static {
    /// Run `job` on the main thread, soon.
    fn run(&self, job: Job<S>);
    /// Run `job` on the main thread once `delay` has passed.
    fn run_after(&self, delay: Duration, job: Job<S>);
}

/// The displays on the main thread, by key.
pub struct Registry<F: Factory> {
    factory: F,
    displays: HashMap<DisplayKey, Entry<F::Display>>,
    /// The last linger started: each has its own number, so a timer outlived by a take-back
    /// and a later let-go releases nothing.
    lingers: u64,
    /// How long a display is kept after its last lease; zero releases it with the lease.
    linger: Duration,
}

impl<F: Factory> std::fmt::Debug for Registry<F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry")
            .field("displays", &self.displays.len())
            .finish_non_exhaustive()
    }
}

impl<F: Factory> Registry<F> {
    /// No display yet; `factory` makes them.
    pub fn new(factory: F) -> Self {
        Self {
            factory,
            displays: HashMap::new(),
            lingers: 0,
            linger: Duration::ZERO,
        }
    }
}

type Settled = Result<u32, NoVirtualDisplay>;

struct Entry<D> {
    display: D,
    plan: Plan,
    /// Leases on it.
    users: usize,
    /// While no lease holds it: the linger that releases it unless its key asks again.
    lingering: Option<u64>,
    /// Kept through a disconnect; `false` once a stream found it unusable.
    keep: bool,
    settled: bool,
    /// When it was last made or changed: the settle deadline runs from here.
    changed_at: Instant,
    /// An enforce tick is scheduled.
    enforcing: bool,
    waiting: Vec<oneshot::Sender<Settled>>,
}

impl<D> Entry<D> {
    /// A display just made (or made anew) for `users` leases.
    fn new(display: D, plan: Plan, users: usize, waiting: Vec<oneshot::Sender<Settled>>) -> Self {
        Self {
            display,
            plan,
            users,
            lingering: None,
            keep: true,
            settled: false,
            changed_at: Instant::now(),
            enforcing: false,
            waiting,
        }
    }

    fn tell(&mut self, answer: Settled) {
        for waiter in self.waiting.drain(..) {
            let _gone = waiter.send(answer);
        }
    }
}

/// What a resize did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Resized {
    /// The display already had that mode.
    Unchanged,
    /// The display took the new mode; `rescaled` when its backing scale changed with it.
    InPlace {
        /// The mode went from 1× to 2× or back.
        rescaled: bool,
    },
    /// The mode outgrew the display, which was made anew (a new id).
    Remade,
}

/// desktopd's handle on the displays: hands asks to the main thread and waits for answers.
pub struct Displays<F: Factory> {
    main: Arc<dyn Main<Registry<F>>>,
    settle_within: Duration,
}

impl<F: Factory> Clone for Displays<F> {
    fn clone(&self) -> Self {
        Self {
            main: Arc::clone(&self.main),
            settle_within: self.settle_within,
        }
    }
}

impl<F: Factory> std::fmt::Debug for Displays<F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Displays")
            .field("settle_within", &self.settle_within)
            .finish_non_exhaustive()
    }
}

/// The plan for `shape`, asked with `key`.
#[must_use]
pub fn plan_for(key: DisplayKey, shape: &DisplayShape) -> Plan {
    plan(&Request {
        pixels: (shape.width, shape.height),
        scale: f64::from(shape.scale),
        refresh_hz: u32::from(shape.refresh_hz),
        client: ClientKey::new(&key.0),
    })
}

/// The reason a stream gives its client for a display it could not have.
const fn why(error: &DisplayError) -> NoVirtualDisplay {
    match error {
        DisplayError::Unavailable(_) | DisplayError::NotMainThread => NoVirtualDisplay::Unavailable,
        DisplayError::Refused
        | DisplayError::Rejected
        | DisplayError::Outgrown { .. }
        | DisplayError::Configure(_) => NoVirtualDisplay::Refused,
    }
}

impl<F: Factory> Displays<F> {
    /// Displays living where `main` runs its jobs, given `settle_within` to settle, each
    /// released with its last lease.
    pub fn new(main: Arc<dyn Main<Registry<F>>>, settle_within: Duration) -> Self {
        Self {
            main,
            settle_within,
        }
    }

    /// Keep a display `linger` after its last lease from now on, for its key to take back: the
    /// desktopd sets [`LINGER`] at start. A linger already running keeps the length it started
    /// with.
    pub fn set_linger(&self, linger: Duration) {
        self.main
            .run(Box::new(move |registry| registry.linger = linger));
    }

    /// Ask the main thread `job` and wait for its answer; `None` when the main thread dropped
    /// it unanswered.
    async fn ask<R: Send + 'static>(
        &self,
        job: impl FnOnce(&mut Registry<F>, &Self) -> R + Send + 'static,
    ) -> Option<R> {
        let (tx, rx) = oneshot::channel();
        let this = self.clone();
        self.main.run(Box::new(move |registry| {
            let _gone = tx.send(job(registry, &this));
        }));
        rx.await.ok()
    }

    /// The display for `key` at `shape`: made, or shared with the streams that already have
    /// it (and put in `shape`'s mode). Wait for it with [`Lease::settled`].
    ///
    /// # Errors
    ///
    /// Why no display could be had.
    pub async fn acquire(
        &self,
        key: DisplayKey,
        shape: &DisplayShape,
    ) -> Result<Lease<F>, NoVirtualDisplay> {
        let plan = plan_for(key, shape);
        let made = self
            .ask(move |registry, this| {
                if let Some(entry) = registry.displays.get_mut(&key) {
                    entry.users = entry.users.saturating_add(1);
                    if entry.lingering.take().is_some() {
                        let id = registry.factory.id(&entry.display);
                        tracing::info!(id, "virtual display taken back by its client");
                    }
                    return Ok(());
                }
                let display = registry.factory.create(key, &plan).map_err(|e| {
                    tracing::info!(error = %e, "no virtual display");
                    why(&e)
                })?;
                let id = registry.factory.id(&display);
                tracing::info!(id, mode = ?plan.mode, "virtual display made");
                registry
                    .displays
                    .insert(key, Entry::new(display, plan, 1, Vec::new()));
                this.enforce_soon(registry, key);
                Ok(())
            })
            .await
            .unwrap_or(Err(NoVirtualDisplay::Unavailable));
        made?;
        let lease = Lease {
            displays: self.clone(),
            key,
        };
        // A display shared with another stream takes this stream's shape as well.
        lease.resize(shape).await.map(|_resized| lease)
    }

    /// Hand `job` the factory, on the main thread.
    pub fn with_factory(&self, job: impl FnOnce(&mut F) + Send + 'static) {
        self.main
            .run(Box::new(move |registry| job(&mut registry.factory)));
    }

    /// Take the displays the factory holds already ([`Factory::held`]) as kept for their
    /// clients: each lingers as if its last lease had just let go, so its client takes it back
    /// within the linger, windows where they were. desktopd does this at start, after
    /// [`Self::set_linger`].
    pub fn adopt(&self) {
        let this = self.clone();
        self.main.run(Box::new(move |registry| {
            for (key, plan, display) in registry.factory.held() {
                if registry.displays.contains_key(&key) {
                    continue;
                }
                let id = registry.factory.id(&display);
                tracing::info!(id, mode = ?plan.mode, "virtual display kept from before");
                registry
                    .displays
                    .insert(key, Entry::new(display, plan, 1, Vec::new()));
                this.let_go(registry, key);
                this.enforce_soon(registry, key);
            }
        }));
    }

    /// The displays were reconfigured: every display is enforced again, until it settles.
    pub fn reconfigured(&self) {
        let this = self.clone();
        self.main.run(Box::new(move |registry| {
            let keys: Vec<DisplayKey> = registry.displays.keys().copied().collect();
            for key in keys {
                if let Some(entry) = registry.displays.get_mut(&key) {
                    entry.settled = false;
                    // A window of ticks of its own: macOS may still be moving it.
                    entry.changed_at = Instant::now();
                }
                this.enforce_soon(registry, key);
            }
        }));
    }

    /// Start `key`'s enforce ticks unless they are running.
    fn enforce_soon(&self, registry: &mut Registry<F>, key: DisplayKey) {
        let Some(entry) = registry.displays.get_mut(&key) else {
            return;
        };
        if entry.enforcing {
            return;
        }
        entry.enforcing = true;
        let this = self.clone();
        self.main
            .run(Box::new(move |registry| this.tick(registry, key)));
    }

    /// Enforce `key`'s display once: settled tells the waiters, pending asks again a tick later
    /// until the deadline, which tells them it never settled.
    fn tick(&self, registry: &mut Registry<F>, key: DisplayKey) {
        let Registry {
            factory, displays, ..
        } = registry;
        let Some(entry) = displays.get_mut(&key) else {
            return;
        };
        let id = factory.id(&entry.display);
        match factory.enforce(&entry.display) {
            Ok(Enforced::Settled) => {
                entry.enforcing = false;
                if !entry.settled {
                    tracing::info!(id, took = ?entry.changed_at.elapsed(), "virtual display settled");
                }
                entry.settled = true;
                entry.tell(Ok(id));
            }
            Ok(state @ (Enforced::Pending | Enforced::Applied)) => {
                if entry.changed_at.elapsed() > self.settle_within && !entry.waiting.is_empty() {
                    tracing::warn!(id, ?state, "virtual display never settled");
                    entry.tell(Err(NoVirtualDisplay::Unsettled));
                }
                if entry.waiting.is_empty() && entry.changed_at.elapsed() > self.settle_within {
                    // Nobody waits; the next reconfiguration starts the ticks again.
                    entry.enforcing = false;
                    return;
                }
                let this = self.clone();
                self.main.run_after(
                    ENFORCE_EVERY,
                    Box::new(move |registry| this.tick(registry, key)),
                );
            }
            Err(e) => {
                tracing::warn!(id, error = %e, "virtual display enforce");
                entry.enforcing = false;
                entry.tell(Err(NoVirtualDisplay::Refused));
            }
        }
    }
}

impl<F: Factory> Displays<F> {
    /// One lease on `key` let go: the last keeps the display for the linger, unless a stream
    /// found it unusable or there is no linger, and then releases it.
    fn let_go(&self, registry: &mut Registry<F>, key: DisplayKey) {
        let Some(entry) = registry.displays.get_mut(&key) else {
            return;
        };
        entry.users = entry.users.saturating_sub(1);
        if entry.users > 0 {
            return;
        }
        if !entry.keep || registry.linger.is_zero() {
            Self::release(registry, key);
            return;
        }
        registry.lingers = registry.lingers.wrapping_add(1);
        let linger = registry.lingers;
        entry.lingering = Some(linger);
        let id = registry.factory.id(&entry.display);
        tracing::info!(id, linger = ?registry.linger, "virtual display kept for its client");
        self.main.run_after(
            registry.linger,
            Box::new(move |registry| {
                if registry
                    .displays
                    .get(&key)
                    .is_some_and(|e| e.lingering == Some(linger))
                {
                    Self::release(registry, key);
                }
            }),
        );
    }

    /// Remove `key`'s display, telling whoever waits on it.
    fn release(registry: &mut Registry<F>, key: DisplayKey) {
        if let Some(mut gone) = registry.displays.remove(&key) {
            gone.tell(Err(NoVirtualDisplay::Refused));
            tracing::info!(
                id = registry.factory.id(&gone.display),
                "virtual display released"
            );
            registry.factory.release(gone.display);
        }
    }
}

/// A stream's hold on the display for its key. Dropping it lets go on the main thread; the last
/// lease on a display keeps it for the linger and then releases it, which is what removes it.
pub struct Lease<F: Factory> {
    displays: Displays<F>,
    key: DisplayKey,
}

impl<F: Factory> std::fmt::Debug for Lease<F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lease")
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

impl<F: Factory> Lease<F> {
    /// The key the display was asked with.
    #[must_use]
    pub const fn key(&self) -> DisplayKey {
        self.key
    }

    /// The stream found the display unusable: its last lease releases it at once rather than
    /// keeping it for a client that would only find it unusable again.
    pub fn lost(&self) {
        let key = self.key;
        self.displays.main.run(Box::new(move |registry| {
            if let Some(entry) = registry.displays.get_mut(&key) {
                entry.keep = false;
            }
        }));
    }

    /// The display's id once it is in its mode.
    ///
    /// # Errors
    ///
    /// [`NoVirtualDisplay::Unsettled`] when it did not settle in time, `Refused` when enforcing
    /// it failed.
    pub async fn settled(&self) -> Settled {
        let key = self.key;
        let (tx, rx) = oneshot::channel();
        let asked = self
            .displays
            .ask(move |registry, this| {
                let Some(entry) = registry.displays.get_mut(&key) else {
                    let _gone = tx.send(Err(NoVirtualDisplay::Refused));
                    return;
                };
                if entry.settled {
                    let _gone = tx.send(Ok(registry.factory.id(&entry.display)));
                    return;
                }
                entry.waiting.push(tx);
                this.enforce_soon(registry, key);
            })
            .await;
        if asked.is_none() {
            return Err(NoVirtualDisplay::Unavailable);
        }
        rx.await.unwrap_or(Err(NoVirtualDisplay::Unavailable))
    }

    /// Put the display in `shape`'s mode: in place when it fits, else made anew. Wait for it
    /// with [`Self::settled`].
    ///
    /// # Errors
    ///
    /// Why the display could not take the shape; it may be gone ([`Self::settled`] says).
    pub async fn resize(&self, shape: &DisplayShape) -> Result<Resized, NoVirtualDisplay> {
        let (key, plan) = (self.key, plan_for(self.key, shape));
        self.displays
            .ask(move |registry, this| {
                let Registry {
                    factory, displays, ..
                } = registry;
                let Some(entry) = displays.get_mut(&key) else {
                    return Err(NoVirtualDisplay::Refused);
                };
                if entry.plan.mode == plan.mode {
                    return Ok(Resized::Unchanged);
                }
                let rescaled = entry.plan.mode.hidpi != plan.mode.hidpi;
                let resized = match factory.resize(&mut entry.display, &plan) {
                    Ok(()) => Resized::InPlace { rescaled },
                    Err(DisplayError::Outgrown { wanted, max }) => {
                        tracing::info!(?wanted, ?max, "virtual display outgrown: making it anew");
                        // The old one goes first: the new one has its identity.
                        let (users, waiting) = match displays.remove(&key) {
                            Some(Entry {
                                display,
                                users,
                                waiting,
                                ..
                            }) => {
                                factory.release(display);
                                (users, waiting)
                            }
                            None => (1, Vec::new()),
                        };
                        let display = factory.create(key, &plan).map_err(|e| why(&e))?;
                        displays.insert(key, Entry::new(display, plan, users, waiting));
                        this.enforce_soon(registry, key);
                        return Ok(Resized::Remade);
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "virtual display resize");
                        return Err(why(&e));
                    }
                };
                entry.plan = plan;
                entry.settled = false;
                entry.changed_at = Instant::now();
                this.enforce_soon(registry, key);
                Ok(resized)
            })
            .await
            .unwrap_or(Err(NoVirtualDisplay::Unavailable))
    }
}

impl<F: Factory> Drop for Lease<F> {
    fn drop(&mut self) {
        let key = self.key;
        let displays = self.displays.clone();
        self.displays
            .main
            .run(Box::new(move |registry| displays.let_go(registry, key)));
    }
}

/// CoreGraphics' virtual displays.
#[derive(Clone, Copy, Debug, Default)]
pub struct Cg;

impl Factory for Cg {
    type Display = onicode_desktop_vdisplay::VirtualDisplay;

    fn create(&mut self, _key: DisplayKey, plan: &Plan) -> Result<Self::Display, DisplayError> {
        onicode_desktop_vdisplay::VirtualDisplay::create(plan)
    }

    fn resize(&mut self, display: &mut Self::Display, plan: &Plan) -> Result<(), DisplayError> {
        display.resize(plan)
    }

    fn enforce(&mut self, display: &Self::Display) -> Result<Enforced, DisplayError> {
        display.enforce()
    }

    fn id(&self, display: &Self::Display) -> u32 {
        display.display_id()
    }
}

/// The main dispatch queue, holding the registry where only the main thread reaches it.
pub struct MainQueue<S>(Arc<dispatch2::MainThreadBound<std::cell::RefCell<S>>>);

impl<S> std::fmt::Debug for MainQueue<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MainQueue").finish_non_exhaustive()
    }
}

impl<S: 'static> MainQueue<S> {
    /// The main queue, holding `state`; `None` off the main thread.
    #[must_use]
    pub fn new(state: S) -> Option<Self> {
        let mtm = objc2::MainThreadMarker::new()?;
        Some(Self(Arc::new(dispatch2::MainThreadBound::new(
            std::cell::RefCell::new(state),
            mtm,
        ))))
    }

    fn job(&self, job: Job<S>) -> impl FnOnce() + Send + 'static {
        let state = Arc::clone(&self.0);
        move || {
            let Some(mtm) = objc2::MainThreadMarker::new() else {
                return;
            };
            // Jobs never run inside one another: each is its own turn of the queue.
            if let Ok(mut state) = state.get(mtm).try_borrow_mut() {
                job(&mut state);
            }
        }
    }
}

impl<S: 'static> Main<S> for MainQueue<S> {
    fn run(&self, job: Job<S>) {
        dispatch2::DispatchQueue::main().exec_async(self.job(job));
    }

    fn run_after(&self, delay: Duration, job: Job<S>) {
        let queue = dispatch2::DispatchQueue::main();
        let job = self.job(job);
        match dispatch2::DispatchTime::try_from(delay) {
            Ok(when) => {
                if queue.after(when, job).is_err() {
                    tracing::warn!("the main queue refused a timer");
                }
            }
            Err(()) => queue.exec_async(job),
        }
    }
}

/// desktopd's displays on the main queue, made by `factory`.
///
/// The factory is the display holder ([`crate::holder::Remote`]). Every display is enforced again
/// on each display reconfiguration, with no linger until desktopd gives them its own.
///
/// `None` off the main thread or where no display can be made, and desktopd then answers
/// every `OpenDisplay` with a physical display.
#[must_use]
pub fn on_main_queue<F: Factory>(factory: F) -> Option<Displays<F>> {
    if !onicode_desktop_vdisplay::available() {
        return None;
    }
    let main = MainQueue::new(Registry::new(factory))?;
    let displays = Displays::new(Arc::new(main), SETTLE_WITHIN);
    let notified = displays.clone();
    if let Err(e) = onicode_desktop_vdisplay::on_reconfiguration(move || notified.reconfigured()) {
        tracing::warn!(error = %e, "no display reconfiguration notices; enforcing on changes only");
    }
    Some(displays)
}

/// How long a settled display has to appear in ScreenCaptureKit's list before the stream shows
/// a physical display instead.
pub const LISTED_WITHIN: Duration = Duration::from_secs(2);

/// How often ScreenCaptureKit is asked again whether it lists a new display.
const LIST_EVERY: Duration = Duration::from_millis(100);

/// The display a settled lease has, once ScreenCaptureKit lists it.
async fn listed<P: Platform>(id: u32) -> Result<DisplayId, NoVirtualDisplay> {
    let display = DisplayId(id);
    let started = Instant::now();
    loop {
        match Pipeline::<P>::lists_display(display).await {
            Ok(true) => return Ok(display),
            Ok(false) if started.elapsed() < LISTED_WITHIN => {
                tokio::time::sleep(LIST_EVERY).await;
            }
            Ok(false) => return Err(NoVirtualDisplay::Unlisted),
            Err(e) => {
                tracing::warn!(error = %e, "listing a virtual display");
                return Err(NoVirtualDisplay::Unlisted);
            }
        }
    }
}

/// The display `key` gets at `shape`: made, settled and listed, or why not.
async fn made<P: Platform, F: Factory>(
    displays: Option<&Displays<F>>,
    key: DisplayKey,
    shape: &DisplayShape,
) -> Result<(Lease<F>, DisplayId), NoVirtualDisplay> {
    let displays = displays.ok_or(NoVirtualDisplay::Unavailable)?;
    let lease = displays.acquire(key, shape).await?;
    let listed = match lease.settled().await {
        Ok(id) => listed::<P>(id).await,
        Err(why) => Err(why),
    };
    match listed {
        Ok(display) => Ok((lease, display)),
        Err(why) => {
            lease.lost();
            Err(why)
        }
    }
}

/// What a client's `OpenDisplay` asked for.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Asked {
    /// The client's key.
    pub key: DisplayKey,
    /// The display's size, scale and refresh.
    pub shape: DisplayShape,
    /// The stream's quality.
    pub quality: Quality,
}

/// A stream of a display made for its client: what its resizes ask of the display, and the
/// display switches that come of them.
pub struct Sized {
    /// The key the client asked with.
    pub key: DisplayKey,
    /// Hand a client's `Resize` to the display; returns at once.
    pub resize: Box<dyn Fn(u32, u32, Option<f32>) + Send + Sync>,
    /// The displays the stream is to show from now on, with what to tell its client; call
    /// [`Pipeline::switch_display`] with each.
    pub switches: mpsc::UnboundedReceiver<(DisplayId, VirtualDisplay)>,
}

impl std::fmt::Debug for Sized {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sized").finish_non_exhaustive()
    }
}

/// Open a stream of the display made for the client that asked, or of a physical display.
///
/// Returns the stream, the `Display` and `Opened` events to send in that order, and for a made
/// display what serves its resizes. Dropping the [`Sized`] (after closing the stream) lets go
/// of the display on the main thread (see [`Lease`]). `opening` is the stream's opening with its
/// target still to be filled in.
///
/// # Errors
///
/// When not even a physical display could be streamed.
pub async fn open<P: Platform, F: Factory>(
    displays: Option<&Displays<F>>,
    asked: Asked,
    opening: Opening,
) -> Result<(Pipeline<P>, [ScreenEvent; 2], Option<Sized>), ScreenError> {
    let Asked {
        key,
        shape,
        quality,
    } = asked;
    let id = opening.id;
    let (lease, told) = match made::<P, F>(displays, key, &shape).await {
        Ok((lease, display)) => (Some(lease), VirtualDisplay::Made(display)),
        Err(why) => {
            tracing::info!(stream = %id, ?why, "streaming a physical display in place of one made");
            let display = Pipeline::<P>::physical_display(None).await?;
            (None, VirtualDisplay::Physical { display, why })
        }
    };
    let display = match told {
        VirtualDisplay::Made(display) | VirtualDisplay::Physical { display, .. } => display,
    };
    let opening = Opening {
        target: CaptureTarget::Display(display),
        quality,
        ..opening
    };
    let (stream, opened) = Pipeline::<P>::open(opening).await?;
    let sized = lease.map(|lease| sizing::<P, F>(lease, shape, display));
    Ok((
        stream,
        [
            ScreenEvent::Display {
                stream: id,
                key,
                display: told,
            },
            opened,
        ],
        sized,
    ))
}

/// What serves a made display's resizes: each goes to the main thread on a task of its own,
/// and one that made the display anew or changed its backing scale waits for it to settle and
/// be listed, then asks the stream to switch. A display lost on the way hands the stream a
/// physical one, and later resizes are ignored.
fn sizing<P: Platform, F: Factory>(
    lease: Lease<F>,
    shape: DisplayShape,
    shown: DisplayId,
) -> Sized {
    let key = lease.key();
    let (tx, switches) = mpsc::unbounded_channel();
    let lease = Arc::new(parking_lot::Mutex::new(Some(Arc::new(lease))));
    let scale = Arc::new(parking_lot::Mutex::new(shape.scale));
    let resize = move |width: u32, height: u32, asked: Option<f32>| {
        let Some(held) = lease.lock().clone() else {
            return;
        };
        let scale = {
            let mut scale = scale.lock();
            if let Some(asked) = asked {
                *scale = asked;
            }
            *scale
        };
        let shape = DisplayShape {
            width,
            height,
            scale,
            ..shape
        };
        let (tx, lease) = (tx.clone(), Arc::clone(&lease));
        tokio::spawn(async move {
            let resized = held.resize(&shape).await;
            let settled = match resized {
                Ok(Resized::Unchanged | Resized::InPlace { rescaled: false }) => return,
                Ok(Resized::InPlace { rescaled: true } | Resized::Remade) => held.settled().await,
                Err(why) => Err(why),
            };
            let told = match settled {
                Ok(id) => listed::<P>(id).await.map(VirtualDisplay::Made),
                Err(why) => Err(why),
            };
            let told = match told {
                Ok(told) => told,
                Err(why) => {
                    tracing::warn!(?why, "the display made for the client was lost");
                    held.lost();
                    lease.lock().take();
                    match Pipeline::<P>::physical_display(Some(shown)).await {
                        Ok(display) => VirtualDisplay::Physical { display, why },
                        Err(e) => {
                            tracing::warn!(error = %e, "no physical display to fall back to");
                            return;
                        }
                    }
                }
            };
            let display = match told {
                VirtualDisplay::Made(display) | VirtualDisplay::Physical { display, .. } => display,
            };
            let _gone = tx.send((display, told));
        });
    };
    Sized {
        key,
        resize: Box::new(resize),
        switches,
    }
}

#[cfg(test)]
mod tests;
