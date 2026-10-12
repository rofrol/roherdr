//! Answers a headless worker's `can_use_tool` requests.
//!
//! Bash runs inside Claude Code's sandbox (`autoAllowBashIfSandboxed`,
//! `allowUnsandboxedCommands: false`), so the sandbox, not herdr, bounds what
//! it can write, read and reach (trial 3, T3-2). It still reaches herdr when a
//! user's or project's `ask` rule matches it, or through the CLI's built-in
//! check on compound commands ("This command requires approval",
//! `decision_reason_type: "other"`, e.g. `cd <worktree> && awk ...`), which
//! bypasses `autoAllowBashIfSandboxed`. Every Bash command runs inside the
//! sandbox anyway, so herdr allows these requests itself and journals them.
//! The file tools are not sandboxed, so herdr decides them here:
//!
//! - File tools are allowed when the real path of their target is inside the
//!   worker's directory or its temp dir and denied otherwise (a symlink that
//!   escapes is resolved and denied). Environment files (`.env`, `.env.*`,
//!   `.envrc`) are denied wherever they are. A `Glob` pattern or `Grep` glob
//!   that is absolute outside those roots, starts with `~` or has a `..`
//!   component is denied too.
//! - A call that would wait in the background is denied: Bash or an agent
//!   with `run_in_background`, `Monitor`, `ScheduleWakeup` and `CronCreate`.
//!   Nothing wakes a headless worker after its turn ends, so it would stop
//!   with its work undone ([`background_wait_denial`]). The same rule runs in
//!   herdr's PreToolUse hook, because the CLI asks herdr about a sandboxed
//!   Bash command only through that hook.
//! - A request the CLI's auto mode classifier escalated
//!   (`decision_reason_type: "classifier"`), a Bash request whose
//!   `blocked_path` is outside the roots, every `AskUserQuestion` and every
//!   other tool is asked of the user.
//!
//! The CLI's own `decision_reason` and `blocked_path` are hints only; it asks
//! even for paths it flags, so the real-path check here is the guard.

use std::path::{Component, Path, PathBuf};

use serde_json::Value;

/// Why a worker may not wait in the background.
pub(super) const BACKGROUND_WAIT_DENIAL: &str =
    "you are a headless worker: nothing wakes you after your turn ends, so run it in the foreground";

/// The tools that only wait in the background, whatever their input.
pub(super) const BACKGROUND_WAIT_TOOLS: &[&str] = &["Monitor", "ScheduleWakeup", "CronCreate"];

/// The tools that wait in the background when their input asks for it, and
/// whether they do when it does not say (an agent runs in the background by
/// default).
pub(super) const BACKGROUND_CAPABLE_TOOLS: &[(&str, bool)] =
    &[("Bash", false), ("Agent", true), ("Task", true)];

/// The denial for a call that would leave the worker waiting in the
/// background for something to wake it, which never comes: a headless
/// worker's turn ends and nothing starts the next one.
pub(super) fn background_wait_denial(tool_name: &str, input: &Value) -> Option<&'static str> {
    if BACKGROUND_WAIT_TOOLS.contains(&tool_name) {
        return Some(BACKGROUND_WAIT_DENIAL);
    }
    let (_, default) = BACKGROUND_CAPABLE_TOOLS
        .iter()
        .find(|(name, _)| *name == tool_name)?;
    let background = input
        .get("run_in_background")
        .and_then(Value::as_bool)
        .unwrap_or(*default);
    background.then_some(BACKGROUND_WAIT_DENIAL)
}

/// Why a worker may not kill processes by name, pattern or group.
pub(super) const PROCESS_KILL_DENIAL: &str = "herdr worker policy: never kill processes by name or pattern; end only processes you started, by the PID you recorded.";

/// The denial for a Bash command that kills processes the worker did not
/// start: `pkill`, `killall`, `xargs kill`, `kill` with a name, option,
/// process group, `0`, `-1` or a substitution. `kill <pid>` with literal
/// positive PIDs and `kill %<job>` stay allowed. A worker's broad kill once
/// ended the coordinator's waits (they share "herdr-" in their command lines).
pub(super) fn process_kill_denial(tool_name: &str, input: &Value) -> Option<&'static str> {
    if tool_name != "Bash" {
        return None;
    }
    let command = input.get("command").and_then(Value::as_str)?;
    command_kills_by_pattern(command, 0).then_some(PROCESS_KILL_DENIAL)
}

