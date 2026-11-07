use gpui::prelude::*;
use gpui::{SharedString, px};
use onicode_protocol::agent::Settings;
use onicode_protocol::response::{Target, TransferKind};
use onicode_protocol::transcript::Row;

use crate::popover::AnchorSlot;
use crate::theme::*;

pub(crate) enum Request {
    QuoteSelection {
        text: String,
    },
    EditLastTurn {
        turn: usize,
    },
    Draft {
        send: bool,
    },
    SaveNote {
        turn: usize,
        text: String,
        selection: bool,
    },
    Target {
        turn: usize,
        kind: TransferKind,
        anchor: AnchorSlot,
    },
}
#[derive(Clone)]
pub(crate) struct Transfer {
    pub kind: TransferKind,
    pub target: Target,
    pub from: String,
    pub turn: Vec<Row>,
    pub through: Vec<Row>,
    pub cwd: String,
    pub title: String,
    pub settings: Settings,
}
pub(crate) fn button(
    turn: usize,
    action: &'static str,
    label: impl Into<SharedString>,
    glyph: &'static str,
    active: bool,
) -> crate::kit::Button {
    let rest = if active {
        content(0.08)
    } else {
        gpui::transparent_black()
    };
    let colors = crate::kit::Colors::colors(rest, content(if active { 0.7 } else { 0.4 }))
        .hover(content(0.08))
        .hover_foreground(content(0.7));
    crate::kit::icon_button((action, turn), label, colors, 22., 6.)
        .group("response-action")
        .p(px(4.))
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .child(
            icon(glyph, 14.)
                .text_color(content(if active { 0.7 } else { 0.4 }))
                .group_hover("response-action", |s| s.text_color(content(0.7))),
        )
}
