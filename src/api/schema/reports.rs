//! `report.*`: protocol problems coordinators report themselves. A report
//! is one fingerprint (its kind and its summary with ids, paths, numbers and
//! commit hashes taken out); each `report.record` of the same fingerprint
//! adds an occurrence to it. Only the first occurrence notifies (and the one
//! that reopens a closed report); the herdr repository's coordinator closes
//! reports, naming the TODO item or commit that fixed it.

use serde::{Deserialize, Serialize};

/// Records one occurrence of a protocol problem: a herdr command, wait,
/// sandbox or skill step that did not give its promised outcome or a clear
/// next step.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ReportRecordParams {
    /// A short slug: lowercase letters, digits and `-`, at most 40.
    pub kind: String,
    /// One line of at most 500 bytes saying what failed.
    pub summary: String,
    /// The reporter is not sure the problem is herdr's; the herdr
    /// coordinator decides.
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub uncertain: bool,
    /// The command the problem came from, as it was run; cut to its first
    /// 2000 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// A file's text the server keeps with the occurrence, so a temporary
    /// file may go away.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<ReportEvidenceUpload>,
    /// The reporter's pane (the CLI sends its `HERDR_PANE_ID`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    /// The reporter's agent session; the pane's agent session when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// The reporter's directory, which names the repository; the pane's
    /// directory when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

/// An evidence file as the CLI read it: at most 64 KiB of its text.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ReportEvidenceUpload {
    /// The file's path as the reporter named it.
    pub source: String,
    /// Its text (invalid UTF-8 replaced), at most 65536 bytes.
    pub content: String,
    /// The file's size in bytes.
    pub bytes: u64,
    /// Only the first 65536 bytes were sent.
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub truncated: bool,
}

/// Lists the reports: the open ones, or all with `all`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ReportListParams {
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub all: bool,
}

/// Closes a report, from the pane that holds the herdr repository's
/// coordination tenure.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ReportCloseParams {
    /// `r-` and the report's number.
    pub report_id: String,
    /// The TODO item (`t-...`) or commit that fixed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
    /// The problem could not be reproduced; excludes `fix`.
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub not_reproducible: bool,
    /// The closing coordinator's pane (the CLI sends its `HERDR_PANE_ID`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReportState {
    Open,
    Closed,
    #[serde(other)]
    Unknown,
}

/// One protocol problem: every occurrence of one fingerprint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ReportInfo {
    /// `r-` and the report's number.
    pub report_id: String,
    pub kind: String,
    /// The first occurrence's summary.
    pub summary: String,
    /// The summary as fingerprinted: ids, paths, numbers and commit hashes
    /// replaced by `<id>`, `<path>`, `<n>` and `<sha>`.
    pub normalized: String,
    /// 16 hex characters of the SHA-256 of the kind and the normalized
    /// summary.
    pub fingerprint: String,
    pub state: ReportState,
    /// Every occurrence was reported as uncertain.
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub uncertain: bool,
    /// Unix milliseconds of the first and the latest occurrence.
    pub first_ms: u64,
    pub last_ms: u64,
    /// How many occurrences it has.
    pub count: u32,
    /// How many times a later occurrence reopened it.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub reopened: u32,
    /// While it is closed: when, by which tenure, and with what.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_by: Option<String>,
    /// The TODO item or commit that fixed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub not_reproducible: bool,
    /// Its occurrences, oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub occurrences: Vec<ReportOccurrence>,
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

/// One `report.record` of a report's fingerprint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ReportOccurrence {
    /// 1 for the first; the `K` of "report r-N, occurrence K".
    pub number: u32,
    /// Unix milliseconds.
    pub ts_ms: u64,
    /// This occurrence's own summary.
    pub summary: String,
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub uncertain: bool,
    /// The reporter's repository: the parent of its git common directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// The tenure the reporter's pane held then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinator_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Where the server keeps the evidence's copy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    /// The path the reporter named.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_source: Option<String>,
    #[serde(default, skip_serializing_if = "super::is_false")]
    pub evidence_truncated: bool,
    /// The closure this occurrence reopened: `fix <item or commit>` or `not
    /// reproducible`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reopened: Option<String>,
}
