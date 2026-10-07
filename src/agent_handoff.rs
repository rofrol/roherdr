//! Handing an agent session over to another agent (`agent.handoff`): where the
//! source session's transcript lives and the first prompt that points the new
//! agent at it. Herdr never reads the transcript; the new agent does, so this
//! works after the source agent hit its usage limit.

use std::path::{Path, PathBuf};

use crate::agent_resume::{AgentSessionRef, AgentSessionRefKind};
use crate::detect::Agent;

/// The agents a session can be handed to: each takes its first prompt as a
/// command-line argument and stays interactive.
pub(crate) const HANDOFF_TARGETS: [Agent; 3] = [Agent::Claude, Agent::Pi, Agent::Codex];

/// The directories the agents keep their sessions under.
#[derive(Debug, Clone, Default)]
pub(crate) struct TranscriptDirs {
    /// `~/.claude` or `$CLAUDE_CONFIG_DIR`.
    pub claude: Option<PathBuf>,
    /// `~/.codex` or `$CODEX_HOME`.
    pub codex: Option<PathBuf>,
    /// `~/.pi/agent` or `$PI_CODING_AGENT_DIR`.
    pub pi: Option<PathBuf>,
}

impl TranscriptDirs {
    pub(crate) fn from_env() -> Self {
        Self {
            claude: crate::integration::claude_dir().ok(),
            codex: crate::integration::codex_dir().ok(),
            pi: crate::integration::pi_agent_dir().ok(),
        }
    }
}

/// The transcript of `agent`'s session `session_ref`, which ran in `cwd`.
/// Only a file that exists is returned: a path is never guessed.
pub(crate) fn transcript_path(
    dirs: &TranscriptDirs,
    agent: &str,
    session_ref: &AgentSessionRef,
    cwd: Option<&Path>,
) -> Result<PathBuf, String> {
    let missing = || {
        format!(
            "no transcript found for {agent} session {}",
            session_ref.value
        )
    };
    if session_ref.kind == AgentSessionRefKind::Path {
        // Pi reports its session file itself.
        let path = PathBuf::from(&session_ref.value);
        return if matches!(agent, "pi" | "omp") && path.is_file() {
            Ok(path)
        } else {
            Err(missing())
        };
    }
    let id = session_ref.value.as_str();
    if !is_file_name_safe(id) {
        return Err(format!("{agent} session id {id} is not a plain id"));
    }
    let dir = match agent {
        "claude" => dirs.claude.as_deref(),
        "codex" => dirs.codex.as_deref(),
        "pi" => dirs.pi.as_deref(),
        _ => return Err(format!("herdr cannot locate {agent} transcripts")),
    }
    .ok_or_else(missing)?;
    let found = match agent {
        "claude" => claude_transcript(dir, id, cwd),
        "codex" => codex_transcript(dir, id),
        _ => pi_transcript(dir, id),
    };
    found.ok_or_else(missing)
}

/// Claude keeps `projects/<cwd with every other character than a letter or
/// digit as '-'>/<id>.jsonl`. It shortens long directory names, so another
/// project directory holding the id is accepted too: ids are unique.
fn claude_transcript(dir: &Path, id: &str, cwd: Option<&Path>) -> Option<PathBuf> {
    let projects = dir.join("projects");
    let file = format!("{id}.jsonl");
    if let Some(cwd) = cwd {
        let path = projects.join(claude_project_slug(cwd)).join(&file);
        if path.is_file() {
            return Some(path);
        }
    }
    std::fs::read_dir(&projects)
        .ok()?
        .flatten()
        .map(|entry| entry.path().join(&file))
        .find(|path| path.is_file())
}

pub(crate) fn claude_project_slug(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Codex keeps `sessions/<year>/<month>/<day>/rollout-<time>-<id>.jsonl`.
fn codex_transcript(dir: &Path, id: &str) -> Option<PathBuf> {
    let suffix = format!("-{id}.jsonl");
    let subdirs = |path: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(path)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|path| path.is_dir())
                    .collect()
            })
            .unwrap_or_default()
    };
    for year in subdirs(&dir.join("sessions")) {
        for month in subdirs(&year) {
            for day in subdirs(&month) {
                let found = std::fs::read_dir(&day).ok().and_then(|entries| {
                    entries.flatten().map(|entry| entry.path()).find(|path| {
                        path.file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| {
                                name.starts_with("rollout-") && name.ends_with(&suffix)
                            })
                            && path.is_file()
                    })
                });
                if found.is_some() {
                    return found;
                }
            }
        }
    }
    None
}

/// Pi keeps `sessions/<cwd>/<time>_<id>.jsonl`; it normally reports the file
/// itself, so this covers a session known only by its id.
fn pi_transcript(dir: &Path, id: &str) -> Option<PathBuf> {
    let suffix = format!("_{id}.jsonl");
    std::fs::read_dir(dir.join("sessions"))
        .ok()?
        .flatten()
        .filter_map(|entry| std::fs::read_dir(entry.path()).ok())
        .flat_map(|entries| entries.flatten().map(|entry| entry.path()))
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(&suffix))
                && path.is_file()
        })
}

fn is_file_name_safe(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !id.starts_with('.')
}

/// Where the handed-over work came from.
#[derive(Debug, Clone)]
pub(crate) struct HandoffSource<'a> {
    pub agent: &'a str,
    pub session_id: &'a str,
    pub transcript: &'a Path,
    pub task: Option<&'a str>,
    pub repository: Option<&'a Path>,
    pub revision: Option<&'a str>,
    pub time: time::OffsetDateTime,
}

