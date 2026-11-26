//! The search tabs a face keeps, and how many searches it remembers.

use serde::{Deserialize, Serialize};

/// How many past queries are kept. Applied on write, so the file cannot grow without
/// bound however long the service runs.
pub const HISTORY: usize = 100;

/// How many search tabs are kept. Applied on write, as [`HISTORY`] is.
pub const TABS: usize = 24;

/// One search tab: what it looks for and in what order, in the protocol's words,
/// so every face that has tabs reads the same list.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SearchTab {
    #[serde(default)]
    pub query: String,
    /// As [`Settings::sort`]: `modified`, not a column's name. Empty is the default.
    #[serde(default)]
    pub sort: String,
    #[serde(default)]
    pub descending: Option<bool>,
    /// The content line's pattern, empty when the line is closed: a new tab
    /// starts without one, and coming back to a tab brings its own.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub content: String,
}
