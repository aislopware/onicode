//! Edge endpoint shapes the client still calls (push targets, health). HTTP endpoints take a
//! Bearer header.

pub(crate) fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// A phone's APNs registration (POST) / removal (DELETE).
pub(crate) fn registry_push_target(edge: &str, org_id: &str, device_id: &str) -> String {
    format!(
        "{}/registry/{}/push-target?device={}",
        edge.trim_end_matches('/'),
        encode(org_id),
        encode(device_id)
    )
}

pub(crate) fn health(edge: &str) -> String {
    format!("{}/health", edge.trim_end_matches('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_shapes() {
        assert_eq!(
            registry_push_target("https://e.sh/", "org_1", "ios-1"),
            "https://e.sh/registry/org_1/push-target?device=ios-1"
        );
        assert_eq!(encode("t@o"), "t%40o");
        assert_eq!(health("https://e.sh/"), "https://e.sh/health");
    }
}
