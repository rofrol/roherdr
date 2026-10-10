//! Worker capabilities: the coordination protocol's vocabulary for what a
//! repository's operation or its worker may do beyond the default
//! confinement, versioned and named without any language, registry or tool
//! (an implementation in another language follows the same contract).
//!
//! A repository requests capabilities in [`OPERATIONS_FILE`], which the
//! driver reads from the run's base commit, never from the worker's tree;
//! the user's grant covers one repository, operation and definition hash
//! ([`definition_hash`]) and is stored server-side (`capability_grants`);
//! the adapter that runs the operation or the worker enforces it. Each step
//! fails closed: an unknown capability or field is refused when the file is
//! parsed, a capability the adapter cannot enforce refuses the run with a
//! typed reason (`capability_unsupported`), and a definition without a grant
//! for its exact hash is the user's question (`grant_required`), never
//! granted by the driver. A run records the grants it ran with.
//!
//! Protocol version 1 knows one operation, `prepare` (an argv, no
//! parameters), run by the driver before the run's worker starts.

use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::runs::git;
use super::{now_ms, repository_of, WorkerError, WorkerSupervisor};
use crate::api::schema::{TodoGrant, TodoGrantParams, TodoGrantsParams};

/// The protocol version this herdr speaks; a file naming another is refused.
pub(crate) const PROTOCOL_VERSION: i64 = 1;
/// A repository's operations and requests, relative to its checkout.
pub(crate) const OPERATIONS_FILE: &str = ".herdr/operations.toml";
/// The one operation protocol version 1 knows.
pub(crate) const PREPARE: &str = "prepare";

/// A capability's name in the protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum CapabilityKind {
    /// Outbound connections to the listed host names only.
    NetEgress,
    /// Writes under the listed paths only.
    FsWrite,
    /// The listed environment variables, passed from the caller.
    Env,
    /// The listed programs: arbitrary code inside the granted confinement,
    /// not a narrowing of it.
    Exec,
    /// A pseudo-terminal.
    Pty,
    /// Listening on and connecting to local sockets.
    NetLocal,
    /// Listing the host's processes.
    ProcList,
    /// The listed programs outside any confinement.
    ExecUnsandboxed,
}

impl CapabilityKind {
    pub(crate) const ALL: [Self; 8] = [
        Self::NetEgress,
        Self::FsWrite,
        Self::Env,
        Self::Exec,
        Self::Pty,
        Self::NetLocal,
        Self::ProcList,
        Self::ExecUnsandboxed,
    ];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::NetEgress => "net.egress",
            Self::FsWrite => "fs.write",
            Self::Env => "env",
            Self::Exec => "exec",
            Self::Pty => "pty",
            Self::NetLocal => "net.local",
            Self::ProcList => "proc.list",
            Self::ExecUnsandboxed => "exec.unsandboxed",
        }
    }

    pub(crate) fn of(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }

    /// The capability's scope field, none for a capability without scope.
    fn field(self) -> Option<&'static str> {
        match self {
            Self::NetEgress => Some("hosts"),
            Self::FsWrite => Some("paths"),
            Self::Env => Some("names"),
            Self::Exec | Self::ExecUnsandboxed => Some("argv"),
            Self::Pty | Self::NetLocal | Self::ProcList => None,
        }
    }

    /// Checks one scope value.
    fn check_value(self, value: &str) -> Result<(), String> {
        let ok = match self {
            // An exact host name: no wildcard, port or scheme in version 1.
            Self::NetEgress => {
                !value.is_empty()
                    && !value.starts_with(['.', '-'])
                    && !value.ends_with(['.', '-'])
                    && value.chars().all(|c| {
                        c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-'
                    })
            }
            // Absolute, or under the home directory; never `..`.
            Self::FsWrite => {
                (value.starts_with('/') || value.starts_with("~/"))
                    && !value.split('/').any(|part| part == "..")
            }
            Self::Env => {
                value
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                    && value.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    && !value.starts_with("HERDR_")
            }
            Self::Exec | Self::ExecUnsandboxed => !value.trim().is_empty(),
            Self::Pty | Self::NetLocal | Self::ProcList => false,
        };
        if ok {
            Ok(())
        } else {
            Err(format!("`{value}` is not a valid {} value", self.name()))
        }
    }
}

