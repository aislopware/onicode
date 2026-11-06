//! AppKit boundary for the browser. Wry builds the WKWebView and owns its UI
//! delegate; our navigation delegate supplies browser policy and state. The
//! page lives in a gpui-fast native host under GPUI's layer: GPUI places,
//! clips, orders and focuses it every frame (`native_view` in `view.rs`), and
//! whatever GPUI paints after it (menus, tooltips, toasts) is drawn over it.
//! All callbacks enqueue events, never re-enter GPUI. No page-to-engine IPC.
use std::cell::{Cell, RefCell};
use std::mem::ManuallyDrop;
use std::ptr::NonNull;
use std::rc::Rc;

use gpui::composition::{NativeHost, NativeHostOptions};
use gpui::{App, Window};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSColor, NSEvent, NSEventMask, NSEventModifierFlags, NSEventType, NSScreen, NSView,
};
use objc2_foundation::{
    NSDictionary, NSError, NSKeyValueChangeKey, NSKeyValueObservingOptions, NSObject,
    NSObjectNSKeyValueObserverRegistration, NSObjectProtocol, NSString,
};
use objc2_web_kit::{
    WKNavigation, WKNavigationAction, WKNavigationActionPolicy, WKNavigationDelegate,
    WKNavigationResponse, WKNavigationResponsePolicy, WKWebView,
};
use wry::{WebView, WebViewBuilderExtMacos, WebViewExtMacOS};

use super::model::{PageState, Presentation, allowed_navigation};

#[derive(Default)]
struct BrowserStore {
    profile: Option<super::profile::BrowserProfile>,
    store: Option<Retained<objc2_web_kit::WKWebsiteDataStore>>,
    preview_hosts: std::collections::BTreeSet<String>,
    /// The local preview proxy's port, as the previews' addresses carry it:
    /// 7331, unless another engine on this computer holds that and the
    /// engine's proxy listens elsewhere.
    preview_port: Option<u16>,
}
#[derive(Clone, Default)]
pub(super) struct BrowserData(Rc<RefCell<BrowserStore>>);
impl BrowserData {
    pub fn for_profile(profile: super::profile::BrowserProfile) -> Self {
        thread_local! {
            // Keep named stores alive through process termination, including
            // after the last window closes or the active identity changes.
            // macOS runs Rust TLS destructors during AppKit's exit(). Releasing
            // a WKWebsiteDataStore there removes its network session before
            // WebKit's UI-process-disconnect handler can flush its cookies.
            // In particular, deleted cookies can reappear on the next launch.
            // Intentionally skip this registry's destructor: WebKit must still
            // own the sessions when our process disconnects, and the OS reclaims
            // these process-lifetime references on exit.
            static PROFILES: ManuallyDrop<RefCell<std::collections::HashMap<std::path::PathBuf, Rc<RefCell<BrowserStore>>>>> =
                ManuallyDrop::new(RefCell::new(std::collections::HashMap::new()));
        }
        PROFILES.with(|profiles| {
            let mut profiles = profiles.borrow_mut();
            if let Some(store) = profiles.get(&profile.root) {
                return Self(store.clone());
            }
            let root = profile.root.clone();
            let store = Rc::new(RefCell::new(BrowserStore {
                profile: Some(profile),
                ..Default::default()
            }));
            profiles.insert(root, store.clone());
            Self(store)
        })
    }

    fn configuration(
        &self,
        mtm: MainThreadMarker,
    ) -> Retained<objc2_web_kit::WKWebViewConfiguration> {
        let mut data = self.0.borrow_mut();
        if data.store.is_none() {
            // SAFETY: WebKit's data store constructors, on the main thread (`mtm`).
            data.store = Some(unsafe {
                match &data.profile {
                    Some(profile) => {
                        let identifier =
                            objc2_foundation::NSUUID::from_bytes(profile.data_store_identifier());
                        objc2_web_kit::WKWebsiteDataStore::dataStoreForIdentifier(&identifier, mtm)
                    }
                    None => objc2_web_kit::WKWebsiteDataStore::nonPersistentDataStore(mtm),
                }
            });
            let port = data
                .preview_port
                .unwrap_or(onicode_proto::PREVIEW_PROXY_PORT);
            if let Err(error) =
                configure_preview_proxy(data.store.as_ref().unwrap(), &data.preview_hosts, port)
            {
                tracing::warn!(%error, "preview hostname proxy unavailable");
            }
        }
        // SAFETY: a fresh configuration given the live store, on the main thread.
        unsafe {
            let configuration = objc2_web_kit::WKWebViewConfiguration::new(mtm);
            configuration.setWebsiteDataStore(data.store.as_ref().unwrap());
            configuration
        }
    }

