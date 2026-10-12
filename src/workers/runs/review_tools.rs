//! The read-only repository tools of a review call, and the diff lines its
//! citation check uses.
//!
//! The tools (`read_file`, `grep`, `git_log`) are executed by herdr itself
//! against the worker's commit, read through git objects: never the shared
//! checkout's working tree, so another session's uncommitted edit never
//! shows and a symlink never leads out. A path is relative to the
//! repository root, with no `..`, no absolute form and no symlink at its
//! end. The budget is counted, never timed: [`MAX_CALLS`] calls and
//! [`MAX_BYTES`] of results per review, each result cut at [`RESULT_MAX`];
//! when it is spent the model gets a refusal that tells it to answer.

use std::path::Path;

use serde_json::{json, Value};

use super::git;

/// Tool calls one review may run.
pub(super) const MAX_CALLS: usize = 12;
/// Bytes of tool results one review may take in all.
pub(super) const MAX_BYTES: usize = 120_000;
/// One tool result is cut at this many bytes.
const RESULT_MAX: usize = 10_000;
/// Lines `read_file` returns at most.
const READ_LINES: usize = 400;
/// Matches `grep` returns at most.
const GREP_MATCHES: usize = 80;
/// Commits `git_log` returns at most.
const LOG_COMMITS: usize = 20;
/// The longest pattern or glob a tool takes.
const ARG_MAX: usize = 200;

/// One tool call as recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::workers) struct ToolCall {
    pub(in crate::workers) name: String,
    pub(in crate::workers) arguments: Value,
    pub(in crate::workers) bytes: usize,
    /// The call failed or was refused (budget spent); its text says why.
    pub(in crate::workers) error: bool,
}

impl ToolCall {
    pub(in crate::workers) fn to_json(&self) -> Value {
        json!({"name": self.name, "arguments": self.arguments, "bytes": self.bytes,
               "error": self.error})
    }
}

/// The tools of one review, over one commit of one repository.
pub(in crate::workers) struct RepoTools<'a> {
    repo: &'a Path,
    commit: String,
    pub(in crate::workers) calls: Vec<ToolCall>,
    executed: usize,
    bytes: usize,
}

/// The tool definitions the API gets.
pub(in crate::workers) fn definitions() -> Value {
    json!([
        {"name": "read_file",
         "description": "Read lines of a file as it is in the reviewed commit. The path is relative to the repository root.",
         "input_schema": {"type": "object", "additionalProperties": false, "required": ["path"],
            "properties": {
                "path": {"type": "string"},
                "start_line": {"type": "integer", "description": "first line, 1-based (default 1)"},
                "end_line": {"type": "integer", "description": "last line, at most 400 lines after the first"}}}},
        {"name": "grep",
         "description": "Search the files of the reviewed commit; the results are capped.",
         "input_schema": {"type": "object", "additionalProperties": false, "required": ["pattern"],
            "properties": {
                "pattern": {"type": "string"},
                "path_glob": {"type": "string", "description": "limit to paths matching this glob, e.g. src/**/*.rs"},
                "regex": {"type": "boolean", "description": "true: POSIX extended regex; default false: fixed string"}}}},
        {"name": "git_log",
         "description": "The latest commits that touched a path (or the repository when the path is empty), up to the reviewed commit.",
         "input_schema": {"type": "object", "additionalProperties": false,
            "properties": {"path": {"type": "string"}}}}
    ])
}

impl<'a> RepoTools<'a> {
    /// The tools over `commit` (a hex object name) of `repo`.
    pub(in crate::workers) fn new(repo: &'a Path, commit: &str) -> Option<Self> {
        let hex = commit.len() >= 7 && commit.chars().all(|c| c.is_ascii_hexdigit());
        hex.then(|| Self {
            repo,
            commit: commit.to_owned(),
            calls: Vec::new(),
            executed: 0,
            bytes: 0,
        })
    }

    /// Whether the budget is spent: the model must answer.
    pub(in crate::workers) fn spent(&self) -> bool {
        self.executed >= MAX_CALLS || self.bytes >= MAX_BYTES
    }

