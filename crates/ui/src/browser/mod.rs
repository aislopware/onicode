//! Device-local browser tabs. GPUI owns chrome; a WKWebView under GPUI's
//! layer owns each page (macOS). Elsewhere addresses open in the default
//! browser.
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as native;
pub mod model;
pub(crate) mod profile;
mod view;

use gpui::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, Subscription, Window,
};
use model::{PageState, Presentation};

use crate::composer::{ComposerInput, ComposerInputEvent};
gpui::actions!(
    browser,
    [Reload, FocusAddress, NewTab, CloseTab, Back, Forward]
);

pub(crate) fn bind_keys(cx: &mut App, keymap: &crate::settings::KeymapConfig) {
    use crate::settings::{ShortcutId, platform_combo};
    // Existing customized app chords win. In particular old settings may
    // already assign mod-shift-r to the right pane.
    let available = |combo: &str, own: Option<ShortcutId>| {
        !combo.is_empty()
            && !ShortcutId::ALL.iter().any(|id| {
                Some(*id) != own
                    && crate::keys::parse(&platform_combo(keymap.get(*id))).ok()
                        == crate::keys::parse(&platform_combo(combo)).ok()
            })
    };
    macro_rules! bind {
        ($combo:literal, $action:ident) => {
            if available($combo, None) {
                cx.bind_keys([crate::keys::binding(
                    cx,
                    &platform_combo($combo),
                    $action,
                    Some("Browser"),
                )]);
            }
        };
    }
    bind!("mod-l", FocusAddress);
    bind!("mod-t", NewTab);
    bind!("mod-w", CloseTab);
    bind!("mod-[", Back);
    bind!("mod-]", Forward);
    let reload = keymap.get(ShortcutId::BrowserReload);
    if available(reload, Some(ShortcutId::BrowserReload))
        && crate::keys::parse(&platform_combo(reload)).is_ok()
    {
        cx.bind_keys([crate::keys::binding(
            cx,
            &platform_combo(reload),
            Reload,
            Some("Browser"),
        )]);
    }
}

#[derive(Clone, Debug)]
pub enum BrowserEvent {
    Changed,
    NewTab(Option<String>),
    Close,
    /// An element the user picked; `last` ends the pick (focus the composer).
    Annotation {
        annotation: Box<onicode_proto::annotation::BrowserAnnotation>,
        /// The page's marker for the element, labelled once numbered.
        marker: u32,
        last: bool,
    },
}

/// The in-page picker (`annotate.js`), injected only while annotating.
#[cfg(target_os = "macos")]
const ANNOTATE_SCRIPT: &str = include_str!("annotate.js");
/// How often an active pick is polled. Nothing polls otherwise.
#[cfg(target_os = "macos")]
const ANNOTATION_POLL: std::time::Duration = std::time::Duration::from_millis(100);

/// While the user picks elements on the page. Dropping it stops the poll.
#[cfg(target_os = "macos")]
struct AnnotationSession {
    nonce: u32,
    /// Picks received so far; the session ends at [`MAX_ANNOTATION_PICKS`].
    picks: usize,
    /// Polls in a row that returned nothing readable.
    misses: u32,
    _poll: gpui::Task<()>,
}

/// One session's picks. The page's script answers the polls, so a hostile
/// page could otherwise stream chips into the draft without a click.
#[cfg(target_os = "macos")]
const MAX_ANNOTATION_PICKS: usize = 24;
/// Unreadable polls (about two seconds' worth) before the session gives up.
#[cfg(target_os = "macos")]
const MAX_ANNOTATION_MISSES: u32 = 20;

