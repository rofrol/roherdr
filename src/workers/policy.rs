//! Answers a headless worker's `can_use_tool` requests.
//!
//! Bash runs inside Claude Code's sandbox (`autoAllowBashIfSandboxed`), so it
//! reaches herdr only when a user's or project's `ask` rule matches it; the
//! sandbox, not herdr, bounds what it can write, read and reach (trial 3,
//! T3-2). The file tools are not sandboxed, so herdr decides them here:
//!
//! - File tools are allowed when the real path of their target is inside the
//!   worker's directory or its temp dir and denied otherwise (a symlink that
//!   escapes is resolved and denied). Environment files (`.env`, `.env.*`,
//!   `.envrc`) are denied wherever they are. A `Glob` pattern or `Grep` glob
//!   that is absolute outside those roots, starts with `~` or has a `..`
//!   component is denied too.
//! - A request the CLI's auto mode classifier escalated
//!   (`decision_reason_type: "classifier"`), every Bash request, every
//!   `AskUserQuestion` and every other tool is asked of the user.
//!
//! The CLI's own `decision_reason` and `blocked_path` are hints only; it asks
//! even for paths it flags, so the real-path check here is the guard.

use std::path::{Component, Path, PathBuf};

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Decision {
    Allow,
    Deny(String),
    /// The user decides; the string says why the policy did not.
    Ask(String),
}

#[derive(Debug, Clone)]
pub(super) struct Policy {
    cwd: PathBuf,
    /// Real paths the file tools may use: the worker's directory, then its
    /// temp dir.
    roots: Vec<PathBuf>,
}

impl Policy {
    /// `cwd` resolves relative paths; `cwd_real` and `temp_real` are the
    /// canonical roots.
    pub(super) fn new(cwd: &Path, cwd_real: &Path, temp_real: &Path) -> Self {
        Self {
            cwd: cwd.to_owned(),
            roots: vec![cwd_real.to_owned(), temp_real.to_owned()],
        }
    }

    /// The roots in effect, for the journal.
    pub(super) fn roots(&self) -> Vec<String> {
        self.roots
            .iter()
            .map(|root| root.display().to_string())
            .collect()
    }

    /// Decides one `can_use_tool` request. `reason_type` is the request's
    /// `decision_reason_type`.
    pub(super) fn decide(
        &self,
        tool_name: &str,
        input: &Value,
        reason_type: Option<&str>,
    ) -> Decision {
        if reason_type == Some("classifier") {
            return Decision::Ask(
                "the auto mode classifier blocked repeated actions and asks for a review".into(),
            );
        }
        if tool_name == "AskUserQuestion" {
            return Decision::Ask("a question for the user".into());
        }
        if tool_name == "Bash" {
            return Decision::Ask("a Bash command that an ask rule sends to the user".into());
        }
        let Some((field, required)) = file_tool_path_field(tool_name) else {
            return Decision::Ask(format!("{tool_name} is not decided by herdr's policy"));
        };
        if let Some(pattern_field) = file_tool_pattern_field(tool_name) {
            if let Some(pattern) = input.get(pattern_field).and_then(Value::as_str) {
                let base = input
                    .get(field)
                    .and_then(Value::as_str)
                    .filter(|path| !path.is_empty())
                    .map_or_else(|| self.cwd.clone(), |path| self.cwd.join(path));
                if let Err(message) = self.check_pattern(pattern, &base) {
                    return Decision::Deny(message);
                }
            }
        }
        let path = match input.get(field).and_then(Value::as_str) {
            Some(path) if !path.is_empty() => path,
            _ if !required => return Decision::Allow,
            _ => {
                return Decision::Deny(format!(
                    "herdr worker policy: {tool_name} without `{field}` is not allowed."
                ))
            }
        };
        self.check_path(path)
    }

    /// The real-path rule. Known limit: the file tools run outside the
    /// sandbox and act after this check, so a symlink that sandboxed Bash
    /// swaps in between the check and the write could send a write outside
    /// the roots. What bounds it: Bash itself can write only inside the
    /// roots, so it cannot plant anything elsewhere, the race needs the
    /// model to aim at it, and the coordinator reviews the worker's diff
    /// before bringing it in.
    fn check_path(&self, path: &str) -> Decision {
        let candidate = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            self.cwd.join(path)
        };
        match real_path_allowing_missing_tail(&candidate) {
            Some(real)
                if is_env_file(&real) || is_env_file(&candidate) =>
            {
                Decision::Deny(format!(
                    "herdr worker policy: {path} is an environment file; workers do not read or write those."
                ))
            }
            Some(real) if self.inside_roots(&real) => Decision::Allow,
            _ => Decision::Deny(self.outside_message(path)),
        }
    }

    fn inside_roots(&self, real: &Path) -> bool {
        self.roots.iter().any(|root| real.starts_with(root))
    }

    fn outside_message(&self, path: &str) -> String {
        format!(
            "herdr worker policy: {path} is outside the worker's directory {} and its temp dir {}.",
            self.roots[0].display(),
            self.roots[1].display()
        )
    }

    /// A glob pattern resolves against `base` (the tool's `path`, else the
    /// working directory); it must not climb out (`..`) or start at the home
    /// directory (`~`), and its literal prefix, the components before the
    /// first one with a glob character, joined to `base` (an absolute
    /// pattern replaces it), must resolve inside the roots, so neither an
    /// absolute pattern nor a symlinked directory leads outside.
    fn check_pattern(&self, pattern: &str, base: &Path) -> Result<(), String> {
        let components: Vec<&str> = pattern.split(['/', '\\']).collect();
        if pattern.starts_with('~') || components.contains(&"..") {
            return Err(format!(
                "herdr worker policy: the pattern {pattern} leaves the worker's directory."
            ));
        }
        let literal: PathBuf = Path::new(pattern)
            .components()
            .take_while(|component| {
                !component
                    .as_os_str()
                    .to_string_lossy()
                    .contains(['*', '?', '[', ']', '{', '}'])
            })
            .collect();
        match real_path_allowing_missing_tail(&base.join(literal)) {
            Some(real) if self.inside_roots(&real) => Ok(()),
            _ => Err(self.outside_message(pattern)),
        }
    }
}

