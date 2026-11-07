//! ConnectionsSettings.tsx's machine list, read from the engine's `WatchMachines`: this machine
//! and every engine and viewer on the tailnet, with how each link travels. "Add machine" opens a
//! card that says how a Mac joins: there is nothing to type, since admission is tailnet whois.
//! The directory has no lasting forget (a machine leaves the list when it leaves the tailnet),
//! so a row has no remove button.
use gpui::prelude::*;
use gpui::{AnyElement, Context, Div, FontWeight, SharedString, Task, div, px};
use onicode_protocol::engine::{
    LinkPath, Machine, MachineKind, MachineLink, Machines, WatchMachines,
};

use crate::engine_client::Watching;
use crate::settings::widgets::secondary_button;
use crate::settings::{Section, SettingsView};
use crate::theme::*;

#[derive(Default)]
pub(in crate::settings) struct ConnectionsState {
    /// The machines as last seen; `None` until the engine first answers.
    machines: Option<Machines>,
    /// The add-machine card is open.
    adding: bool,
    watch: Option<(
        Watching<Machines, onicode_protocol::engine::MachineEvent>,
        Task<()>,
    )>,
}

fn paragraph(text: impl Into<SharedString>) -> Div {
    div()
        .text_size(px(12.))
        .line_height(px(19.5))
        .text_color(content(0.45))
        .child(text.into())
}

/// How the link to `machine` travels, its round trip and its build.
fn detail(machine: &Machine) -> String {
    let mut parts = Vec::new();
    match (&machine.kind, &machine.path) {
        (MachineKind::This, _) => parts.push("This machine".to_owned()),
        (_, Some(LinkPath::Direct(address))) => parts.push(format!("Direct · {address}")),
        (_, Some(LinkPath::PeerRelay(relay))) => parts.push(format!("Peer relay · {relay}")),
        (_, Some(LinkPath::Derp(region))) => parts.push(format!("DERP · {region}")),
        (_, Some(LinkPath::Idle) | None) => parts.push("Tailnet".to_owned()),
    }
    if let Some(rtt) = machine.rtt_us {
        parts.push(format!("{} ms", rtt.div_ceil(1000)));
    }
    if !machine.version.is_empty() {
        parts.push(machine.version.clone());
    }
    parts.join(" · ")
}

/// Where the link to `machine` stands, and what it runs.
fn status(machine: &Machine) -> String {
    let mut status = match &machine.link {
        MachineLink::Here | MachineLink::Live => "Connected".to_owned(),
        MachineLink::Dialing => "Checking connection…".to_owned(),
        MachineLink::Found => "On your tailnet".to_owned(),
        MachineLink::Offline { reason, .. } => format!("Offline · {reason}"),
    };
    if !matches!(machine.kind, MachineKind::This)
        && machine
            .protocol
            .is_some_and(|protocol| protocol != onicode_protocol::PROTOCOL)
    {
        status.push_str(" · needs update");
    }
    match machine.live_agents {
        Some(1) => status.push_str(" · 1 agent running"),
        Some(agents) if agents > 1 => status.push_str(&format!(" · {agents} agents running")),
        _ => {}
    }
    status
}

impl SettingsView {
    /// Follows the engine's machines while the page shows.
    pub(in crate::settings) fn watch_connections(&mut self, cx: &mut Context<Self>) {
        if self.section != Section::Connections || self.connections.watch.is_some() {
            return;
        }
        let engine = crate::engine_client::local(cx);
        self.connections.machines = engine.machines_now();
        let watching = engine.watch::<WatchMachines>(|since| WatchMachines { since });
        let updates = watching.updates.clone();
        let task = cx.spawn(async move |this, cx| {
            while let Ok(update) = updates.recv().await {
                let applied = this.update(cx, |this, cx| {
                    match update {
                        crate::engine_client::WatchUpdate::Snapshot(machines) => {
                            this.connections.machines = Some(*machines);
                        }
                        crate::engine_client::WatchUpdate::Events(events) => {
                            if let Some(machines) = this.connections.machines.as_mut() {
                                for event in &events {
                                    machines.apply(event);
                                }
                            }
                        }
                    }
                    cx.notify();
                });
                if applied.is_err() {
                    break;
                }
            }
        });
        self.connections.watch = Some((watching, task));
    }

    pub(in crate::settings) fn leave_connections(&mut self) {
        self.connections.watch = None;
        self.connections.adding = false;
    }

    /// Opens the add-machine card: the sidebar's Machines `+` and the folder modal's
    /// "Add a machine" land here.
    pub(crate) fn add_machine(&mut self, cx: &mut Context<Self>) {
        self.connections.adding = true;
        cx.notify();
    }