/// The overlay's palette and type, so the page chrome reads as Onicode's.
#[cfg(target_os = "macos")]
fn annotation_theme(cx: &App) -> serde_json::Value {
    let theme = crate::theme::Theme::of(cx);
    let css = |color: gpui::Hsla| {
        let rgba: gpui::Rgba = color.into();
        let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        format!(
            "#{:02x}{:02x}{:02x}{:02x}",
            byte(rgba.r),
            byte(rgba.g),
            byte(rgba.b),
            byte(rgba.a)
        )
    };
    serde_json::json!({
        "accent": css(theme.accent),
        "surface": css(theme.surface_dialog),
        "border": css(theme.border),
        "text": css(theme.text),
        "muted": css(theme.text_muted),
        "faint": css(theme.text_faint),
        "chip": css(theme.ink(0.09)),
        "dark": theme.appearance.is_dark(),
        "font": crate::typography::effective_family_name(cx).to_string(),
    })
}

/// A profile's website data, allocated on first navigation. Default contexts are
/// ephemeral, for unresolved identities and isolated fixtures.
#[derive(Clone, Default)]
pub struct BrowserContext {
    #[cfg(target_os = "macos")]
    data: native::BrowserData,
}

impl BrowserContext {
    pub(crate) fn for_profile(profile: profile::BrowserProfile) -> Self {
        #[cfg(target_os = "macos")]
        {
            Self {
                data: native::BrowserData::for_profile(profile),
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = profile;
            Self::default()
        }
    }

    /// Route a preview's `*.localhost` address through the preview proxy
    /// before a page opens on it (macOS; elsewhere the name resolves as is).
    pub(crate) fn register_preview(&self, url: &str) {
        #[cfg(target_os = "macos")]
        self.data.register_preview(url);
        #[cfg(not(target_os = "macos"))]
        let _ = url;
    }
}

pub struct BrowserSurface {
    #[cfg(target_os = "macos")]
    context: BrowserContext,
    address: Entity<ComposerInput>,
    focus: FocusHandle,
    /// The page's own focus: while it is focused the web view holds the
    /// keyboard (gpui-fast keeps the two in step both ways).
    #[cfg(target_os = "macos")]
    page_focus: FocusHandle,
    pub page: PageState,
    pub favicon: Option<std::sync::Arc<gpui::Image>>,
    address_edited: bool,
    validation: Option<String>,
    remote: bool,
    previews: onicode_proto::PreviewSnapshot,
    previews_loading: bool,
    previews_task: Option<gpui::Task<()>>,
    #[cfg(feature = "browser-fixture")]
    fixture_preview_open: std::rc::Rc<std::cell::Cell<Option<gpui::Point<gpui::Pixels>>>>,
    presentation: Presentation,
    #[cfg(target_os = "macos")]
    annotation: Option<AnnotationSession>,
    _input_sub: Subscription,
    #[cfg(target_os = "macos")]
    native: Option<native::NativePage>,
    #[cfg(target_os = "macos")]
    native_tx: tokio::sync::mpsc::Sender<native::NativeEvent>,
    #[cfg(target_os = "macos")]
    _native_task: gpui::Task<()>,
    #[cfg(target_os = "macos")]
    favicon_task: Option<gpui::Task<()>>,
    #[cfg(target_os = "macos")]
    favicon_generation: u64,
}

impl EventEmitter<BrowserEvent> for BrowserSurface {}
impl Focusable for BrowserSurface {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl BrowserSurface {
    pub fn new(
        context: BrowserContext,
        remote: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let address = cx.new(|cx| {
            ComposerInput::with_context("Website or localhost:3000", "PaletteSearch", cx)
                .with_text_metrics(11.0, 16.0)
                .with_single_line()
                .with_accessibility_role(gpui::Role::TextInput)
        });
        let input_sub = cx.subscribe(&address, |this, _, event, cx| {
            if matches!(event, ComposerInputEvent::Edited) {
                this.address_edited =
                    this.address.read(cx).text() != this.page.url.as_deref().unwrap_or_default();
                this.validation = None;
                cx.notify();
            }
        });
        #[cfg(target_os = "macos")]
        let (native_tx, mut events) = tokio::sync::mpsc::channel(64);
        #[cfg(target_os = "macos")]
        let native_task = cx.spawn_in(window, async move |this, cx| {
            while let Some(event) = events.recv().await {
                if this
                    .update_in(cx, |this, window, cx| {
                        this.on_native_event(event, window, cx)
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        #[cfg(not(target_os = "macos"))]
        let _ = (window, context);
        Self {
            #[cfg(target_os = "macos")]
            context,
            address,
            focus: cx.focus_handle(),
            #[cfg(target_os = "macos")]
            page_focus: cx.focus_handle(),
            page: PageState::default(),
            favicon: None,
            address_edited: false,
            validation: None,
            remote,
            previews: onicode_proto::PreviewSnapshot::default(),
            previews_loading: true,
            previews_task: None,
            #[cfg(feature = "browser-fixture")]
            fixture_preview_open: Default::default(),
            presentation: Presentation::Hidden,
            #[cfg(target_os = "macos")]
            annotation: None,
            _input_sub: input_sub,
            #[cfg(target_os = "macos")]
            native: None,
            #[cfg(target_os = "macos")]
            native_tx,
            #[cfg(target_os = "macos")]
            _native_task: native_task,
            #[cfg(target_os = "macos")]
            favicon_task: None,
            #[cfg(target_os = "macos")]
            favicon_generation: 0,
        }
    }

    pub fn title(&self) -> gpui::SharedString {
        self.page.label().into()
    }

    pub fn set_remote(&mut self, remote: bool) {
        self.remote = remote;
    }

    /// Moving GPUI's focus off the page also takes the keyboard back from it.
    pub fn focus_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.address.focus_handle(cx), cx);
        window.dispatch_action(Box::new(crate::composer::SelectAll), cx);
    }

    pub fn set_presentation(&mut self, presentation: Presentation, cx: &mut Context<Self>) {
        if self.presentation == presentation {
            return;
        }
        if presentation == Presentation::Hidden {
            self.stop_annotation(cx);
        }
        self.presentation = presentation;
        #[cfg(target_os = "macos")]
        if let Some(native) = &mut self.native {
            native.present(presentation);
        }
        cx.notify();
    }

    #[cfg(feature = "browser-fixture")]
    pub fn fixture_previews(&self) -> onicode_proto::PreviewSnapshot {
        self.previews.clone()
    }

    #[cfg(feature = "browser-fixture")]
    pub fn fixture_preview_open_position(&self) -> Option<gpui::Point<gpui::Pixels>> {
        self.fixture_preview_open.get()
    }

    /// The daemon resolves the session's current cwd for every update, so a
    /// checkout change cannot leave this tab discovering the previous project.
    pub fn watch_previews(
        &mut self,
        handle: crate::state::EngineHandle,
        chat_id: String,
        cx: &mut Context<Self>,
    ) {
        self.previews_task = Some(cx.spawn(async move |this, cx| {
            loop {
                let subscription = handle
                    .client()
                    .subscribe(
                        onicode_rpc::methods::WATCH_PREVIEWS,
                        serde_json::json!({"chatId": chat_id}),
                    )
                    .await;
                if let Ok(mut updates) = subscription {
                    while let Some(value) = updates.recv().await {
                        if let Ok(snapshot) =
                            serde_json::from_value::<onicode_proto::PreviewSnapshot>(value)
                            && this
                                .update(cx, |this, cx| {
                                    #[cfg(target_os = "macos")]
                                    for service in &snapshot.services {
                                        this.context
                                            .data
                                            .register_preview(&service.url(snapshot.proxy_port));
                                    }
                                    this.previews = snapshot;
                                    this.previews_loading = false;
                                    cx.notify();
                                })
                                .is_err()
                        {
                            return;
                        }
                    }
                }
                if this
                    .update(cx, |this, cx| {
                        this.previews.services.clear();
                        this.previews.error = Some("Connecting to preview discovery…".into());
                        this.previews_loading = false;
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(2))
                    .await;
            }
        }));
    }

    pub fn navigate(&mut self, input: &str, window: &mut Window, cx: &mut Context<Self>) {
        let url = match model::normalize_address(input) {
            Ok(url) => url,
            Err(message) => {
                self.validation = Some(message.into());
                cx.notify();
                return;
            }
        };
        self.validation = None;
        self.address
            .update(cx, |input, cx| input.set_text(url.clone(), cx));
        self.address_edited = false;
        self.page.url = Some(url.clone());
        self.page.title.clear();
        self.page.error = None;
        self.clear_favicon(cx);
        #[cfg(target_os = "macos")]
        {
            self.favicon_generation += 1;
            self.favicon_task = None;
            let result = if let Some(native) = &self.native {
                native.load(&url)
            } else {
                native::NativePage::new(window, &self.context.data, self.native_tx.clone(), cx)
                    .and_then(|mut native| {
                        native.present(self.presentation);
                        self.native.insert(native).load(&url)
                    })
            };
            self.page.loading = result.is_ok();
            if let Err(error) = result {
                self.page.error = Some(format!("Could not open this page: {error}"));
            }
            window.focus(&self.focus, cx);
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = window;
            cx.open_url(&url);
        }
        cx.emit(BrowserEvent::Changed);
        cx.notify();
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let url = self.address.read(cx).text().to_owned();
        self.navigate(&url, window, cx);
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        #[cfg(target_os = "macos")]
        if let Some(native) = &self.native {
            if self.page.error.is_some() {
                if let Some(url) = &self.page.url {
                    let _ = native.load(url);
                }
            } else {
                native.reload();
            }
            self.page.error = None;
        }
        #[cfg(not(target_os = "macos"))]
        self.open_external(cx);
        cx.notify();
    }

    fn open_external(&self, cx: &mut Context<Self>) {
        if let Some(url) = self
            .page
            .url
            .as_deref()
            .filter(|url| model::allowed_navigation(url))
        {
            cx.open_url(url);
        }
    }

    pub fn annotating(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            self.annotation.is_some()
        }
        #[cfg(not(target_os = "macos"))]
        {
            false
        }
    }

    /// Annotation needs an embedded, loaded page.
    pub fn can_annotate(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            self.native.is_some() && self.page.url.is_some() && self.page.error.is_none()
        }
        #[cfg(not(target_os = "macos"))]
        {
            false
        }
    }

    pub fn toggle_annotation(&mut self, cx: &mut Context<Self>) {
        if self.annotating() {
            self.stop_annotation(cx);
        } else {
            self.start_annotation(cx);
        }
    }

    #[cfg(target_os = "macos")]
    fn start_annotation(&mut self, cx: &mut Context<Self>) {
        if !self.can_annotate() {
            return;
        }
        // Below 2^32, so the page's JavaScript numbers hold it exactly.
        let nonce = (uuid::Uuid::new_v4().as_u128() as u32).max(1);
        self.evaluate(&format!(
            "{ANNOTATE_SCRIPT}\n;window.__onicodeAnnotator.start({nonce}, {}); 'started'",
            annotation_theme(cx)
        ));
        let poll = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(ANNOTATION_POLL).await;
                let polling = this.update(cx, |this, _| {
                    let live = this
                        .annotation
                        .as_ref()
                        .is_some_and(|session| session.nonce == nonce);
                    if live {
                        this.evaluate(&format!(
                            "(() => {{ const a = window.__onicodeAnnotator; return a ? a.take({nonce}) : '{{\"state\":\"gone\"}}'; }})()"
                        ));
                    }
                    live
                });
                if !matches!(polling, Ok(true)) {
                    return;
                }
            }
        });
        self.annotation = Some(AnnotationSession {
            nonce,
            picks: 0,
            misses: 0,
            _poll: poll,
        });
        cx.emit(BrowserEvent::Changed);
        cx.notify();
    }