    pub(super) fn register_preview(&self, address: &str) {
        let Ok(url) = url::Url::parse(address) else {
            return;
        };
        let Some(host) = url.host_str() else {
            return;
        };
        // (A preview's address: a `*.localhost` name on the proxy's port,
        // whichever the engine's proxy got.)
        let Some(port) = url.port() else {
            return;
        };
        if url.scheme() != "http" || !host.ends_with(".localhost") {
            return;
        }
        let mut data = self.0.borrow_mut();
        let moved = data
            .preview_port
            .replace(port)
            .is_some_and(|was| was != port);
        if !data.preview_hosts.insert(host.into()) && !moved {
            return;
        }
        if let Some(store) = &data.store
            && let Err(error) = configure_preview_proxy(store, &data.preview_hosts, port)
        {
            tracing::warn!(%error, "preview hostname proxy unavailable");
        }
    }
}

// Network.framework's proxy objects are Objective-C OS objects, returned
// retained.
#[link(name = "Network", kind = "framework")]
unsafe extern "C" {
    fn nw_endpoint_create_host(
        hostname: *const std::ffi::c_char,
        port: *const std::ffi::c_char,
    ) -> *mut objc2_foundation::NSObject;
    fn nw_proxy_config_create_http_connect(
        proxy_endpoint: *mut objc2_foundation::NSObject,
        proxy_tls_options: *mut objc2_foundation::NSObject,
    ) -> *mut objc2_foundation::NSObject;
    fn nw_proxy_config_add_match_domain(
        config: *mut objc2_foundation::NSObject,
        match_domain: *const std::ffi::c_char,
    );
}

/// Route only discovered/opened preview hostnames through the local preview
/// proxy; other websites keep normal routing.
fn configure_preview_proxy(
    store: &objc2_web_kit::WKWebsiteDataStore,
    hosts: &std::collections::BTreeSet<String>,
    port: u16,
) -> Result<(), String> {
    if hosts.is_empty() {
        return Ok(());
    }
    use std::ffi::CString;

    use objc2_foundation::NSArray;
    // SAFETY: Network.framework returns retained objects (adopted by
    // `from_raw`) from NUL-terminated strings that outlive each call; the
    // store takes the proxy array on the main thread.
    unsafe {
        let port = CString::new(port.to_string()).map_err(|_| "Invalid preview port")?;
        let endpoint = Retained::from_raw(nw_endpoint_create_host(
            c"127.0.0.1".as_ptr(),
            port.as_ptr(),
        ))
        .ok_or("Could not create preview endpoint")?;
        let config = Retained::from_raw(nw_proxy_config_create_http_connect(
            Retained::as_ptr(&endpoint) as *mut _,
            std::ptr::null_mut(),
        ))
        .ok_or("Could not create preview proxy")?;
        for host in hosts {
            let host = CString::new(host.as_str()).map_err(|_| "Invalid preview hostname")?;
            nw_proxy_config_add_match_domain(Retained::as_ptr(&config) as *mut _, host.as_ptr());
        }
        let proxies = NSArray::arrayWithObject(&*config);
        let _: () = msg_send![store, setProxyConfigurations: &*proxies];
    }
    Ok(())
}

type Sender = tokio::sync::mpsc::Sender<NativeEvent>;
const OBSERVED: [&str; 6] = [
    "URL",
    "title",
    "loading",
    "canGoBack",
    "canGoForward",
    "underPageBackgroundColor",
];

pub(super) enum NativeEvent {
    Changed,
    Finished,
    NewTab(String),
    Favicon {
        page: String,
        url: String,
    },
    /// A script's string result (`None` for any other value or a failure).
    Evaluated(Option<String>),
}

