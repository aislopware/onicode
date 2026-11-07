//! monocode's primitives on gpui-kit's components (docs/onicode/decisions/rendering.md): each
//! primitive keeps monocode's metrics and colours exactly and gains gpui-kit's behaviour (focus,
//! keyboard press, disabled state, accessibility). The golden images hold every move to pixel
//! parity.
use gpui::prelude::*;
use gpui::{ElementId, px};
pub(crate) use gpui_component::Disableable;
pub(crate) use gpui_component::button::{Button, ButtonCustomVariant as Colors};
use gpui_component::button::{ButtonRounded, ButtonVariants as _};
use gpui_component::switch::Switch;
use gpui_component::{Sizable as _, Size};

/// A button painted in exactly `colors` with `radius`-point corners: the base of every monocode
/// button. Its metrics, text and children are the caller's.
pub(crate) fn button(id: impl Into<ElementId>, colors: Colors, radius: f32) -> Button {
    Button::new(id)
        .custom(colors)
        .rounded(ButtonRounded::Size(px(radius)))
}

/// monocode's square icon button: `side` points, `radius`-point corners, `colors` at rest and under
/// the pointer, and the icon it holds as its child.
pub(crate) fn icon_button(
    id: impl Into<ElementId>,
    label: impl Into<gpui::SharedString>,
    colors: Colors,
    side: f32,
    radius: f32,
) -> Button {
    button(id, colors, radius)
        .accessibility_label(label)
        .size(px(side))
        .p_0()
}

/// monocode's toggle: a 36 by 20 pt pill (28 by 16 when `small`) in the accent colour while on and
/// a 20% wash while off, with a white thumb 2 pt in, the whole control at 40% while disabled.
fn monocode_switch(id: impl Into<ElementId>, on: bool, disabled: bool) -> Switch {
    Switch::new(id)
        .checked(on)
        .color(crate::theme::accent_color())
        .off_color(crate::theme::content(0.2))
        .thumb_color(gpui::white())
        .disabled(disabled)
        .disabled_opacity(0.4)
        .when(!disabled, |switch| switch.cursor_pointer())
}

/// [`monocode_switch`] at the settings pages' size.
pub(crate) fn switch(id: impl Into<ElementId>, on: bool, disabled: bool) -> Switch {
    monocode_switch(id, on, disabled)
}

/// [`monocode_switch`] at the Automations list's smaller size.
pub(crate) fn small_switch(id: impl Into<ElementId>, on: bool) -> Switch {
    monocode_switch(id, on, false).with_size(Size::Small)
}
