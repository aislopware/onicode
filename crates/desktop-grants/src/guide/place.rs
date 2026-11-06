//! Where the panel goes and which of System Settings' windows it follows, from window bounds
//! alone: `CGWindowListCopyWindowInfo` gives bounds without Screen Recording, titles only with it.
//!
//! Two coordinate spaces meet here. CoreGraphics measures from the primary display's top-left
//! corner, y down ([`WindowInfo::bounds`]); AppKit from its bottom-left corner, y up (every other
//! [`Rect`]). [`cocoa_from_cg`] flips with the primary display's height, never the window's own
//! screen's, which would misplace the panel on a second display.

use std::f64::consts::TAU;

/// System Settings.
pub const SETTINGS_BUNDLE_ID: &str = "com.apple.systempreferences";

/// The agents that own the Touch ID and password prompts adding an app to a privacy list raises.
/// The panel stays up over them to say what to do.
pub const AUTH_AGENTS: [&str; 2] = [
    "com.apple.SecurityAgent",
    "com.apple.LocalAuthentication.UIAgent",
];

/// The panel's size in points.
pub const PANEL_WIDTH: f64 = 380.0;
/// The panel's height in points.
pub const PANEL_HEIGHT: f64 = 64.0;
/// The gap between the panel and the window's bottom edge, inside or below it.
pub const INSET: f64 = 20.0;
/// Below this height the window has no spare strip at its bottom, so the panel goes under it.
pub const INSIDE_MIN_HEIGHT: f64 = 480.0;
/// The gap between the window and a panel below it.
pub const GAP: f64 = 12.0;

/// A rectangle in points.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Rect {
    /// Left edge.
    pub x: f64,
    /// Bottom edge in AppKit's space, top edge in CoreGraphics'.
    pub y: f64,
    /// Width.
    pub w: f64,
    /// Height.
    pub h: f64,
}

impl Rect {
    /// A rectangle.
    #[must_use]
    pub const fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }

    /// Right edge.
    #[must_use]
    pub fn max_x(self) -> f64 {
        self.x + self.w
    }

    /// Top edge in AppKit's space.
    #[must_use]
    pub fn max_y(self) -> f64 {
        self.y + self.h
    }

    /// Horizontal centre.
    #[must_use]
    pub fn mid_x(self) -> f64 {
        self.x + self.w / 2.0
    }

    /// Vertical centre.
    #[must_use]
    pub fn mid_y(self) -> f64 {
        self.y + self.h / 2.0
    }

    /// Whether `(x, y)` lies inside, edges included.
    #[must_use]
    pub fn contains(self, x: f64, y: f64) -> bool {
        x >= self.x && x <= self.max_x() && y >= self.y && y <= self.max_y()
    }

    /// The area shared with `other`.
    #[must_use]
    pub fn overlap(self, other: Self) -> f64 {
        let w = self.max_x().min(other.max_x()) - self.x.max(other.x);
        let h = self.max_y().min(other.max_y()) - self.y.max(other.y);
        w.max(0.0) * h.max(0.0)
    }
}

/// One of System Settings' on-screen windows.
#[derive(Clone, PartialEq, Debug)]
pub struct WindowInfo {
    /// Its window number.
    pub number: u32,
    /// Its bounds in CoreGraphics' space.
    pub bounds: Rect,
    /// Its title, readable only with Screen Recording.
    pub title: Option<String>,
}

/// System Settings' sidebar width on a macOS major version, measured from its accessibility tree;
/// `0` for a version not measured, which centres the panel on the whole window rather than guess.
#[must_use]
pub const fn sidebar_width(macos_major: i64) -> f64 {
    match macos_major {
        27 => 232.0,
        _ => 0.0,
    }
}

/// `bounds` in CoreGraphics' space as AppKit's, for a primary display `primary_height` tall.
#[must_use]
pub fn cocoa_from_cg(bounds: Rect, primary_height: f64) -> Rect {
    Rect::new(
        bounds.x,
        primary_height - bounds.max_y(),
        bounds.w,
        bounds.h,
    )
}

/// The window the panel follows: the one it followed before while it is still there, else the
/// largest big enough to be a main window (search popovers and sheets are smaller).
#[must_use]
pub fn pick_main(windows: &[WindowInfo], remembered: Option<u32>) -> Option<&WindowInfo> {
    if let Some(window) = remembered.and_then(|n| windows.iter().find(|w| w.number == n)) {
        return Some(window);
    }
    windows
        .iter()
        .filter(|w| w.bounds.w >= 300.0 && w.bounds.h >= 200.0)
        .max_by(|a, b| (a.bounds.w * a.bounds.h).total_cmp(&(b.bounds.w * b.bounds.h)))
}

