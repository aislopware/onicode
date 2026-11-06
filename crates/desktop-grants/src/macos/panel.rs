//! The guide's panel: a borderless, non-activating `NSPanel` in glass at the bottom of System
//! Settings' window, with the helper bundle's icon as a file drag source.
//!
//! Everything runs on the main thread. The guide lives in a thread-local; the panel's timer (in
//! the run loop's common modes, so it keeps following while a drag or a window move tracks the
//! mouse), its buttons, its drag source, the global click monitor and [`report`] all reach it
//! through [`drive`], which takes the guide's effect once the borrow is over.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{
    AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel,
};
use objc2_app_kit::{
    NSAccessibility, NSBackingStoreType, NSBezelStyle, NSButton, NSColor, NSControl, NSControlSize,
    NSCursor, NSDragOperation, NSDraggingContext, NSDraggingItem, NSDraggingSession,
    NSDraggingSource, NSEvent, NSEventMask, NSFloatingWindowLevel, NSFont, NSGlassEffectView,
    NSImage, NSImageScaling, NSImageView, NSPanel, NSProgressIndicator, NSProgressIndicatorStyle,
    NSResponder, NSScreen, NSTextField, NSView, NSWindow, NSWindowCollectionBehavior,
    NSWindowStyleMask, NSWorkspace,
};
use objc2_foundation::{
    NSArray, NSObject, NSObjectProtocol, NSPoint, NSRect, NSRunLoop, NSRunLoopCommonModes, NSSize,
    NSString, NSTimer, NSURL,
};

use super::settings::{frontmost_bundle, macos_major, settings_windows};
use crate::bundle::display_name;
use crate::guide::place::{
    PANEL_HEIGHT, PANEL_WIDTH, Rect, cocoa_from_cg, fade_alpha, icon_lift, is_auth_agent,
    is_navigation_click, is_prompt, panel_frame, pick_main, screen_for, shows_alongside,
    sidebar_width,
};
use crate::guide::{Accessory, Effect, Event, Guide, Look, Phase, SettingsView};
use crate::{Grants, Pane};

/// How often the panel looks at System Settings.
const TICK: f64 = 1.0 / 30.0;
/// The glass's corner radius.
const CORNER: f64 = 14.0;
/// Where the icon, the spinner's and the check's square sits, at rest.
const ACCESSORY: (f64, f64, f64) = (16.0, 16.0, 32.0);
/// Where the words start.
const TEXT_X: f64 = 60.0;
/// System Settings' own icon, shown while the guide points back at it.
const SETTINGS_APP: &str = "/System/Applications/System Settings.app";

thread_local! {
    static ACTIVE: RefCell<Option<Active>> = const { RefCell::new(None) };
    /// The open guide's phase, or how the last one ended.
    static LAST: Cell<Option<Phase>> = const { Cell::new(None) };
    /// Icons whose drag outlived their panel, kept until the drag ends.
    static LINGERING: RefCell<Vec<Retained<IconView>>> = const { RefCell::new(Vec::new()) };
}

/// Why the guide did not open.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum ShowError {
    /// AppKit is reached only from the main thread.
    #[error("the guide opens only on the main thread")]
    NotMainThread,
    /// The drag source must be an app bundle on disk.
    #[error("{0} is not an app bundle")]
    NotABundle(PathBuf),
}

/// Opens System Settings at `pane`'s list, brings it forward, and shows the panel with the helper
/// bundle at `helper_bundle` to drag into the list. Replaces a guide already open.
///
/// While it is open, hand every grants report from desktopd to [`report`]; the panel closes on
/// its own once the grant is on, System Settings closes, or the person closes it.
///
/// # Errors
///
/// [`ShowError`]: not on the main thread, or `helper_bundle` is not an `.app` directory.
pub fn show(pane: Pane, helper_bundle: &Path) -> Result<(), ShowError> {
    let mtm = MainThreadMarker::new().ok_or(ShowError::NotMainThread)?;
    if helper_bundle.extension().is_none_or(|ext| ext != "app") || !helper_bundle.is_dir() {
        return Err(ShowError::NotABundle(helper_bundle.to_owned()));
    }
    close();
    let (guide, first) = Guide::open(pane, Duration::ZERO);
    let active = Active::new(mtm, guide, helper_bundle);
    ACTIVE.with(|cell| *cell.borrow_mut() = Some(active));
    drive(move |_, _| Some(first));
    Ok(())
}

