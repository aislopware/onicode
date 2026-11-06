//! What `onicode` resolves from its environment.

/// The release feed the updater reads: `ONICODE_UPDATE_FEED`, else none, which leaves the
/// updater off until better-update's feed replaces it.
pub fn update_feed_from_env() -> String {
    std::env::var("ONICODE_UPDATE_FEED")
        .ok()
        .map(|feed| feed.trim().to_owned())
        .unwrap_or_default()
}

/// The engine's IPC port: `ONICODE_IPC_PORT`, else 27654.
pub fn ipc_port_from_env() -> u16 {
    ipc_port_from(std::env::var("ONICODE_IPC_PORT").ok().as_deref())
}

/// [`ipc_port_from_env`] from the variable's value.
pub fn ipc_port_from(value: Option<&str>) -> u16 {
    value
        .and_then(|p| p.parse().ok())
        .unwrap_or(DEFAULT_IPC_PORT)
}

/// The production engine's IPC port.
pub const DEFAULT_IPC_PORT: u16 = 27654;
