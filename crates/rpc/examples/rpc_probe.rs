//! Calls a running engine over its socket, and times the round trip: the Mac bench behind the
//! Local feel table's "receipt from the local engine, ≤ 1 ms p99 over the socket" (engine.md).
//!
//! Usage: `rpc_probe <data dir> [method] [params] [--repeat N]`, run with
//! `cargo run --release -p onicode-rpc --example rpc_probe --`.
//!
//! `Hello` goes first, as on every connection. With no method the probe times `EngineInfo`.
//! Params are the call's body as JSON; the probe wraps them in a `Routed` for this engine. The
//! full token comes from `<data dir>/ipc-token`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use onicode_protocol::engine::Hello;
use onicode_protocol::{PROTOCOL, Routed};
use onicode_rpc::{connect_unix, ipc_auth};

fn routed(body: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "engine": null, "op": null, "body": body })
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(dir) = args.first().map(PathBuf::from) else {
        eprintln!("usage: rpc_probe <data dir> [method] [params-json] [--repeat N]");
        std::process::exit(2);
    };
    let mut positional = args.iter().skip(1).take_while(|arg| *arg != "--repeat");
    let method = positional
        .next()
        .cloned()
        .unwrap_or_else(|| "EngineInfo".into());
    let params: serde_json::Value = match positional.next() {
        Some(raw) => serde_json::from_str(raw).unwrap_or_else(|error| {
            eprintln!("params are not JSON: {error}");
            std::process::exit(2);
        }),
        None => serde_json::json!({}),
    };
    let repeat: usize = args
        .iter()
        .position(|arg| arg == "--repeat")
        .and_then(|at| args.get(at + 1))
        .and_then(|n| n.parse().ok())
        .unwrap_or(1);
    let Some(token) = ipc_auth::client_tokens(Some(&dir)).into_iter().next() else {
        eprintln!("no engine has started in {}", dir.display());
        std::process::exit(1);
    };
    let dialed = Instant::now();
    let client = match connect_unix(&dir.join("engine.sock"), &token, None).await {
        Ok(client) => client,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };
    let hello = Routed {
        engine: None,
        op: None,
        body: Hello {
            protocol: PROTOCOL,
            version: env!("CARGO_PKG_VERSION").into(),
            capabilities: Vec::new(),
        },
    };
    let hello = serde_json::to_value(hello).unwrap_or_default();
    match client.call("Hello", hello).await {
        Ok(reply) => eprintln!("Hello in {:?}: {reply}", dialed.elapsed()),
        Err(error) => {
            eprintln!("Hello failed: {error}");
            std::process::exit(1);
        }
    }
    let params = routed(params);
    let mut times: Vec<Duration> = Vec::with_capacity(repeat);
    let mut last = serde_json::Value::Null;
    for _ in 0..repeat {
        let started = Instant::now();
        match client.call_ref(&method, &params).await {
            Ok(reply) => last = reply,
            Err(error) => {
                eprintln!("{method} failed: {error}");
                std::process::exit(1);
            }
        }
        times.push(started.elapsed());
    }
    println!("{last}");
    times.sort_unstable();
    let at = |q: f64| {
        let index = ((times.len() as f64 - 1.0) * q).round() as usize;
        times.get(index).copied().unwrap_or_default()
    };
    eprintln!(
        "{method} x{repeat}: p50 {:?}, p99 {:?}, max {:?}",
        at(0.5),
        at(0.99),
        at(1.0)
    );
}