struct ObserverState {
    tx: Sender,
    pending: Cell<bool>,
    error: RefCell<Option<String>>,
    requested_url: RefCell<Option<String>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "OnicodeBrowserObserver"]
    #[ivars = ObserverState]
    struct Observer;
    unsafe impl NSObjectProtocol for Observer {}
    impl Observer {
        #[unsafe(method(observeValueForKeyPath:ofObject:change:context:))]
        fn observe(&self, _key: Option<&NSString>, _object: Option<&AnyObject>, _change: Option<&NSDictionary<NSKeyValueChangeKey, AnyObject>>, _context: *mut std::ffi::c_void) {
            self.changed();
        }
    }
    unsafe impl WKNavigationDelegate for Observer {
        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
        fn policy(&self, _view: &WKWebView, action: &WKNavigationAction, decision: &block2::Block<dyn Fn(WKNavigationActionPolicy)>) {
            // SAFETY: the action WebKit hands its delegate, read during the call on the main thread.
            let url = unsafe { action.request().URL() }.and_then(|u| u.absoluteString()).map(|u| u.to_string()).unwrap_or_default();
            // SAFETY: as above.
            if allowed_navigation(&url) && unsafe { action.targetFrame() }.is_some_and(|frame| unsafe { frame.isMainFrame() }) {
                *self.ivars().requested_url.borrow_mut() = Some(url.clone());
            }
            decision.call((if allowed_navigation(&url) { WKNavigationActionPolicy::Allow } else { WKNavigationActionPolicy::Cancel },));
        }
        #[unsafe(method(webView:decidePolicyForNavigationResponse:decisionHandler:))]
        fn response(&self, _view: &WKWebView, response: &WKNavigationResponse, decision: &block2::Block<dyn Fn(WKNavigationResponsePolicy)>) {
            // SAFETY: the response WebKit hands its delegate, read during the call.
            let displayable = unsafe { response.canShowMIMEType() };
            if !displayable { self.fail("This file can’t be previewed here. Open it in your default browser."); }
            decision.call((if displayable { WKNavigationResponsePolicy::Allow } else { WKNavigationResponsePolicy::Cancel },));
        }
        #[unsafe(method(webView:didStartProvisionalNavigation:))]
        fn start(&self, _view: &WKWebView, _navigation: Option<&WKNavigation>) {
            self.ivars().error.borrow_mut().take(); self.changed();
        }
        #[unsafe(method(webView:didCommitNavigation:))]
        fn commit(&self, _view: &WKWebView, _navigation: Option<&WKNavigation>) {
            self.ivars().requested_url.borrow_mut().take(); self.changed();
        }
        #[unsafe(method(webView:didFinishNavigation:))]
        fn finish(&self, _view: &WKWebView, _navigation: Option<&WKNavigation>) {
            self.changed(); let _ = self.ivars().tx.try_send(NativeEvent::Finished);
        }
        #[unsafe(method(webView:didFailProvisionalNavigation:withError:))]
        fn provisional_error(&self, _view: &WKWebView, _navigation: Option<&WKNavigation>, error: &NSError) {
            if error.code() != -999 {
                tracing::warn!(domain = %error.domain(), code = error.code(), "browser provisional navigation failed");
                self.fail("Check the address and make sure your server is running, then try again.");
            }
        }
        #[unsafe(method(webView:didFailNavigation:withError:))]
        fn navigation_error(&self, _view: &WKWebView, _navigation: Option<&WKNavigation>, error: &NSError) {
            if error.code() != -999 {
                tracing::warn!(domain = %error.domain(), code = error.code(), "browser navigation failed");
                self.fail("The connection was interrupted. Try loading this page again.");
            }
        }
        #[unsafe(method(webViewWebContentProcessDidTerminate:))]
        fn terminated(&self, _view: &WKWebView) {
            self.fail("The page stopped responding. Reload to continue.");
        }
    }
);

impl Observer {
    fn new(tx: Sender, mtm: MainThreadMarker) -> Retained<Self> {
        let object = mtm.alloc().set_ivars(ObserverState {
            tx,
            pending: Cell::new(false),
            error: RefCell::new(None),
            requested_url: RefCell::new(None),
        });
        // SAFETY: NSObject's `init` on the allocation `set_ivars` just filled.
        unsafe { msg_send![super(object), init] }
    }

    fn changed(&self) {
        if !self.ivars().pending.replace(true)
            && self.ivars().tx.try_send(NativeEvent::Changed).is_err()
        {
            self.ivars().pending.set(false);
        }
    }