/// One shell word: its text with quotes removed, and whether it is not a
/// plain literal (it has a glob or a `$` that is not a plain parameter
/// expansion). `param` marks a word that is only a parameter expansion
/// (`$name`, `${name}`, `${name[@]}`, `$!`).
struct Word {
    text: String,
    dynamic: bool,
    param: bool,
}

/// Whether `text` is exactly one plain parameter expansion.
fn is_plain_parameter(text: &str) -> bool {
    let Some(rest) = text.strip_prefix('$') else {
        return false;
    };
    let name_ok = |n: &str| {
        !n.is_empty()
            && !n.starts_with(|c: char| c.is_ascii_digit())
            && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    };
    if rest == "!" {
        return true;
    }
    if let Some(inner) = rest.strip_prefix('{').and_then(|r| r.strip_suffix('}')) {
        let inner = inner
            .strip_suffix("[@]")
            .or(inner.strip_suffix("[*]"))
            .unwrap_or(inner);
        return name_ok(inner);
    }
    name_ok(rest)
}

/// Splits a command into simple-command segments of words. Operators
/// (`;`, `&`, `|`, newline, parentheses, backticks, `$(`) end a segment; a
/// substitution leaves a dynamic word in the segment it interrupts.
// The word macros reset state that the last call never reads again.
#[allow(unused_assignments)]
fn shell_segments(command: &str) -> Vec<Vec<Word>> {
    let chars: Vec<char> = command.chars().collect();
    let mut segments: Vec<Vec<Word>> = Vec::new();
    let mut words: Vec<Word> = Vec::new();
    let mut text = String::new();
    let mut dynamic = false;
    let mut dollar = false;
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut i = 0;
    macro_rules! end_word {
        () => {
            if in_word {
                let param = dollar && is_plain_parameter(&text);
                words.push(Word {
                    dynamic: dynamic || (dollar && !param),
                    param,
                    text: std::mem::take(&mut text),
                });
                dynamic = false;
                dollar = false;
                in_word = false;
            }
        };
    }
    macro_rules! end_segment {
        () => {
            end_word!();
            if !words.is_empty() {
                segments.push(std::mem::take(&mut words));
            }
        };
    }
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if quote == Some('\'') {
            if c == '\'' {
                quote = None;
            } else {
                text.push(c);
            }
        } else if c == '\\' {
            in_word = true;
            if quote.is_none() || matches!(next, Some('"' | '\\' | '$' | '`')) {
                if let Some(n) = next {
                    text.push(n);
                    i += 1;
                }
            } else {
                text.push(c);
            }
        } else if c == '`' || (c == '$' && next == Some('(')) {
            // A substitution, also inside double quotes.
            end_word!();
            words.push(Word {
                text: "$".into(),
                dynamic: true,
                param: false,
            });
            end_segment!();
            if c == '$' {
                i += 1;
            }
        } else if quote == Some('"') {
            if c == '"' {
                quote = None;
            } else {
                dollar |= c == '$';
                text.push(c);
            }
        } else if c == '\'' || c == '"' {
            quote = Some(c);
            in_word = true;
        } else if matches!(c, ';' | '&' | '|' | '\n' | '(' | ')') {
            end_segment!();
        } else if c.is_whitespace() {
            end_word!();
        } else {
            in_word = true;
            dollar |= c == '$';
            // `{` and `[` belong to a `${name[@]}` expansion; elsewhere
            // they make the word not a literal either way.
            dynamic |=
                matches!(c, '*' | '?') || (matches!(c, '[' | '{') && !(text.starts_with('$')));
            text.push(c);
        }
        i += 1;
    }
    end_segment!();
    segments
}

fn command_kills_by_pattern(command: &str, depth: u32) -> bool {
    if depth > 4 {
        return true;
    }
    // A substitution (`kill $(pgrep x)`) leaves a dynamic word in the
    // segment it interrupts, which `kill` then refuses.
    shell_segments(command)
        .iter()
        .any(|segment| segment_kills_by_pattern(segment, depth))
}