/// Hands the guide desktopd's latest grants. Does nothing when no guide is open or off the main
/// thread.
pub fn report(grants: Grants) {
    drive(move |active, _| active.step(Event::Grants(grants)));
}

/// Closes the guide, if one is open, without waiting for the grant; [`phase`] reads `None` after.
pub fn close() {
    let active = ACTIVE.with(|cell| cell.try_borrow_mut().ok().and_then(|mut slot| slot.take()));
    if let Some(active) = active {
        active.teardown();
    }
    LAST.set(None);
}

/// The open guide's phase, or how the last one ended until the next [`show`] or [`close`].
#[must_use]
pub fn phase() -> Option<Phase> {
    LAST.get()
}

/// Whether a guide is open.
#[must_use]
pub fn is_open() -> bool {
    ACTIVE.with(|cell| cell.try_borrow().map_or(true, |slot| slot.is_some()))
}

/// Runs `f` on the open guide and carries out the effect it answers: opening the pane at once,
/// closing once the borrow is over. Re-entry and calls off the main thread do nothing.
fn drive(f: impl FnOnce(&mut Active, MainThreadMarker) -> Option<Effect>) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let ended = ACTIVE.with(|cell| {
        let mut slot = cell.try_borrow_mut().ok()?;
        let active = slot.as_mut()?;
        let effect = f(active, mtm);
        LAST.set(Some(active.guide.phase()));
        match effect {
            Some(Effect::Close) => return slot.take(),
            Some(Effect::OpenPane) => active.open_pane(mtm),
            None => {}
        }
        active.render(mtm);
        None
    });
    if let Some(active) = ended {
        active.teardown();
    }
}

struct Active {
    guide: Guide,
    started: Instant,
    app_name: String,
    panel: Retained<GuidePanel>,
    views: Views,
    /// The buttons' target; AppKit holds a control's target weakly.
    _target: Retained<GuideTarget>,
    timer: Retained<NSTimer>,
    monitor: Option<Retained<AnyObject>>,
    helper_icon: Retained<NSImage>,
    settings_icon: Retained<NSImage>,
    /// The System Settings window followed.
    main_window: Option<u32>,
    /// Its frame in AppKit's space, while it is on screen.
    main_frame: Option<Rect>,
    /// The app in front, as of the last tick.
    front: Option<String>,
    sidebar: f64,
    reduce_motion: bool,
    /// When the panel last came on screen.
    shown_at: Option<Instant>,
    look: Option<Look>,
    /// System Settings is to be brought forward once its window shows.
    activate_pending: bool,
}

struct Views {
    icon: Retained<IconView>,
    spinner: Retained<NSProgressIndicator>,
    check: Retained<NSImageView>,
    title: Retained<NSTextField>,
    subtitle: Retained<NSTextField>,
    action: Retained<GuideButton>,
    close: Retained<GuideButton>,
}