    #[cfg(not(target_os = "macos"))]
    fn start_annotation(&mut self, _cx: &mut Context<Self>) {}

    pub fn stop_annotation(&mut self, cx: &mut Context<Self>) {
        #[cfg(target_os = "macos")]
        if self.annotation.take().is_some() {
            self.evaluate("(() => { window.__onicodeAnnotator?.stop(); return 'stopped'; })()");
            cx.emit(BrowserEvent::Changed);
            cx.notify();
        }
        #[cfg(not(target_os = "macos"))]
        let _ = cx;
    }

    /// Picks arrive as the poll's string result. A `gone` state means the
    /// picker left the page (it navigated), which ends the session.
    #[cfg(target_os = "macos")]
    fn on_evaluated(&mut self, value: Option<String>, cx: &mut Context<Self>) {
        #[derive(serde::Deserialize)]
        struct Pick {
            #[serde(flatten)]
            annotation: onicode_proto::annotation::BrowserAnnotation,
            #[serde(default)]
            marker: u32,
        }
        #[derive(serde::Deserialize)]
        struct Poll {
            state: String,
            #[serde(default)]
            picks: Vec<Pick>,
        }
        let Some(session) = self.annotation.as_mut() else {
            return;
        };
        let Some(poll) = value.and_then(|text| serde_json::from_str::<Poll>(&text).ok()) else {
            // Acknowledgements, other scripts' results, or a page that is
            // briefly unreachable. Only a run of these ends the session.
            session.misses += 1;
            if session.misses >= MAX_ANNOTATION_MISSES {
                self.stop_annotation(cx);
            }
            return;
        };
        session.misses = 0;
        let room = MAX_ANNOTATION_PICKS.saturating_sub(session.picks);
        let picks: Vec<Pick> = poll.picks.into_iter().take(room).collect();
        session.picks += picks.len();
        let finished = poll.state != "active" || session.picks >= MAX_ANNOTATION_PICKS;
        let count = picks.len();
        for (index, pick) in picks.into_iter().enumerate() {
            cx.emit(BrowserEvent::Annotation {
                annotation: Box::new(pick.annotation.bounded()),
                marker: pick.marker,
                last: finished && index + 1 == count,
            });
        }
        if finished {
            // `stop` tidies the page's overlay when the cap, not the page,
            // ended the session.
            self.stop_annotation(cx);
        }
    }