/// Requested capabilities, each with its scope (sorted, without
/// duplicates; empty for a capability without scope).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Capabilities(pub(crate) BTreeMap<CapabilityKind, Vec<String>>);

impl Capabilities {
    pub(crate) fn scope(&self, kind: CapabilityKind) -> Option<&[String]> {
        self.0.get(&kind).map(Vec::as_slice)
    }

    /// Parses a `capabilities` table: each key a capability name, each value
    /// a table with exactly the capability's scope field (none for one
    /// without scope). Anything else is refused.
    pub(crate) fn parse(table: &toml::Table) -> Result<Self, String> {
        let mut capabilities = BTreeMap::new();
        for (name, value) in table {
            let kind = CapabilityKind::of(name).ok_or_else(|| {
                format!(
                    "unknown capability `{name}`: protocol version {PROTOCOL_VERSION} knows {}",
                    CapabilityKind::ALL.map(CapabilityKind::name).join(", ")
                )
            })?;
            let fields = value
                .as_table()
                .ok_or_else(|| format!("capability `{name}` must be a table"))?;
            let mut scope = Vec::new();
            for (field, values) in fields {
                if Some(field.as_str()) != kind.field() {
                    return Err(format!(
                        "capability `{name}` has no field `{field}`{}",
                        kind.field()
                            .map(|known| format!(" (its scope is `{known}`)"))
                            .unwrap_or_else(|| " (it takes none)".into())
                    ));
                }
                let values = values
                    .as_array()
                    .ok_or_else(|| format!("`{name}.{field}` must be an array of strings"))?;
                for value in values {
                    let value = value
                        .as_str()
                        .ok_or_else(|| format!("`{name}.{field}` must be an array of strings"))?;
                    kind.check_value(value)?;
                    scope.push(value.to_owned());
                }
            }
            if let Some(field) = kind.field() {
                if scope.is_empty() {
                    return Err(format!("capability `{name}` needs a non-empty `{field}`"));
                }
            }
            scope.sort();
            scope.dedup();
            capabilities.insert(kind, scope);
        }
        Ok(Self(capabilities))
    }

    /// The canonical JSON form: names in order, each with its scope field.
    pub(crate) fn to_json(&self) -> Value {
        let map: serde_json::Map<String, Value> = self
            .0
            .iter()
            .map(|(kind, scope)| {
                let fields = match kind.field() {
                    Some(field) => json!({ field: scope }),
                    None => json!({}),
                };
                (kind.name().to_owned(), fields)
            })
            .collect();
        Value::Object(map)
    }
}

/// A repository-declared operation: its argv (no shell) and what it needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Operation {
    pub(crate) name: String,
    pub(crate) argv: Vec<String>,
    pub(crate) capabilities: Capabilities,
}

impl Operation {
    /// The definition a grant covers, in canonical JSON.
    pub(crate) fn definition(&self) -> Value {
        json!({
            "version": PROTOCOL_VERSION,
            "operation": self.name,
            "argv": self.argv,
            "capabilities": self.capabilities.to_json(),
        })
    }
}