impl Active {
    fn new(mtm: MainThreadMarker, guide: Guide, helper_bundle: &Path) -> Self {
        let workspace = NSWorkspace::sharedWorkspace();
        let helper_path = NSString::from_str(&helper_bundle.to_string_lossy());
        let helper_icon = workspace.iconForFile(&helper_path);
        let settings_icon = workspace.iconForFile(&NSString::from_str(SETTINGS_APP));
        let app_name = display_name(helper_bundle);

        let frame = rect(Rect::new(0.0, 0.0, PANEL_WIDTH, PANEL_HEIGHT));
        let panel = GuidePanel::new(mtm, frame);
        let container = NSView::initWithFrame(NSView::alloc(mtm), frame);
        let glass = NSGlassEffectView::initWithFrame(NSGlassEffectView::alloc(mtm), frame);
        glass.setCornerRadius(CORNER);
        let content = NSView::initWithFrame(NSView::alloc(mtm), frame);
        glass.setContentView(Some(&content));
        container.addSubview(&glass);
        panel.setContentView(Some(&container));

        let target = GuideTarget::new(mtm);
        let icon = IconView::new(mtm, NSURL::fileURLWithPath(&helper_path));
        icon.setImage(Some(&helper_icon));
        icon.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
        icon.setAccessibilityLabel(Some(&NSString::from_str(&format!(
            "{app_name}. Drag into the list in System Settings."
        ))));

        let spinner = NSProgressIndicator::initWithFrame(
            NSProgressIndicator::alloc(mtm),
            rect(Rect::new(24.0, 24.0, 16.0, 16.0)),
        );
        spinner.setStyle(NSProgressIndicatorStyle::Spinning);
        spinner.setControlSize(NSControlSize::Small);
        spinner.setDisplayedWhenStopped(false);

        let check = NSImageView::initWithFrame(
            NSImageView::alloc(mtm),
            rect(Rect::new(20.0, 20.0, 24.0, 24.0)),
        );
        check.setImage(symbol("checkmark.circle.fill", "On").as_deref());
        check.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
        check.setContentTintColor(Some(&NSColor::systemGreenColor()));

        let title = NSTextField::labelWithString(&NSString::from_str(""), mtm);
        title.setFont(Some(&NSFont::boldSystemFontOfSize(13.0)));
        title.setTextColor(Some(&NSColor::labelColor()));
        let subtitle = NSTextField::labelWithString(&NSString::from_str(""), mtm);
        subtitle.setFont(Some(&NSFont::systemFontOfSize(11.0)));
        subtitle.setTextColor(Some(&NSColor::secondaryLabelColor()));

        let action = GuideButton::new(mtm);
        action.setBezelStyle(NSBezelStyle::Push);
        action.setControlSize(NSControlSize::Small);
        let receiver: &AnyObject = &target;
        // SAFETY: `target` outlives the button's use of it: `teardown` clears the target before
        // `target` is dropped, and `act:` is a method `GuideTarget` defines.
        unsafe {
            action.setTarget(Some(receiver));
            action.setAction(Some(sel!(act:)));
        }
        let close = GuideButton::new(mtm);
        close.setBordered(false);
        close.setImage(symbol("xmark.circle.fill", "Close").as_deref());
        close.setContentTintColor(Some(&NSColor::tertiaryLabelColor()));
        close.setToolTip(Some(&NSString::from_str("Close")));
        // SAFETY: as for `action`; `dismiss:` is a method `GuideTarget` defines.
        unsafe {
            close.setTarget(Some(receiver));
            close.setAction(Some(sel!(dismiss:)));
        }

        let subviews: [&NSView; 7] = [&icon, &spinner, &check, &title, &subtitle, &action, &close];
        for view in subviews {
            content.addSubview(view);
        }

        let tick = RcBlock::new(|_: NonNull<NSTimer>| drive(Active::tick));
        // SAFETY: the block only reaches main-thread state through `drive`, and the timer is added
        // to the main run loop below and invalidated in `teardown`.
        let timer = unsafe { NSTimer::timerWithTimeInterval_repeats_block(TICK, true, &tick) };
        // SAFETY: `NSRunLoopCommonModes` is a framework constant; common modes keep the timer
        // firing while AppKit tracks a drag or a window move.
        unsafe { NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes) };

        let clicked = RcBlock::new(|_: NonNull<NSEvent>| {
            let at = NSEvent::mouseLocation();
            drive(move |active, _| active.clicked(at));
        });
        let monitor = NSEvent::addGlobalMonitorForEventsMatchingMask_handler(
            NSEventMask::LeftMouseDown,
            &clicked,
        );

