//! The engine settings (`docs/onicode/decisions/engine.md`, migration step 6): what the engine does
//! on its own behalf, kept in the store under one key and served as `WatchEngineSettings` on the
//! engine's one `Topic`. `SetEngineSetting` replaces one typed key's value; a repeat of its op
//! gets the first reply and changes nothing again. The settings take effect in the engine
//! ([`Effects`]): the provider CLIs' paths and how many idle agents stay parked.
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use futures::stream::BoxStream;
use onicode_protocol::engine_settings::{
    EngineSettings, EngineSettingsEvent, SetEngineSetting, WatchEngineSettings,
};
use onicode_protocol::{Call, Item, OpId, Outcome, Watch};
use onicode_rpc::{RpcError, RpcReply, RpcService};

use crate::feed::{self, Feed, Limits, Pending, Topic};
use crate::ledger::Ledger;
use crate::service::{Group, reply, routed};
use crate::turn::{Database, locked};

#[cfg(test)]
mod tests;

/// The store key the settings live under.
pub(crate) const KEY: &str = "engine-settings";

/// What the settings change outside the hub. A seam, so the model tests count it instead of
/// reconfiguring the provider runners.
pub trait Effects: Send + Sync + 'static {
    /// The settings as they now are: at the hub's start, and after every change.
    fn applied(&self, settings: &EngineSettings);
}

/// The provider runners'.
pub struct CoreEffects;

impl Effects for CoreEffects {
    fn applied(&self, settings: &EngineSettings) {
        // The CLI paths are read once per engine run, as monocode read them once per launch.
        onicode_core::cli::initialize(
            &settings
                .binary_paths
                .iter()
                .map(|(kind, path)| (kind.clone(), path.clone()))
                .collect(),
        );
        onicode_core::provider::configure_idle_agents(settings.idle_agents as usize);
    }
}

pub(crate) struct SettingsFeed;

impl Feed for SettingsFeed {
    type Event = EngineSettingsEvent;
    type State = EngineSettings;

    fn apply(state: &mut EngineSettings, event: &EngineSettingsEvent) {
        if let EngineSettingsEvent::Set(setting) = event {
            state.apply(setting);
        }
    }

    fn weight(event: &EngineSettingsEvent) -> usize {
        serde_json::to_vec(event).map_or(256, |bytes| bytes.len())
    }

    /// A key's value replaces a pending value of the same key; receipts queue in order.
    fn absorb(pending: &mut Pending<Self>, event: EngineSettingsEvent) -> bool {
        pending.replace_or_append(event, |held, next| match (held, next) {
            (EngineSettingsEvent::Set(held), EngineSettingsEvent::Set(next)) => {
                held.key() == next.key()
            }
            _ => false,
        })
    }
}

pub struct SettingsHub {
    store: Database,
    ledger: Ledger,
    effects: Arc<dyn Effects>,
    topic: Mutex<Topic<SettingsFeed>>,
}

impl SettingsHub {
    /// The settings in `store`, moved out of monocode's saved preferences at the first open.
    pub fn new(store: Database) -> Result<Arc<Self>, String> {
        Self::with(store, Arc::new(CoreEffects), Limits::default())
    }

    pub(crate) fn with(
        store: Database,
        effects: Arc<dyn Effects>,
        limits: Limits,
    ) -> Result<Arc<Self>, String> {
        let settings = load(&store)?;
        effects.applied(&settings);
        let epoch = onicode_core::time::now_ms().wrapping_shl(16) | 3;
        Ok(Arc::new(Self {
            ledger: Ledger::new(store.clone()),
            store,
            effects,
            topic: Mutex::new(Topic::new(epoch, settings, limits)),
        }))
    }

    fn lock(&self) -> MutexGuard<'_, Topic<SettingsFeed>> {
        self.topic.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The settings as they are now.
    #[must_use]
    pub fn current(&self) -> EngineSettings {
        self.lock().state().clone()
    }

    /// `WatchEngineSettings`.
    pub fn watch(
        self: &Arc<Self>,
        params: &WatchEngineSettings,
    ) -> BoxStream<'static, Item<EngineSettings, EngineSettingsEvent>> {
        let (id, notify) = self.lock().join(params.since);
        let (draining, leaving) = (self.clone(), self.clone());
        feed::stream(
            notify,
            move || draining.lock().drain(id),
            move || {
                leaving.lock().leave(id);
            },
        )
    }

    /// `SetEngineSetting`: saves the key's value, applies it and publishes it, then the receipt.
    pub async fn set(&self, op: Option<OpId>, body: SetEngineSetting) -> Result<Outcome, String> {
        self.ledger
            .once(op, async {
                let settings = {
                    let mut topic = self.lock();
                    let mut next = topic.state().clone();
                    next.apply(&body.setting);
                    if &next != topic.state() {
                        save(&self.store, &next)?;
                        topic.publish(vec![EngineSettingsEvent::Set(body.setting.clone())]);
                    }
                    if let Some(op) = op {
                        topic.publish(vec![EngineSettingsEvent::Receipt {
                            op,
                            outcome: Outcome::Applied,
                        }]);
                    }
                    next
                };
                self.effects.applied(&settings);
                Ok(Outcome::Applied)
            })
            .await
    }
}

/// The settings saved under [`KEY`], or the defaults (saved at once) in a new store.
fn load(store: &Database) -> Result<EngineSettings, String> {
    let saved = locked(store)?.setting(KEY)?;
    if let Some(raw) = saved {
        return serde_json::from_str(&raw)
            .map_err(|error| format!("Cannot read engine settings: {error}"));
    }
    let settings = EngineSettings::default();
    save(store, &settings)?;
    Ok(settings)
}

fn save(store: &Database, settings: &EngineSettings) -> Result<(), String> {
    let raw = serde_json::to_string(settings).map_err(|error| error.to_string())?;
    locked(store)?.save_setting(KEY, &raw)
}

/// The Engine settings group as an `onicode-rpc` service.
pub struct SettingsService {
    hub: Arc<SettingsHub>,
}

impl SettingsService {
    #[must_use]
    pub fn new(hub: Arc<SettingsHub>) -> Self {
        Self { hub }
    }
}

const METHODS: [&str; 2] = [WatchEngineSettings::NAME, SetEngineSetting::NAME];

impl Group for SettingsService {
    fn serves(&self, method: &str) -> bool {
        METHODS.contains(&method)
    }
}

#[async_trait::async_trait]
impl RpcService for SettingsService {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        match method {
            WatchEngineSettings::NAME => {
                let (_, body) = routed::<WatchEngineSettings>(params)?;
                Ok(RpcReply::Stream(json_items(self.hub.watch(&body))))
            }
            SetEngineSetting::NAME => {
                let (op, body) = routed(params)?;
                reply(self.hub.set(op, body).await)
            }
            _ => Err(RpcError::UnknownMethod(method.into())),
        }
    }
}

/// A watch's items as the JSON values a stream reply carries.
pub(crate) fn json_items<S, E>(
    items: BoxStream<'static, Item<S, E>>,
) -> BoxStream<'static, serde_json::Value>
where
    S: serde::Serialize + Send + 'static,
    E: serde::Serialize + Send + 'static,
{
    use futures::StreamExt;
    items
        .filter_map(|item| futures::future::ready(serde_json::to_value(&item).ok()))
        .boxed()
}