    fn fail(&self, message: &str) {
        *self.ivars().error.borrow_mut() = Some(message.into());
        self.changed();
    }
}

/// One tab's page. Keys need nothing here: while the page holds the keyboard,
/// gpui-fast offers each press to GPUI's keymap first and leaves unmatched
/// equivalents (⌘C, ⌘V, ⌘Z) to WebKit.
pub(super) struct NativePage {
    web: WebView,
    data: BrowserData,
    view: Retained<WKWebView>,
    observer: Retained<Observer>,
    /// The page's container under GPUI's layer; it leaves the window with
    /// the last clone, after `Drop` below.
    host: NativeHost,
    background_color: Option<Retained<NSColor>>,
    wheel_monitor: Option<Retained<AnyObject>>,
    wheel: Rc<PageGlide>,
    presentation: Presentation,
    tx: Sender,
}

impl NativePage {
    pub fn new(
        window: &mut Window,
        data: &BrowserData,
        tx: Sender,
        cx: &mut App,
    ) -> Result<Self, String> {
        let mtm = MainThreadMarker::new().ok_or("Browser must be created on the main thread")?;
        let new_tab = tx.clone();
        let web = wry::WebViewBuilder::new()
            .with_webview_configuration(data.configuration(mtm))
            .with_visible(false)
            .with_focused(false)
            .with_new_window_req_handler(move |url, _| {
                if allowed_navigation(&url) {
                    let _ = new_tab.try_send(NativeEvent::NewTab(url));
                }
                wry::NewWindowResponse::Deny
            })
            .with_download_started_handler(|_, _| false)
            .build_as_child(&*window)
            .map_err(|e| e.to_string())?;
        let view = Retained::into_super(web.webview());
        let host = window
            .create_native_host(
                NativeHostOptions {
                    opaque: true,
                    interactive: true,
                    label: Some("Web page".into()),
                },
                cx,
            )
            .map_err(|error| error.to_string())?;
        // SAFETY: `view` is the live WKWebView wry just built, on the main thread.
        unsafe { host.attach_view(NonNull::from(&*view).cast()) }
            .map_err(|error| error.to_string())?;

        let observer = Observer::new(tx.clone(), mtm);
        // SAFETY: the delegate is weak; the page keeps the observer and clears
        // the delegate and the observations in `Drop` before letting it go.
        unsafe {
            view.setNavigationDelegate(Some(ProtocolObject::from_ref(&*observer)));
            for key in OBSERVED {
                view.addObserver_forKeyPath_options_context(
                    &observer,
                    &NSString::from_str(key),
                    NSKeyValueObservingOptions::empty(),
                    std::ptr::null_mut(),
                );
            }
        }
        // Wheel ticks AppKit routes to the page glide like the rest of the
        // window (`PageGlide`); a press anywhere takes over, as in GPUI.
        let wheel = Rc::new(PageGlide::default());
        let wheel_view = view.clone();
        let page_glide = wheel.clone();
        let wheel_callback = block2::RcBlock::new(move |event: NonNull<NSEvent>| {
            // SAFETY: AppKit hands a local monitor a live event for the call.
            let e = unsafe { event.as_ref() };
            if e.r#type() != NSEventType::ScrollWheel {
                page_glide.stop();
                return event.as_ptr();
            }
            if receives_wheel(&wheel_view, e, mtm) && page_glide.take(&wheel_view, e) {
                std::ptr::null_mut()
            } else {
                event.as_ptr()
            }
        });
        // SAFETY: AppKit copies the block and keeps it until `Drop` removes
        // the monitor; main thread.
        let wheel_monitor = unsafe {
            NSEvent::addLocalMonitorForEventsMatchingMask_handler(
                NSEventMask::ScrollWheel
                    | NSEventMask::LeftMouseDown
                    | NSEventMask::RightMouseDown
                    | NSEventMask::OtherMouseDown,
                &wheel_callback,
            )
        };
        Ok(Self {
            web,
            data: data.clone(),
            view,
            observer,
            host,
            background_color: None,
            wheel_monitor,
            wheel,
            presentation: Presentation::Hidden,
            tx,
        })
    }

    /// What `native_view` places every frame the page shows.
    pub fn host(&self) -> &NativeHost {
        &self.host
    }

    pub fn present(&mut self, presentation: Presentation) {
        self.presentation = presentation;
        self.update_visibility();
    }

    pub fn load(&self, url: &str) -> Result<(), String> {
        self.data.register_preview(url);
        self.observer.ivars().error.borrow_mut().take();
        *self.observer.ivars().requested_url.borrow_mut() = Some(url.into());
        self.web.load_url(url).map_err(|e| e.to_string())
    }

    pub fn reload(&self) {
        self.observer.ivars().error.borrow_mut().take();
        // Retrying the requested URL also works after a failed provisional load.
        // SAFETY: the page's own web view, on the main thread.
        unsafe { self.view.reload() };
        self.observer.changed();
    }

    pub fn history(&self, forward: bool) {
        // SAFETY: the page's own web view, on the main thread.
        unsafe {
            if forward {
                self.view.goForward();
            } else {
                self.view.goBack();
            }
        }
    }

    pub fn state(&self) -> PageState {
        let ivars = self.observer.ivars();
        ivars.pending.set(false);
        // SAFETY: WKWebView's getters, on the main thread.
        unsafe {
            PageState {
                url: ivars.requested_url.borrow().clone().or_else(|| {
                    self.view
                        .URL()
                        .and_then(|u| u.absoluteString())
                        .map(|u| u.to_string())
                }),
                title: self.view.title().map(|s| s.to_string()).unwrap_or_default(),
                loading: self.view.isLoading(),
                can_back: self.view.canGoBack(),
                can_forward: self.view.canGoForward(),
                error: ivars.error.borrow().clone(),
            }
        }
    }

    /// Paint WebKit's backing in the page's own background, so tiles a
    /// resize exposes before WebKit redraws match the page rather than
    /// WebKit's controlBackgroundColor.
    pub fn sync_background(&mut self) {
        // SAFETY: WKWebView's getter and `isEqual:` on two live colors, on
        // the main thread.
        let color = unsafe { self.view.underPageBackgroundColor() };
        let unchanged = self
            .background_color
            .as_ref()
            .is_some_and(|previous| unsafe { msg_send![&**previous, isEqual: &*color] });
        if !unchanged {
            // SAFETY: WKWebView's private `_setBackgroundColor:` takes an
            // NSColor and returns nothing, as Safari calls it; main thread.
            let _: () = unsafe { msg_send![&*self.view, _setBackgroundColor: &*color] };
            self.background_color = Some(color);
        }
    }

    /// Run `script` in the page; a string result arrives as
    /// [`NativeEvent::Evaluated`].
    pub fn evaluate(&self, script: &str) {
        let tx = self.tx.clone();
        let completion = block2::RcBlock::new(move |value: *mut AnyObject, error: *mut NSError| {
            let result = if error.is_null() {
                // SAFETY: WebKit passes the result object, live for the call.
                unsafe { value.as_ref() }
                    .and_then(|value| value.downcast_ref::<NSString>())
                    .map(|text| text.to_string())
            } else {
                None
            };
            let _ = tx.try_send(NativeEvent::Evaluated(result));
        });
        // SAFETY: WebKit copies the completion block; main thread.
        unsafe {
            self.view.evaluateJavaScript_completionHandler(
                &NSString::from_str(script),
                Some(&completion),
            );
        }
    }

    pub fn discover_favicon(&self, page: String) {
        let tx = self.tx.clone();
        // Our navigation delegate owns readiness, so Wry's private pending-
        // script queue is intentionally bypassed. This runs only after finish.
        let completion = block2::RcBlock::new(move |value: *mut AnyObject, error: *mut NSError| {
            if !error.is_null() {
                return;
            }
            // SAFETY: WebKit passes the result object, live for the call.
            if let Some(url) =
                unsafe { value.as_ref() }.and_then(|value| value.downcast_ref::<NSString>())
            {
                let _ = tx.try_send(NativeEvent::Favicon {
                    page: page.clone(),
                    url: url.to_string(),
                });
            }
        });
        // SAFETY: WebKit copies the completion block; main thread.
        unsafe {
            self.view.evaluateJavaScript_completionHandler(
                &NSString::from_str("(() => { const link = document.querySelector('link[rel~=icon]'); return link ? link.href : new URL('/favicon.ico', location.href).href; })()"),
                Some(&completion),
            );
        }
    }

    /// Return the keyboard to GPUI if the page holds it. gpui-fast then
    /// blurs the page's focus handle, keeping the two in step.
    fn focus_chrome(&self) {
        if has_focus(&self.view) {
            let _ = self.web.focus_parent();
        }
    }

    /// Whether the page shows at all; where is GPUI's to say, every frame.
    /// A hidden web view also lets WebKit throttle a background tab.
    fn update_visibility(&self) {
        let visible = self.presentation != Presentation::Hidden
            && self.observer.ivars().error.borrow().is_none();
        if !visible || self.presentation == Presentation::Passthrough {
            self.focus_chrome();
        }
        if self.view.isHidden() == visible {
            let _ = self.web.set_visible(visible);
        }
    }
}

