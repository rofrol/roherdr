//! Answers a headless worker's `can_use_tool` requests.
//!
//! - File tools are allowed when the real path of their target is inside the
//!   worker's directory and denied otherwise (a symlink that escapes is
//!   resolved and denied). Writes into the directory's `.herdr/`, which holds
//!   this policy's own extension file, go to the user instead.
//! - Bash runs without asking only when the command is one simple command
//!   whose words start with an allowed rule: `git` with a known local
//!   subcommand, `just check`, `just test`, `cargo test`, `cargo check`, plus
//!   the rules of the repository's `.herdr/worker-allow.toml`, read once when
//!   the worker starts. Never from the repository's Claude settings.
//! - Everything else, and every `AskUserQuestion`, is asked of the user.
//!
//! The list decides what needs no click; it is not a sandbox: `cargo test`
//! and `just` run code the worker can edit.
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

/// The repository's extension file, relative to the worker's directory.
pub(super) const ALLOW_FILE: &str = ".herdr/worker-allow.toml";

/// Rules every repository gets: a command whose words start with one of
/// these runs without asking. `git` has its own check, [`git_is_allowed`].
const DEFAULT_RULES: &[&str] = &["just check", "just test", "cargo test", "cargo check"];

/// Git subcommands that only touch the local repository (and `fetch`/`pull`,
/// which only read from a remote). A list instead of a deny list, so a user's
/// alias (`git p` for `push`) is asked, not run: git never lets an alias
/// shadow a builtin.
const GIT_SUBCOMMANDS: &[&str] = &[
    "add",
    "blame",
    "branch",
    "cat-file",
    "checkout",
    "cherry-pick",
    "commit",
    "describe",
    "diff",
    "fetch",
    "grep",
    "log",
    "ls-files",
    "merge",
    "merge-base",
    "mv",
    "pull",
    "range-diff",
    "rebase",
    "reflog",
    "reset",
    "restore",
    "rev-list",
    "rev-parse",
    "revert",
    "rm",
    "shortlog",
    "show",
    "show-ref",
    "stash",
    "status",
    "switch",
    "tag",
];

#[derive(Debug, Clone)]
pub(super) struct Policy {
    cwd: PathBuf,
    cwd_real: PathBuf,
    /// Word prefixes of the commands allowed without asking, besides `git`.
    rules: Vec<Vec<String>>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AllowFile {
    #[serde(default)]
    allow: Vec<String>,
}

impl Policy {
    /// The default rules plus the repository's extension file, if any.
    /// Returns the policy and a warning per rule or file it could not use.
    pub(super) fn load(cwd: &Path, cwd_real: &Path) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        let mut extra = Vec::new();
        match std::fs::read_to_string(cwd_real.join(ALLOW_FILE)) {
            Ok(text) => match toml::from_str::<AllowFile>(&text) {
                Ok(file) => extra = file.allow,
                Err(error) => warnings.push(format!("{ALLOW_FILE}: {error}")),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => warnings.push(format!("{ALLOW_FILE}: {error}")),
        }
        let (policy, mut rule_warnings) = Self::with_rules(cwd, cwd_real, &extra);
        warnings.append(&mut rule_warnings);
        (policy, warnings)
    }

    fn with_rules(cwd: &Path, cwd_real: &Path, extra: &[String]) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        let mut rules = Vec::new();
        for rule in DEFAULT_RULES
            .iter()
            .copied()
            .chain(extra.iter().map(String::as_str))
        {
            match split_command(rule) {
                Ok(words) if words.first().is_some_and(|word| word == "git") => warnings.push(
                    format!("rule `{rule}` ignored: git commands are decided by herdr's git list"),
                ),
                Ok(words) => rules.push(words),
                Err(reason) => warnings.push(format!("rule `{rule}` ignored: {reason}")),
            }
        }
        (
            Self {
                cwd: cwd.to_owned(),
                cwd_real: cwd_real.to_owned(),
                rules,
            },
            warnings,
        )
    }

    /// The rules in effect, for the journal.
    pub(super) fn rules(&self) -> Vec<String> {
        std::iter::once("git <local subcommand>".to_owned())
            .chain(self.rules.iter().map(|words| words.join(" ")))
            .collect()
    }

    /// Decides one `can_use_tool` request.
    pub(super) fn decide(&self, tool_name: &str, input: &Value) -> Decision {
        if tool_name == "AskUserQuestion" {
            return Decision::Ask("a question for the user".into());
        }
        if tool_name == "Bash" {
            return self.decide_bash(input);
        }
        let Some((field, required)) = file_tool_path_field(tool_name) else {
            return Decision::Ask(format!("{tool_name} is not decided by herdr's policy"));
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
            self.cwd.join(path)
        };
        match real_path_allowing_missing_tail(&candidate) {
            Some(real) if real.starts_with(self.cwd_real.join(".herdr")) && writes(tool_name) => {
                Decision::Ask(format!("{tool_name} changes herdr's worker settings"))
            }
            Some(real) if real.starts_with(&self.cwd_real) => Decision::Allow,
            _ => Decision::Deny(format!(
                "herdr worker policy: {path} is outside the worker's directory {}.",
                self.cwd_real.display()
            )),
        }
    }