/// `.env`, `.env.<anything>` or `.envrc`: files that usually hold secrets.
fn is_env_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == ".env" || name == ".envrc" || name.starts_with(".env."))
}

/// The input field of a file tool that holds a glob pattern, if any. `Grep`'s
/// `pattern` is a regular expression over file contents, not a path.
fn file_tool_pattern_field(tool_name: &str) -> Option<&'static str> {
    match tool_name {
        "Glob" => Some("pattern"),
        "Grep" => Some("glob"),
        _ => None,
    }
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

    struct Dirs {
        root: PathBuf,
        cwd: PathBuf,
        temp: PathBuf,
        outside: PathBuf,
    }

    impl Dirs {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir()
                .join(format!("herdr-worker-policy-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            let dirs = Self {
                cwd: root.join("repo"),
                temp: root.join("worker-tmp"),
                outside: root.join("outside"),
                root,
            };
            for dir in [&dirs.cwd, &dirs.temp, &dirs.outside] {
                std::fs::create_dir_all(dir).unwrap();
            }
            dirs
        }

        fn policy(&self) -> Policy {
            Policy::new(
                &self.cwd,
                &self.cwd.canonicalize().unwrap(),
                &self.temp.canonicalize().unwrap(),
            )
        }
    }

    impl Drop for Dirs {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn file_tools_inside_the_directory_or_the_temp_dir_are_allowed() {
        let dirs = Dirs::new("inside");
        std::fs::create_dir_all(dirs.cwd.join("src")).unwrap();
        let real = dirs.cwd.canonicalize().unwrap();
        let policy = dirs.policy();

        for (tool, input) in [
            ("Write", json!({"file_path": "new.txt"})),
            (
                "Write",
                json!({"file_path": dirs.cwd.join("src/new/deep.txt")}),
            ),
            ("Read", json!({"file_path": real.join("src")})),
            ("Write", json!({"file_path": dirs.temp.join("msg.txt")})),
            (
                "Write",
                json!({"file_path": dirs.temp.join("drafts/new/msg.txt")}),
            ),
            (
                "Read",
                json!({"file_path": dirs.temp.canonicalize().unwrap()}),
            ),
            ("Glob", json!({"pattern": "*.rs"})),
            ("Grep", json!({"pattern": "x", "path": dirs.temp})),
        ] {
            assert_eq!(
                policy.decide(tool, &input, None),
                Decision::Allow,
                "{tool} {input}"
            );
        }
    }

    #[test]
    fn paths_outside_both_roots_are_denied() {
        let dirs = Dirs::new("outside");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&dirs.outside, dirs.cwd.join("link-out")).unwrap();
            std::os::unix::fs::symlink(&dirs.outside, dirs.temp.join("link-out")).unwrap();
        }
        let policy = dirs.policy();

        let mut cases = vec![
            json!({"file_path": dirs.outside.join("x.txt")}),
            json!({"file_path": "../outside/x.txt"}),
            json!({"file_path": "missing/../../outside/x.txt"}),
            json!({"file_path": dirs.temp.join("../outside/x.txt")}),
            json!({"file_path": ""}),
        ];
        if cfg!(unix) {
            cases.push(json!({"file_path": "link-out/escape.txt"}));
            cases.push(json!({"file_path": dirs.temp.join("link-out/escape.txt")}));
        }
        for input in cases {
            assert!(
                matches!(policy.decide("Write", &input, None), Decision::Deny(_)),
                "{input}"
            );
        }
    }

    #[test]
    fn bash_classifier_escalations_and_other_tools_are_asked() {
        let dirs = Dirs::new("asked");
        let policy = dirs.policy();
        for (tool, input, reason_type) in [
            ("Bash", json!({"command": "git status"}), None),
            ("Bash", json!({"command": "git push origin master"}), None),
            ("Bash", json!({}), None),
            ("AskUserQuestion", json!({}), None),
            ("WebFetch", json!({"url": "https://example.com"}), None),
            ("mcp__x__y", json!({}), None),
            // In the worker's own directory, but the classifier asks for a
            // review of the transcript: the user decides.
            (
                "Write",
                json!({"file_path": "inside.txt"}),
                Some("classifier"),
            ),
            (
                "Bash",
                json!({"command": "git remote set-url origin x"}),
                Some("classifier"),
            ),
        ] {
            assert!(
                matches!(policy.decide(tool, &input, reason_type), Decision::Ask(_)),
                "{tool} {input} {reason_type:?}"
            );
        }
        assert_eq!(
            policy.decide(
                "Write",
                &json!({"file_path": "inside.txt"}),
                Some("workingDir")
            ),
            Decision::Allow
        );
    }

    #[test]
    fn every_file_tool_maps_its_path_field() {
        let dirs = Dirs::new("tools");
        let policy = dirs.policy();
        let outside = dirs.outside.join("x.txt");
        for (tool, field) in [
            ("Read", "file_path"),
            ("Write", "file_path"),
            ("Edit", "file_path"),
            ("MultiEdit", "file_path"),
            ("NotebookEdit", "notebook_path"),
            ("NotebookRead", "notebook_path"),
            ("LS", "path"),
        ] {
            assert_eq!(
                policy.decide(tool, &json!({ field: "inside.ipynb" }), None),
                Decision::Allow,
                "{tool}"
            );
            assert!(
                matches!(
                    policy.decide(tool, &json!({ field: outside }), None),
                    Decision::Deny(_)
                ),
                "{tool}"
            );
            if field != "path" {
                // The wrong field is no path at all: a required one is missing.
                assert!(
                    matches!(
                        policy.decide(tool, &json!({"path": "inside.txt"}), None),
                        Decision::Deny(_)
                    ),
                    "{tool}"
                );
            }
        }
    }

    #[test]
    fn environment_files_are_denied_wherever_they_are() {
        let dirs = Dirs::new("env");
        std::fs::create_dir_all(dirs.cwd.join("sub")).unwrap();
        std::fs::write(dirs.cwd.join("sub/.env"), "SECRET=1").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(dirs.cwd.join("sub/.env"), dirs.cwd.join("notes.txt")).unwrap();
        let policy = dirs.policy();
        let mut paths = vec![
            ".env".to_owned(),
            "sub/.env".to_owned(),
            "sub/deep/.env.local".to_owned(),
            ".envrc".to_owned(),
            dirs.temp.join(".env").display().to_string(),
        ];
        if cfg!(unix) {
            paths.push("notes.txt".to_owned());
        }
        for tool in ["Read", "Write", "Edit", "MultiEdit"] {
            for path in &paths {
                assert!(
                    matches!(
                        policy.decide(tool, &json!({"file_path": path}), None),
                        Decision::Deny(_)
                    ),
                    "{tool} {path}"
                );
            }
        }
        assert!(matches!(
            policy.decide("NotebookEdit", &json!({"notebook_path": ".env"}), None),
            Decision::Deny(_)
        ));
        for allowed in [".environment.md", "env", "sub/dotenv.txt"] {
            assert_eq!(
                policy.decide("Write", &json!({"file_path": allowed}), None),
                Decision::Allow,
                "{allowed}"
            );
        }
    }

    #[test]
    fn glob_patterns_stay_inside_the_roots() {
        let dirs = Dirs::new("patterns");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&dirs.outside, dirs.cwd.join("link-out")).unwrap();
        let policy = dirs.policy();
        let cwd_real = dirs.cwd.canonicalize().unwrap();

        for input in [
            json!({"pattern": "**/*.rs"}),
            json!({"pattern": "src/*.rs", "path": "."}),
            json!({"pattern": format!("{}/**/*.rs", cwd_real.display())}),
            json!({"pattern": format!("{}/*", dirs.temp.display())}),
        ] {
            assert_eq!(
                policy.decide("Glob", &input, None),
                Decision::Allow,
                "{input}"
            );
        }
        let mut denied = vec![
            json!({"pattern": format!("{}/*", dirs.outside.display())}),
            json!({"pattern": "/etc/*"}),
            json!({"pattern": "../outside/*"}),
            json!({"pattern": "src/../../outside/*"}),
            json!({"pattern": "**/../../*"}),
            json!({"pattern": "~/.ssh/*"}),
        ];
        if cfg!(unix) {
            denied.push(json!({"pattern": "link-out/*"}));
        }
        for input in denied {
            assert!(
                matches!(policy.decide("Glob", &input, None), Decision::Deny(_)),
                "{input}"
            );
        }

        // Grep's `pattern` is a regular expression; its `glob` is a path.
        assert_eq!(
            policy.decide("Grep", &json!({"pattern": "a/../b|/etc/.*"}), None),
            Decision::Allow
        );
        for glob in ["../**", "/etc/*", "~/*"] {
            assert!(
                matches!(
                    policy.decide("Grep", &json!({"pattern": "x", "glob": glob}), None),
                    Decision::Deny(_)
                ),
                "{glob}"
            );
        }
        assert_eq!(
            policy.decide("Grep", &json!({"pattern": "x", "glob": "*.rs"}), None),
            Decision::Allow
        );
    }
}