impl Drop for NativePage {
    fn drop(&mut self) {
        self.focus_chrome();
        // SAFETY: removing the monitor and observations `new` added, while the
        // observer lives, then clearing the delegate; main thread.
        unsafe {
            if let Some(monitor) = self.wheel_monitor.take() {
                NSEvent::removeMonitor(&monitor);
            }
            for key in OBSERVED {
                self.view
                    .removeObserver_forKeyPath(&self.observer, &NSString::from_str(key));
            }
            self.view.setNavigationDelegate(None);
            self.view.stopLoading();
        }
        self.wheel.stop();
        self.view.removeFromSuperview();
    }
}

fn has_focus(view: &NSView) -> bool {
    view.window()
        .and_then(|w| w.firstResponder())
        .and_then(|r| r.downcast::<NSView>().ok())
        .is_some_and(|v| v.isDescendantOf(view))
}

/// WebKit's pixels per wheel line: a 3-line tick scrolls a page 120px.
const PAGE_PX_PER_LINE: f64 = 40.0;

/// A page's mouse-wheel glide. AppKit hands the page its wheel directly (the
/// GPUI view declines hits where the page is topmost), so it never reaches
/// GPUI's `smooth_scroll`, and WebKit applies a tick at once. The wheel
/// monitor holds ticks back here; a timer replays the glide as precise pixel
/// deltas, which WebKit routes like a trackpad's: to the scroller under the
/// pointer, clamped at its edges.
#[derive(Default)]
struct PageGlide {
    glide: RefCell<crate::smooth_scroll::Glide>,
    /// The pointer, in window coordinates, where the replay lands.
    at: Cell<objc2_foundation::NSPoint>,
    /// Pixels owed below a whole pixel, per axis.
    carry: Cell<(f64, f64)>,
    timer: RefCell<Option<Retained<objc2_foundation::NSTimer>>>,
}

