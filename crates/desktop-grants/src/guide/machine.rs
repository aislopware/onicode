//! The guide's phases, driven by what the panel sees and what desktopd reports, with time passed
//! in so every path replays in a test.

use std::time::Duration;

use crate::{Grants, Pane};

/// How long System Settings has to show a window after the deep link before the guide gives up.
pub const SETTINGS_WAIT: Duration = Duration::from_secs(10);
/// How long System Settings' window may be gone, once seen, before the guide ends.
pub const SETTINGS_GONE: Duration = Duration::from_secs(3);
/// How long a drop has to raise the authentication prompt. A drop that raises none (the helper is
/// already listed, or it missed the list) goes back to the drag.
pub const PROMPT_WAIT: Duration = Duration::from_secs(4);
/// How long after the prompt goes the grant has to arrive before the guide calls it cancelled.
/// The grant comes from desktopd through the engine, about a second behind.
pub const PROMPT_SETTLE: Duration = Duration::from_secs(3);
/// How long the panel shows "on" before it closes.
pub const GRANTED_LINGER: Duration = Duration::from_millis(1500);
/// How long after the deep link the window's title is taken as the list's own: the link needs a
/// moment to switch panes.
pub const TITLE_SETTLE: Duration = Duration::from_secs(1);

/// Where the guide is.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Phase {
    /// The icon waits to be dragged into the list.
    Drag,
    /// Dropped, or the authentication prompt is up: waiting for desktopd to report the grant.
    Confirming,
    /// The prompt went without the grant arriving: drag again.
    AuthCancelled,
    /// System Settings shows some other pane; the panel offers the way back.
    OffPane,
    /// desktopd reports the grant; the panel shows it, then closes.
    Granted,
    /// The guide is over; the panel is gone.
    Ended(End),
}

impl Phase {
    /// Whether the icon may be dragged now.
    #[must_use]
    pub const fn draggable(self) -> bool {
        matches!(self, Self::Drag | Self::AuthCancelled)
    }

    /// Whether the guide is over.
    #[must_use]
    pub const fn is_ended(self) -> bool {
        matches!(self, Self::Ended(_))
    }
}

/// How a guide ended.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum End {
    /// The grant is on.
    Granted,
    /// System Settings closed, or never opened, before the grant came on. The app's Desktop view
    /// offers the guide again.
    SettingsClosed,
    /// The person closed the panel.
    Dismissed,
}

/// What the panel saw of System Settings on one tick.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct SettingsView {
    /// Its main window is on screen.
    pub open: bool,
    /// An authentication prompt or sheet is up over it.
    pub prompt: bool,
    /// Its main window's title, readable only when the app has Screen Recording itself.
    pub title: Option<String>,
}

/// What happened.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Event {
    /// desktopd's grants, as the app last heard them.
    Grants(Grants),
    /// A drag of the icon ended; `dropped` when something took it.
    DragEnded {
        /// The drop was accepted.
        dropped: bool,
    },
    /// System Settings as the panel saw it.
    Settings(SettingsView),
    /// A click on System Settings' sidebar or back and forward buttons. Without a readable title
    /// it is the only sign the person left the list.
    NavigationClicked,
    /// The panel's button.
    Action,
    /// The panel's close button.
    Dismiss,
    /// Time passed.
    Tick,
}

/// What the guide asks of the panel.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Effect {
    /// Open the pane's deep link and bring System Settings forward.
    OpenPane,
    /// Close the panel: the guide has ended.
    Close,
}

/// What the panel shows beside its words.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Accessory {
    /// The helper's icon.
    Icon,
    /// A spinner.
    Spinner,
    /// A check mark.
    Check,
    /// System Settings' icon.
    Settings,
}

/// What the panel shows in one phase.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Look {
    /// Beside the words.
    pub accessory: Accessory,
    /// The first line.
    pub title: String,
    /// The second line.
    pub subtitle: String,
    /// The button's label, when the phase has one.
    pub action: Option<&'static str>,
    /// The icon may be dragged.
    pub draggable: bool,
}

/// One guide to one pane.
#[derive(Clone, Debug)]
pub struct Guide {
    pane: Pane,
    phase: Phase,
    /// When the pane was last opened.
    opened_at: Duration,
    /// System Settings' window has been seen since then.
    settings_seen: bool,
    /// Since when the window has been missing.
    missing_since: Option<Duration>,
    /// When the current confirmation began.
    confirm_since: Duration,
    prompt_visible: bool,
    /// A prompt has been up during the current confirmation.
    prompt_seen: bool,
    /// When the prompt went.
    prompt_gone_at: Option<Duration>,
    granted_at: Duration,
    /// The list's title, learnt once the deep link settled.
    pane_title: Option<String>,
    /// The last title read; `None` while titles are unreadable.
    last_title: Option<String>,
}