/// Skips assignments and wrappers (`sudo`, `env`, `command`, ...) to the
/// words of the command they run.
fn command_words(segment: &[Word]) -> &[Word] {
    let mut rest = segment;
    loop {
        let Some(first) = rest.first() else {
            return rest;
        };
        let name = first.text.rsplit('/').next().unwrap_or("");
        let is_assignment = !first.text.starts_with('-')
            && first.text.split_once('=').is_some_and(|(key, _)| {
                !key.is_empty() && key.chars().all(|c| c.is_alphanumeric() || c == '_')
            });
        let keyword = matches!(
            first.text.as_str(),
            "!" | "{" | "}" | "then" | "do" | "else" | "if" | "while" | "until"
        );
        if is_assignment || keyword {
            rest = &rest[1..];
        } else if matches!(
            name,
            "sudo"
                | "doas"
                | "env"
                | "command"
                | "exec"
                | "nohup"
                | "time"
                | "nice"
                | "builtin"
                | "stdbuf"
                | "timeout"
                | "setsid"
        ) {
            rest = &rest[1..];
            // Options of the wrapper (and the value of the few that take one).
            while let Some(word) = rest.first() {
                let takes_value = matches!(
                    word.text.as_str(),
                    "-u" | "-g" | "-n" | "-C" | "-S" | "-s" | "-k"
                ) && matches!(name, "sudo" | "doas" | "env" | "nice" | "timeout");
                if word.text.starts_with('-') && word.text != "--" {
                    rest = &rest[1..];
                    if takes_value && !rest.is_empty() {
                        rest = &rest[1..];
                    }
                } else if word.text == "--"
                    || (name == "timeout"
                        && word.text.chars().next().is_some_and(|c| c.is_ascii_digit()))
                {
                    rest = &rest[1..];
                } else {
                    break;
                }
            }
        } else {
            return rest;
        }
    }
}

fn segment_kills_by_pattern(segment: &[Word], depth: u32) -> bool {
    let words = command_words(segment);
    let Some(first) = words.first() else {
        return false;
    };
    let name = first.text.rsplit('/').next().unwrap_or("");
    let args = &words[1..];
    match name {
        "pkill" | "killall" | "killall5" | "skill" | "snice" => true,
        "kill" => !kill_args_are_own_pids(args),
        // `trap 'kill "$pid"' EXIT`: the handler is a script.
        "trap" => args
            .first()
            .is_some_and(|w| command_kills_by_pattern(&w.text, depth + 1)),
        "xargs" | "parallel" | "find" => args.iter().any(|word| {
            matches!(
                word.text.rsplit('/').next().unwrap_or(""),
                "kill" | "pkill" | "killall"
            )
        }),
        "bash" | "sh" | "zsh" | "dash" | "ksh" | "eval" => {
            let script = if name == "eval" {
                Some(
                    args.iter()
                        .map(|w| w.text.as_str())
                        .collect::<Vec<_>>()
                        .join(" "),
                )
            } else {
                args.iter()
                    .position(|w| w.text.starts_with('-') && w.text.contains('c'))
                    .and_then(|at| args.get(at + 1))
                    .map(|w| w.text.clone())
            };
            script.is_some_and(|script| command_kills_by_pattern(&script, depth + 1))
        }
        _ => false,
    }
}

/// Whether `kill`'s arguments are an optional signal and literal positive
/// PIDs or `%job`s (or a listing with `-l`).
fn kill_args_are_own_pids(args: &[Word]) -> bool {
    // Drop redirections (`2>/dev/null`, `> file`): they are not arguments.
    let mut kept: Vec<&Word> = Vec::new();
    let mut skip_target = false;
    for word in args {
        let text = word.text.trim_start_matches(|c: char| c.is_ascii_digit());
        if skip_target {
            skip_target = false;
        } else if text.starts_with('>') || text.starts_with('<') {
            skip_target = text.chars().all(|c| matches!(c, '>' | '<'));
        } else {
            kept.push(word);
        }
    }
    let args: Vec<&Word> = kept;
    let mut rest: &[&Word] = &args;
    if let Some(first) = rest.first() {
        let text = first.text.as_str();
        if !first.dynamic && matches!(text, "-l" | "-L") {
            return true;
        }
        if !first.dynamic && matches!(text, "-s" | "-n") {
            rest = rest.get(2..).unwrap_or(&[]);
        } else if !first.dynamic
            && text.len() > 1
            && text.starts_with('-')
            && text[1..].chars().all(|c| c.is_ascii_alphanumeric())
        {
            rest = &rest[1..];
        }
    }
    if rest.first().is_some_and(|w| w.text == "--") {
        rest = &rest[1..];
    }
    !rest.is_empty()
        && rest.iter().all(|word| {
            if word.dynamic {
                return false;
            }
            if word.param {
                return true;
            }
            let text = word.text.as_str();
            if let Some(job) = text.strip_prefix('%') {
                return job.is_empty() || job == "%" || job.chars().all(|c| c.is_ascii_digit());
            }
            !text.is_empty()
                && text.chars().all(|c| c.is_ascii_digit())
                && text.chars().any(|c| c != '0')
        })
}