/// The hash a grant names: SHA-256 of the canonical definition.
pub(crate) fn definition_hash(definition: &Value) -> String {
    let digest = Sha256::digest(definition.to_string().as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// [`OPERATIONS_FILE`] as parsed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct OperationsFile {
    pub(crate) prepare: Option<Operation>,
    /// What the run's worker needs beyond its kind's default sandbox.
    pub(crate) worker: Capabilities,
}

/// Parses [`OPERATIONS_FILE`]: `version`, `[prepare]` (`argv` and a
/// `capabilities` table) and `[worker]` (a `capabilities` table). Any other
/// key, an unknown operation among them, is refused.
pub(crate) fn parse_operations(text: &str) -> Result<OperationsFile, String> {
    let table: toml::Table = text.parse().map_err(|error| format!("{error}"))?;
    let mut file = OperationsFile::default();
    match table.get("version").and_then(toml::Value::as_integer) {
        Some(PROTOCOL_VERSION) => {}
        Some(other) => {
            return Err(format!(
                "protocol version {other} is not supported: this herdr speaks version \
                 {PROTOCOL_VERSION}"
            ))
        }
        None => {
            return Err(format!(
                "`version = {PROTOCOL_VERSION}` is missing or not an integer"
            ))
        }
    }
    for (key, value) in &table {
        match key.as_str() {
            "version" => {}
            PREPARE => {
                let section = value
                    .as_table()
                    .ok_or_else(|| format!("[{PREPARE}] must be a table"))?;
                file.prepare = Some(parse_operation(PREPARE, section)?);
            }
            "worker" => {
                let section = value
                    .as_table()
                    .ok_or_else(|| "[worker] must be a table".to_owned())?;
                for field in section.keys() {
                    if field != "capabilities" {
                        return Err(format!("[worker] has no field `{field}`"));
                    }
                }
                file.worker = capabilities_of(section, "worker")?;
            }
            other => {
                return Err(format!(
                    "operation `{other}` is not supported: protocol version {PROTOCOL_VERSION} \
                     knows only `{PREPARE}`"
                ))
            }
        }
    }
    Ok(file)
}

fn parse_operation(name: &str, section: &toml::Table) -> Result<Operation, String> {
    for field in section.keys() {
        if !matches!(field.as_str(), "argv" | "capabilities") {
            return Err(format!("[{name}] has no field `{field}`"));
        }
    }
    let argv: Vec<String> = section
        .get("argv")
        .and_then(toml::Value::as_array)
        .ok_or_else(|| format!("[{name}] needs `argv`, an array of strings"))?
        .iter()
        .map(|arg| arg.as_str().map(str::to_owned))
        .collect::<Option<_>>()
        .ok_or_else(|| format!("[{name}] `argv` must be an array of strings"))?;
    if argv.first().is_none_or(|program| program.trim().is_empty()) {
        return Err(format!("[{name}] `argv` has no program"));
    }
    Ok(Operation {
        name: name.to_owned(),
        argv,
        capabilities: capabilities_of(section, name)?,
    })
}

fn capabilities_of(section: &toml::Table, name: &str) -> Result<Capabilities, String> {
    match section.get("capabilities") {
        None => Ok(Capabilities::default()),
        Some(value) => Capabilities::parse(
            value
                .as_table()
                .ok_or_else(|| format!("[{name}.capabilities] must be a table"))?,
        ),
    }
}

/// [`OPERATIONS_FILE`] as `commit` has it in `repo`, none when it has no
/// such file. Read from git's objects, never from a working tree.
pub(crate) fn read_operations_at(
    repo: &Path,
    commit: &str,
) -> Result<Option<OperationsFile>, String> {
    let listed = git(
        repo,
        &["ls-tree", "--name-only", commit, "--", OPERATIONS_FILE],
    )?;
    if listed.trim().is_empty() {
        return Ok(None);
    }
    let text = git(repo, &["show", &format!("{commit}:{OPERATIONS_FILE}")])?;
    parse_operations(&text)
        .map(Some)
        .map_err(|error| format!("{OPERATIONS_FILE} at {commit}: {error}"))
}

/// What an adapter (the runner of an operation, or a worker kind) can
/// enforce as the protocol specifies it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Adapter {
    pub(crate) name: &'static str,
    pub(crate) enforces: &'static [CapabilityKind],
}

/// The headless Claude worker: its sandbox settings are herdr's own, and no
/// requested capability maps onto them yet.
pub(crate) const HEADLESS_CLAUDE: Adapter = Adapter {
    name: "claude-headless",
    enforces: &[],
};