/// Whether `window` is a prompt over `main`: the Touch ID or password sheet and the file chooser
/// open as separate windows centred on the main one.
#[must_use]
pub fn is_prompt(window: &WindowInfo, main: &WindowInfo) -> bool {
    window.number != main.number
        && window.bounds.w >= 200.0
        && window.bounds.h >= 150.0
        && (window.bounds.mid_x() - main.bounds.mid_x()).abs() <= 24.0
}

/// Whether `bundle_id` is an authentication agent.
#[must_use]
pub fn is_auth_agent(bundle_id: Option<&str>) -> bool {
    bundle_id.is_some_and(|id| AUTH_AGENTS.contains(&id))
}

/// Whether the panel may show while the app `bundle_id` is in front: System Settings or one of
/// its authentication agents.
#[must_use]
pub fn shows_alongside(bundle_id: Option<&str>) -> bool {
    bundle_id == Some(SETTINGS_BUNDLE_ID) || is_auth_agent(bundle_id)
}

/// The screen a window is on: the one holding its centre, else the one it overlaps most.
#[must_use]
pub fn screen_for(window: Rect, screens: &[Rect]) -> Option<usize> {
    screens
        .iter()
        .position(|screen| screen.contains(window.mid_x(), window.mid_y()))
        .or_else(|| {
            screens
                .iter()
                .enumerate()
                .filter(|(_, screen)| screen.overlap(window) > 0.0)
                .max_by(|(_, a), (_, b)| a.overlap(window).total_cmp(&b.overlap(window)))
                .map(|(index, _)| index)
        })
}

/// The panel's frame for System Settings' window at `window`, on a screen whose usable area is
/// `visible`, both in AppKit's space.
///
/// Across, it is centred on the content beside the sidebar when that is wide enough, else on the
/// whole window. Up and down, it sits inside the window's bottom strip when the window is tall
/// enough to spare it, else just below the window, else inside after all. It is then clamped to
/// `visible`, and lands on whole points.
#[must_use]
pub fn panel_frame(window: Rect, visible: Rect, sidebar: f64) -> Rect {
    let content = window.w - sidebar;
    let centre = if sidebar > 0.0 && content >= PANEL_WIDTH + 32.0 {
        window.x + sidebar + content / 2.0
    } else {
        window.mid_x()
    };
    let inside = window.y + INSET;
    let below = window.y - GAP - PANEL_HEIGHT;
    let y = if window.h >= INSIDE_MIN_HEIGHT || below < visible.y {
        inside
    } else {
        below
    };
    let x = clamp(
        centre - PANEL_WIDTH / 2.0,
        visible.x,
        visible.max_x() - PANEL_WIDTH,
    );
    let y = clamp(y, visible.y, visible.max_y() - PANEL_HEIGHT);
    Rect::new(x.round(), y.round(), PANEL_WIDTH, PANEL_HEIGHT)
}

/// `value` within `[low, high]`; `low` when the range is empty (a screen narrower than the
/// panel keeps its left edge on screen).
fn clamp(value: f64, low: f64, high: f64) -> f64 {
    if high < low {
        low
    } else {
        value.clamp(low, high)
    }
}

/// Whether a click at `(x, y)` (AppKit's space) on System Settings' window at `window` hit its
/// sidebar or its back and forward buttons, which leave the list.
#[must_use]
pub fn is_navigation_click(window: Rect, x: f64, y: f64, sidebar: f64) -> bool {
    if !window.contains(x, y) {
        return false;
    }
    let sidebar = if sidebar > 0.0 { sidebar } else { 220.0 };
    let from_top = window.max_y() - y;
    let from_left = x - window.x;
    let in_sidebar = from_left <= sidebar && from_top > 40.0;
    let in_buttons = from_top <= 52.0 && from_left > sidebar && from_left <= sidebar + 90.0;
    in_sidebar || in_buttons
}

/// How far the draggable icon floats up at `seconds` since the panel appeared: a slow bob that
/// says "pick me up", still under Reduce Motion.
#[must_use]
pub fn icon_lift(seconds: f64, reduce_motion: bool) -> f64 {
    if reduce_motion {
        return 0.0;
    }
    2.5 * (1.0 - (TAU * seconds / 1.2).cos()) / 2.0
}