    /// Runs one tool call and returns its result text and whether it is an
    /// error. Counts it, and records it in [`Self::calls`].
    pub(in crate::workers) fn call(&mut self, name: &str, arguments: &Value) -> (String, bool) {
        let refused = self.spent();
        let (text, error) = if refused {
            (
                "the tool budget of this review is spent: answer now with the JSON decision"
                    .to_owned(),
                true,
            )
        } else {
            match self.run(name, arguments) {
                Ok(text) => (text, false),
                Err(error) => (error, true),
            }
        };
        let room = RESULT_MAX.min(MAX_BYTES.saturating_sub(self.bytes).max(200));
        let text = cut_bytes(text, room);
        if !refused {
            self.executed += 1;
            self.bytes += text.len();
        }
        self.calls.push(ToolCall {
            name: name.to_owned(),
            arguments: arguments.clone(),
            bytes: text.len(),
            error,
        });
        (text, error)
    }

    fn run(&self, name: &str, arguments: &Value) -> Result<String, String> {
        match name {
            "read_file" => self.read_file(arguments),
            "grep" => self.grep(arguments),
            "git_log" => self.git_log(arguments),
            other => Err(format!("no tool named {other}")),
        }
    }

    fn read_file(&self, arguments: &Value) -> Result<String, String> {
        let path = text_arg(arguments, "path")?;
        let text = blob_at(self.repo, &self.commit, &path)?;
        let number = |field: &str, default: usize| match &arguments[field] {
            Value::Null => Ok(default),
            value => value
                .as_u64()
                .filter(|n| *n >= 1)
                .map(|n| n as usize)
                .ok_or_else(|| format!("{field} must be an integer of 1 or more")),
        };
        let start = number("start_line", 1)?;
        let end = number("end_line", start + READ_LINES - 1)?;
        if end < start {
            return Err("end_line is before start_line".into());
        }
        let end = end.min(start + READ_LINES - 1);
        let total = text.lines().count();
        if start > total {
            return Err(format!("{path} has {total} lines"));
        }
        let mut out: String = text
            .lines()
            .enumerate()
            .skip(start - 1)
            .take(end - start + 1)
            .map(|(index, line)| format!("{}: {line}\n", index + 1))
            .collect();
        if end < total {
            out.push_str(&format!("({total} lines in all)\n"));
        }
        Ok(out)
    }

    fn grep(&self, arguments: &Value) -> Result<String, String> {
        let pattern = text_arg(arguments, "pattern")?;
        if pattern.len() > ARG_MAX || pattern.contains(['\n', '\0']) {
            return Err(format!(
                "the pattern must be one line of at most {ARG_MAX} bytes"
            ));
        }
        let mode = match arguments["regex"].as_bool() {
            Some(true) => "-E",
            _ => "-F",
        };
        let mut args = vec![
            "grep".to_owned(),
            "-n".into(),
            "-I".into(),
            "--no-color".into(),
            mode.into(),
            "-e".into(),
            pattern,
            self.commit.clone(),
            "--".into(),
        ];
        if let Some(glob) = arguments["path_glob"].as_str().filter(|g| !g.is_empty()) {
            if glob.len() > ARG_MAX {
                return Err(format!("the glob must be at most {ARG_MAX} bytes"));
            }
            args.push(format!(":(glob){}", safe_path(glob)?));
        }
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let output = match git(self.repo, &args) {
            Ok(output) => output,
            Err(error) if error.ends_with(": ") => return Ok("(no matches)\n".into()),
            Err(error) => return Err(error),
        };
        let prefix = format!("{}:", self.commit);
        let lines: Vec<&str> = output.lines().collect();
        let mut out: String = lines
            .iter()
            .take(GREP_MATCHES)
            .map(|line| format!("{}\n", line.strip_prefix(&prefix).unwrap_or(line)))
            .collect();
        if lines.len() > GREP_MATCHES {
            out.push_str(&format!(
                "(the first {GREP_MATCHES} of {} matches)\n",
                lines.len()
            ));
        }
        Ok(out)
    }

    fn git_log(&self, arguments: &Value) -> Result<String, String> {
        let path = arguments["path"].as_str().unwrap_or_default().trim();
        let mut args = vec![
            "--literal-pathspecs".to_owned(),
            "log".into(),
            format!("-n{LOG_COMMITS}"),
            "--date=short".into(),
            "--format=%h %ad %s".into(),
            self.commit.clone(),
            "--".into(),
        ];
        if !path.is_empty() && path != "." {
            args.push(safe_path(path)?);
        }
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = git(self.repo, &args)?;
        Ok(if out.trim().is_empty() {
            "(no commits)\n".into()
        } else {
            out
        })
    }
}