    /// Number a picked element's marker like its composer chip.
    pub fn label_marker(&self, marker: u32, number: u32) {
        #[cfg(target_os = "macos")]
        self.evaluate(&format!(
            "(() => {{ window.__onicodeAnnotator?.label({marker}, {number}); return 'ok'; }})()"
        ));
        #[cfg(not(target_os = "macos"))]
        let _ = (marker, number);
    }

    /// Keep only the markers whose chips are still in the draft.
    pub fn keep_markers(&self, numbers: &[u32]) {
        #[cfg(target_os = "macos")]
        self.evaluate(&format!(
            "(() => {{ window.__onicodeAnnotator?.keep({numbers:?}); return 'ok'; }})()"
        ));
        #[cfg(not(target_os = "macos"))]
        let _ = numbers;
    }

    /// Emphasize the marker of the chip under the pointer (`None`: none).
    pub fn focus_marker(&self, number: Option<u32>) {
        #[cfg(target_os = "macos")]
        self.evaluate(&format!(
            "(() => {{ window.__onicodeAnnotator?.focus({}); return 'ok'; }})()",
            number.unwrap_or(0)
        ));
        #[cfg(not(target_os = "macos"))]
        let _ = number;
    }

    #[cfg(target_os = "macos")]
    fn evaluate(&self, script: &str) {
        if let Some(native) = &self.native {
            native.evaluate(script);
        }
    }