/// Why the policy asks about a request the CLI's auto mode classifier
/// escalated.
pub(super) const CLASSIFIER_ASK: &str =
    "the auto mode classifier blocked repeated actions and asks for a review";

/// The tools whose requests a todo run's `answer_question` decision call
/// may allow: they only read (the web included) and write nothing.
pub(super) const MODEL_DECIDABLE_TOOLS: &[&str] = &["WebFetch", "WebSearch"];

/// Whether a question the policy left needs the user: a request the
/// classifier escalated, a Bash command naming a path outside the worker's
/// folders, and every tool not in [`MODEL_DECIDABLE_TOOLS`] (a tool herdr
/// does not know may write or send anything). A decision call may only
/// escalate it; an `AskUserQuestion` it may answer.
pub(super) fn needs_the_user(question: &crate::api::schema::WorkerQuestion) -> bool {
    use crate::api::schema::WorkerQuestionKind;
    match question.kind {
        WorkerQuestionKind::Choice => false,
        WorkerQuestionKind::Approval => {
            question.reason.as_deref() == Some(CLASSIFIER_ASK)
                || !MODEL_DECIDABLE_TOOLS.contains(&question.tool_name.as_str())
        }
        WorkerQuestionKind::Unknown => true,
    }
}

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
    /// None means credential policy was unreadable: fail closed for file tools.
    api_key: Option<Option<PathBuf>>,
}

impl Policy {
    /// `cwd` resolves relative paths; `cwd_real` and `temp_real` are the
    /// canonical roots.
    pub(super) fn new(cwd: &Path, cwd_real: &Path, temp_real: &Path) -> Self {
        Self {
            cwd: cwd.to_owned(),
            roots: vec![cwd_real.to_owned(), temp_real.to_owned()],
            api_key: super::configured_anthropic_key_path().ok(),
        }
    }

    /// The roots in effect, for the journal.
    pub(super) fn roots(&self) -> Vec<String> {
        self.roots
            .iter()
            .map(|root| root.display().to_string())
            .collect()
    }