fn text_arg(arguments: &Value, field: &str) -> Result<String, String> {
    arguments[field]
        .as_str()
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("{field} must be a nonempty string"))
}

/// `text` cut at `max` bytes on a character boundary, with a note.
fn cut_bytes(text: String, max: usize) -> String {
    if text.len() <= max {
        return text;
    }
    let mut at = max;
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    format!("{}\n(cut at {max} bytes)\n", &text[..at])
}

/// A path inside the repository: relative, no `..`, no backslash, colon or
/// NUL. Returned without empty and `.` components.
pub(super) fn safe_path(path: &str) -> Result<String, String> {
    let path = path.trim();
    if path.is_empty() || path.len() > 1_000 {
        return Err("the path must be nonempty".into());
    }
    if path.starts_with('/') || path.contains(['\\', ':', '\0']) {
        return Err("the path must be relative to the repository root".into());
    }
    let parts: Vec<&str> = path
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    if parts.is_empty() || parts.contains(&"..") {
        return Err("the path must stay inside the repository".into());
    }
    Ok(parts.join("/"))
}

/// The text of the regular file `path` at `commit`: a symlink, a directory
/// or a submodule is refused, so nothing leads out of the repository.
pub(super) fn blob_at(repo: &Path, commit: &str, path: &str) -> Result<String, String> {
    let path = safe_path(path)?;
    let entry = git(
        repo,
        &["--literal-pathspecs", "ls-tree", "-z", commit, "--", &path],
    )?;
    let entry = entry.trim_end_matches('\0');
    let (meta, name) = entry
        .split_once('\t')
        .ok_or_else(|| format!("{path} is not in the reviewed commit"))?;
    let meta: Vec<&str> = meta.split(' ').collect();
    match (meta.first(), meta.get(1), meta.get(2)) {
        _ if name != path => Err(format!("{path} is not in the reviewed commit")),
        (Some(&"120000"), _, _) => Err(format!("{path} is a symlink")),
        (_, Some(&"blob"), Some(sha)) => git(repo, &["cat-file", "blob", sha]),
        (_, Some(&"tree"), _) => Err(format!("{path} is a directory")),
        _ => Err(format!("{path} is not a regular file")),
    }
}

/// What the diff of a commit against its base changed: for each changed
/// file, the line ranges (new side) it touches.
pub(super) struct DiffLines {
    files: Vec<(String, Vec<(usize, usize)>)>,
}

impl DiffLines {
    pub(super) fn read(repo: &Path, base: &str, commit: &str) -> Self {
        let text = git(
            repo,
            &["diff", "-U0", "--no-color", "--no-renames", base, commit],
        )
        .unwrap_or_default();
        Self::parse(&text)
    }

    pub(super) fn parse(diff: &str) -> Self {
        let mut files: Vec<(String, Vec<(usize, usize)>)> = Vec::new();
        for line in diff.lines() {
            if let Some(name) = line.strip_prefix("+++ b/") {
                files.push((name.to_owned(), Vec::new()));
            } else if line.starts_with("+++ ") {
                // `+++ /dev/null`: a deleted file has no new side.
                files.push((String::new(), Vec::new()));
            } else if let Some(hunk) = line.strip_prefix("@@ ") {
                let Some(new) = hunk.split(' ').find(|part| part.starts_with('+')) else {
                    continue;
                };
                let (start, count) = new[1..].split_once(',').unwrap_or((&new[1..], "1"));
                if let (Ok(start), Ok(count), Some(file)) = (
                    start.parse::<usize>(),
                    count.parse::<usize>(),
                    files.last_mut(),
                ) {
                    if count > 0 {
                        file.1.push((start, start + count - 1));
                    }
                }
            }
        }
        Self { files }
    }