/// The panel's opacity at `seconds` since it appeared: a short fade in, none under Reduce Motion.
#[must_use]
pub fn fade_alpha(seconds: f64, reduce_motion: bool) -> f64 {
    if reduce_motion {
        return 1.0;
    }
    (seconds / 0.18).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use proptest::prelude::*;

    use super::*;

    const SCREEN: Rect = Rect::new(0.0, 0.0, 1512.0, 944.0);

    fn window(number: u32, w: f64, h: f64) -> WindowInfo {
        WindowInfo {
            number,
            bounds: Rect::new(100.0, 100.0, w, h),
            title: None,
        }
    }

    #[test]
    fn the_y_axis_flips_on_the_primary_display() {
        let cg = Rect::new(10.0, 50.0, 700.0, 600.0);
        assert_eq!(
            cocoa_from_cg(cg, 1000.0),
            Rect::new(10.0, 350.0, 700.0, 600.0)
        );
        // A display above the primary one has negative CoreGraphics y.
        let above = Rect::new(0.0, -800.0, 700.0, 600.0);
        assert_eq!(cocoa_from_cg(above, 1000.0).y, 1200.0);
    }

    #[test]
    fn the_panel_sits_in_the_content_beside_the_sidebar() {
        let settings = Rect::new(200.0, 100.0, 840.0, 700.0);
        let frame = panel_frame(settings, SCREEN, 232.0);
        // Content spans 432..1040, centred at 736.
        assert_eq!(frame, Rect::new(546.0, 120.0, PANEL_WIDTH, PANEL_HEIGHT));
    }

    #[test]
    fn a_narrow_content_centres_on_the_window() {
        let settings = Rect::new(200.0, 100.0, 600.0, 700.0);
        assert_eq!(
            panel_frame(settings, SCREEN, 232.0).mid_x(),
            settings.mid_x()
        );
        assert_eq!(panel_frame(settings, SCREEN, 0.0).mid_x(), settings.mid_x());
    }

    #[test]
    fn a_short_window_takes_the_panel_below_unless_the_screen_ends() {
        let short = Rect::new(200.0, 300.0, 700.0, 400.0);
        assert_eq!(
            panel_frame(short, SCREEN, 0.0).y,
            300.0 - GAP - PANEL_HEIGHT
        );
        let low = Rect::new(200.0, 30.0, 700.0, 400.0);
        assert_eq!(panel_frame(low, SCREEN, 0.0).y, 30.0 + INSET);
    }

    #[test]
    fn the_main_window_is_remembered_then_the_largest() {
        let windows = vec![
            window(1, 400.0, 300.0),
            window(2, 800.0, 600.0),
            window(3, 250.0, 900.0),
        ];
        assert_eq!(pick_main(&windows, None).map(|w| w.number), Some(2));
        assert_eq!(pick_main(&windows, Some(1)).map(|w| w.number), Some(1));
        assert_eq!(pick_main(&windows, Some(9)).map(|w| w.number), Some(2));
        assert_eq!(pick_main(&[window(4, 200.0, 100.0)], None), None);
    }

    #[test]
    fn a_centred_sheet_is_a_prompt() {
        let main = WindowInfo {
            number: 1,
            bounds: Rect::new(100.0, 100.0, 800.0, 600.0),
            title: None,
        };
        let sheet = WindowInfo {
            number: 2,
            bounds: Rect::new(340.0, 200.0, 320.0, 260.0),
            title: None,
        };
        let popover = WindowInfo {
            number: 3,
            bounds: Rect::new(110.0, 120.0, 240.0, 120.0),
            title: None,
        };
        assert!(is_prompt(&sheet, &main));
        assert!(!is_prompt(&popover, &main));
        assert!(!is_prompt(&main, &main));
    }

    #[test]
    fn the_panel_shows_beside_settings_and_its_prompts_only() {
        assert!(shows_alongside(Some("com.apple.systempreferences")));
        assert!(shows_alongside(Some("com.apple.SecurityAgent")));
        assert!(shows_alongside(Some(
            "com.apple.LocalAuthentication.UIAgent"
        )));
        assert!(!shows_alongside(Some("dev.aislopware.onicode")));
        assert!(!shows_alongside(None));
    }

    #[test]
    fn only_macos_27_has_a_measured_sidebar() {
        assert_eq!(sidebar_width(27), 232.0);
        assert_eq!(sidebar_width(28), 0.0);
    }

    #[test]
    fn sidebar_and_back_button_clicks_leave_the_list() {
        let settings = Rect::new(0.0, 0.0, 800.0, 600.0);
        assert!(is_navigation_click(settings, 100.0, 300.0, 232.0));
        assert!(is_navigation_click(settings, 260.0, 580.0, 232.0));
        assert!(!is_navigation_click(settings, 500.0, 300.0, 232.0));
        assert!(!is_navigation_click(settings, 100.0, 590.0, 232.0));
        assert!(!is_navigation_click(settings, 900.0, 300.0, 232.0));
    }

    #[test]
    fn reduce_motion_stills_the_panel() {
        assert_eq!(icon_lift(0.3, true), 0.0);
        assert_eq!(fade_alpha(0.0, true), 1.0);
        assert_eq!(icon_lift(0.0, false), 0.0);
        assert!((icon_lift(0.6, false) - 2.5).abs() < 1e-9);
        assert_eq!(fade_alpha(0.0, false), 0.0);
        assert_eq!(fade_alpha(1.0, false), 1.0);
    }

    #[test]
    fn a_window_finds_its_screen() {
        let screens = [SCREEN, Rect::new(1512.0, 0.0, 1920.0, 1080.0)];
        assert_eq!(
            screen_for(Rect::new(1600.0, 100.0, 800.0, 600.0), &screens),
            Some(1)
        );
        assert_eq!(
            screen_for(Rect::new(1200.0, 100.0, 800.0, 600.0), &screens),
            Some(1)
        );
        assert_eq!(
            screen_for(Rect::new(1000.0, 100.0, 800.0, 600.0), &screens),
            Some(0)
        );
        assert_eq!(
            screen_for(Rect::new(9000.0, 100.0, 800.0, 600.0), &screens),
            None
        );
    }

    fn rect(max_side: f64) -> impl Strategy<Value = Rect> {
        (
            -3000.0..3000.0_f64,
            -3000.0..3000.0_f64,
            1.0..max_side,
            1.0..max_side,
        )
            .prop_map(|(x, y, w, h)| Rect::new(x, y, w, h))
    }

    proptest! {
        #[test]
        fn the_panel_stays_on_screen(window in rect(3000.0), visible in rect(4000.0), sidebar in 0.0..400.0_f64) {
            let frame = panel_frame(window, visible, sidebar);
            prop_assert_eq!((frame.w, frame.h), (PANEL_WIDTH, PANEL_HEIGHT));
            if visible.w >= PANEL_WIDTH + 1.0 && visible.h >= PANEL_HEIGHT + 1.0 {
                // Rounding moves an edge by at most half a point.
                prop_assert!(frame.x >= visible.x - 0.5 && frame.max_x() <= visible.max_x() + 0.5);
                prop_assert!(frame.y >= visible.y - 0.5 && frame.max_y() <= visible.max_y() + 0.5);
            } else {
                prop_assert!(frame.x >= visible.x - 0.5);
            }
        }

        #[test]
        fn a_tall_window_on_screen_holds_the_panel(
            x in 0.0..600.0_f64,
            y in 0.0..200.0_f64,
            w in PANEL_WIDTH..900.0,
            h in INSIDE_MIN_HEIGHT..740.0,
            sidebar in prop_oneof![Just(0.0), Just(232.0)],
        ) {
            let window = Rect::new(x, y, w, h);
            let frame = panel_frame(window, SCREEN, sidebar);
            prop_assert!(frame.x >= window.x - 0.5 && frame.max_x() <= window.max_x() + 0.5);
            prop_assert!(frame.y >= window.y && frame.max_y() <= window.max_y());
        }

        #[test]
        fn flipping_twice_is_identity(bounds in rect(3000.0), primary in 600.0..3000.0_f64) {
            let back = cocoa_from_cg(cocoa_from_cg(bounds, primary), primary);
            prop_assert!((back.y - bounds.y).abs() < 1e-6);
            prop_assert_eq!((back.x, back.w, back.h), (bounds.x, bounds.w, bounds.h));
        }

        #[test]
        fn the_bob_stays_small(seconds in 0.0..1e4_f64) {
            let lift = icon_lift(seconds, false);
            prop_assert!((0.0..=2.5 + 1e-9).contains(&lift));
            prop_assert!((0.0..=1.0).contains(&fade_alpha(seconds, false)));
        }
    }
}