        let active = Self {
            guide,
            started: Instant::now(),
            app_name,
            panel,
            views: Views {
                icon,
                spinner,
                check,
                title,
                subtitle,
                action,
                close,
            },
            _target: target,
            timer,
            monitor,
            helper_icon,
            settings_icon,
            main_window: None,
            main_frame: None,
            front: None,
            sidebar: sidebar_width(macos_major()),
            reduce_motion: NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion(),
            shown_at: None,
            look: None,
            activate_pending: false,
        };
        active.layout(false);
        active
    }

    fn step(&mut self, event: Event) -> Option<Effect> {
        self.guide.step(event, self.started.elapsed())
    }

    fn open_pane(&mut self, mtm: MainThreadMarker) {
        super::open_pane(self.guide.pane());
        // A running System Settings switches lists without coming forward; one being launched
        // comes forward by itself but has no window yet, so try again once it shows one.
        self.activate_pending = !super::activate_settings(mtm);
    }

    fn tick(&mut self, mtm: MainThreadMarker) -> Option<Effect> {
        let windows = settings_windows();
        let main = pick_main(&windows, self.main_window).cloned();
        self.main_window = main.as_ref().map(|window| window.number);
        self.front = frontmost_bundle();
        let prompt = is_auth_agent(self.front.as_deref())
            || main
                .as_ref()
                .is_some_and(|main| windows.iter().any(|w| is_prompt(w, main)));
        self.main_frame = main
            .as_ref()
            .map(|window| cocoa_from_cg(window.bounds, primary_height(mtm)));
        if self.activate_pending && main.is_some() {
            super::activate_settings(mtm);
            self.activate_pending = false;
        }
        let view = SettingsView {
            open: main.is_some(),
            prompt,
            title: main.and_then(|window| window.title),
        };
        self.step(Event::Settings(view))
    }

    fn clicked(&mut self, at: NSPoint) -> Option<Effect> {
        let frame = self.main_frame?;
        is_navigation_click(frame, at.x, at.y, self.sidebar)
            .then(|| self.step(Event::NavigationClicked))
            .flatten()
    }

    /// Puts the look on the views and the panel where it belongs, or off screen.
    fn render(&mut self, mtm: MainThreadMarker) {
        let look = self.guide.look(&self.app_name);
        if look != self.look {
            self.apply(look.as_ref());
            self.look = look;
        }
        let draggable = self.look.as_ref().map(|look| look.draggable);
        let (Some(draggable), Some(window)) = (draggable, self.main_frame) else {
            self.hide();
            return;
        };
        if !shows_alongside(self.front.as_deref()) {
            self.hide();
            return;
        }
        let target = rect(panel_frame(
            window,
            visible_frame(mtm, window),
            self.sidebar,
        ));
        if self.panel.frame() != target {
            self.panel.setFrame_display(target, true);
        }
        let shown_at = *self.shown_at.get_or_insert_with(Instant::now);
        let seconds = shown_at.elapsed().as_secs_f64();
        self.panel
            .setAlphaValue(fade_alpha(seconds, self.reduce_motion));
        if !self.panel.isVisible() {
            self.panel.orderFrontRegardless();
            self.panel.invalidateShadow();
        }
        let lift = if draggable {
            icon_lift(seconds, self.reduce_motion)
        } else {
            0.0
        };
        let (x, y, side) = ACCESSORY;
        self.views
            .icon
            .setFrame(rect(Rect::new(x, y + lift, side, side)));
    }

    fn hide(&mut self) {
        if self.panel.isVisible() {
            self.panel.orderOut(None);
        }
        self.shown_at = None;
    }

    fn apply(&self, look: Option<&Look>) {
        let Some(look) = look else {
            return;
        };
        let views = &self.views;
        views.title.setStringValue(&NSString::from_str(&look.title));
        views
            .subtitle
            .setStringValue(&NSString::from_str(&look.subtitle));
        let icon = matches!(look.accessory, Accessory::Icon | Accessory::Settings);
        views.icon.setHidden(!icon);
        let image: &NSImage = if look.accessory == Accessory::Settings {
            &self.settings_icon
        } else {
            &self.helper_icon
        };
        views.icon.setImage(Some(image));
        views.icon.ivars().draggable.set(look.draggable);
        self.panel.invalidateCursorRectsForView(&views.icon);
        views.check.setHidden(look.accessory != Accessory::Check);
        let spinning = look.accessory == Accessory::Spinner;
        views.spinner.setHidden(!spinning);
        // SAFETY: plain start and stop of the spinner's animation on the main thread.
        unsafe {
            if spinning {
                views.spinner.startAnimation(None);
            } else {
                views.spinner.stopAnimation(None);
            }
        }
        if let Some(label) = look.action {
            views.action.setTitle(&NSString::from_str(label));
        }
        views.action.setHidden(look.action.is_none());
        self.layout(look.action.is_some());
    }

    /// Lays the words and buttons out, right to left: close, the action, then the words.
    fn layout(&self, with_action: bool) {
        let views = &self.views;
        let mut right = PANEL_WIDTH - 10.0;
        views.close.setFrame(rect(Rect::new(
            right - 20.0,
            (PANEL_HEIGHT - 20.0) / 2.0,
            20.0,
            20.0,
        )));
        right -= 28.0;
        if with_action {
            views.action.sizeToFit();
            let size = views.action.frame().size;
            let width = size.width.max(64.0);
            views.action.setFrame(rect(Rect::new(
                right - width,
                ((PANEL_HEIGHT - size.height) / 2.0).round(),
                width,
                size.height,
            )));
            right -= width + 8.0;
        }
        let width = (right - TEXT_X).max(40.0);
        views
            .title
            .setFrame(rect(Rect::new(TEXT_X, 34.0, width, 17.0)));
        views
            .subtitle
            .setFrame(rect(Rect::new(TEXT_X, 14.0, width, 15.0)));
    }

    fn teardown(self) {
        self.timer.invalidate();
        if let Some(monitor) = &self.monitor {
            // SAFETY: `monitor` is what `addGlobalMonitorForEventsMatchingMask:handler:` returned.
            unsafe { NSEvent::removeMonitor(monitor) };
        }
        // SAFETY: the buttons stop messaging `target` before it is dropped below; stopping the
        // spinner is a plain main-thread call.
        unsafe {
            self.views.action.setTarget(None);
            self.views.close.setTarget(None);
            self.views.spinner.stopAnimation(None);
        }
        self.panel.orderOut(None);
        if self.views.icon.ivars().dragging.get() {
            // The drag session still calls its source when it ends.
            let icon = self.views.icon.clone();
            LINGERING.with(|lingering| lingering.borrow_mut().push(icon));
        }
    }
}