impl Guide {
    /// A guide to `pane` opened at `now`, and its first effect: open the pane.
    #[must_use]
    pub const fn open(pane: Pane, now: Duration) -> (Self, Effect) {
        let guide = Self {
            pane,
            phase: Phase::Drag,
            opened_at: now,
            settings_seen: false,
            missing_since: None,
            confirm_since: now,
            prompt_visible: false,
            prompt_seen: false,
            prompt_gone_at: None,
            granted_at: now,
            pane_title: None,
            last_title: None,
        };
        (guide, Effect::OpenPane)
    }

    /// The pane.
    #[must_use]
    pub const fn pane(&self) -> Pane {
        self.pane
    }

    /// The phase.
    #[must_use]
    pub const fn phase(&self) -> Phase {
        self.phase
    }

    /// Takes `event` at `now` (time since any fixed point, never going back) and answers what the
    /// panel must do. An ended guide takes nothing and asks nothing.
    pub fn step(&mut self, event: Event, now: Duration) -> Option<Effect> {
        if self.phase.is_ended() {
            return None;
        }
        let effect = match event {
            Event::Grants(grants) => {
                if grants.has(self.pane) && self.phase != Phase::Granted {
                    self.phase = Phase::Granted;
                    self.granted_at = now;
                }
                None
            }
            Event::DragEnded { dropped } => {
                if dropped && self.phase.draggable() {
                    self.confirm(now);
                }
                None
            }
            Event::Settings(view) => {
                self.see(view, now);
                None
            }
            Event::NavigationClicked => {
                if self.last_title.is_none() && self.phase.draggable() {
                    self.phase = Phase::OffPane;
                }
                None
            }
            Event::Action => (self.phase == Phase::OffPane).then(|| {
                self.reopen(now);
                Effect::OpenPane
            }),
            Event::Dismiss => {
                self.phase = Phase::Ended(End::Dismissed);
                Some(Effect::Close)
            }
            Event::Tick => None,
        };
        effect.or_else(|| self.advance(now))
    }

    /// What the panel shows, for the helper named `app`; `None` once the guide ended.
    #[must_use]
    pub fn look(&self, app: &str) -> Option<Look> {
        let pane = self.pane.name();
        let look = match self.phase {
            Phase::Drag => Look {
                accessory: Accessory::Icon,
                title: format!("Drag {app} into {pane}"),
                subtitle: "Then confirm with Touch ID or your password.".to_owned(),
                action: None,
                draggable: true,
            },
            Phase::AuthCancelled => Look {
                accessory: Accessory::Icon,
                title: format!("{app} isn’t in the list yet"),
                subtitle: "Drag it in again and confirm with Touch ID or your password.".to_owned(),
                action: None,
                draggable: true,
            },
            Phase::Confirming => Look {
                accessory: Accessory::Spinner,
                title: "Confirm in System Settings".to_owned(),
                subtitle: format!("Waiting for {pane} to turn on for {app}…"),
                action: None,
                draggable: false,
            },
            Phase::OffPane => Look {
                accessory: Accessory::Settings,
                title: format!("{app} goes in {pane}"),
                subtitle: "System Settings is showing another list.".to_owned(),
                action: Some("Go Back"),
                draggable: false,
            },
            Phase::Granted => Look {
                accessory: Accessory::Check,
                title: format!("{pane} is on"),
                subtitle: if self.pane.needs_restart() {
                    format!("{app} restarts to use it.")
                } else {
                    format!("{app} can use it now.")
                },
                action: None,
                draggable: false,
            },
            Phase::Ended(_) => return None,
        };
        Some(look)
    }

    fn confirm(&mut self, now: Duration) {
        self.phase = Phase::Confirming;
        self.confirm_since = now;
        self.prompt_seen = self.prompt_visible;
        self.prompt_gone_at = None;
    }

    fn reopen(&mut self, now: Duration) {
        self.phase = Phase::Drag;
        self.opened_at = now;
        self.settings_seen = false;
        self.missing_since = None;
        self.pane_title = None;
    }

