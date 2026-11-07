//! Project content search: what a search asks and finds. Running it is the engine's.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchOptions {
    pub cwd: String,
    pub query: String,
    #[serde(default)]
    pub case_sensitive: bool,
    #[serde(default)]
    pub whole_word: bool,
    #[serde(default)]
    pub regex: bool,
    pub include: Option<String>,
    pub exclude: Option<String>,
    #[serde(default)]
    pub search_id: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SearchMatch {
    pub path: String,
    pub relative: String,
    pub line: u32,
    pub column: u32,
    pub preview: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    pub matches: Vec<SearchMatch>,
    pub truncated: bool,
}

/// Byte ranges for the first highlighted match in each project-search preview.
/// Compile once for the complete result set, away from the UI's paint path.
pub fn preview_ranges(
    matches: &[SearchMatch],
    options: &SearchOptions,
) -> Vec<Option<std::ops::Range<usize>>> {
    let pattern = if options.regex {
        options.query.trim().to_owned()
    } else {
        regex::escape(options.query.trim())
    };
    let matcher = regex::RegexBuilder::new(&pattern)
        .case_insensitive(!options.case_sensitive)
        .build()
        .ok();
    matches
        .iter()
        .map(|m| {
            matcher
                .as_ref()?
                .find(m.preview.trim_end())
                .map(|found| found.range())
        })
        .collect()
}