impl PageGlide {
    /// Take a wheel event AppKit routes to the page: hold a tick back to
    /// glide it, or let anything else through, ending a glide in flight.
    fn take(self: &Rc<Self>, view: &Retained<WKWebView>, event: &NSEvent) -> bool {
        // Zoom and other modified gestures keep their discrete steps.
        let commanded = event.modifierFlags().intersects(
            NSEventModifierFlags::Control
                | NSEventModifierFlags::Option
                | NSEventModifierFlags::Command
                | NSEventModifierFlags::Function,
        );
        if event.hasPreciseScrollingDeltas() || commanded || !crate::smooth_scroll::gliding() {
            self.stop();
            return false;
        }
        let lines = gpui::point(
            event.scrollingDeltaX() as f32,
            event.scrollingDeltaY() as f32,
        );
        if lines == gpui::Point::default() {
            return false;
        }
        self.push(view, lines, event.locationInWindow());
        true
    }

    fn push(
        self: &Rc<Self>,
        view: &Retained<WKWebView>,
        lines: gpui::Point<f32>,
        at: objc2_foundation::NSPoint,
    ) {
        self.at.set(at);
        self.glide
            .borrow_mut()
            .push(lines, std::time::Instant::now());
        if self.timer.borrow().is_some() {
            return;
        }
        let glide = Rc::downgrade(self);
        let view = view.clone();
        let tick = block2::RcBlock::new(move |_: NonNull<objc2_foundation::NSTimer>| {
            if let Some(glide) = glide.upgrade() {
                glide.frame(&view);
            }
        });
        // A frame at 120Hz; the curve runs on the clock, not on ticks.
        // SAFETY: the run loop keeps the timer and its copied block until
        // `stop` invalidates it; the block holds the glide weakly. Main thread.
        let timer = unsafe {
            objc2_foundation::NSTimer::scheduledTimerWithTimeInterval_repeats_block(
                1.0 / 120.0,
                true,
                &tick,
            )
        };
        *self.timer.borrow_mut() = Some(timer);
    }

