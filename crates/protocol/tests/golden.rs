//! The protocol's golden table: every method's name, kind and agent flag, and a hash of the serde
//! shape of every type it carries, recorded in `tests/golden/protocol.txt` with the `PROTOCOL` it
//! was recorded at. A change to any of them fails here until `PROTOCOL` is bumped and the table is
//! recorded again (`ONICODE_GOLDEN=record`); recording over a table of the same `PROTOCOL` is
//! refused.
//!
//! The shapes are traced by serde-reflection through the types' own Serialize and Deserialize
//! impls, so a renamed field, a new variant or a changed `#[serde]` attribute counts, in the
//! protocol's types and in the core types they carry alike. Each hash covers the type and every
//! named type it reaches.
use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::fmt::Write as _;
use std::path::PathBuf;

use onicode_protocol::agent::AccessMode;
use onicode_protocol::engine::{EngineInfo, Hello, StopEngine, UpdateEngine};
use onicode_protocol::provider::Kind;
use onicode_protocol::session::{
    Answer, Btw, Configure, Decide, EditLastTurn, Fork, KeepQuestionOpen, QueueChange, QueueEdit,
    ReadTranscript, Rewind, Send, SendNow, Steer, SteerNow, Stop, SwitchProvider, Verdict,
    WatchSession,
};
use onicode_protocol::transcript::Row;
use onicode_protocol::{
    Call, Cursor, Item, METHODS, MessageId, Method, MethodKind, OpId, PROTOCOL, Rejection, Routed,
    SessionId, TerminalId, Watch,
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use serde_reflection::{Format, FormatHolder, Registry, Samples, Tracer, TracerConfig};

/// One line of the table: its label and the formats it hashes, by role.
type Line = (String, Vec<(&'static str, Format)>);

/// Every type of the table traced into one registry.
struct Shapes {
    tracer: Tracer,
    samples: Samples,
    methods: Vec<Method>,
    lines: Vec<Line>,
}

impl Shapes {
    fn new() -> Self {
        // Serde's compact mode, the one serde-reflection replays samples in: a Uuid checks its
        // input, so the tracer hands each id type a real one.
        let mut tracer = Tracer::new(TracerConfig::default());
        let mut samples = Samples::new();
        tracer.trace_value(&mut samples, &SessionId::new()).unwrap();
        tracer.trace_value(&mut samples, &MessageId::new()).unwrap();
        tracer.trace_value(&mut samples, &OpId::new()).unwrap();
        tracer
            .trace_value(&mut samples, &TerminalId::new())
            .unwrap();
        // A row's echo id is a bare Uuid, so the row itself is the sample, every field set so none
        // is skipped.
        let row = Row {
            role: "user".into(),
            text: String::new(),
            first: true,
            sent_at: Some(1),
            id: Some(uuid::Uuid::now_v7()),
        };
        tracer.trace_value(&mut samples, &row).unwrap();
        Self {
            tracer,
            samples,
            methods: Vec::new(),
            lines: Vec::new(),
        }
    }

    fn trace<T: DeserializeOwned>(&mut self) -> Format {
        let (mut format, _) = self
            .tracer
            .trace_type::<T>(&self.samples)
            .unwrap_or_else(|error| panic!("tracing {}: {error}", std::any::type_name::<T>()));
        format.normalize().unwrap();
        format
    }

    fn call<C: Call>(&mut self) {
        let params = self.trace::<C::Params>();
        let reply = self.trace::<C::Reply>();
        let agent = if C::AGENT { " agent" } else { "" };
        self.methods.push(Method {
            name: C::NAME,
            kind: MethodKind::Call,
            agent: C::AGENT,
        });
        self.lines.push((
            format!("call {}{agent}", C::NAME),
            vec![("params", params), ("reply", reply)],
        ));
    }

    fn watch<W: Watch>(&mut self) {
        let params = self.trace::<W::Params>();
        let state = self.trace::<W::State>();
        let event = self.trace::<W::Event>();
        self.methods.push(Method {
            name: W::NAME,
            kind: MethodKind::Watch,
            agent: false,
        });
        self.lines.push((
            format!("watch {}", W::NAME),
            vec![("params", params), ("state", state), ("event", event)],
        ));
    }

    fn envelope<T: DeserializeOwned>(&mut self, name: &str) {
        let shape = self.trace::<T>();
        self.lines
            .push((format!("envelope {name}"), vec![("shape", shape)]));
    }

    /// An enum only ever reached inside another type: tracing it alone finds every variant.
    fn variants<T: DeserializeOwned>(&mut self) {
        self.trace::<T>();
    }

    fn table(self) -> (Vec<Method>, String) {
        let registry = self
            .tracer
            .registry()
            .unwrap_or_else(|error| panic!("{error}: trace the enums it names with `variants`"));
        let mut table = format!("protocol {PROTOCOL}\n");
        for (label, formats) in &self.lines {
            table.push_str(label);
            for (role, format) in formats {
                write!(table, " {role}={}", digest(format, &registry)).unwrap();
            }
            table.push('\n');
        }
        (self.methods, table)
    }
}

/// The FNV-1a hash of the format and of every container it reaches, as 16 hex digits.
fn digest(format: &Format, registry: &Registry) -> String {
    let format = serde_json::to_value(format).unwrap();
    let mut reached = BTreeMap::new();
    reach(&format, registry, &mut reached);
    let text = serde_json::to_string(&sorted(&serde_json::json!([format, reached]))).unwrap();
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// `value` with every object's keys in order, whether or not serde_json keeps insertion order
/// (its `preserve_order` feature, which other crates of the workspace turn on).
fn sorted(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<_> = map.keys().collect();
            keys.sort_unstable();
            Value::Object(
                keys.into_iter()
                    .map(|key| (key.clone(), sorted(&map[key])))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
        other => other.clone(),
    }
}

/// Collects the containers `value` names (`{"TYPENAME": name}`), and the ones they name.
fn reach(value: &Value, registry: &Registry, reached: &mut BTreeMap<String, Value>) {
    match value {
        Value::Object(map) => {
            for (key, inner) in map {
                if key == "TYPENAME"
                    && let Value::String(name) = inner
                {
                    if let Entry::Vacant(entry) = reached.entry(name.clone()) {
                        let container = serde_json::to_value(&registry[name]).unwrap();
                        entry.insert(container.clone());
                        reach(&container, registry, reached);
                    }
                } else {
                    reach(inner, registry, reached);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                reach(item, registry, reached);
            }
        }
        _ => {}
    }
}

fn shapes() -> (Vec<Method>, String) {
    let mut shapes = Shapes::new();
    // Engine.
    shapes.call::<Hello>();
    shapes.call::<EngineInfo>();
    shapes.call::<UpdateEngine>();
    shapes.call::<StopEngine>();
    // Session.
    shapes.watch::<WatchSession>();
    shapes.call::<Send>();
    shapes.call::<Steer>();
    shapes.call::<Stop>();
    shapes.call::<Decide>();
    shapes.call::<Answer>();
    shapes.call::<KeepQuestionOpen>();
    shapes.call::<QueueEdit>();
    shapes.call::<SendNow>();
    shapes.call::<SteerNow>();
    shapes.call::<Configure>();
    shapes.call::<EditLastTurn>();
    shapes.call::<Rewind>();
    shapes.call::<Fork>();
    shapes.call::<Btw>();
    shapes.call::<SwitchProvider>();
    shapes.call::<ReadTranscript>();
    // What every call and watch travels in.
    shapes.envelope::<Cursor>("Cursor");
    shapes.envelope::<Item<(), ()>>("Item");
    shapes.envelope::<Routed<()>>("Routed");
    shapes.variants::<Rejection>();
    shapes.variants::<Verdict>();
    shapes.variants::<QueueChange>();
    shapes.variants::<Kind>();
    shapes.variants::<AccessMode>();
    shapes.table()
}

#[test]
fn every_method_is_in_the_golden_table() {
    let (methods, _) = shapes();
    assert_eq!(methods, METHODS);
    let mut names: Vec<_> = METHODS.iter().map(|method| method.name).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), METHODS.len(), "two methods share a name");
}

#[test]
fn the_table_matches_its_golden() {
    let (_, table) = shapes();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/protocol.txt");
    let recorded = std::fs::read_to_string(&path).ok();
    if recorded.as_deref() == Some(table.as_str()) {
        return;
    }
    let record = match std::env::var("ONICODE_GOLDEN").as_deref() {
        Ok("record") => true,
        Ok("check" | "") | Err(_) => false,
        Ok(other) => panic!("ONICODE_GOLDEN={other}: expected record or check"),
    };
    let recorded_protocol = recorded
        .as_deref()
        .and_then(|text| text.lines().next())
        .and_then(|line| line.strip_prefix("protocol "))
        .and_then(|number| number.parse::<u32>().ok());
    if record {
        assert_ne!(
            recorded_protocol,
            Some(PROTOCOL),
            "the table changed while PROTOCOL stayed {PROTOCOL}: bump it in \
             crates/protocol/src/lib.rs, then record again"
        );
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &table).unwrap();
        return;
    }
    panic!(
        "the protocol's table changed.\n--- recorded\n{}--- now\n{table}Bump PROTOCOL in \
         crates/protocol/src/lib.rs, then run this test with ONICODE_GOLDEN=record",
        recorded.unwrap_or_default()
    );
}