/// An `NSRect` from a [`Rect`].
fn rect(r: Rect) -> NSRect {
    NSRect::new(NSPoint::new(r.x, r.y), NSSize::new(r.w, r.h))
}

/// A [`Rect`] from an `NSRect`.
fn from_ns(r: NSRect) -> Rect {
    Rect::new(r.origin.x, r.origin.y, r.size.width, r.size.height)
}

/// An SF Symbol.
fn symbol(name: &str, label: &str) -> Option<Retained<NSImage>> {
    NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str(name),
        Some(&NSString::from_str(label)),
    )
}

/// The primary display's height, which CoreGraphics' and AppKit's spaces flip on.
fn primary_height(mtm: MainThreadMarker) -> f64 {
    NSScreen::screens(mtm)
        .firstObject()
        .map_or(0.0, |screen| screen.frame().size.height)
}

/// The usable area of the screen `window` is on.
fn visible_frame(mtm: MainThreadMarker, window: Rect) -> Rect {
    let screens = NSScreen::screens(mtm);
    let frames: Vec<Rect> = screens
        .iter()
        .map(|screen| from_ns(screen.frame()))
        .collect();
    screen_for(window, &frames)
        .and_then(|index| screens.iter().nth(index))
        .or_else(|| NSScreen::mainScreen(mtm))
        .map_or(window, |screen| from_ns(screen.visibleFrame()))
}

define_class!(
    // SAFETY:
    // - `NSPanel` may be subclassed; the overrides only refuse key and main status, which a
    //   non-activating panel is meant to.
    // - `GuidePanel` has no ivars and does not implement `Drop`.
    #[unsafe(super(NSPanel, NSWindow, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "OnicodeGrantsGuidePanel"]
    struct GuidePanel;

    impl GuidePanel {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key(&self) -> bool {
            false
        }

        #[unsafe(method(canBecomeMainWindow))]
        fn can_become_main(&self) -> bool {
            false
        }
    }
);

impl GuidePanel {
    fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: `NSWindow`'s designated initializer, with a valid style and backing.
        let panel: Retained<Self> = unsafe {
            msg_send![
                super(this),
                initWithContentRect: frame,
                styleMask: NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
                backing: NSBackingStoreType::Buffered,
                defer: false
            ]
        };
        // SAFETY: the panel is held by `Retained` alone and is never closed, only ordered out, so
        // AppKit must not release it on close as well.
        unsafe { panel.setReleasedWhenClosed(false) };
        // Above System Settings without taking focus from it, on every Space and beside a full
        // screen window, and out of ⌘` and the Window menu.
        panel.setFloatingPanel(true);
        panel.setLevel(NSFloatingWindowLevel);
        panel.setHidesOnDeactivate(false);
        panel.setBecomesKeyOnlyIfNeeded(true);
        panel.setMovable(false);
        panel.setOpaque(false);
        panel.setHasShadow(true);
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        panel.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::IgnoresCycle,
        );
        panel
    }
}

/// The icon's state, read by AppKit's callbacks without the guide's borrow.
struct IconState {
    /// The icon may be dragged now.
    draggable: Cell<bool>,
    /// A drag session is under way.
    dragging: Cell<bool>,
    /// The helper bundle the drag carries.
    url: Retained<NSURL>,
}