    /// Whether the diff changes `file`, and then whether it touches `line`.
    pub(super) fn touches(&self, file: &str, line: usize) -> Option<bool> {
        self.files
            .iter()
            .find(|(name, _)| name == file)
            .map(|(_, ranges)| ranges.iter().any(|(a, b)| (*a..=*b).contains(&line)))
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use std::process::Command;

    /// A repository with one commit of `files` (and a symlink `link`), and
    /// that commit's sha.
    pub(in crate::workers) fn repo_with(files: &[(&str, &str)]) -> (std::path::PathBuf, String) {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "herdr-review-tools-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let run = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(["-c", "commit.gpgsign=false"])
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "{args:?}");
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        run(&["init", "-q"]);
        for (name, text) in files {
            let path = dir.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink("/etc/hosts", dir.join("link")).unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-q", "-m", "first"]);
        let sha = run(&["rev-parse", "HEAD"]).trim().to_owned();
        (dir, sha)
    }

    #[test]
    fn paths_stay_inside_the_repository() {
        assert_eq!(safe_path("./src//a.rs"), Ok("src/a.rs".into()));
        for bad in [
            "/etc/passwd",
            "../x",
            "a/../../x",
            "a\\b",
            "C:x",
            "",
            ".",
            "a\0b",
        ] {
            assert!(safe_path(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_tools_read_the_commit_not_the_working_tree() {
        let (dir, sha) = repo_with(&[("src/a.rs", "one\ntwo\nthree\n"), ("b.txt", "needle\n")]);
        std::fs::write(dir.join("src/a.rs"), "changed\n").unwrap();
        let mut tools = RepoTools::new(&dir, &sha).unwrap();
        let (text, error) = tools.call("read_file", &json!({"path": "src/a.rs", "start_line": 2}));
        assert!(!error, "{text}");
        assert_eq!(text, "2: two\n3: three\n");
        let (text, _) = tools.call("grep", &json!({"pattern": "needle"}));
        assert_eq!(text, "b.txt:1:needle\n");
        let args = json!({"pattern": "nee+dle", "regex": true, "path_glob": "*.txt"});
        assert_eq!(tools.call("grep", &args).0, "b.txt:1:needle\n");
        assert_eq!(
            tools.call("grep", &json!({"pattern": "absent"})).0,
            "(no matches)\n"
        );
        let (text, _) = tools.call("git_log", &json!({"path": "src/a.rs"}));
        assert!(text.contains("first"), "{text}");
        assert_eq!(tools.calls.len(), 5);
        assert!(tools.calls.iter().all(|call| !call.error));
        assert!(tools.calls[0].bytes == 16);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn escapes_symlinks_and_missing_files_are_refused() {
        let (dir, sha) = repo_with(&[("a.txt", "x\n")]);
        let mut tools = RepoTools::new(&dir, &sha).unwrap();
        for path in ["/etc/hosts", "../a.txt", "link", "src", "missing.txt", "."] {
            let (text, error) = tools.call("read_file", &json!({"path": path}));
            assert!(error, "{path}: {text}");
        }
        assert!(
            tools
                .call("grep", &json!({"pattern": "x", "path_glob": "../*"}))
                .1
        );
        assert!(tools.call("git_log", &json!({"path": "/etc"})).1);
        assert!(tools.call("rm_rf", &json!({})).1);
        assert!(RepoTools::new(&dir, "--upload-pack=x").is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_budget_is_counted_and_spent_calls_are_refused_not_run() {
        let big = "line of text\n".repeat(2_000);
        let (dir, sha) = repo_with(&[("big.txt", &big)]);
        let mut tools = RepoTools::new(&dir, &sha).unwrap();
        for _ in 0..MAX_CALLS {
            assert!(!tools.spent());
            let (text, error) = tools.call("read_file", &json!({"path": "big.txt"}));
            assert!(!error);
            assert!(text.len() <= RESULT_MAX + 40, "{}", text.len());
        }
        assert!(tools.spent());
        let (text, error) = tools.call("read_file", &json!({"path": "big.txt"}));
        assert!(error && text.contains("answer now"));
        assert_eq!(tools.calls.len(), MAX_CALLS + 1);
        assert_eq!(tools.executed, MAX_CALLS);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_diff_lines_say_whether_a_line_is_in_the_diff() {
        let diff = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,0 +2,3 @@\n+x\n@@ -9 +12 @@\n-y\n+z\n@@ -20,2 +22,0 @@\n";
        let lines = DiffLines::parse(diff);
        assert_eq!(lines.touches("a.rs", 3), Some(true));
        assert_eq!(lines.touches("a.rs", 12), Some(true));
        assert_eq!(lines.touches("a.rs", 22), Some(false));
        assert_eq!(lines.touches("a.rs", 1), Some(false));
        assert_eq!(lines.touches("b.rs", 1), None);
    }
}