    fn see(&mut self, view: SettingsView, now: Duration) {
        if view.open {
            self.settings_seen = true;
            self.missing_since = None;
        } else {
            self.missing_since.get_or_insert(now);
        }
        if view.prompt != self.prompt_visible {
            self.prompt_visible = view.prompt;
            if view.prompt {
                if matches!(
                    self.phase,
                    Phase::Drag | Phase::AuthCancelled | Phase::OffPane
                ) {
                    self.confirm(now);
                }
                self.prompt_seen = true;
                self.prompt_gone_at = None;
            } else {
                self.prompt_gone_at = Some(now);
            }
        }
        let Some(title) = view.title.filter(|_| view.open) else {
            return;
        };
        self.last_title = Some(title.clone());
        if now < self.opened_at.saturating_add(TITLE_SETTLE) {
            return;
        }
        match &self.pane_title {
            None => self.pane_title = Some(title),
            Some(pane_title) if *pane_title == title => {
                if self.phase == Phase::OffPane {
                    self.phase = Phase::Drag;
                }
            }
            Some(_) => {
                if self.phase.draggable() {
                    self.phase = Phase::OffPane;
                }
            }
        }
    }

    /// The timeouts.
    fn advance(&mut self, now: Duration) -> Option<Effect> {
        let since = |at: Duration, wait: Duration| now >= at.saturating_add(wait);
        match self.phase {
            Phase::Ended(_) => None,
            Phase::Granted => since(self.granted_at, GRANTED_LINGER).then(|| {
                self.phase = Phase::Ended(End::Granted);
                Effect::Close
            }),
            Phase::Drag | Phase::Confirming | Phase::AuthCancelled | Phase::OffPane => {
                let never_came = !self.settings_seen && since(self.opened_at, SETTINGS_WAIT);
                let went = self.settings_seen
                    && self
                        .missing_since
                        .is_some_and(|at| since(at, SETTINGS_GONE));
                if never_came || went {
                    self.phase = Phase::Ended(End::SettingsClosed);
                    return Some(Effect::Close);
                }
                if self.phase == Phase::Confirming && !self.prompt_visible {
                    match self.prompt_gone_at {
                        Some(at) if since(at, PROMPT_SETTLE) => self.phase = Phase::AuthCancelled,
                        None if !self.prompt_seen && since(self.confirm_since, PROMPT_WAIT) => {
                            self.phase = Phase::Drag;
                        }
                        _ => {}
                    }
                }
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use proptest::prelude::*;

    use super::*;

    const fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn open_view() -> Event {
        Event::Settings(SettingsView {
            open: true,
            ..SettingsView::default()
        })
    }

    fn prompt_view() -> Event {
        Event::Settings(SettingsView {
            open: true,
            prompt: true,
            title: None,
        })
    }

    fn titled(title: &str) -> Event {
        Event::Settings(SettingsView {
            open: true,
            prompt: false,
            title: Some(title.to_owned()),
        })
    }

    fn granted(pane: Pane) -> Event {
        Event::Grants(match pane {
            Pane::ScreenRecording => Grants {
                screen_recording: true,
                ..Grants::default()
            },
            Pane::Accessibility => Grants {
                accessibility: true,
                post_event: true,
                ..Grants::default()
            },
        })
    }

    #[test]
    fn drag_confirm_granted_then_closed() {
        let (mut guide, first) = Guide::open(Pane::ScreenRecording, ms(0));
        assert_eq!(first, Effect::OpenPane);
        assert_eq!(guide.step(open_view(), ms(300)), None);
        assert_eq!(guide.phase(), Phase::Drag);
        assert!(
            guide
                .look("Onicode Desktop")
                .is_some_and(|look| look.draggable)
        );
        guide.step(Event::DragEnded { dropped: true }, ms(2000));
        assert_eq!(guide.phase(), Phase::Confirming);
        guide.step(prompt_view(), ms(2500));
        guide.step(open_view(), ms(6000));
        guide.step(granted(Pane::ScreenRecording), ms(7000));
        assert_eq!(guide.phase(), Phase::Granted);
        assert_eq!(guide.step(Event::Tick, ms(8000)), None);
        assert_eq!(guide.step(Event::Tick, ms(8500)), Some(Effect::Close));
        assert_eq!(guide.phase(), Phase::Ended(End::Granted));
        assert_eq!(guide.look("Onicode Desktop"), None);
    }

    #[test]
    fn a_drop_without_a_prompt_goes_back_to_the_drag() {
        let (mut guide, _) = Guide::open(Pane::Accessibility, ms(0));
        guide.step(open_view(), ms(100));
        guide.step(Event::DragEnded { dropped: true }, ms(1000));
        guide.step(open_view(), ms(4999));
        assert_eq!(guide.phase(), Phase::Confirming);
        guide.step(open_view(), ms(5000));
        assert_eq!(guide.phase(), Phase::Drag);
    }

    #[test]
    fn a_cancelled_prompt_asks_for_the_drag_again() {
        let (mut guide, _) = Guide::open(Pane::ScreenRecording, ms(0));
        guide.step(open_view(), ms(100));
        guide.step(Event::DragEnded { dropped: true }, ms(1000));
        guide.step(prompt_view(), ms(1200));
        guide.step(open_view(), ms(9000));
        assert_eq!(guide.phase(), Phase::Confirming);
        guide.step(Event::Tick, ms(12_000));
        assert_eq!(guide.phase(), Phase::AuthCancelled);
        assert!(
            guide
                .look("Onicode Desktop")
                .is_some_and(|look| look.draggable)
        );
    }

    #[test]
    fn a_drag_that_missed_changes_nothing() {
        let (mut guide, _) = Guide::open(Pane::ScreenRecording, ms(0));
        guide.step(open_view(), ms(100));
        guide.step(Event::DragEnded { dropped: false }, ms(900));
        assert_eq!(guide.phase(), Phase::Drag);
    }

    #[test]
    fn settings_that_never_open_end_the_guide() {
        let (mut guide, _) = Guide::open(Pane::ScreenRecording, ms(0));
        assert_eq!(guide.step(Event::Tick, ms(9999)), None);
        assert_eq!(guide.step(Event::Tick, ms(10_000)), Some(Effect::Close));
        assert_eq!(guide.phase(), Phase::Ended(End::SettingsClosed));
    }

    #[test]
    fn settings_closed_ends_the_guide() {
        let (mut guide, _) = Guide::open(Pane::ScreenRecording, ms(0));
        guide.step(open_view(), ms(100));
        guide.step(Event::Settings(SettingsView::default()), ms(1000));
        assert_eq!(guide.step(Event::Tick, ms(3999)), None);
        // Back within the grace: still going.
        assert_eq!(guide.step(open_view(), ms(3999)), None);
        assert_eq!(
            guide.step(Event::Settings(SettingsView::default()), ms(5000)),
            None
        );
        assert_eq!(guide.step(Event::Tick, ms(8000)), Some(Effect::Close));
        assert_eq!(guide.phase(), Phase::Ended(End::SettingsClosed));
    }

    #[test]
    fn another_title_is_another_pane_and_go_back_reopens() {
        let (mut guide, _) = Guide::open(Pane::ScreenRecording, ms(0));
        guide.step(titled("Privacy"), ms(200));
        guide.step(titled("Screen Recording"), ms(1200));
        guide.step(titled("Wi-Fi"), ms(3000));
        assert_eq!(guide.phase(), Phase::OffPane);
        let look = guide.look("Onicode Desktop");
        assert_eq!(look.as_ref().map(|look| look.action), Some(Some("Go Back")));
        assert_eq!(guide.step(Event::Action, ms(4000)), Some(Effect::OpenPane));
        assert_eq!(guide.phase(), Phase::Drag);
        guide.step(titled("Screen Recording"), ms(5100));
        guide.step(titled("Screen Recording"), ms(6000));
        assert_eq!(guide.phase(), Phase::Drag);
    }

    #[test]
    fn coming_back_to_the_list_by_hand_resumes_the_drag() {
        let (mut guide, _) = Guide::open(Pane::Accessibility, ms(0));
        guide.step(titled("Accessibility"), ms(1500));
        guide.step(titled("General"), ms(2000));
        assert_eq!(guide.phase(), Phase::OffPane);
        guide.step(titled("Accessibility"), ms(2500));
        assert_eq!(guide.phase(), Phase::Drag);
    }

    #[test]
    fn a_navigation_click_counts_only_without_titles() {
        let (mut guide, _) = Guide::open(Pane::ScreenRecording, ms(0));
        guide.step(open_view(), ms(100));
        guide.step(Event::NavigationClicked, ms(2000));
        assert_eq!(guide.phase(), Phase::OffPane);

        let (mut titled_guide, _) = Guide::open(Pane::ScreenRecording, ms(0));
        titled_guide.step(titled("Screen Recording"), ms(1500));
        titled_guide.step(Event::NavigationClicked, ms(2000));
        assert_eq!(titled_guide.phase(), Phase::Drag);
    }

    #[test]
    fn dismissing_ends_at_once() {
        let (mut guide, _) = Guide::open(Pane::ScreenRecording, ms(0));
        assert_eq!(guide.step(Event::Dismiss, ms(50)), Some(Effect::Close));
        assert_eq!(guide.phase(), Phase::Ended(End::Dismissed));
        assert_eq!(guide.step(granted(Pane::ScreenRecording), ms(60)), None);
        assert_eq!(guide.phase(), Phase::Ended(End::Dismissed));
    }

    #[test]
    fn the_other_panes_grant_does_not_count() {
        let (mut guide, _) = Guide::open(Pane::ScreenRecording, ms(0));
        guide.step(granted(Pane::Accessibility), ms(100));
        assert_eq!(guide.phase(), Phase::Drag);
    }

    fn pane() -> impl Strategy<Value = Pane> {
        prop_oneof![Just(Pane::ScreenRecording), Just(Pane::Accessibility)]
    }

    fn event() -> impl Strategy<Value = Event> {
        let grants = (any::<bool>(), any::<bool>(), any::<bool>()).prop_map(|(s, a, p)| {
            Event::Grants(Grants {
                screen_recording: s,
                accessibility: a,
                post_event: p,
            })
        });
        let title = prop_oneof![
            Just(None),
            Just(Some("Screen Recording".to_owned())),
            Just(Some("Accessibility".to_owned())),
            Just(Some("Wi-Fi".to_owned())),
        ];
        let view = (any::<bool>(), any::<bool>(), title).prop_map(|(open, prompt, title)| {
            Event::Settings(SettingsView {
                open,
                prompt,
                title,
            })
        });
        prop_oneof![
            1 => grants,
            1 => any::<bool>().prop_map(|dropped| Event::DragEnded { dropped }),
            4 => view,
            1 => Just(Event::NavigationClicked),
            1 => Just(Event::Action),
            1 => Just(Event::Dismiss),
            3 => Just(Event::Tick),
        ]
    }

    fn run() -> impl Strategy<Value = (Pane, Vec<(Event, u64)>)> {
        (pane(), prop::collection::vec((event(), 0_u64..4000), 0..60))
    }

    proptest! {
        #[test]
        fn an_ended_guide_stays_ended_and_closes_once((pane, events) in run()) {
            let (mut guide, _) = Guide::open(pane, Duration::ZERO);
            let mut now = Duration::ZERO;
            let mut closes = 0;
            let mut ended: Option<Phase> = None;
            for (event, step) in events {
                now += ms(step);
                let effect = guide.step(event, now);
                if effect == Some(Effect::Close) {
                    closes += 1;
                    prop_assert!(guide.phase().is_ended());
                }
                if let Some(phase) = ended {
                    prop_assert_eq!(guide.phase(), phase);
                    prop_assert_eq!(effect, None);
                }
                if guide.phase().is_ended() {
                    ended.get_or_insert(guide.phase());
                }
            }
            prop_assert!(closes <= 1);
            prop_assert_eq!(closes == 1, guide.phase().is_ended());
        }

        #[test]
        fn granted_only_on_desktopds_word((pane, events) in run()) {
            let (mut guide, _) = Guide::open(pane, Duration::ZERO);
            let mut now = Duration::ZERO;
            let mut heard = false;
            for (event, step) in events {
                now += ms(step);
                let grants_it = matches!(&event, Event::Grants(grants) if grants.has(pane));
                let before = guide.phase();
                guide.step(event, now);
                heard |= grants_it;
                if grants_it && !before.is_ended() {
                    prop_assert!(matches!(
                        guide.phase(),
                        Phase::Granted | Phase::Ended(End::Granted)
                    ));
                }
                if matches!(guide.phase(), Phase::Granted | Phase::Ended(End::Granted)) {
                    prop_assert!(heard);
                }
            }
        }

        #[test]
        fn the_look_follows_the_phase((pane, events) in run()) {
            let (mut guide, _) = Guide::open(pane, Duration::ZERO);
            let mut now = Duration::ZERO;
            for (event, step) in events {
                now += ms(step);
                guide.step(event, now);
                let look = guide.look("Onicode Desktop");
                prop_assert_eq!(look.is_none(), guide.phase().is_ended());
                if let Some(look) = look {
                    prop_assert_eq!(look.draggable, guide.phase().draggable());
                    prop_assert_eq!(look.action.is_some(), guide.phase() == Phase::OffPane);
                }
            }
        }

        #[test]
        fn opening_the_pane_comes_only_from_go_back((pane, events) in run()) {
            let (mut guide, _) = Guide::open(pane, Duration::ZERO);
            let mut now = Duration::ZERO;
            for (event, step) in events {
                now += ms(step);
                let before = guide.phase();
                let action = event == Event::Action;
                if guide.step(event, now) == Some(Effect::OpenPane) {
                    prop_assert!(action && before == Phase::OffPane);
                    prop_assert_eq!(guide.phase(), Phase::Drag);
                }
            }
        }
    }
}