define_class!(
    // SAFETY:
    // - `NSImageView` may be subclassed; the overrides take the mouse for a drag instead of
    //   moving the window, and keep AppKit's event and dragging-source contracts.
    // - `IconState` does not implement `Drop`.
    #[unsafe(super(NSImageView, NSControl, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "OnicodeGrantsGuideIcon"]
    #[ivars = IconState]
    struct IconView;

    unsafe impl NSObjectProtocol for IconView {}

    impl IconView {
        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(mouseDownCanMoveWindow))]
        fn mouse_down_can_move_window(&self) -> bool {
            false
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _event: &NSEvent) {}

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            self.begin_drag(event);
        }

        #[unsafe(method(resetCursorRects))]
        fn reset_cursor_rects(&self) {
            if self.ivars().draggable.get() {
                self.addCursorRect_cursor(self.bounds(), &NSCursor::openHandCursor());
            }
        }
    }

    unsafe impl NSDraggingSource for IconView {
        #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
        fn source_operations(
            &self,
            _session: &NSDraggingSession,
            _context: NSDraggingContext,
        ) -> NSDragOperation {
            NSDragOperation::Copy | NSDragOperation::Link | NSDragOperation::Generic
        }

        #[unsafe(method(draggingSession:endedAtPoint:operation:))]
        fn ended(&self, _session: &NSDraggingSession, _at: NSPoint, operation: NSDragOperation) {
            self.ivars().dragging.set(false);
            let dropped = operation != NSDragOperation::None;
            drive(move |active, _| active.step(Event::DragEnded { dropped }));
            LINGERING.with(|lingering| lingering.borrow_mut().retain(|icon| icon.ivars().dragging.get()));
        }
    }
);

impl IconView {
    fn new(mtm: MainThreadMarker, url: Retained<NSURL>) -> Retained<Self> {
        let (x, y, side) = ACCESSORY;
        let this = Self::alloc(mtm).set_ivars(IconState {
            draggable: Cell::new(false),
            dragging: Cell::new(false),
            url,
        });
        // SAFETY: `NSView`'s designated initializer.
        unsafe { msg_send![super(this), initWithFrame: rect(Rect::new(x, y, side, side))] }
    }

    /// Starts dragging the helper bundle as a file URL, its icon under the pointer.
    fn begin_drag(&self, event: &NSEvent) {
        let state = self.ivars();
        if !state.draggable.get() || state.dragging.get() {
            return;
        }
        let item = NSDraggingItem::initWithPasteboardWriter(
            NSDraggingItem::alloc(),
            ProtocolObject::from_ref(&*state.url),
        );
        if let Some(image) = self.image() {
            let contents: &AnyObject = &image;
            // SAFETY: an `NSImage` is what a dragging item's contents may be.
            unsafe { item.setDraggingFrame_contents(self.bounds(), Some(contents)) };
        }
        let items = NSArray::from_retained_slice(&[item]);
        let session = self.beginDraggingSessionWithItems_event_source(
            &items,
            event,
            ProtocolObject::from_ref(self),
        );
        session.setAnimatesToStartingPositionsOnCancelOrFail(true);
        state.dragging.set(true);
    }
}

define_class!(
    // SAFETY:
    // - `NSButton` may be subclassed; the override only lets the first click on the
    //   never-key panel press the button.
    // - `GuideButton` has no ivars and does not implement `Drop`.
    #[unsafe(super(NSButton, NSControl, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "OnicodeGrantsGuideButton"]
    struct GuideButton;

    impl GuideButton {
        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }
    }
);

impl GuideButton {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: `NSView`'s designated initializer.
        unsafe { msg_send![super(this), initWithFrame: NSRect::ZERO] }
    }
}

define_class!(
    // SAFETY:
    // - `NSObject` may be subclassed; the methods are button actions.
    // - `GuideTarget` has no ivars and does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "OnicodeGrantsGuideTarget"]
    struct GuideTarget;

    unsafe impl NSObjectProtocol for GuideTarget {}

    impl GuideTarget {
        #[unsafe(method(act:))]
        fn act(&self, _sender: Option<&AnyObject>) {
            drive(|active, _| active.step(Event::Action));
        }

        #[unsafe(method(dismiss:))]
        fn dismiss(&self, _sender: Option<&AnyObject>) {
            drive(|active, _| active.step(Event::Dismiss));
        }
    }
);

impl GuideTarget {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: `NSObject`'s initializer.
        unsafe { msg_send![super(this), init] }
    }
}
