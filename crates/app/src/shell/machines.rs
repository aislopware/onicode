//! The focused sidebar's Machines section (`layout-gaps.md` §3), in the slot monocode's Monos
//! held, with its `section_label` and `icon_button` header: every other engine on the tailnet,
//! its `internet` glyph tinted by how the link stands, its name and its running agents. A row
//! opens "Open folder on a machine" on that machine; the `+` opens Settings → Connections' add
//! card. Viewers (phones) serve nothing and are not listed, nor is this machine.
//!
//! The shell follows `WatchMachines` here and keeps only what the section draws, so the
//! sidebar is told, and remeasured, only when a row it shows changes: round trips move every
//! few seconds and the section never reads them.
use onicode_protocol::EngineId;
use onicode_protocol::engine::{LinkPath, MachineKind, MachineLink, Machines, WatchMachines};

use super::*;
use crate::engine_client::{WatchUpdate, Watching};

// Tailwind emerald-400 and amber-400, as the live agents card's.
const EMERALD: u32 = 0x34d399;
const AMBER: u32 = 0xfbbf24;

/// How a machine's link stands, as its glyph is tinted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Tone {
    /// Linked straight over UDP.
    Online,
    /// Linked through a peer relay or DERP.
    Relay,
    /// Found, being dialled, or offline.
    Away,
}

/// One row of the section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct MachineRow {
    pub(super) engine: EngineId,
    pub(super) name: String,
    pub(super) tone: Tone,
    pub(super) agents: Option<u32>,
}

#[derive(Default)]
pub(super) struct Followed {
    /// The machines as the engine last listed them.
    all: Option<Machines>,
    /// What the section draws of them.
    pub(super) rows: Vec<MachineRow>,
    _watch: Option<(
        Watching<Machines, onicode_protocol::engine::MachineEvent>,
        gpui::Task<()>,
    )>,
}

/// The section's rows: every other engine, by name.
pub(super) fn rows(machines: &Machines) -> Vec<MachineRow> {
    let mut rows: Vec<_> = machines
        .machines
        .iter()
        .filter(|machine| matches!(machine.kind, MachineKind::Engine))
        .map(|machine| MachineRow {
            engine: machine.engine.clone(),
            name: machine.name.clone(),
            tone: match (&machine.link, &machine.path) {
                (MachineLink::Live, Some(LinkPath::PeerRelay(_) | LinkPath::Derp(_))) => {
                    Tone::Relay
                }
                (MachineLink::Live | MachineLink::Here, _) => Tone::Online,
                _ => Tone::Away,
            },
            agents: machine.live_agents,
        })
        .collect();
    rows.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.engine.cmp(&b.engine)));
    rows
}

fn tint(tone: Tone) -> gpui::Hsla {
    match tone {
        Tone::Online => rgb(EMERALD).into(),
        Tone::Relay => rgb(AMBER).into(),
        Tone::Away => content(0.3),
    }
}

