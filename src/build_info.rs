//! Build identity helpers.

pub const BASE_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn channel() -> &'static str {
    non_empty(option_env!("HERDR_BUILD_CHANNEL")).unwrap_or("stable")
}

pub fn build_id() -> Option<&'static str> {
    non_empty(option_env!("HERDR_BUILD_ID"))
}

/// Short hash and subject of the commit this binary was built from, when the
/// build ran inside a git checkout.
pub fn commit_line() -> Option<&'static str> {
    non_empty(option_env!("HERDR_GIT_COMMIT_LINE"))
}

pub fn version() -> String {
    match channel() {
        "stable" => BASE_VERSION.to_string(),
        channel => match build_id() {
            Some(build_id) => format!("{BASE_VERSION}-{channel}.{build_id}"),
            None => format!("{BASE_VERSION}-{channel}"),
        },
    }
}

pub fn is_preview() -> bool {
    channel() == "preview"
}

/// Whether this binary was built from the fork (`HERDR_FORK_BUILD`, set in
/// `.cargo/config.toml`), whose installs come from `scripts/herdr_live.sh`.
pub fn is_fork() -> bool {
    fork_marker_set(option_env!("HERDR_FORK_BUILD"))
}

/// Upstream binary releases must not be checked for, announced or installed:
/// a fork build is replaced by rebuilding it, not by upstream's release.
/// Unit tests always behave like upstream builds, so they stay deterministic.
pub fn upstream_updates_disabled() -> bool {
    !cfg!(test) && is_fork()
}

fn fork_marker_set(value: Option<&'static str>) -> bool {
    non_empty(value).is_some_and(|value| value != "0")
}

fn non_empty(value: Option<&'static str>) -> Option<&'static str> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn fork_marker_must_be_present_and_not_zero() {
        assert!(super::fork_marker_set(Some("1")));
        assert!(!super::fork_marker_set(Some("0")));
        assert!(!super::fork_marker_set(Some("  ")));
        assert!(!super::fork_marker_set(None));
        assert!(
            !super::upstream_updates_disabled(),
            "tests act like upstream"
        );
    }

    #[test]
    fn stable_version_defaults_to_cargo_version() {
        assert!(!super::version().is_empty());
    }
}