    fn frame(&self, view: &WKWebView) {
        let lines = if crate::smooth_scroll::gliding() {
            self.glide.borrow_mut().advance(std::time::Instant::now())
        } else {
            self.glide.borrow_mut().finish()
        };
        let (dx, dy, carry) = whole_pixels(self.carry.get(), lines);
        self.carry.set(carry);
        if (dx, dy) != (0, 0) {
            let height = NSScreen::screens(view.mtm())
                .firstObject()
                .map(|screen| screen.frame().size.height);
            if let Some(event) =
                height.and_then(|height| precise_wheel(dx, dy, self.at.get(), height))
            {
                view.scrollWheel(&event);
            }
        }
        if !self.glide.borrow().active() {
            self.stop();
        }
    }

    fn stop(&self) {
        self.glide.borrow_mut().stop();
        self.carry.set((0.0, 0.0));
        if let Some(timer) = self.timer.borrow_mut().take() {
            timer.invalidate();
        }
    }
}

/// A slice of lines as whole page pixels, carrying the fractions to the next
/// slice, so a tick still scrolls exactly its 40px a line.
fn whole_pixels(carry: (f64, f64), lines: gpui::Point<f32>) -> (i32, i32, (f64, f64)) {
    let x = carry.0 + f64::from(lines.x) * PAGE_PX_PER_LINE;
    let y = carry.1 + f64::from(lines.y) * PAGE_PX_PER_LINE;
    let (dx, dy) = (x.round(), y.round());
    (dx as i32, dy as i32, (x - dx, y - dy))
}

/// A precise (trackpad-style) wheel event of whole pixels at `at`, a point
/// in a window. A synthesized `NSEvent` has no window and reports its
/// location, flipped by the primary screen's `height`, as `locationInWindow`,
/// which is the point WebKit hit-tests for the scroller: so it is placed
/// where `at` would be in a window at the screen's origin.
fn precise_wheel(
    dx: i32,
    dy: i32,
    at: objc2_foundation::NSPoint,
    height: f64,
) -> Option<Retained<NSEvent>> {
    use objc2_core_graphics::{CGEvent, CGEventField, CGScrollEventUnit};
    let event = CGEvent::new_scroll_wheel_event2(None, CGScrollEventUnit::Pixel, 2, dy, dx, 0)?;
    CGEvent::set_integer_value_field(Some(&*event), CGEventField::ScrollWheelEventIsContinuous, 1);
    CGEvent::set_location(
        Some(&*event),
        objc2_foundation::NSPoint::new(at.x, height - at.y),
    );
    NSEvent::eventWithCGEvent(&event)
}

/// Whether AppKit routes `event` to `view`: the view under the pointer is it
/// or inside it. GPUI's hit map decides that (a menu over the page keeps it).
fn receives_wheel(view: &NSView, event: &NSEvent, mtm: MainThreadMarker) -> bool {
    let Some(window) = view.window() else {
        return false;
    };
    if event.window(mtm).as_deref() != Some(&*window) {
        return false;
    }
    // The content view hit-tests points in its superview's space: the window's.
    window
        .contentView()
        .and_then(|content| content.hitTest(event.locationInWindow()))
        .is_some_and(|hit| hit.isDescendantOf(view))
}

#[cfg(feature = "browser-fixture")]
impl NativePage {
    /// GPUI's window point (top-left origin) in AppKit's window space.
    fn fixture_window_point(
        &self,
        x: f64,
        y: f64,
    ) -> (Retained<objc2_app_kit::NSWindow>, objc2_foundation::NSPoint) {
        let window = self.view.window().unwrap();
        let height = window.contentView().unwrap().bounds().size.height;
        (window, objc2_foundation::NSPoint::new(x, height - y))
    }