    fn history(&mut self, forward: bool) {
        #[cfg(target_os = "macos")]
        if let Some(native) = &self.native {
            native.history(forward);
        }
        #[cfg(not(target_os = "macos"))]
        let _ = forward;
    }

    /// Explicitly close even if an async callback temporarily retains an entity.
    fn clear_favicon(&mut self, cx: &mut Context<Self>) {
        if let Some(image) = self.favicon.take() {
            cx.defer(move |cx| gpui::ImageSource::Image(image).evict(None, cx));
        }
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.stop_annotation(cx);
        self.set_presentation(Presentation::Hidden, cx);
        self.clear_favicon(cx);
        #[cfg(target_os = "macos")]
        {
            self.native = None;
            self.favicon_task = None;
            self.favicon_generation += 1;
        }
    }
}

#[cfg(target_os = "macos")]
impl BrowserSurface {
    fn on_native_event(
        &mut self,
        event: native::NativeEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(native) = &mut self.native else {
            return;
        };
        match event {
            native::NativeEvent::Changed | native::NativeEvent::Finished => {
                let finished = matches!(event, native::NativeEvent::Finished);
                native.sync_background();
                let mut page = native.state();
                // A failed provisional request has no committed WebKit URL.
                if page.url.is_none() {
                    page.url = self.page.url.clone();
                }
                if page.error.is_some() {
                    page.loading = false;
                }
                if page.url != self.page.url || (!self.page.loading && page.loading) {
                    if let Some(image) = self.favicon.take() {
                        cx.defer(move |cx| gpui::ImageSource::Image(image).evict(None, cx));
                    }
                    self.favicon_generation += 1;
                    self.favicon_task = None;
                }
                if !self.address.focus_handle(cx).is_focused(window) {
                    if let Some(url) = &page.url
                        && self.address.read(cx).text() != url
                    {
                        self.address
                            .update(cx, |input, cx| input.set_text(url.clone(), cx));
                    }
                    self.address_edited = false;
                }
                native.present(self.presentation);
                if finished && let Some(url) = &page.url {
                    native.discover_favicon(url.clone());
                }
                if page != self.page {
                    self.page = page;
                    cx.emit(BrowserEvent::Changed);
                }
                cx.notify();
            }
            native::NativeEvent::NewTab(url) => {
                if self.presentation == Presentation::Live {
                    cx.emit(BrowserEvent::NewTab(Some(url)));
                }
            }
            native::NativeEvent::Evaluated(value) => self.on_evaluated(value, cx),
            native::NativeEvent::Favicon { page, url } => {
                if self.page.url.as_deref() != Some(&page) || !model::allowed_navigation(&url) {
                    return;
                }
                let generation = self.favicon_generation;
                // reqwest's system proxy has no loopback exemption; a local dev
                // server's favicon must be fetched directly.
                let direct = url::Url::parse(&url).is_ok_and(|parsed| model::loopback(&parsed));
                let download = gpui_tokio::Tokio::spawn(cx, async move {
                    let mut builder = reqwest::Client::builder()
                        .timeout(std::time::Duration::from_secs(5))
                        .redirect(reqwest::redirect::Policy::limited(3));
                    if direct {
                        builder = builder.no_proxy();
                    }
                    let client = builder.build().ok()?;
                    let mut response =
                        client.get(url).send().await.ok()?.error_for_status().ok()?;
                    if response.content_length().is_some_and(|n| n > 1024 * 1024) {
                        return None;
                    }
                    let mut bytes = Vec::new();
                    while let Some(chunk) = response.chunk().await.ok()? {
                        if bytes.len() + chunk.len() > 1024 * 1024 {
                            return None;
                        }
                        bytes.extend_from_slice(&chunk);
                    }
                    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
                        .with_guessed_format()
                        .ok()?;
                    let mut limits = image::Limits::default();
                    limits.max_image_width = Some(1024);
                    limits.max_image_height = Some(1024);
                    limits.max_alloc = Some(8 * 1024 * 1024);
                    reader.limits(limits);
                    let icon = reader.decode().ok()?.thumbnail(32, 32);
                    let mut png = std::io::Cursor::new(Vec::new());
                    icon.write_to(&mut png, image::ImageFormat::Png).ok()?;
                    Some(png.into_inner())
                });
                self.favicon_task = Some(cx.spawn(async move |this, cx| {
                    let Ok(Some(bytes)) = download.await else {
                        return;
                    };
                    let _ = this.update(cx, |this, cx| {
                        if this.favicon_generation == generation
                            && this.page.url.as_deref() == Some(&page)
                        {
                            this.favicon = Some(std::sync::Arc::new(gpui::Image::from_bytes(
                                gpui::ImageFormat::Png,
                                bytes,
                            )));
                            cx.emit(BrowserEvent::Changed);
                            cx.notify();
                        }
                    });
                }));
            }
        }
    }
}

#[cfg(feature = "browser-fixture")]
impl BrowserContext {
    pub fn fixture_persistence_supported() -> bool {
        cfg!(target_os = "macos")
    }