    pub(super) fn connections_page(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut page = vec![
            div()
                .relative()
                .child(self.reveal_probe("remote-machines"))
                .flex()
                .items_end()
                .justify_between()
                .gap_4()
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .child(
                            div()
                                .text_size(px(13.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("Your machines"),
                        )
                        .child(paragraph("Run agents on another computer and return to them from your laptop. The host keeps working when you close Onicode here.").mt_1()),
                )
                .when(!self.connections.adding, |d| {
                    d.child(
                        secondary_button("add-machine", "Add machine", false, false)
                            .child(icon("plus", 16.))
                            .child("Add machine")
                            .on_click(cx.listener(|this, _, _, cx| this.add_machine(cx))),
                    )
                })
                .into_any_element(),
        ];
        let machines = self
            .connections
            .machines
            .as_ref()
            .map(|machines| machines.machines.as_slice())
            .unwrap_or_default();
        if machines.is_empty() {
            page.push(
                div()
                    .mt_5()
                    .rounded_xl()
                    .border_1()
                    .border_color(content(0.15))
                    .px_5()
                    .py_8()
                    .text_center()
                    .text_size(px(13.))
                    .text_color(content(0.45))
                    .child("Checking connection…")
                    .into_any_element(),
            );
            page.extend(self.add_machine_card(cx));
            return page;
        }
        let mut list = div()
            .mt_5()
            .rounded_xl()
            .border_1()
            .border_color(content(0.1))
            .overflow_hidden();
        for machine in machines {
            list = list.child(
                div().border_b_1().border_color(content(0.1)).child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .p_4()
                        .child(icon("globe", 20.).text_color(content(0.45)))
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(px(13.))
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(machine.name.clone()),
                                )
                                .child(paragraph(detail(machine)).mt_1())
                                .child(paragraph(status(machine)).mt_1().text_color(content(0.5))),
                        ),
                ),
            );
        }
        page.push(list.into_any_element());
        let others = machines
            .iter()
            .any(|machine| !matches!(machine.kind, MachineKind::This));
        if !others && !self.connections.adding {
            page.push(
                div()
                    .mt_5()
                    .rounded_xl()
                    .border_1()
                    .border_color(content(0.15))
                    .px_5()
                    .py_8()
                    .text_center()
                    .text_size(px(13.))
                    .text_color(content(0.45))
                    .child("Macs on your tailnet that run Onicode appear here.")
                    .into_any_element(),
            );
        }
        page.extend(self.add_machine_card(cx));
        page
    }

    /// The add-machine card, in the page's bordered idiom, while it is open.
    fn add_machine_card(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.connections.adding {
            return None;
        }
        Some(
            div()
                .mt_5()
                .p_5()
                .rounded_xl()
                .border_1()
                .border_color(content(0.1))
                .flex()
                .flex_col()
                .gap_4()
                .child(
                    div()
                        .flex()
                        .justify_between()
                        .child(
                            div()
                                .text_size(px(14.))
                                .font_weight(FontWeight::MEDIUM)
                                .child("Add a machine"),
                        )
                        .child(
                            div()
                                .rounded_sm()
                                .bg(content(0.1))
                                .px_2()
                                .py_1()
                                .text_size(px(11.))
                                .text_color(content(0.6))
                                .child("Tailscale"),
                        ),
                )
                .child(paragraph(
                    "Install Onicode on the machine and sign in to the same tailnet; it appears here.",
                ))
                .child(
                    div().flex().justify_end().gap_2().child(
                        secondary_button("cancel-machine", "Done", false, false)
                            .child("Done")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.connections.adding = false;
                                cx.notify();
                            })),
                    ),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use onicode_protocol::EngineId;

    use super::*;

    fn machine(kind: MachineKind, link: MachineLink) -> Machine {
        Machine {
            engine: EngineId("n1".into()),
            name: "studio".into(),
            kind,
            link,
            path: None,
            rtt_us: None,
            version: "0.1.0".into(),
            protocol: Some(onicode_protocol::PROTOCOL),
            owner: true,
            live_agents: None,
        }
    }

    #[test]
    fn this_machine_reads_as_itself() {
        let this = machine(MachineKind::This, MachineLink::Here);
        assert_eq!(detail(&this), "This machine · 0.1.0");
        assert_eq!(status(&this), "Connected");
    }

    #[test]
    fn a_peer_names_its_path_round_trip_and_agents() {
        let mut peer = machine(MachineKind::Engine, MachineLink::Live);
        peer.path = Some(LinkPath::Direct("100.64.0.2:41641".into()));
        peer.rtt_us = Some(3_200);
        peer.live_agents = Some(2);
        assert_eq!(detail(&peer), "Direct · 100.64.0.2:41641 · 4 ms · 0.1.0");
        assert_eq!(status(&peer), "Connected · 2 agents running");
    }

    #[test]
    fn another_protocol_needs_an_update_and_offline_says_why() {
        let mut peer = machine(
            MachineKind::Engine,
            MachineLink::Offline {
                reason: "no route".into(),
                retry_in_ms: 5_000,
            },
        );
        peer.protocol = Some(onicode_protocol::PROTOCOL + 1);
        assert_eq!(status(&peer), "Offline · no route · needs update");
    }
}