    pub fn fixture_move_cursor(&self, x: f64, y: f64) {
        #[link(name = "CoreGraphics", kind = "framework")]
        unsafe extern "C" {
            fn CGWarpMouseCursorPosition(point: objc2_foundation::NSPoint) -> i32;
        }
        let (window, point) = self.fixture_window_point(x, y);
        let p = window.convertPointToScreen(point);
        let screen = NSScreen::mainScreen(self.view.mtm()).unwrap();
        // SAFETY: a plain CoreGraphics call with a point by value.
        unsafe {
            CGWarpMouseCursorPosition(objc2_foundation::NSPoint::new(
                p.x,
                screen.frame().size.height - p.y,
            ));
        }
    }

    /// A real click through AppKit's routing: the hit map decides whether
    /// the page or GPUI gets it.
    pub fn fixture_click(&self, x: f64, y: f64) {
        self.fixture_move_cursor(x, y);
        let (window, point) = self.fixture_window_point(x, y);
        let app = objc2_app_kit::NSApplication::sharedApplication(self.view.mtm());
        for kind in [NSEventType::LeftMouseDown, NSEventType::LeftMouseUp] {
            let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(kind, point, NSEventModifierFlags::empty(), 0., window.windowNumber(), None, 0, 1, 1.).unwrap();
            app.postEvent_atStart(&event, false);
        }
    }

    pub fn fixture_focus(&self) {
        let _ = self.web.focus();
    }

    pub fn fixture_focused(&self) -> bool {
        has_focus(&self.view)
    }

    pub fn fixture_visible(&self) -> bool {
        !self.view.isHidden()
    }

    /// The web view's own width, which WebKit lays the page out to.
    pub fn fixture_frame_width(&self) -> f32 {
        self.view.frame().size.width as f32
    }

    pub fn fixture_cookies(&self, completion: impl FnOnce(Vec<String>) + 'static) {
        use objc2_foundation::{NSArray, NSHTTPCookie};
        let completion = RefCell::new(Some(completion));
        let block = block2::RcBlock::new(move |cookies: NonNull<NSArray<NSHTTPCookie>>| {
            // SAFETY: WebKit passes a live array for the call.
            let cookies = unsafe { cookies.as_ref() };
            let values = cookies
                .iter()
                .map(|cookie| {
                    format!(
                        "{}={}; domain={}; path={}; httpOnly={}; sessionOnly={}",
                        cookie.name(),
                        cookie.value(),
                        cookie.domain(),
                        cookie.path(),
                        cookie.isHTTPOnly(),
                        cookie.isSessionOnly()
                    )
                })
                .collect();
            if let Some(completion) = completion.borrow_mut().take() {
                completion(values);
            }
        });
        // SAFETY: the page's own store; WebKit copies the block. Main thread.
        unsafe {
            self.view
                .configuration()
                .websiteDataStore()
                .httpCookieStore()
                .getAllCookies(&block);
        }
    }

    pub fn fixture_eval(&self, script: &str) {
        // SAFETY: a script and no completion, on the main thread.
        unsafe {
            self.view
                .evaluateJavaScript_completionHandler(&NSString::from_str(script), None);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn a_tick_glides_in_whole_pixels_to_exactly_its_lines() {
        let mut glide = crate::smooth_scroll::Glide::default();
        let mut now = Instant::now();
        glide.push(gpui::point(-1.0, -3.0), now);
        let (mut carry, mut total, mut slices) = ((0.0, 0.0), (0, 0), 0);
        while glide.active() {
            now += Duration::from_millis(8);
            let (dx, dy, rest) = whole_pixels(carry, glide.advance(now));
            carry = rest;
            total = (total.0 + dx, total.1 + dy);
            slices += 1;
        }
        // WebKit's step for a line, spread over Chrome's 200ms.
        assert_eq!(total, (-40, -120));
        assert_eq!(slices, 25);
    }

    #[test]
    fn a_replay_is_precise_and_hit_tests_where_the_tick_was() {
        let height = objc2_core_graphics::CGDisplayBounds(objc2_core_graphics::CGMainDisplayID())
            .size
            .height;
        let at = objc2_foundation::NSPoint::new(212.0, 340.0);
        let event = precise_wheel(3, -14, at, height).unwrap();
        assert!(event.hasPreciseScrollingDeltas());
        assert_eq!(
            (event.scrollingDeltaX(), event.scrollingDeltaY()),
            (3.0, -14.0)
        );
        assert_eq!(event.locationInWindow(), at);
    }
}