/// The driver's confined job, which runs an operation: what the platform
/// can confine ([`crate::platform::CONFINED_JOB_SUPPORTED`]).
pub(crate) fn confined_job() -> Adapter {
    Adapter {
        name: "herdr-confined-job",
        enforces: if crate::platform::CONFINED_JOB_SUPPORTED {
            &[
                CapabilityKind::NetEgress,
                CapabilityKind::FsWrite,
                CapabilityKind::Env,
                CapabilityKind::Exec,
            ]
        } else {
            &[]
        },
    }
}

/// Refuses a request the adapter cannot enforce, naming the first such
/// capability.
pub(crate) fn check_enforceable(
    adapter: Adapter,
    what: &str,
    capabilities: &Capabilities,
) -> Result<(), WorkerError> {
    match capabilities
        .0
        .keys()
        .find(|kind| !adapter.enforces.contains(kind))
    {
        None => Ok(()),
        Some(kind) => Err(WorkerError::CapabilityUnsupported(format!(
            "{what} requests `{}`, which the {} adapter cannot enforce on this host{}; the run \
             is refused rather than run without it",
            kind.name(),
            adapter.name,
            if adapter.enforces.is_empty() {
                " (it enforces no requested capability)".to_owned()
            } else {
                format!(
                    " (it enforces {})",
                    adapter
                        .enforces
                        .iter()
                        .map(|kind| kind.name())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        ))),
    }
}

/// A run's prepare as its preflight planned it: the definition, its hash,
/// the grant it runs under and the adapter that enforces it. Stored with
/// the run: the grants it ran with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PreparePlan {
    pub(crate) argv: Vec<String>,
    /// The canonical definition ([`Operation::definition`]).
    pub(crate) definition: Value,
    pub(crate) hash: String,
    pub(crate) granted_ms: u64,
    pub(crate) adapter: String,
}

impl PreparePlan {
    pub(crate) fn capabilities(&self) -> Result<Capabilities, String> {
        let table: toml::Table = toml::Table::try_from(
            self.definition
                .get("capabilities")
                .cloned()
                .unwrap_or_else(|| json!({})),
        )
        .map_err(|error| format!("the stored grant does not parse: {error}"))?;
        Capabilities::parse(&table)
    }
}

/// The grants table's migration.
pub(super) const GRANTS_MIGRATION: &str = r#"
-- The user's capability grants (`todo.grant`): one per repository,
-- operation and definition hash (SHA-256 of the canonical definition,
-- which `definition` holds as JSON). A changed definition has another
-- hash and needs its own grant. `granted_ms` is Unix milliseconds.
CREATE TABLE capability_grants (
    repo TEXT NOT NULL,
    operation TEXT NOT NULL,
    definition_hash TEXT NOT NULL,
    definition TEXT NOT NULL,
    granted_ms INTEGER NOT NULL,
    PRIMARY KEY (repo, operation, definition_hash)
);
"#;

impl WorkerSupervisor {
    /// What a run of `repo` at `base` needs from [`OPERATIONS_FILE`]: its
    /// worker's request must be enforceable by the worker kind, and its
    /// prepare's by the confined job and granted for its exact definition.
    /// None when the repository declares no prepare.
    pub(super) fn plan_operations(
        &self,
        repo: &Path,
        base: &str,
    ) -> Result<Option<PreparePlan>, WorkerError> {
        let refuse = |why: String| WorkerError::Preflight(format!("preflight: {why}"));
        let Some(file) = read_operations_at(repo, base).map_err(refuse)? else {
            return Ok(None);
        };
        check_enforceable(HEADLESS_CLAUDE, "the run's worker", &file.worker)?;
        let Some(prepare) = file.prepare else {
            return Ok(None);
        };
        let adapter = confined_job();
        if !crate::platform::CONFINED_JOB_SUPPORTED {
            return Err(WorkerError::CapabilityUnsupported(format!(
                "[{PREPARE}] needs the {} adapter, which cannot confine a job on this host; the \
                 run is refused rather than run it unconfined",
                adapter.name
            )));
        }
        check_enforceable(adapter, &format!("[{PREPARE}]"), &prepare.capabilities)?;
        let definition = prepare.definition();
        let hash = definition_hash(&definition);
        // The key `todo.grant` stores grants under.
        let repo_text = repository_of(repo).unwrap_or_else(|| repo.to_string_lossy().into_owned());
        let granted = self
            .grant_of(&repo_text, PREPARE, &hash)
            .map_err(super::runs::store_error)?;
        let Some(granted_ms) = granted else {
            return Err(WorkerError::GrantRequired(format!(
                "[{PREPARE}] of {repo_text} at {base} has no grant for its definition (a new or \
                 changed definition is the user's decision, never granted by the driver): \
                 {definition}; the user grants it with `herdr todo grant --operation {PREPARE} \
                 --hash {hash}` in the repository"
            )));
        };
        Ok(Some(PreparePlan {
            argv: prepare.argv,
            definition,
            hash,
            granted_ms,
            adapter: adapter.name.to_owned(),
        }))
    }

    fn grant_of(&self, repo: &str, operation: &str, hash: &str) -> rusqlite::Result<Option<u64>> {
        let Ok(store) = self.run_store() else {
            return Ok(None);
        };
        store.read(|conn| {
            conn.query_row(
                "SELECT granted_ms FROM capability_grants
                 WHERE repo = ?1 AND operation = ?2 AND definition_hash = ?3",
                params![repo, operation, hash],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map(|ms| ms.map(|ms| ms as u64))
        })
    }

    /// The user's grant of the repository's operation as `master` defines
    /// it now, refused unless `hash` is that definition's: the user grants
    /// exactly the definition they were shown.
    pub(crate) fn todo_grant(&self, params: TodoGrantParams) -> Result<TodoGrant, WorkerError> {
        if params.operation != PREPARE {
            return Err(WorkerError::Invalid(format!(
                "operation `{}` is not supported: protocol version {PROTOCOL_VERSION} knows only \
                 `{PREPARE}`",
                params.operation
            )));
        }
        let repo = repository_of(Path::new(&params.cwd)).ok_or_else(|| {
            WorkerError::Invalid(format!("{} is not in a git repository", params.cwd))
        })?;
        let file = read_operations_at(Path::new(&repo), "master")
            .map_err(WorkerError::Invalid)?
            .and_then(|file| file.prepare)
            .ok_or_else(|| {
                WorkerError::Invalid(format!(
                    "master of {repo} declares no [{PREPARE}] in {OPERATIONS_FILE}"
                ))
            })?;
        let definition = file.definition();
        let hash = definition_hash(&definition);
        if hash != params.hash {
            return Err(WorkerError::Invalid(format!(
                "[{PREPARE}] of {repo} at master has hash {hash}, not {}: its definition is \
                 {definition}",
                params.hash
            )));
        }
        let granted_ms = now_ms();
        self.run_store()?
            .transaction(|tx| {
                tx.connection().execute(
                    "INSERT INTO capability_grants
                     (repo, operation, definition_hash, definition, granted_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT (repo, operation, definition_hash) DO NOTHING",
                    params![
                        repo,
                        PREPARE,
                        hash,
                        definition.to_string(),
                        granted_ms as i64
                    ],
                )
            })
            .map_err(super::runs::store_error)?;
        Ok(TodoGrant {
            repo,
            operation: PREPARE.to_owned(),
            hash,
            definition,
            granted_ms,
        })
    }

    /// The stored grants, of one repository when `cwd` names one.
    pub(crate) fn todo_grants(
        &self,
        params: TodoGrantsParams,
    ) -> Result<Vec<TodoGrant>, WorkerError> {
        let repo = match params.cwd.as_deref() {
            Some(cwd) => Some(repository_of(Path::new(cwd)).ok_or_else(|| {
                WorkerError::Invalid(format!("{cwd} is not in a git repository"))
            })?),
            None => None,
        };
        self.run_store()?
            .read(|conn| {
                let mut statement = conn.prepare(
                    "SELECT repo, operation, definition_hash, definition, granted_ms
                     FROM capability_grants WHERE ?1 IS NULL OR repo = ?1
                     ORDER BY repo, operation, granted_ms",
                )?;
                let rows = statement.query_map([repo.as_deref()], |row| {
                    let definition: String = row.get(3)?;
                    Ok(TodoGrant {
                        repo: row.get(0)?,
                        operation: row.get(1)?,
                        hash: row.get(2)?,
                        definition: serde_json::from_str(&definition).unwrap_or(Value::Null),
                        granted_ms: row.get::<_, i64>(4)? as u64,
                    })
                })?;
                rows.collect()
            })
            .map_err(super::runs::store_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PREPARE_FILE: &str = r#"
version = 1

[prepare]
argv = ["fetch-deps", "--locked"]

[prepare.capabilities]
"net.egress" = { hosts = ["registry.example", "cdn.registry.example"] }
"fs.write" = { paths = ["~/.cache/deps"] }
env = { names = ["HOME", "PATH"] }
"#;

    #[test]
    fn a_prepare_definition_parses_into_capabilities() {
        let file = parse_operations(PREPARE_FILE).unwrap();
        let prepare = file.prepare.unwrap();
        assert_eq!(prepare.argv, ["fetch-deps", "--locked"]);
        assert_eq!(
            prepare.capabilities.scope(CapabilityKind::NetEgress),
            Some(
                &[
                    "cdn.registry.example".to_owned(),
                    "registry.example".to_owned()
                ][..]
            )
        );
        assert_eq!(
            prepare.capabilities.scope(CapabilityKind::Env),
            Some(&["HOME".to_owned(), "PATH".to_owned()][..])
        );
        assert!(file.worker.0.is_empty());
    }

    #[test]
    fn every_named_capability_parses() {
        let file = parse_operations(
            r#"
version = 1
[worker.capabilities]
"net.egress" = { hosts = ["a.example"] }
"fs.write" = { paths = ["/tmp/x"] }
env = { names = ["A"] }
exec = { argv = ["make"] }
pty = {}
"net.local" = {}
"proc.list" = {}
"exec.unsandboxed" = { argv = ["./launch"] }
"#,
        )
        .unwrap();
        let kinds: Vec<_> = file.worker.0.keys().copied().collect();
        assert_eq!(kinds, CapabilityKind::ALL);
    }

    #[test]
    fn unknown_or_malformed_requests_are_refused() {
        for (text, needle) in [
            ("[prepare]\nargv = [\"x\"]\n", "version"),
            ("version = 2\n", "version 2 is not supported"),
            ("version = 1\n[vm-test]\nargv = [\"x\"]\n", "operation `vm-test`"),
            ("version = 1\n[prepare]\nargv = []\n", "no program"),
            ("version = 1\n[prepare]\nargv = [\"x\"]\nshell = true\n", "no field `shell`"),
            (
                "version = 1\n[prepare]\nargv = [\"x\"]\n[prepare.capabilities]\n\"net.ingress\" = {}\n",
                "unknown capability `net.ingress`",
            ),
            (
                "version = 1\n[prepare]\nargv = [\"x\"]\n[prepare.capabilities]\n\"net.egress\" = { hosts = [\"*.example\"] }\n",
                "not a valid net.egress",
            ),
            (
                "version = 1\n[prepare]\nargv = [\"x\"]\n[prepare.capabilities]\n\"net.egress\" = { hosts = [] }\n",
                "non-empty `hosts`",
            ),
            (
                "version = 1\n[prepare]\nargv = [\"x\"]\n[prepare.capabilities]\n\"fs.write\" = { paths = [\"rel/dir\"] }\n",
                "not a valid fs.write",
            ),
            (
                "version = 1\n[prepare]\nargv = [\"x\"]\n[prepare.capabilities]\n\"fs.write\" = { paths = [\"/a/../b\"] }\n",
                "not a valid fs.write",
            ),
            (
                "version = 1\n[prepare]\nargv = [\"x\"]\n[prepare.capabilities]\nenv = { names = [\"HERDR_SOCKET_PATH\"] }\n",
                "not a valid env",
            ),
            (
                "version = 1\n[prepare]\nargv = [\"x\"]\n[prepare.capabilities]\npty = { argv = [\"x\"] }\n",
                "takes none",
            ),
            ("version = 1\n[worker]\nrequests = 1\n", "no field `requests`"),
        ] {
            let error = parse_operations(text).unwrap_err();
            assert!(error.contains(needle), "{text:?}: {error}");
        }
    }

    #[test]
    fn the_hash_covers_the_definition_not_its_spelling() {
        let one = parse_operations(PREPARE_FILE).unwrap().prepare.unwrap();
        let reordered = parse_operations(
            r#"
version = 1
[prepare.capabilities]
env = { names = ["PATH", "HOME", "PATH"] }
"fs.write" = { paths = ["~/.cache/deps"] }
"net.egress" = { hosts = ["cdn.registry.example", "registry.example"] }
[prepare]
argv = ["fetch-deps", "--locked"]
"#,
        )
        .unwrap()
        .prepare
        .unwrap();
        assert_eq!(
            definition_hash(&one.definition()),
            definition_hash(&reordered.definition())
        );
        let widened = parse_operations(&PREPARE_FILE.replace(
            "\"registry.example\"",
            "\"registry.example\", \"other.example\"",
        ))
        .unwrap()
        .prepare
        .unwrap();
        assert_ne!(
            definition_hash(&one.definition()),
            definition_hash(&widened.definition())
        );
    }

    #[test]
    fn an_adapter_refuses_what_it_cannot_enforce() {
        let file =
            parse_operations("version = 1\n[worker.capabilities]\npty = {}\n\"proc.list\" = {}\n")
                .unwrap();
        let error =
            check_enforceable(HEADLESS_CLAUDE, "the run's worker", &file.worker).unwrap_err();
        assert_eq!(error.code(), "capability_unsupported");
        assert!(error.to_string().contains("`pty`"), "{error}");
        assert!(check_enforceable(HEADLESS_CLAUDE, "w", &Capabilities::default()).is_ok());
        let prepare = parse_operations(PREPARE_FILE).unwrap().prepare.unwrap();
        let job = Adapter {
            name: "job",
            enforces: &[
                CapabilityKind::NetEgress,
                CapabilityKind::FsWrite,
                CapabilityKind::Env,
            ],
        };
        assert!(check_enforceable(job, "p", &prepare.capabilities).is_ok());
        let unsandboxed = parse_operations(
            "version = 1\n[prepare]\nargv = [\"x\"]\n[prepare.capabilities]\n\"exec.unsandboxed\" = { argv = [\"x\"] }\n",
        )
        .unwrap()
        .prepare
        .unwrap();
        assert_eq!(
            check_enforceable(confined_job(), "p", &unsandboxed.capabilities)
                .unwrap_err()
                .code(),
            "capability_unsupported"
        );
    }

    #[test]
    fn a_stored_plan_gives_its_capabilities_back() {
        let prepare = parse_operations(PREPARE_FILE).unwrap().prepare.unwrap();
        let definition = prepare.definition();
        let plan = PreparePlan {
            argv: prepare.argv.clone(),
            hash: definition_hash(&definition),
            definition,
            granted_ms: 1,
            adapter: "job".into(),
        };
        let stored: PreparePlan =
            serde_json::from_str(&serde_json::to_string(&plan).unwrap()).unwrap();
        assert_eq!(stored.capabilities().unwrap(), prepare.capabilities);
    }

    #[test]
    fn the_repositorys_operations_file_parses() {
        let text =
            std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(OPERATIONS_FILE))
                .unwrap();
        let file = parse_operations(&text).unwrap();
        assert!(file.prepare.is_some());
    }
}