    /// Named WK stores live outside the fixture's temporary data directory.
    /// Remove only the store derived from this fixture installation and identity.
    pub fn fixture_remove_profile(
        data_dir: &std::path::Path,
        device: &str,
        completion: impl FnOnce(Result<(), String>) + Send + 'static,
    ) {
        #[cfg(target_os = "macos")]
        {
            use wry::WebViewExtDarwin;
            let profile = profile::BrowserProfile::for_device(data_dir, Some(device)).unwrap();
            eprintln!(
                "Storage cleanup: invoke; main_thread={}",
                objc2::MainThreadMarker::new().is_some()
            );
            // A cleanup-only process has not constructed a WebKit store yet.
            // Initialize its main run loop before removal dispatches back from the
            // WebsiteDataStoreIO queue. An ephemeral store never opens user data.
            let mtm =
                objc2::MainThreadMarker::new().expect("profile cleanup requires the main thread");
            let initialization =
                unsafe { objc2_web_kit::WKWebsiteDataStore::nonPersistentDataStore(mtm) };
            drop(initialization);
            wry::WebView::remove_data_store(&profile.data_store_identifier(), move |result| {
                eprintln!(
                    "Storage cleanup: callback; main_thread={}; result={result:?}",
                    objc2::MainThreadMarker::new().is_some()
                );
                completion(result.map_err(|error| error.to_string()));
            });
            eprintln!("Storage cleanup: invocation returned");
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (data_dir, device);
            completion(Ok(()));
        }
    }
}

#[cfg(feature = "browser-fixture")]
impl BrowserSurface {
    pub fn fixture_history(&mut self, forward: bool) {
        self.history(forward);
    }

