//! Provider CLI updates: which are behind, and how versions compare. Checking is the engine's.
use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

use crate::provider::Kind;

/// A provider CLI with a newer release than the one installed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Update {
    pub harness: Kind,
    pub installed: String,
    pub latest: String,
}
pub fn parse_version(text: &str) -> Option<String> {
    regex::Regex::new(r"\d+\.\d+\.\d+")
        .ok()?
        .find(text)
        .map(|m| m.as_str().to_owned())
}
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    let parts = |s: &str| {
        s.split('.')
            .map(|n| n.parse::<u64>().unwrap_or(0))
            .take(3)
            .collect::<Vec<_>>()
    };
    parts(a).cmp(&parts(b))
}
