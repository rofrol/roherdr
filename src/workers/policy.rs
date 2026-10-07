//! Answers a headless worker's `can_use_tool` requests (slice 1): file tools
//! inside the worker's directory are allowed, everything else is denied.
//! The CLI's own `decision_reason` and `blocked_path` are hints only; it asks
//! even for paths it flags, so the real-path check here is the guard.

use std::path::{Component, Path, PathBuf};

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Decision {
    Allow,
    Deny(String),
}

/// File tools, the input field that names their path, and whether that field
/// is required (an optional one defaults to the working directory).
fn file_tool_path_field(tool_name: &str) -> Option<(&'static str, bool)> {
    match tool_name {
        "Read" | "Write" | "Edit" | "MultiEdit" => Some(("file_path", true)),
        "NotebookEdit" | "NotebookRead" => Some(("notebook_path", true)),
        "Glob" | "Grep" | "LS" => Some(("path", false)),
        _ => None,
    }
}

/// Decides one `can_use_tool` request. `cwd` is the worker's directory and
/// `cwd_real` its canonical path.
pub(super) fn decide(cwd: &Path, cwd_real: &Path, tool_name: &str, input: &Value) -> Decision {
    let Some((field, required)) = file_tool_path_field(tool_name) else {
        return Decision::Deny(format!(
            "herdr worker policy: {tool_name} needs an approval that this headless worker \
             cannot ask for yet. Continue without it, or stop and say what you need."
        ));
    };
    let path = match input.get(field).and_then(Value::as_str) {
        Some(path) if !path.is_empty() => path,
        _ if !required => return Decision::Allow,
        _ => {
            return Decision::Deny(format!(
                "herdr worker policy: {tool_name} without `{field}` is not allowed."
            ))
        }
    };
    let candidate = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        cwd.join(path)
    };
    match real_path_allowing_missing_tail(&candidate) {
        Some(real) if real.starts_with(cwd_real) => Decision::Allow,
        _ => Decision::Deny(format!(
            "herdr worker policy: {path} is outside the worker's directory {}.",
            cwd_real.display()
        )),
    }
}

/// The canonical path of `path`. A missing tail (a file about to be written)
/// is appended to its nearest existing ancestor's canonical path; a tail with
/// `.` or `..` is refused, because it cannot be resolved without the files.
pub(super) fn real_path_allowing_missing_tail(path: &Path) -> Option<PathBuf> {
    let mut existing = path;
    let mut tail = Vec::new();
    loop {
        match existing.canonicalize() {
            Ok(real) => {
                let mut real = real;
                for component in tail.iter().rev() {
                    real.push(component);
                }
                return Some(real);
            }
            Err(_) => {
                let name = existing.file_name()?;
                match existing.components().next_back() {
                    Some(Component::Normal(_)) => tail.push(name.to_os_string()),
                    _ => return None,
                }
                existing = existing.parent()?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("herdr-worker-policy-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn file_tools_inside_the_directory_are_allowed() {
        let root = temp_dir("inside");
        let cwd = root.join("repo");
        std::fs::create_dir_all(cwd.join("src")).unwrap();
        let real = cwd.canonicalize().unwrap();

        for (tool, input) in [
            ("Write", json!({"file_path": "new.txt"})),
            ("Write", json!({"file_path": cwd.join("src/new/deep.txt")})),
            ("Read", json!({"file_path": real.join("src")})),
            ("Glob", json!({"pattern": "*.rs"})),
            ("Grep", json!({"pattern": "x", "path": "src"})),
        ] {
            assert_eq!(
                decide(&cwd, &real, tool, &input),
                Decision::Allow,
                "{tool} {input}"
            );
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn paths_outside_the_directory_are_denied() {
        let root = temp_dir("outside");
        let cwd = root.join("repo");
        let outside = root.join("outside");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let real = cwd.canonicalize().unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, cwd.join("link-out")).unwrap();

        let mut cases = vec![
            json!({"file_path": outside.join("x.txt")}),
            json!({"file_path": "../outside/x.txt"}),
            json!({"file_path": "missing/../../outside/x.txt"}),
            json!({"file_path": ""}),
        ];
        if cfg!(unix) {
            cases.push(json!({"file_path": "link-out/escape.txt"}));
        }
        for input in cases {
            assert!(
                matches!(decide(&cwd, &real, "Write", &input), Decision::Deny(_)),
                "{input}"
            );
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn other_tools_are_denied_with_a_message() {
        let cwd = temp_dir("other");
        let real = cwd.canonicalize().unwrap();
        for tool in ["Bash", "AskUserQuestion", "WebFetch", "mcp__x__y"] {
            let Decision::Deny(message) = decide(&cwd, &real, tool, &json!({})) else {
                panic!("{tool} must be denied");
            };
            assert!(message.contains(tool), "{message}");
        }
        let _ = std::fs::remove_dir_all(cwd);
    }
}