    fn decide_bash(&self, input: &Value) -> Decision {
        let Some(command) = input.get("command").and_then(Value::as_str) else {
            return Decision::Ask("Bash without a command".into());
        };
        let words = match split_command(command) {
            Ok(words) => words,
            Err(reason) => return Decision::Ask(reason),
        };
        if words[0] == "git" {
            return if git_is_allowed(&words) {
                Decision::Allow
            } else {
                Decision::Ask("not a local git subcommand on the list".into())
            };
        }
        if self.rules.iter().any(|rule| words.starts_with(rule)) {
            Decision::Allow
        } else {
            Decision::Ask("not on the worker's command list".into())
        }
    }
}

fn writes(tool_name: &str) -> bool {
    matches!(tool_name, "Write" | "Edit" | "MultiEdit" | "NotebookEdit")
}

/// `git <subcommand> ...` with a subcommand from [`GIT_SUBCOMMANDS`]. A global
/// option before the subcommand (`-c`, `-C`, `--git-dir`) is refused, so
/// configuration and the repository cannot be changed from the command line.
fn git_is_allowed(words: &[String]) -> bool {
    words
        .get(1)
        .is_some_and(|subcommand| GIT_SUBCOMMANDS.contains(&subcommand.as_str()))
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

/// Splits one simple shell command into the words the shell would pass to
/// the program, or says why it is not one. Anything the shell would expand
/// or treat as syntax is refused outside quotes (`;`, `&`, `|`, `<`, `>`,
/// `(`, `)`, `` ` ``, `$`, `\`, globs, `~`, `#`, `{`, `}`), and `$`, `` ` ``,
/// `\` and `!` inside double quotes; single quotes are literal. A newline is
/// refused anywhere, so a command is always one line.
pub(super) fn split_command(command: &str) -> Result<Vec<String>, String> {
    #[derive(PartialEq)]
    enum Quote {
        None,
        Single,
        Double,
    }
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut quote = Quote::None;
    for c in command.chars() {
        if c == '\n' || c == '\r' {
            return Err("more than one line".into());
        }
        match quote {
            Quote::Single => {
                if c == '\'' {
                    quote = Quote::None;
                } else {
                    word.push(c);
                }
            }
            Quote::Double => match c {
                '"' => quote = Quote::None,
                '$' | '`' | '\\' | '!' => {
                    return Err(format!(
                        "`{c}` inside double quotes is expanded by the shell"
                    ))
                }
                _ => word.push(c),
            },
            Quote::None => match c {
                ' ' | '\t' => {
                    if in_word {
                        words.push(std::mem::take(&mut word));
                        in_word = false;
                    }
                }
                '\'' => {
                    quote = Quote::Single;
                    in_word = true;
                }
                '"' => {
                    quote = Quote::Double;
                    in_word = true;
                }
                ';' | '&' | '|' | '<' | '>' | '(' | ')' | '`' | '$' | '\\' | '*' | '?' | '['
                | ']' | '~' | '#' | '{' | '}' | '!' => {
                    return Err(format!("`{c}` makes it more than one simple command"))
                }
                _ => {
                    word.push(c);
                    in_word = true;
                }
            },
        }
    }
    if quote != Quote::None {
        return Err("an unclosed quote".into());
    }
    if in_word {
        words.push(word);
    }
    if words.is_empty() {
        return Err("an empty command".into());
    }
    Ok(words)
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

    fn policy(cwd: &Path) -> Policy {
        let real = cwd.canonicalize().unwrap();
        let (policy, warnings) = Policy::load(cwd, &real);
        assert!(warnings.is_empty(), "{warnings:?}");
        policy
    }

    fn bash(policy: &Policy, command: &str) -> Decision {
        policy.decide("Bash", &json!({ "command": command }))
    }

    #[test]
    fn file_tools_inside_the_directory_are_allowed() {
        let root = temp_dir("inside");
        let cwd = root.join("repo");
        std::fs::create_dir_all(cwd.join("src")).unwrap();
        let real = cwd.canonicalize().unwrap();
        let policy = policy(&cwd);

        for (tool, input) in [
            ("Write", json!({"file_path": "new.txt"})),
            ("Write", json!({"file_path": cwd.join("src/new/deep.txt")})),
            ("Read", json!({"file_path": real.join("src")})),
            ("Read", json!({"file_path": ".herdr/worker-allow.toml"})),
            ("Glob", json!({"pattern": "*.rs"})),
            ("Grep", json!({"pattern": "x", "path": "src"})),
        ] {
            assert_eq!(
                policy.decide(tool, &input),
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
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, cwd.join("link-out")).unwrap();
        let policy = policy(&cwd);

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
                matches!(policy.decide("Write", &input), Decision::Deny(_)),
                "{input}"
            );
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn writes_to_the_policy_file_are_asked() {
        let cwd = temp_dir("herdr-dir");
        let policy = policy(&cwd);
        for tool in ["Write", "Edit"] {
            assert!(matches!(
                policy.decide(tool, &json!({"file_path": ALLOW_FILE})),
                Decision::Ask(_)
            ));
        }
        let _ = std::fs::remove_dir_all(cwd);
    }

    #[test]
    fn listed_bash_commands_are_allowed() {
        let cwd = temp_dir("bash-allowed");
        let policy = policy(&cwd);
        for command in [
            "git status",
            "git add src/main.rs",
            "git commit -m 'feat(x): a; b | c'",
            "git commit -m \"fix(y): z\"",
            "git switch -c topic",
            "  git   log  --oneline ",
            "just check",
            "just test",
            "cargo test -p herdr workers::",
            "cargo check",
        ] {
            assert_eq!(bash(&policy, command), Decision::Allow, "{command}");
        }
        let _ = std::fs::remove_dir_all(cwd);
    }

    #[test]
    fn other_and_compound_bash_commands_are_asked() {
        let cwd = temp_dir("bash-asked");
        let policy = policy(&cwd);
        for command in [
            "git push",
            "git push origin master",
            "git config user.name x",
            "git -c core.hooksPath=/tmp commit",
            "git -C /elsewhere status",
            "git remote add x y",
            "git p",
            "git",
            "git status; rm -rf /",
            "git status && rm -rf /",
            "git status || true",
            "git status | sh",
            "git status > /tmp/out",
            "git status < /dev/null",
            "git status &",
            "git log $(rm -rf /)",
            "git log `rm -rf /`",
            "git log \"$HOME\"",
            "git log\nrm -rf /",
            "git commit -m 'unclosed",
            "git add *",
            "just deploy",
            "cargo build",
            "cargo",
            "rm -rf target",
            "FOO=bar cargo test",
            "",
        ] {
            assert!(
                matches!(bash(&policy, command), Decision::Ask(_)),
                "{command:?}"
            );
        }
        assert!(matches!(
            policy.decide("Bash", &json!({})),
            Decision::Ask(_)
        ));
        let _ = std::fs::remove_dir_all(cwd);
    }

    #[test]
    fn the_repository_file_extends_the_list_but_not_git() {
        let cwd = temp_dir("extension");
        std::fs::create_dir_all(cwd.join(".herdr")).unwrap();
        std::fs::write(
            cwd.join(ALLOW_FILE),
            "allow = [\"npm test\", \"make lint\", \"git push\", \"make a; b\"]\n",
        )
        .unwrap();
        let real = cwd.canonicalize().unwrap();
        let (policy, warnings) = Policy::load(&cwd, &real);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings.iter().any(|warning| warning.contains("git push")));
        assert!(warnings.iter().any(|warning| warning.contains("make a; b")));
        assert_eq!(bash(&policy, "npm test -- --watch=false"), Decision::Allow);
        assert_eq!(bash(&policy, "make lint"), Decision::Allow);
        assert!(matches!(bash(&policy, "make"), Decision::Ask(_)));
        assert!(matches!(bash(&policy, "git push"), Decision::Ask(_)));
        assert!(policy.rules().contains(&"npm test".to_owned()));

        std::fs::write(cwd.join(ALLOW_FILE), "allow = \"npm test\"\n").unwrap();
        let (policy, warnings) = Policy::load(&cwd, &real);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(matches!(bash(&policy, "npm test"), Decision::Ask(_)));
        assert_eq!(bash(&policy, "cargo test"), Decision::Allow);
        let _ = std::fs::remove_dir_all(cwd);
    }

    #[test]
    fn other_tools_and_questions_are_asked() {
        let cwd = temp_dir("other");
        let policy = policy(&cwd);
        for tool in ["AskUserQuestion", "WebFetch", "mcp__x__y"] {
            assert!(
                matches!(policy.decide(tool, &json!({})), Decision::Ask(_)),
                "{tool}"
            );
        }
        let _ = std::fs::remove_dir_all(cwd);
    }

    #[test]
    fn split_command_keeps_quoted_words() {
        assert_eq!(
            split_command(r#"git commit -m 'a b' -m "c d" ''"#).unwrap(),
            vec!["git", "commit", "-m", "a b", "-m", "c d", ""]
        );
    }
}