    /// The page shows: its web view is visible and the last presented frame
    /// placed it somewhere it can be seen.
    pub fn fixture_native_visible(&self, window: &Window) -> bool {
        #[cfg(target_os = "macos")]
        {
            self.native.as_ref().is_some_and(|n| n.fixture_visible())
                && self
                    .fixture_placement(window)
                    .is_some_and(|placement| !placement.visible.is_empty())
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = window;
            false
        }
    }

    pub fn fixture_eval(&self, script: &str) {
        #[cfg(target_os = "macos")]
        if let Some(native) = &self.native {
            native.fixture_eval(script);
        }
        #[cfg(not(target_os = "macos"))]
        let _ = script;
    }
}

#[cfg(all(feature = "browser-fixture", target_os = "macos"))]
impl BrowserSurface {
    fn fixture_page(&self) -> &native::NativePage {
        self.native.as_ref().expect("the browser has no page")
    }

    /// Where the last presented frame placed the page.
    pub fn fixture_placement(
        &self,
        window: &Window,
    ) -> Option<gpui::composition::NativeFramePlacement> {
        let id = self.native.as_ref()?.host().id();
        window
            .presented_natives()?
            .placements
            .iter()
            .find(|placement| placement.id == id)
            .cloned()
    }

    pub fn fixture_origin(&self, window: &Window) -> (f32, f32) {
        let bounds = self
            .fixture_placement(window)
            .expect("page not placed")
            .bounds;
        (bounds.origin.x.into(), bounds.origin.y.into())
    }

    /// The page's layout width, its web view's width and the width that shows.
    pub fn fixture_geometry(&self, window: &Window) -> (f32, f32, f32) {
        let placement = self.fixture_placement(window).expect("page not placed");
        (
            placement.bounds.size.width.into(),
            self.fixture_page().fixture_frame_width(),
            placement.visible.size.width.into(),
        )
    }

    /// Whether the page takes the pointer at a window point, by GPUI's hit map.
    pub fn fixture_page_hit(&self, window: &Window, x: f64, y: f64) -> bool {
        let id = self.fixture_page().host().id();
        window.native_hit_map().is_some_and(|map| {
            map.native_at(gpui::point(gpui::px(x as f32), gpui::px(y as f32))) == Some(id)
        })
    }