    /// Decides one `can_use_tool` request. `reason_type` and `blocked_path`
    /// are the request's `decision_reason_type` and `blocked_path`.
    pub(super) fn decide(
        &self,
        tool_name: &str,
        input: &Value,
        reason_type: Option<&str>,
        blocked_path: Option<&str>,
    ) -> Decision {
        if let Some(reason) = background_wait_denial(tool_name, input) {
            return Decision::Deny(reason.to_owned());
        }
        if let Some(reason) = process_kill_denial(tool_name, input) {
            return Decision::Deny(reason.to_owned());
        }
        if let Some((field, _)) = file_tool_path_field(tool_name) {
            let path = input.get(field).and_then(Value::as_str).unwrap_or(".");
            let protected = match &self.api_key {
                None => true,
                Some(None) => false,
                Some(Some(key)) => real_path_allowing_missing_tail(&self.cwd.join(path))
                    .is_none_or(|real| key.starts_with(real)),
            };
            if matches!(self.api_key, Some(Some(_))) {
                if let Some(pattern) = file_tool_pattern_field(tool_name)
                    .and_then(|field| input.get(field))
                    .and_then(Value::as_str)
                {
                    if let Err(message) = self.check_pattern(pattern, &self.cwd.join(path)) {
                        return Decision::Deny(message);
                    }
                }
            }
            if protected {
                // Credential boundaries cannot become user permission prompts.
                return Decision::Deny(
                    "herdr worker policy: credential access is forbidden.".into(),
                );
            }
        }
        if reason_type == Some("classifier") {
            return Decision::Ask(CLASSIFIER_ASK.into());
        }
        if tool_name == "AskUserQuestion" {
            return Decision::Ask("a question for the user".into());
        }
        if tool_name == "Bash" {
            return self.decide_bash(blocked_path);
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

    /// Bash runs in the sandbox, which bounds it whatever herdr answers, so a
    /// request is allowed unless it names a path outside the roots: that one
    /// the user sees, because the command aims outside the worker's folders.
    fn decide_bash(&self, blocked_path: Option<&str>) -> Decision {
        let Some(path) = blocked_path.filter(|path| !path.is_empty()) else {
            return Decision::Allow;
        };
        let candidate = if Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            self.cwd.join(path)
        };
        match real_path_allowing_missing_tail(&candidate) {
            Some(real) if self.inside_roots(&real) => Decision::Allow,
            _ => Decision::Ask(format!(
                "a Bash command that names {path}, outside the worker's directory and its temp dir"
            )),
        }
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
        let Some(key) = &self.api_key else {
            return false;
        };
        if key
            .as_ref()
            .is_some_and(|key| real == key || key.starts_with(real))
        {
            // Refuse directory/glob reads as well as the key itself.
            return false;
        }
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
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let name = existing.file_name()?;
                match existing.components().next_back() {
                    Some(Component::Normal(_)) => tail.push(name.to_os_string()),
                    _ => return None,
                }
                existing = existing.parent()?;
            }
            // A denied canonicalization is not a missing file. In particular,
            // never treat a symlink into a sandbox-denied credential as a new
            // file inside an allowed root.
            Err(_) => return None,
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
                policy.decide(tool, &input, None, None),
                Decision::Allow,
                "{tool} {input}"
            );
        }
    }

    #[test]
    fn anthropic_key_is_denied_even_inside_a_worker_root_and_directory_scan() {
        let dirs = Dirs::new("api-key");
        let key = dirs.cwd.join("private-key");
        std::fs::write(&key, "offline-test-credential").unwrap();
        let mut policy = dirs.policy();
        policy.api_key = Some(Some(key.canonicalize().unwrap()));
        for (tool, input) in [
            ("Read", json!({"file_path": key})),
            ("Write", json!({"file_path": key})),
            ("Read", json!({"file_path": dirs.cwd})),
            ("Glob", json!({"pattern": "*"})),
            ("Grep", json!({"pattern": "x", "path": dirs.cwd})),
        ] {
            assert!(matches!(
                policy.decide(tool, &input, None, None),
                Decision::Deny(_)
            ));
        }
        assert!(matches!(
            policy.decide("Read", &json!({"file_path": key}), Some("classifier"), None),
            Decision::Deny(_)
        ));
        let src = dirs.cwd.join("src");
        std::fs::create_dir_all(&src).unwrap();
        assert!(matches!(
            policy.decide(
                "Glob",
                &json!({"path":src,"pattern":key}),
                Some("classifier"),
                None
            ),
            Decision::Deny(_)
        ));
        let mut settings = super::super::worker_settings(&dirs.cwd, &dirs.temp, &[]);
        super::super::deny_worker_api_key(&mut settings, &key).unwrap();
        assert!(settings["sandbox"]["filesystem"]["denyRead"]
            .as_array()
            .unwrap()
            .contains(&json!(key)));
        assert!(settings["permissions"]["deny"]
            .as_array()
            .unwrap()
            .contains(&json!(format!("Read({})", key.display()))));
        assert_eq!(settings["sandbox"]["network"]["allowedDomains"], json!([]));
        assert_eq!(settings["sandbox"]["network"]["strictAllowlist"], true);
        assert_eq!(settings["sandbox"]["allowUnsandboxedCommands"], false);
        policy.api_key = None;
        assert!(matches!(
            policy.decide("Read", &json!({"file_path": "new.txt"}), None, None),
            Decision::Deny(_)
        ));
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
                matches!(
                    policy.decide("Write", &input, None, None),
                    Decision::Deny(_)
                ),
                "{input}"
            );
        }
    }

    #[test]
    fn classifier_escalations_questions_and_other_tools_are_asked() {
        let dirs = Dirs::new("asked");
        let policy = dirs.policy();
        for (tool, input, reason_type) in [
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
                matches!(
                    policy.decide(tool, &input, reason_type, None),
                    Decision::Ask(_)
                ),
                "{tool} {input} {reason_type:?}"
            );
        }
        assert_eq!(
            policy.decide(
                "Write",
                &json!({"file_path": "inside.txt"}),
                Some("workingDir"),
                None
            ),
            Decision::Allow
        );
    }

    #[test]
    fn sandboxed_bash_requests_are_allowed_unless_they_name_a_path_outside() {
        let dirs = Dirs::new("bash");
        let policy = dirs.policy();
        let cwd = dirs.cwd.display().to_string();
        // The shape seen on worker `w2`: the CLI's built-in check on
        // compound commands, which `autoAllowBashIfSandboxed` does not cover.
        let compound = json!({
            "command": format!("cd {cwd} && awk 'NR<5' TODO.md"),
            "description": "Show the first lines",
        });
        for (input, reason_type, blocked_path) in [
            (compound.clone(), Some("other"), None),
            (json!({"command": "git status"}), None, None),
            (json!({}), None, None),
            (json!({"command": "ls"}), Some("other"), Some("")),
            (json!({"command": "ls"}), Some("other"), Some("src/new.rs")),
            (json!({"command": "ls"}), Some("other"), Some(cwd.as_str())),
        ] {
            assert_eq!(
                policy.decide("Bash", &input, reason_type, blocked_path),
                Decision::Allow,
                "{input} {reason_type:?} {blocked_path:?}"
            );
        }
        let outside = dirs.outside.join("x.txt").display().to_string();
        for blocked_path in [outside.as_str(), "../outside/x.txt", "/etc/hosts"] {
            assert!(
                matches!(
                    policy.decide("Bash", &compound, Some("other"), Some(blocked_path)),
                    Decision::Ask(_)
                ),
                "{blocked_path}"
            );
        }
    }

    #[test]
    fn waiting_in_the_background_is_denied() {
        let dirs = Dirs::new("background");
        let policy = dirs.policy();
        let denied = Decision::Deny(BACKGROUND_WAIT_DENIAL.into());
        for (tool, input) in [
            (
                "Bash",
                json!({"command": "just test", "run_in_background": true}),
            ),
            ("Monitor", json!({"command": "tail -f log"})),
            ("ScheduleWakeup", json!({})),
            ("CronCreate", json!({})),
            ("Agent", json!({"prompt": "look"})),
            (
                "Agent",
                json!({"prompt": "look", "run_in_background": true}),
            ),
        ] {
            assert_eq!(
                policy.decide(tool, &input, None, None),
                denied,
                "{tool} {input}"
            );
        }
        for input in [
            json!({"command": "just test"}),
            json!({"command": "just test", "run_in_background": false}),
        ] {
            assert_eq!(
                policy.decide("Bash", &input, None, None),
                Decision::Allow,
                "{input}"
            );
        }
        assert_eq!(
            background_wait_denial("Agent", &json!({"run_in_background": false})),
            None
        );
    }

    #[test]
    fn process_kills_by_pattern_are_denied_and_own_pids_allowed() {
        let denied = [
            r#"pkill -f "herdr-""#,
            "pkill -f nextest",
            "killall cargo",
            "/usr/bin/pkill foo",
            "sudo pkill foo",
            "env FOO=1 pkill foo",
            "command killall foo",
            "ls; pkill foo",
            "true && pkill foo",
            "false || pkill foo",
            "pgrep -f foo | xargs kill",
            "pgrep foo | xargs -r kill -9",
            "kill -f foo",
            "kill foo",
            "kill -9 -1234",
            "kill -TERM -- -1234",
            "kill 0",
            "kill -1",
            "kill -9 -1",
            "kill $(pgrep foo)",
            "kill `pgrep foo`",
            "kill -9 $(pgrep -f foo)",
            "echo $(pkill foo)",
            "bash -c 'pkill foo'",
            "kill 12 foo",
            "kill $(pgrep x)",
            "kill `pgrep x`",
            "kill \"$(pgrep x)\"",
            "kill $pid*",
            "kill a$pid",
            "trap 'pkill foo' EXIT",
            "trap 'kill $(pgrep x)' EXIT",
        ];
        for command in denied {
            assert_eq!(
                process_kill_denial("Bash", &json!({"command": command})),
                Some(PROCESS_KILL_DENIAL),
                "{command}"
            );
        }
        for command in [
            "kill 1234",
            "kill -TERM 1234",
            "kill -9 1234 5678",
            "kill -s TERM 1234",
            "kill %1",
            "kill -9 %1",
            "sleep 1 & kill 4242; wait",
            "echo pkill foo",
            "cargo nextest run -E 'test(/workers::/)'",
            "pgrep foo",
            "kill -l",
            "pid=$!; sleep 1; kill \"$pid\"",
            "kill -TERM $pid",
            "kill $!",
            "kill ${pid}",
            r#"kill "${pids[@]}" 2>/dev/null"#,
        ] {
            assert_eq!(
                process_kill_denial("Bash", &json!({"command": command})),
                None,
                "{command}"
            );
        }
        assert_eq!(
            process_kill_denial("Read", &json!({"command": "pkill x"})),
            None
        );
    }

    #[test]
    fn the_documented_stress_loop_is_allowed() {
        let doc = include_str!("../../AGENTS.md");
        let block = doc
            .split("```bash")
            .map(|b| b.split("```").next().unwrap_or(""))
            .find(|b| b.contains("yes > /dev/null"))
            .expect("the stress loop block");
        assert!(block.contains("trap 'kill \"${pids[@]}\" 2>/dev/null' EXIT INT TERM"));
        assert_eq!(
            process_kill_denial("Bash", &json!({"command": block})),
            None
        );
    }

    #[test]
    fn decide_denies_process_kills_for_every_worker_kind() {
        let dirs = Dirs::new("kill");
        let policy = dirs.policy();
        assert_eq!(
            policy.decide("Bash", &json!({"command": "pkill -f nextest"}), None, None),
            Decision::Deny(PROCESS_KILL_DENIAL.into())
        );
        // pi's `bash` is named as Claude's by `claude_tool` before it gets here.
        let (tool, input) =
            crate::workers::pi::claude_tool("bash", &json!({"command": "pkill -f herdr-"}));
        assert_eq!(
            policy.decide(&tool, &input, None, None),
            Decision::Deny(PROCESS_KILL_DENIAL.into())
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
                policy.decide(tool, &json!({ field: "inside.ipynb" }), None, None),
                Decision::Allow,
                "{tool}"
            );
            assert!(
                matches!(
                    policy.decide(tool, &json!({ field: outside }), None, None),
                    Decision::Deny(_)
                ),
                "{tool}"
            );
            if field != "path" {
                // The wrong field is no path at all: a required one is missing.
                assert!(
                    matches!(
                        policy.decide(tool, &json!({"path": "inside.txt"}), None, None),
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
                        policy.decide(tool, &json!({"file_path": path}), None, None),
                        Decision::Deny(_)
                    ),
                    "{tool} {path}"
                );
            }
        }
        assert!(matches!(
            policy.decide(
                "NotebookEdit",
                &json!({"notebook_path": ".env"}),
                None,
                None
            ),
            Decision::Deny(_)
        ));
        for allowed in [".environment.md", "env", "sub/dotenv.txt"] {
            assert_eq!(
                policy.decide("Write", &json!({"file_path": allowed}), None, None),
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
                policy.decide("Glob", &input, None, None),
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
                matches!(policy.decide("Glob", &input, None, None), Decision::Deny(_)),
                "{input}"
            );
        }

        // Grep's `pattern` is a regular expression; its `glob` is a path.
        assert_eq!(
            policy.decide("Grep", &json!({"pattern": "a/../b|/etc/.*"}), None, None),
            Decision::Allow
        );
        for glob in ["../**", "/etc/*", "~/*"] {
            assert!(
                matches!(
                    policy.decide("Grep", &json!({"pattern": "x", "glob": glob}), None, None),
                    Decision::Deny(_)
                ),
                "{glob}"
            );
        }
        assert_eq!(
            policy.decide("Grep", &json!({"pattern": "x", "glob": "*.rs"}), None, None),
            Decision::Allow
        );
    }
}