/// The new agent's first prompt: a pointer to the source transcript, what to
/// carry over from it, and its provenance. One line, so every shell takes it
/// as one argument.
pub(crate) fn handoff_prompt(source: &HandoffSource<'_>) -> String {
    let transcript = source.transcript.display().to_string();
    let mut prompt = format!(
        "Continue the work from {} session {}, transcript {}",
        source.agent, source.session_id, transcript
    );
    if let Some(task) = source.task {
        prompt.push_str(&format!(", task {task}"));
    }
    prompt.push_str(
        "; read it first. Carry over from it: the user's and the assistant's \
         messages, the commands run with their output, the errors, the names \
         of the changed files, and the plans. It is a pointer to that \
         session, not shared context: check the current state of the files \
         before you act.",
    );
    let mut provenance = vec![
        format!("source agent {}", source.agent),
        format!("session {}", source.session_id),
        format!("transcript {transcript}"),
    ];
    if let Some(repository) = source.repository {
        provenance.push(format!("repository {}", repository.display()));
    }
    if let Some(revision) = source.revision {
        provenance.push(format!("revision {revision}"));
    }
    let time = source
        .time
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default();
    provenance.push(format!("handed over at {time}"));
    prompt.push_str(&format!(" Provenance: {}.", provenance.join(", ")));
    prompt
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// The id a session is known by: a reported session file is named
/// `<time>_<id>.jsonl`.
pub(crate) fn session_display_id(session_ref: &AgentSessionRef) -> String {
    match session_ref.kind {
        AgentSessionRefKind::Id => session_ref.value.clone(),
        AgentSessionRefKind::Path => Path::new(&session_ref.value)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(|stem| stem.rsplit('_').next().unwrap_or(stem).to_owned())
            .unwrap_or_else(|| session_ref.value.clone()),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn temp_dir(name: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "herdr-handoff-{name}-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "{}\n").unwrap();
    }

    #[test]
    fn claude_slug_replaces_every_other_character_than_letters_and_digits() {
        assert_eq!(
            claude_project_slug(Path::new("/Users/me/personal_projects/herdr.wt")),
            "-Users-me-personal-projects-herdr-wt"
        );
    }

    #[test]
    fn claude_transcript_is_found_under_the_cwd_slug_or_another_project() {
        let root = temp_dir("claude");
        let dirs = TranscriptDirs {
            claude: Some(root.clone()),
            ..Default::default()
        };
        let cwd = Path::new("/work/repo");
        let session = AgentSessionRef::id("abc-123").unwrap();
        assert!(transcript_path(&dirs, "claude", &session, Some(cwd)).is_err());

        let elsewhere = root.join("projects/-shortened-123/abc-123.jsonl");
        touch(&elsewhere);
        assert_eq!(
            transcript_path(&dirs, "claude", &session, Some(cwd)).unwrap(),
            elsewhere
        );
        let exact = root.join("projects/-work-repo/abc-123.jsonl");
        touch(&exact);
        assert_eq!(
            transcript_path(&dirs, "claude", &session, Some(cwd)).unwrap(),
            exact
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn codex_and_pi_transcripts_are_found_by_id() {
        let root = temp_dir("codex-pi");
        let dirs = TranscriptDirs {
            codex: Some(root.join("codex")),
            pi: Some(root.join("pi")),
            ..Default::default()
        };
        let codex = root.join("codex/sessions/2026/10/04/rollout-2026-10-04T11-39-28-01a1.jsonl");
        touch(&codex);
        let session = AgentSessionRef::id("01a1").unwrap();
        assert_eq!(
            transcript_path(&dirs, "codex", &session, None).unwrap(),
            codex
        );
        let pi = root.join("pi/sessions/--work--/2026-09-25T22-56-52-526Z_01a0.jsonl");
        touch(&pi);
        let by_id = AgentSessionRef::id("01a0").unwrap();
        assert_eq!(transcript_path(&dirs, "pi", &by_id, None).unwrap(), pi);
        let by_path = AgentSessionRef::path(pi.to_string_lossy()).unwrap();
        assert_eq!(transcript_path(&dirs, "pi", &by_path, None).unwrap(), pi);
        assert_eq!(session_display_id(&by_path), "01a0");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unknown_agents_missing_files_and_unsafe_ids_are_errors() {
        let root = temp_dir("errors");
        let dirs = TranscriptDirs {
            claude: Some(root.clone()),
            ..Default::default()
        };
        let session = AgentSessionRef::id("../escape").unwrap();
        assert!(transcript_path(&dirs, "claude", &session, None).is_err());
        let session = AgentSessionRef::id("abc").unwrap();
        assert!(transcript_path(&dirs, "opencode", &session, None).is_err());
        let gone = AgentSessionRef::path("/nonexistent/session.jsonl").unwrap();
        assert!(transcript_path(&dirs, "pi", &gone, None).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn prompt_points_at_the_transcript_with_its_provenance() {
        let prompt = handoff_prompt(&HandoffSource {
            agent: "claude",
            session_id: "abc-123",
            transcript: Path::new("/home/me/.claude/projects/-r/abc-123.jsonl"),
            task: Some("Fix login"),
            repository: Some(Path::new("/r")),
            revision: Some("0123abc"),
            time: time::OffsetDateTime::UNIX_EPOCH,
        });
        assert!(prompt.starts_with(
            "Continue the work from claude session abc-123, transcript \
             /home/me/.claude/projects/-r/abc-123.jsonl, task Fix login; read it first."
        ));
        for part in [
            "commands run with their output",
            "names of the changed files",
            "repository /r",
            "revision 0123abc",
            "1970-01-01T00:00:00Z",
        ] {
            assert!(prompt.contains(part), "{part}: {prompt}");
        }
        assert!(!prompt.chars().any(char::is_control));
    }
}