    /// The backdrop blurs drawn over the page: (x, y, width, height).
    pub fn fixture_backdrops(&self, window: &Window) -> Vec<(f64, f64, f64, f64)> {
        let (Some(placement), Some(frame)) =
            (self.fixture_placement(window), window.presented_natives())
        else {
            return Vec::new();
        };
        frame
            .blurs
            .iter()
            .filter(|blur| blur.natives_below > placement.rank)
            .map(|blur| {
                let b = blur.bounds;
                let f = |p: gpui::Pixels| f64::from(f32::from(p));
                (
                    f(b.origin.x),
                    f(b.origin.y),
                    f(b.size.width),
                    f(b.size.height),
                )
            })
            .collect()
    }

    pub fn fixture_move_cursor(&self, x: f64, y: f64) {
        self.fixture_page().fixture_move_cursor(x, y);
    }

    pub fn fixture_click(&self, x: f64, y: f64) {
        self.fixture_page().fixture_click(x, y);
    }

    pub fn fixture_focus(&self) {
        self.fixture_page().fixture_focus();
    }

    /// Whether the web view holds AppKit's keyboard.
    pub fn fixture_focused(&self) -> bool {
        self.fixture_page().fixture_focused()
    }

    /// Whether GPUI's focus is on the page.
    pub fn fixture_page_focused(&self, window: &Window) -> bool {
        self.page_focus.is_focused(window)
    }

    pub fn fixture_cookies(&self, completion: impl FnOnce(Vec<String>) + 'static) {
        self.fixture_page().fixture_cookies(completion);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[gpui::test]
    fn address_enter_rejects_unsupported_schemes_and_escape_restores_url(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_base::init(cx);
            crate::composer::init(cx, Default::default());
            cx.set_global(crate::theme::Theme::default());
            bind_keys(cx, &crate::settings::KeymapConfig::default());
        });
        let window = cx.add_window(|window, cx| {
            BrowserSurface::new(BrowserContext::default(), false, window, cx)
        });
        window
            .update(cx, |browser, window, cx| {
                browser.page.url = Some("https://example.com/".into());
                browser
                    .address
                    .update(cx, |input, cx| input.set_text("javascript:alert(1)", cx));
                browser.focus_address(window, cx);
            })
            .unwrap();
        cx.simulate_keystrokes(window.into(), "enter");
        window
            .update(cx, |browser, _, _| {
                assert!(
                    browser.validation.is_some(),
                    "Enter did not reach browser address handling"
                );
                assert_eq!(browser.page.url.as_deref(), Some("https://example.com/"));
            })
            .unwrap();
        cx.simulate_keystrokes(window.into(), "escape");
        window
            .update(cx, |browser, window, cx| {
                assert!(browser.validation.is_none());
                assert_eq!(browser.address.read(cx).text(), "https://example.com/");
                assert!(browser.focus.is_focused(window));
                window.blur(cx);
            })
            .unwrap();
    }

    #[gpui::test]
    fn customized_app_shortcuts_win_over_browser_defaults(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let mut config = crate::settings::KeymapConfig::default();
            bind_keys(cx, &config);
            assert_eq!(
                cx.key_bindings()
                    .borrow()
                    .bindings_for_action(&FocusAddress)
                    .count(),
                1
            );
            assert_eq!(
                cx.key_bindings()
                    .borrow()
                    .bindings_for_action(&Reload)
                    .count(),
                1
            );
            cx.clear_key_bindings();
            config.toggle_sidebar = crate::settings::platform_combo("mod-l");
            config.toggle_changes = "mod-shift-r".into();
            bind_keys(cx, &config);
            assert_eq!(
                cx.key_bindings()
                    .borrow()
                    .bindings_for_action(&FocusAddress)
                    .count(),
                0
            );
            assert_eq!(
                cx.key_bindings()
                    .borrow()
                    .bindings_for_action(&Reload)
                    .count(),
                0
            );
            assert_eq!(
                cx.key_bindings()
                    .borrow()
                    .bindings_for_action(&NewTab)
                    .count(),
                1
            );
        });
    }
}
