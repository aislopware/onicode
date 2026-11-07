//! What the shell shows follows what changed though frames draw its views
//! again from the last one (docs/onicode/decisions/rendering.md, "Retained
//! mode"). Each window here checks every frame that drew anything again
//! against the same frame drawn from scratch, and keeps what differed.

use gpui::{AppContext as _, RetainedCheck, TestAppContext, WindowAppearance, WindowHandle};

use super::Shell;
use super::tests::window;

/// The test shell, checked, after the frames that settle its first layout.
fn checked(cx: &mut TestAppContext) -> (WindowHandle<Shell>, i64, i64) {
    let (window, a, b) = window(cx);
    cx.update_window(window.into(), |_, window, _| {
        window.set_check_retained(Some(RetainedCheck::Record));
    })
    .unwrap();
    frame(cx, window);
    frame(cx, window);
    (window, a, b)
}

/// One platform frame: the next-frame callbacks, then the draw.
fn frame(cx: &mut TestAppContext, window: WindowHandle<Shell>) {
    cx.update_window(window.into(), |_, window, cx| {
        window.simulate_next_frame(cx);
        window.draw(cx).clear(cx);
    })
    .unwrap();
}

fn reports(cx: &mut TestAppContext, window: WindowHandle<Shell>) -> Vec<String> {
    cx.update_window(window.into(), |_, window, _| {
        window.take_retained_check_reports()
    })
    .unwrap()
}

/// The colors live outside any entity, so a theme painted as the shell
/// renders would reach the views built after it and not those drawn again
/// beside them. Synced as the shell is notified, the first frame after the
/// switch is drawn whole in the new colors.
#[gpui::test]
fn a_theme_switch_reaches_every_view_on_its_first_frame(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_reduce_motion(true));
    let (window, _, _) = checked(cx);
    let light = crate::theme::is_light();
    window
        .update(cx, |shell, _, cx| {
            shell.workspace.preferences.theme = if light { "dark" } else { "light" }.into();
            cx.notify();
        })
        .unwrap();
    frame(cx, window);
    frame(cx, window);
    assert_eq!(crate::theme::is_light(), !light);
    assert_eq!(reports(cx, window), Vec::<String>::new());
}

/// The system turning dark tells no view through anything it reads: the
/// shell observes it, so a theme following the system turns with it.
#[gpui::test]
fn the_system_theme_follows_the_system_turning_dark_and_light(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_reduce_motion(true));
    let (window, _, _) = checked(cx);
    window
        .update(cx, |shell, _, cx| {
            shell.workspace.preferences.theme = "system".into();
            cx.notify();
        })
        .unwrap();
    frame(cx, window);
    for appearance in [WindowAppearance::Dark, WindowAppearance::Light] {
        cx.simulate_system_appearance(appearance);
        cx.run_until_parked();
        frame(cx, window);
        frame(cx, window);
        assert_eq!(
            crate::theme::is_light(),
            appearance == WindowAppearance::Light
        );
    }
    assert_eq!(reports(cx, window), Vec::<String>::new());
}

/// Notices are reconciled as the shell renders, and a chat notifies only
/// itself: a turn it begins reaches them on the next frame, though nothing
/// else notifies the shell.
#[gpui::test]
fn a_turn_begun_reaches_the_shells_notices(cx: &mut TestAppContext) {
    let (window, _, id) = checked(cx);
    let chat = window
        .update(cx, |shell, window, cx| {
            shell.open(id, window, cx);
            shell.sessions[&id].clone()
        })
        .unwrap();
    frame(cx, window);
    let responding = |cx: &mut TestAppContext| {
        window
            .update(cx, |shell, _, _| shell.responding_notices().contains(&id))
            .unwrap()
    };
    assert!(!responding(cx));
    chat.update(cx, |chat, cx| {
        chat.busy = true;
        cx.notify();
    });
    frame(cx, window);
    assert!(responding(cx), "the shell's frame saw the turn begin");
}