impl Shell {
    /// Follows the machines the app's engine knows, for the section.
    pub(super) fn follow_machines(&mut self, cx: &mut Context<Self>) {
        self.machines.all = self.engine.machines_now();
        self.machines.rows = self.machines.all.as_ref().map(rows).unwrap_or_default();
        let watching = self
            .engine
            .watch::<WatchMachines>(|since| WatchMachines { since });
        let updates = watching.updates.clone();
        let task = cx.spawn(async move |this, cx| {
            while let Ok(update) = updates.recv().await {
                if this
                    .update(cx, |this, cx| this.machines_moved(update, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        self.machines._watch = Some((watching, task));
    }

    fn machines_moved(
        &mut self,
        update: WatchUpdate<Machines, onicode_protocol::engine::MachineEvent>,
        cx: &mut Context<Self>,
    ) {
        match update {
            WatchUpdate::Snapshot(machines) => self.machines.all = Some(*machines),
            WatchUpdate::Events(events) => {
                if let Some(machines) = self.machines.all.as_mut() {
                    for event in &events {
                        machines.apply(event);
                    }
                }
            }
        }
        let rows = self.machines.all.as_ref().map(rows).unwrap_or_default();
        if rows != self.machines.rows {
            self.machines.rows = rows;
            // The sidebar rebuilds its rows, remeasuring the section's.
            self.notify_sidebar(cx);
        }
    }

    /// Settings → Connections, with its add-machine card open.
    pub(super) fn open_add_machine(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_settings(crate::settings::Section::Connections, window, cx);
        if let Some(settings) = &self.settings {
            settings.update(cx, |settings, cx| settings.add_machine(cx));
        }
    }

    /// The Machines section: `section_label` and `+`, then a row per machine; with none, the
    /// add row in their place, as monocode's empty Monos section showed "New mono".
    pub(super) fn machines_section(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if !self.workspace.sidebar_machines_visible {
            return div().into_any_element();
        }
        let rows = self.machines.rows.clone();
        let has = !rows.is_empty();
        let heading = section_label("Machines").when(has, |d| {
            d.child(
                icon_button("add-machine", "Add a machine", "plus")
                    .debug_selector(|| "sidebar-machine-button".into())
                    .size(px(20.))
                    // Centred on the sidebar's trailing column, with the session filter and
                    // the navigation hints and counts.
                    .mr(px(-3.))
                    .on_click(cx.listener(|this, _, window, cx| this.open_add_machine(window, cx))),
            )
        });
        div()
            .px_2()
            .pb_3()
            .child(heading)
            .when(!has, |d| {
                d.child(
                    div()
                        .id("add-machine-row")
                        .debug_selector(|| "add-machine".into())
                        .role(Role::Button)
                        .aria_label("Add a machine")
                        .h(px(32.))
                        .px_2()
                        .rounded_md()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_size(px(14.))
                        .text_color(content(0.5))
                        .hover(|s| s.bg(content(0.1)).text_color(content(1.)))
                        .child(icon("plus", 16.))
                        .child("Add a machine")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.open_add_machine(window, cx)),
                        ),
                )
            })
            .children(rows.into_iter().map(|row| {
                let engine = row.engine.clone();
                let status = match row.tone {
                    Tone::Online => "connected",
                    Tone::Relay => "connected through a relay",
                    Tone::Away => "not connected",
                };
                let label = match row.agents {
                    Some(1) => format!("{}, {status}, 1 agent running", row.name),
                    Some(agents) if agents > 1 => {
                        format!("{}, {status}, {agents} agents running", row.name)
                    }
                    _ => format!("{}, {status}", row.name),
                };
                div()
                    .id(SharedString::from(format!("machine-row-{}", row.engine)))
                    .debug_selector({
                        let engine = row.engine.clone();
                        move || format!("machine-row-{engine}")
                    })
                    .role(Role::Button)
                    .aria_label(label)
                    .h(px(32.))
                    .px_2()
                    .rounded_md()
                    .relative()
                    .flex()
                    .items_center()
                    .gap_2()
                    .opacity(0.65)
                    .hover(|s| s.bg(content(0.08)).opacity(1.))
                    .child(
                        div()
                            .size(px(16.))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon("internet", 16.).text_color(tint(row.tone))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(14.))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .child(row.name.clone()),
                    )
                    .children(row.agents.filter(|agents| *agents > 0).map(|agents| {
                        div()
                            .flex_shrink_0()
                            .text_size(px(12.))
                            .text_color(content(0.5))
                            .child(agents.to_string())
                    }))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_peer_project(Some(engine.clone()), window, cx);
                    }))
            }))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use onicode_protocol::engine::Machine;

    use super::*;

    fn machine(id: &str, name: &str, kind: MachineKind, link: MachineLink) -> Machine {
        Machine {
            engine: EngineId(id.into()),
            name: name.into(),
            kind,
            link,
            path: None,
            rtt_us: None,
            version: String::new(),
            protocol: Some(onicode_protocol::PROTOCOL),
            owner: true,
            live_agents: None,
        }
    }

    /// Only other engines are listed, by name, each tinted by how its link travels.
    #[test]
    fn the_section_lists_other_engines_by_name() {
        let mut relayed = machine("n3", "attic", MachineKind::Engine, MachineLink::Live);
        relayed.path = Some(LinkPath::Derp("sin".into()));
        relayed.live_agents = Some(2);
        let mut direct = machine("n2", "studio", MachineKind::Engine, MachineLink::Live);
        direct.path = Some(LinkPath::Direct("100.64.0.2:41641".into()));
        let machines = Machines {
            machines: vec![
                machine("n1", "laptop", MachineKind::This, MachineLink::Here),
                direct,
                relayed,
                machine("n4", "phone", MachineKind::Viewer, MachineLink::Live),
                machine(
                    "n5",
                    "server",
                    MachineKind::Engine,
                    MachineLink::Offline {
                        reason: "no route".into(),
                        retry_in_ms: 5_000,
                    },
                ),
            ],
        };
        let shown: Vec<_> = rows(&machines)
            .into_iter()
            .map(|row| (row.name, row.tone, row.agents))
            .collect();
        assert_eq!(
            shown,
            vec![
                ("attic".to_owned(), Tone::Relay, Some(2)),
                ("server".to_owned(), Tone::Away, None),
                ("studio".to_owned(), Tone::Online, None),
            ]
        );
    }
}
