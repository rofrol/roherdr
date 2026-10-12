//! Starting, stopping and killing workers, and what their processes do on
//! the way out: held tool output and the tool sessions a kill ends.

use super::super::*;

impl WorkerSupervisor {
    /// Admits a worker start unless the server drains, counting it until
    /// the returned guard drops.
    pub(in crate::workers) fn admit_start(&self) -> Result<StartAdmission<'_>, WorkerError> {
        let mut registry = lock(&self.shared.registry);
        Self::admits_turns(&registry)?;
        registry.starts_admitted += 1;
        Ok(StartAdmission { supervisor: self })
    }

    /// Starts a worker. With a command id, a repeat returns the worker that
    /// id started, as the first reply showed it; one a server restart cut
    /// off returns that worker's status now.
    pub(crate) fn start(&self, params: &WorkerStartParams) -> Result<WorkerInfo, WorkerError> {
        self.command(
            params.command_id.as_deref(),
            "worker.start",
            params,
            |stored| match &stored.worker_id {
                Some(worker_id) => self.status(worker_id),
                None => Err(Self::cut_off(stored)),
            },
            |receipt| self.start_once(params, receipt, None),
        )
    }

    /// Starts a worker; with `role`, as a headless item coordinator
    /// ([`item_coordinators`]): its own contract, herdr's coordinator
    /// allowlist as its first pre-tool check, and the server's socket
    /// reachable from its sandbox.
    pub(in crate::workers) fn start_once(
        &self,
        params: &WorkerStartParams,
        receipt: Option<&Receipt>,
        role: Option<&item_coordinators::CoordinatorRole>,
    ) -> Result<WorkerInfo, WorkerError> {
        let cwd = params.cwd.as_str();
        let prompt = params.prompt.as_str();
        let model = params.model.as_deref();
        let cwd_path = PathBuf::from(cwd);
        if !cwd_path.is_absolute() {
            return Err(WorkerError::Invalid(format!("cwd must be absolute: {cwd}")));
        }
        let cwd_real = cwd_path
            .canonicalize()
            .map_err(|error| WorkerError::Invalid(format!("cwd {cwd}: {error}")))?;
        if !cwd_real.is_dir() {
            return Err(WorkerError::Invalid(format!(
                "cwd is not a directory: {cwd}"
            )));
        }
        if prompt.trim().is_empty() {
            return Err(WorkerError::Invalid("prompt must not be empty".into()));
        }
        if let Some(item) = &params.item {
            check_item_id(item)?;
        }
        if !crate::platform::WORKER_SANDBOX_SUPPORTED {
            return Err(WorkerError::Unsupported(
                "headless workers run Bash in Claude Code's sandbox, which this platform does not \
                 have; refusing to start a worker without it"
                    .into(),
            ));
        }
        let agent = params.agent.unwrap_or_default();
        if agent == WorkerAgent::Pi {
            if role.is_some() {
                return Err(WorkerError::Invalid(
                    "a headless item coordinator runs Claude only".into(),
                ));
            }
            let probe = crate::platform::ShellSandbox {
                write_paths: &[],
                deny_read: &[],
            };
            if crate::platform::sandboxed_shell_prefix(&probe).is_none() {
                return Err(WorkerError::Unsupported(
                    "a pi worker's shell commands run in herdr's shell sandbox (sandbox-exec), \
                     which this platform does not have; refusing to start it without one"
                        .into(),
                ));
            }
        }
        // Counted until the worker is registered, so a drain that starts
        // meanwhile waits for its first turn.
        let _admission = self.admit_start()?;
        // Held until the worker is registered, so the next start into the
        // same slot sees it running.
        let _slot_guard = params
            .folder_slot
            .as_ref()
            .map(|_| lock(&self.shared.slot_lock));
        let (slot, slot_killed) = match params.folder_slot.as_deref() {
            Some(name) => {
                let (slot, killed) = self.prepare_slot(&cwd_real, name, params)?;
                (Some(slot), killed)
            }
            None if params.branch.is_some() || params.base.is_some() || params.fresh_build => {
                return Err(WorkerError::Invalid(
                    "branch, base and fresh_build need folder_slot".into(),
                ))
            }
            None => (None, Vec::new()),
        };
        let (cwd_path, cwd_real) = match &slot {
            Some(slot) => (slot.path.clone(), slot.path.clone()),
            None => (cwd_path, cwd_real),
        };
        let slot_caches: Vec<PathBuf> = slot
            .iter()
            .flat_map(|slot| [slot.target_dir(), slot.zig_cache_dir()])
            .collect();
        // The sandbox lets Bash write the working directory and these: the
        // live home directory and the agents' configuration stay read-only.
        let writable: Vec<&Path> = std::iter::once(cwd_real.as_path())
            .chain(slot_caches.iter().map(PathBuf::as_path))
            .collect();
        home_repo::check_worker_writes(
            &writable,
            repository_of_dir(&cwd_real.display().to_string()).as_deref(),
            slot.is_some(),
        )?;

        let number = {
            let mut registry = lock(&self.shared.registry);
            let number = registry.next_number;
            registry.next_number += 1;
            number
        };
        let worker_id = format!("w{number}");
        std::fs::create_dir_all(&self.shared.dir)?;
        let journal_path = self.shared.dir.join(format!("{worker_id}.jsonl"));
        let owner_lock = own_journal(&journal_path, &worker_id)?;
        let temp_dir = self.create_temp_dir(&worker_id)?;
        let mut settings = worker_settings(&cwd_real, &temp_dir, &slot_caches);
        if let Some(key) = configured_anthropic_key_path()? {
            deny_worker_api_key(&mut settings, &key)?;
        }
        let (contract, mut checks, mut check_errors) = match role {
            Some(role) => {
                role.restrict_settings(&mut settings);
                let (mut checks, errors) = self.configured_pre_tool_checks();
                // Herdr's own check first: a user's check never sees a call
                // the allowlist refuses.
                checks.insert(0, role.allowlist_check(&temp_dir));
                (item_coordinators::contract(&temp_dir), checks, errors)
            }
            None => {
                let (checks, errors) = self.configured_pre_tool_checks();
                (worker_contract(&temp_dir, slot.as_ref()), checks, errors)
            }
        };
        let settings = settings.to_string();

        let mut pi_extension = None;
        #[cfg(unix)]
        let mut credit_proxy = None;
        let mut args: Vec<String> = [
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--replay-user-messages",
            "--permission-mode",
            "manual",
            "--permission-prompt-tool",
            "stdio",
            "--settings",
            &settings,
            "--append-system-prompt",
            &contract,
        ]
        .iter()
        .map(|arg| (*arg).to_owned())
        .collect();
        if let Some(model) = model {
            args.push("--model".into());
            args.push(model.to_owned());
        }
        let mut program = self.shared.program.clone();
        let mut model = model.map(str::to_owned);
        if agent == WorkerAgent::Pi {
            let pi_model = model
                .get_or_insert_with(|| pi::DEFAULT_MODEL.to_owned())
                .clone();
            let extension = self
                .write_pi_extension(&worker_id, &cwd_real, &temp_dir, &slot_caches)
                .inspect_err(|_| remove_temp_dir(&temp_dir))?;
            args = pi::args(&pi_model, &extension.display().to_string(), &contract);
            program = lock(&self.shared.pi_program).clone();
            pi_extension = Some(extension);
            if !checks.is_empty() {
                check_errors.push(
                    "the pre-tool checks run for Claude workers only; none runs for this pi \
                     worker"
                        .to_owned(),
                );
            }
            checks.clear();
        }

        // The worker's hooks and herdr CLI calls must not act on the
        // server's own pane or session (trial 1).
        // Nor may it use the user's credentials for other services (the
        // sandbox has no network, but a token can still end up in a file).
        let enabled = |name: &str| {
            std::env::var_os(name).is_some_and(|value| !value.is_empty() && value != "0")
        };
        let paid_pi = agent == WorkerAgent::Pi
            && model
                .as_deref()
                .is_some_and(|model| model.starts_with(pi::ANTHROPIC_PREFIX));
        let bedrock = !paid_pi && enabled("CLAUDE_CODE_USE_BEDROCK");
        let vertex = !paid_pi && enabled("CLAUDE_CODE_USE_VERTEX");
        let mut removed_env = Vec::new();
        let mut env_removed = Vec::new();
        for (key, _) in std::env::vars_os() {
            let name = key.to_string_lossy();
            if name.starts_with("HERDR_") {
                env_removed.push(key);
            } else if (paid_pi
                && (name.starts_with("ANTHROPIC_") || name.starts_with("CLAUDE_CODE_")))
                || is_credential_env(&name, bedrock, vertex)
            {
                removed_env.push(name.into_owned());
                env_removed.push(key);
            }
        }
        removed_env.sort();
        // The slot's caches, inside the slot, so a sandboxed build writes
        // only there instead of the user's `~/.cache/zig` or another target.
        let mut env_set = Vec::new();
        // An item coordinator runs `herdr todo ...` against this server.
        if let Some(role) = role {
            env_set.push((crate::api::SOCKET_PATH_ENV_VAR, role.socket.clone()));
        }
        if let Some(slot) = &slot {
            let zig_cache = slot.zig_cache_dir();
            env_set.push(("CARGO_TARGET_DIR", slot.target_dir()));
            env_set.push(("ZIG_GLOBAL_CACHE_DIR", zig_cache.clone()));
            env_set.push(("ZIG_LOCAL_CACHE_DIR", zig_cache));
        }
        #[cfg(unix)]
        if paid_pi {
            use crate::platform::anthropic_proxy::{ledger_path, AnthropicProxy, LoopbackProxy};
            let config = crate::config::load_live_config()
                .map_err(|_| std::io::Error::other("cannot load credit configuration"))?
                .config
                .workers;
            let mut config = config;
            config.anthropic_api_model = model
                .as_deref()
                .and_then(|model| model.strip_prefix(pi::ANTHROPIC_PREFIX))
                .ok_or_else(|| std::io::Error::other("missing credit model"))?
                .to_owned();
            pi::anthropic_args(&mut args, &config)?;
            let ledger = ledger_path(&config)?;
            std::fs::create_dir_all(crate::config::state_dir())?;
            let proxy = AnthropicProxy::open(&config, &ledger)?;
            let accounting_run = params
                .command_id
                .as_deref()
                .and_then(|id| id.split_once(':'))
                .map(|(run, _)| run)
                .unwrap_or(&worker_id);
            let proxy = LoopbackProxy::start(
                proxy,
                ledger,
                accounting_run,
                params.item.as_deref().unwrap_or(&worker_id),
            )?;
            let agent_dir = temp_dir.join("pi-agent");
            std::fs::create_dir(&agent_dir)?;
            std::fs::write(
                agent_dir.join("models.json"),
                pi::anthropic_models(&proxy.address, &config.anthropic_api_model).to_string(),
            )?;
            env_set.push(("PI_CODING_AGENT_DIR", agent_dir));
            env_set.push(("HERDR_ANTHROPIC_PROXY_TOKEN", PathBuf::from(&proxy.token)));
            credit_proxy = Some(proxy);
        }
        let configure = |command: &mut Command| {
            for key in &env_removed {
                command.env_remove(key);
            }
            for (key, value) in &env_set {
                command.env(key, value);
            }
        };
        let spawned = self
            .spawn_worker(&worker_id, &program, &args, &cwd_real, configure)
            .inspect_err(|_| {
                remove_temp_dir(&temp_dir);
                if let Some(extension) = &pi_extension {
                    let _ = std::fs::remove_file(extension);
                }
            })?;
        let pid = spawned.pid;

        let policy = policy::Policy::new(&cwd_path, &cwd_real, &temp_dir);
        let name = params
            .name
            .as_deref()
            .map(|name| one_line(name, 80))
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| task_name(prompt));
        let repo = repository_of(&cwd_real);
        // The title as it is now: the item leaves TODO.md once finished.
        let item_title = params
            .item
            .as_ref()
            .zip(repo.as_ref())
            .and_then(|(item, repo)| todo_titles::read_titles(Path::new(repo)).remove(item));
        let started = json!({
            "type": "started",
            "worker_id": worker_id,
            "name": name,
            "workspace_id": params.workspace_id,
            "cwd": cwd_real.display().to_string(),
            "model": model,
            "agent": agent.as_str(),
            "temp_dir": temp_dir.display().to_string(),
            "removed_env": removed_env,
            "pid": pid,
            "broker": spawned.broker.as_ref().map(|(broker_pid, socket)| json!({
                "pid": broker_pid,
                "socket": socket,
                "cwd": cwd_path,
                "pre_tool_checks": checks,
            })),
            "pre_tool_checks": checks,
            "program": program.display().to_string(),
            "args": args,
            "owner": params.owner_pane_id.as_ref().map(|pane_id| json!({
                "pane_id": pane_id,
                "session_id": params.owner_session_id,
                "coordinator_id": self.coordinator_of_pane(pane_id).map(|tenure| tenure.coordinator_id),
            })),
            "item": params.item,
            "item_title": item_title,
            "repo": repo,
            "coordinator": role.map(|role| json!({"coordinator_id": role.coordinator_id})),
            "folder_slot": slot.as_ref().map(|slot| json!({
                "name": params.folder_slot,
                "branch": slot.branch,
                "base": slot.base,
                "created": slot.created,
                "fresh_build": params.fresh_build,
                "killed_leftover_pids": slot_killed,
            })),
        });
        let live = Arc::new(match agent {
            WorkerAgent::Claude => Live::new(number, spawned.input),
            WorkerAgent::Pi => Live::new_pi(number, spawned.input, false),
        });
        {
            let mut registry = lock(&self.shared.registry);
            let mut entry = Entry::new(Status::new(worker_id.clone()), journal_path);
            entry.live = Some(Arc::clone(&live));
            registry.workers.insert(number, entry);
            self.commit_command_locked(
                &mut registry,
                number,
                Direction::Herdr,
                store::Recorded::Event(&started),
                receipt,
            );
            self.commit_locked(
                &mut registry,
                number,
                Direction::Herdr,
                store::Recorded::Event(
                    &json!({"type": "policy", "file_tool_roots": policy.roots()}),
                ),
            );
            for error in &check_errors {
                self.commit_locked(
                    &mut registry,
                    number,
                    Direction::Herdr,
                    store::Recorded::Event(
                        &json!({"type": "pre_tool_check_failed", "error": error}),
                    ),
                );
            }
        }
        self.shared.changed.notify_all();
        notify_clients();

        #[cfg(unix)]
        let spool = spawned
            .broker
            .as_ref()
            .map(|(_, socket)| broker::spool_path(socket));
        #[cfg(not(unix))]
        let spool = None;
        let checks = pre_tool_checks::Runner::new(checks, &cwd_real);
        let reader = Reader {
            supervisor: self.clone(),
            number,
            pid,
            policy,
            checks: checks.clone(),
            temp_dir: temp_dir.clone(),
            spool,
            live: Arc::clone(&live),
            owner_lock,
            pi: (agent == WorkerAgent::Pi).then(Default::default),
            pi_extension,
            #[cfg(unix)]
            credit_proxy,
        };
        let reading = match spawned.source {
            Source::Pipes {
                stdout,
                stderr,
                child,
            } => {
                let stderr_supervisor = self.clone();
                if let Err(error) =
                    crate::thread_spawn::spawn_named("herdr-worker-err", move || {
                        for line in BufReader::new(stderr).lines() {
                            let Ok(line) = line else { break };
                            stderr_supervisor.record_raw(number, Direction::Err, &line);
                        }
                    })
                {
                    warn!(%error, "worker stderr reader unavailable");
                }
                crate::thread_spawn::spawn_named("herdr-worker", move || reader.run(stdout, child))
                    .map(drop)
            }
            #[cfg(unix)]
            Source::Broker { messages, process } => {
                crate::thread_spawn::spawn_named("herdr-worker", move || {
                    reader.run_broker(messages, Some(process));
                })
                .map(drop)
            }
        };
        if let Err(error) = reading {
            // Without a reader nobody would reap or journal the process.
            let _ = crate::platform::signal_process_group(pid, Signal::Kill);
            if let Some((broker_pid, _)) = &spawned.broker {
                let _ = crate::platform::signal_process_group(*broker_pid, Signal::Kill);
            }
            remove_temp_dir(&temp_dir);
            self.record(
                number,
                Direction::Herdr,
                &json!({"type": "exited", "code": null}),
            );
            return Err(WorkerError::Io(error));
        }
        self.watch_held_output(number, pid);

        // The CLI buffers input written before `system/init` (trial 1). The
        // hook is registered before the prompt, so it covers the first call;
        // every worker has it, for the rule against background waits. A pi
        // worker gets `get_state` instead, whose answer releases the prompt
        // once herdr's extension proved itself.
        match agent {
            WorkerAgent::Claude => live.send(self, &pre_tool_checks::initialize_request())?,
            WorkerAgent::Pi => live.send(self, &pi::init_request())?,
        };
        let turn_seq = live.send(self, &user_message(prompt))?;
        let mut info = self.status(&worker_id)?;
        info.turn_seq = Some(turn_seq);
        Ok(info)
    }

    /// Writes herdr's extension for pi worker `worker_id` ([`pi`]) and
    /// returns its path: beside the worker's journal, which its sandboxed
    /// shell cannot write, unlike its temp dir. Its shell may write the
    /// worker's folder, its temp dir, the slot's caches and the
    /// repository's git directory (a commit in a worktree writes there),
    /// and read none of [`HOME_CREDENTIALS`].
    fn write_pi_extension(
        &self,
        worker_id: &str,
        cwd_real: &Path,
        temp_dir: &Path,
        slot_caches: &[PathBuf],
    ) -> Result<PathBuf, WorkerError> {
        let mut write_paths = vec![cwd_real.to_owned(), temp_dir.canonicalize()?];
        write_paths.extend(
            slot_caches
                .iter()
                .map(|dir| dir.canonicalize().unwrap_or_else(|_| dir.clone())),
        );
        if let Ok(common) = slot::common_dir(cwd_real) {
            if !common.starts_with(cwd_real) {
                write_paths.push(common);
            }
        }
        let home = home_repo::home_dir();
        let mut deny_read: Vec<PathBuf> = home
            .iter()
            .flat_map(|home| {
                HOME_CREDENTIALS
                    .iter()
                    .filter_map(|(path, _)| path.strip_prefix("~/"))
                    .map(|path| home.join(path))
            })
            .collect();
        if let Some(key) = configured_anthropic_key_path()? {
            deny_read.push(key);
        }
        let prefix = crate::platform::sandboxed_shell_prefix(&crate::platform::ShellSandbox {
            write_paths: &write_paths,
            deny_read: &deny_read,
        })
        .ok_or_else(|| {
            WorkerError::Unsupported("this platform has no shell sandbox for pi workers".into())
        })?;
        let path = pi_extension_path(&self.shared.dir, worker_id);
        let staged = path.with_extension("js.tmp");
        std::fs::write(&staged, pi::extension_source(&prefix))?;
        std::fs::rename(&staged, &path)?;
        Ok(path)
    }

    /// The `[workers] pre_tool_checks` for a worker that starts now, with
    /// `~` expanded, and why a check is left out. The config file is read
    /// at each start, so a change applies to the next worker. A config that
    /// cannot be read runs no check: the worker starts anyway, and the
    /// reason is its `pre_tool_check_failed` event.
    pub(in crate::workers) fn configured_pre_tool_checks(&self) -> (Vec<Vec<String>>, Vec<String>) {
        #[cfg(test)]
        let configured: Result<Vec<Vec<String>>, String> =
            Ok(lock(&self.shared.pre_tool_checks).clone());
        #[cfg(not(test))]
        let configured = match crate::config::load_live_config() {
            Ok(loaded) if loaded.invalid_sections.iter().any(|name| name == "workers") => {
                Err(loaded
                    .diagnostics
                    .into_iter()
                    .filter(|diagnostic| diagnostic.contains("workers"))
                    .collect::<Vec<_>>()
                    .join("; "))
            }
            Ok(loaded) => Ok(loaded.config.workers.pre_tool_checks),
            Err(diagnostics) => Err(diagnostics.join("; ")),
        };
        match configured {
            Ok(checks) => pre_tool_checks::expand(&checks),
            Err(error) => (
                Vec::new(),
                vec![format!(
                    "the config could not be read, so no pre-tool check runs: {error}"
                )],
            ),
        }
    }

    /// Closes the worker's input, sends SIGTERM to its process group and
    /// returns. The outcome comes from the process's exit event, which turns
    /// the state to `exited`; until then the status carries
    /// `stop_requested_ms`, so asking again shows a worker that is still
    /// alive. A repeated stop sends nothing. It never escalates; `worker.kill`
    /// does.
    pub(crate) fn stop(&self, worker_id: &str) -> Result<WorkerInfo, WorkerError> {
        self.stop_once(worker_id, None, None)
    }

    /// [`Self::stop`] with the client's command id.
    pub(crate) fn stop_command(
        &self,
        target: &WorkerCommandTarget,
    ) -> Result<WorkerInfo, WorkerError> {
        self.command(
            target.command_id.as_deref(),
            "worker.stop",
            target,
            |stored| Err(Self::cut_off(stored)),
            |receipt| self.stop_once(&target.worker_id, target.caller_pane_id.as_deref(), receipt),
        )
    }

    /// `caller_pane`: the pane that asks for the stop or kill, if known.
    pub(in crate::workers) fn stop_once(
        &self,
        worker_id: &str,
        caller_pane: Option<&str>,
        receipt: Option<&Receipt>,
    ) -> Result<WorkerInfo, WorkerError> {
        let (number, live, status) = self.live(worker_id)?;
        if status.stop_requested_ms.is_some() {
            return self.status(worker_id);
        }
        let pid = status
            .pid
            .ok_or_else(|| WorkerError::NotRunning(format!("worker {worker_id} has no process")))?;
        self.record_tool_sessions(number, pid);
        let mut signal = json!({"type": "signal", "signal": "SIGTERM", "at_ms": now_ms()});
        mark_by_owner(&mut signal, &status, caller_pane);
        self.record_command(number, Direction::Herdr, &signal, receipt);
        live.close_input();
        crate::platform::signal_process_group(pid, Signal::Terminate)?;
        self.status(worker_id)
    }

    /// [`Self::kill_command`] without a command id.
    #[cfg(all(test, unix))]
    pub(crate) fn kill(
        &self,
        worker_id: &str,
        force: bool,
    ) -> Result<(WorkerInfo, WorkerKillReport), WorkerError> {
        self.kill_once(worker_id, force, None, None)
    }

    /// Force-stops a worker: SIGKILL to its process group, then to the
    /// processes of its recorded tool sessions that are still the ones
    /// recorded ([`plan_session_kill`]). A worker that exited or was lost is
    /// signalled only with `force`; without it the kill is refused with what
    /// it would signal, since its sessions may have ended long ago.
    pub(crate) fn kill_command(
        &self,
        params: &WorkerKillParams,
    ) -> Result<(WorkerInfo, WorkerKillReport), WorkerError> {
        self.command(
            params.command_id.as_deref(),
            "worker.kill",
            params,
            |stored| Err(Self::cut_off(stored)),
            |receipt| {
                self.kill_once(
                    &params.worker_id,
                    params.force,
                    params.caller_pane_id.as_deref(),
                    receipt,
                )
            },
        )
    }

    pub(in crate::workers) fn kill_once(
        &self,
        worker_id: &str,
        force: bool,
        caller_pane: Option<&str>,
        receipt: Option<&Receipt>,
    ) -> Result<(WorkerInfo, WorkerKillReport), WorkerError> {
        let (number, status) = {
            let registry = lock(&self.shared.registry);
            let number = Self::entry_number(&registry, worker_id)?;
            (number, registry.workers[&number].status.clone())
        };
        if status.is_gone() && !force {
            let plan = plan_session_kill(
                &status.tool_sessions,
                crate::platform::process_start_token,
                crate::platform::session_members,
            );
            let state = if status.exited { "exited" } else { "lost" };
            return Err(WorkerError::NeedsForce(format!(
                "worker {worker_id} is {state}, so nothing was signalled; with force, kill would signal {}",
                describe_kill(&plan)
            )));
        }
        if let (Some(pid), false) = (status.pid, status.is_gone()) {
            self.record_tool_sessions(number, pid);
            let mut signal = json!({"type": "signal", "signal": "SIGKILL"});
            mark_by_owner(&mut signal, &status, caller_pane);
            self.record_command(number, Direction::Herdr, &signal, receipt);
            crate::platform::signal_process_group(pid, Signal::Kill)?;
        }
        let sessions = self.with_entry(worker_id, |entry| entry.status.tool_sessions.clone())?;
        let plan = plan_session_kill(
            &sessions,
            crate::platform::process_start_token,
            crate::platform::session_members,
        );
        crate::platform::signal_processes(&plan.pids, Signal::Kill);
        self.record_command(
            number,
            Direction::Herdr,
            &json!({
                "type": "killed_tool_processes",
                "pids": plan.pids,
                "unverified_sessions": plan.unverified_sessions,
                "stale_sessions": plan.stale_sessions,
            }),
            receipt,
        );
        Ok((self.status(worker_id)?, plan))
    }

    /// Readies the folder slot `name` of `caller_cwd`'s repository for a new
    /// worker: refuses while a worker still runs there, ends what the
    /// earlier workers there left running, then cleans and switches it
    /// ([`slot::prepare`]). Returns the slot and the pids it ended.
    pub(in crate::workers) fn prepare_slot(
        &self,
        caller_cwd: &Path,
        name: &str,
        params: &WorkerStartParams,
    ) -> Result<(slot::Slot, Vec<u32>), WorkerError> {
        let branch = params
            .branch
            .as_deref()
            .ok_or_else(|| WorkerError::Invalid("folder_slot needs branch".into()))?;
        let base = params.base.as_deref().unwrap_or("master");
        let (path, created) = slot::locate_or_create(caller_cwd, name, base)?;
        let killed = self.end_slot_leftovers(&path)?;
        // Before the switch, so a refused start leaves no branch behind.
        slot::bound_target(&path)?;
        let slot = slot::prepare(&path, branch, base, params.fresh_build, created)?;
        Ok((slot, killed))
    }

    /// Refuses while one of this server's workers still runs in the slot
    /// (also between turns: stop it first). Then SIGKILLs what earlier
    /// workers there left: a lost worker's process group and the recorded
    /// tool sessions, each process only when its working directory is
    /// inside the slot (a reused pid or session is someone else's).
    pub(in crate::workers) fn end_slot_leftovers(
        &self,
        path: &Path,
    ) -> Result<Vec<u32>, WorkerError> {
        let shown = path.display().to_string();
        let previous: Vec<(Status, bool)> = {
            let registry = lock(&self.shared.registry);
            registry
                .workers
                .values()
                .filter(|entry| entry.status.cwd == shown)
                .map(|entry| (entry.status.clone(), entry.live.is_some()))
                .collect()
        };
        if let Some((status, _)) = previous
            .iter()
            .find(|(status, live)| *live && !status.is_gone())
        {
            return Err(WorkerError::Busy(format!(
                "worker {} still runs in folder slot {shown}; wait for it and stop it first",
                slot::holder(status)
            )));
        }
        let mut killed = Vec::new();
        for (status, _) in previous {
            let mut ended = Vec::new();
            if let (Some(pid), false) = (status.pid, status.exited) {
                if crate::platform::process_group_alive(pid) && slot::runs_in_slot(pid, path) {
                    crate::platform::signal_process_group(pid, Signal::Kill)?;
                    ended.push(pid);
                }
            }
            for session in status.tool_sessions.keys() {
                let members: Vec<u32> = crate::platform::session_members(*session)
                    .into_iter()
                    .filter(|pid| slot::runs_in_slot(*pid, path))
                    .collect();
                crate::platform::signal_processes(&members, Signal::Kill);
                ended.extend(members);
            }
            if ended.is_empty() {
                continue;
            }
            if let Some(number) = worker_number(&status.worker_id) {
                self.record(
                    number,
                    Direction::Herdr,
                    &json!({"type": "killed_tool_processes", "pids": ended, "reason": "folder slot reused"}),
                );
            }
            killed.extend(ended);
        }
        Ok(killed)
    }

    /// Creates the worker's own temp dir and returns its real path: the
    /// sandbox and the policy compare real paths. The sandbox's `$TMPDIR` is
    /// `/tmp/claude-<uid>`, shared by every sandboxed session of the user,
    /// so workers get this one by its absolute path (T3-2). It is created
    /// new (anything already at its path, a symlink included, fails the
    /// start) and private, inside a private parent that is a real directory.
    pub(in crate::workers) fn create_temp_dir(&self, worker_id: &str) -> std::io::Result<PathBuf> {
        let path = temp_dir_path(&self.shared.dir, worker_id);
        let parent = path
            .parent()
            .ok_or_else(|| std::io::Error::other("worker temp dir has no parent"))?;
        std::fs::create_dir_all(&self.shared.dir)?;
        match crate::platform::create_private_dir(parent) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        require_real_dir(parent)?;
        crate::platform::create_private_dir(&path).map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!("worker temp dir {}: {error}", path.display()),
            )
        })?;
        require_real_dir(&path)?;
        path.canonicalize()
    }

    /// Watches the CLI's children's exits and reports a tool call whose
    /// output its descendants hold open: when a child of the CLI exits
    /// (the positive event, from the platform's process watch) and
    /// processes outside the CLI's tree still hold the write end of a pipe
    /// the CLI reads, a `tool_output_held` event names those processes.
    /// Each holder is reported once. Herdr kills nothing and sets no timer:
    /// the report is a signal for the worker's coordinator, whose wait
    /// returns with `held_output`. Only macOS has the fork event this
    /// needs; elsewhere nothing is watched.
    pub(in crate::workers) fn watch_held_output(&self, number: u64, pid: u32) {
        let supervisor = self.clone();
        let mut reported: BTreeSet<(u32, Option<u64>)> = BTreeSet::new();
        let watched = crate::platform::watch_child_exits(
            pid,
            Box::new(move |exited| {
                let holders: Vec<WorkerPipeHolder> = crate::platform::outside_pipe_writers(pid)
                    .into_iter()
                    .filter(|writer| {
                        reported
                            .insert((writer.pid, crate::platform::process_start_token(writer.pid)))
                    })
                    .map(|writer| WorkerPipeHolder {
                        pid: writer.pid,
                        command: writer.command,
                    })
                    .collect();
                if !holders.is_empty() {
                    supervisor.report_held_output(number, exited, holders);
                }
            }),
        );
        if let Err(error) = watched {
            if error.kind() != std::io::ErrorKind::Unsupported {
                warn!(%error, pid, "cannot watch the worker's tool processes");
            }
        }
    }

    /// Reports held output, naming the tool calls open now. The CLI writes
    /// a call's `tool_use` line before it runs the tool, but the reader
    /// may not have read it yet when the tool's child exits: a report found
    /// mid-turn with no open call waits in the entry for that line
    /// ([`Self::flush_held_output`]) instead of naming no call.
    pub(in crate::workers) fn report_held_output(
        &self,
        number: u64,
        exited: u32,
        holders: Vec<WorkerPipeHolder>,
    ) {
        let report = {
            let mut registry = lock(&self.shared.registry);
            // Only the server attached to the worker reports: one that let
            // go of it at a live handoff leaves that to the new one.
            let Some(entry) = registry
                .workers
                .get_mut(&number)
                .filter(|entry| entry.live.is_some())
            else {
                return;
            };
            if entry.status.open_tool_calls.is_empty() && entry.status.mid_turn() {
                match &mut entry.held_pending {
                    Some((_, pending)) => pending.extend(holders),
                    None => entry.held_pending = Some((exited, holders)),
                }
                return;
            }
            (
                entry.status.name.clone(),
                WorkerHeldOutput {
                    exited_pid: exited,
                    holders,
                    tool_calls: entry.status.open_tool_calls.clone(),
                    at_ms: now_ms(),
                },
            )
        };
        self.record_held_output(number, report);
    }

    /// Records the held output waiting for its tool call
    /// ([`Self::report_held_output`]), naming the calls open now. The
    /// reader calls it after each `assistant` line, where it records only
    /// once a call is open (a text block can come before the `tool_use`),
    /// and with `at_turn_end` before the turn's `result` and the worker's
    /// exit, so a report whose call never came is still recorded in its
    /// turn.
    pub(in crate::workers) fn flush_held_output(&self, number: u64, at_turn_end: bool) {
        let report = {
            let mut registry = lock(&self.shared.registry);
            let Some(entry) = registry.workers.get_mut(&number) else {
                return;
            };
            if entry.held_pending.is_none()
                || (!at_turn_end && entry.status.open_tool_calls.is_empty())
            {
                return;
            }
            let Some((exited, holders)) = entry.held_pending.take() else {
                return;
            };
            (
                entry.status.name.clone(),
                WorkerHeldOutput {
                    exited_pid: exited,
                    holders,
                    tool_calls: entry.status.open_tool_calls.clone(),
                    at_ms: now_ms(),
                },
            )
        };
        self.record_held_output(number, report);
    }

    pub(in crate::workers) fn record_held_output(
        &self,
        number: u64,
        (name, report): (String, WorkerHeldOutput),
    ) {
        let mut event = json!({"type": "tool_output_held"});
        if let (Some(event), Ok(Value::Object(fields))) =
            (event.as_object_mut(), serde_json::to_value(&report))
        {
            event.extend(fields);
        }
        self.record(number, Direction::Herdr, &event);
        notify_user(UserNotice {
            pane_id: None,
            title: format!("Worker {name}: a tool call's output is held open"),
            body: held_output_summary(&report),
        });
    }

    /// Records the sessions of the CLI's descendants that are new. Claude
    /// Code runs each Bash tool with `setsid`, so after a crash only these
    /// recorded sessions find its tools (trial 2, T2-3).
    pub(in crate::workers) fn record_tool_sessions(&self, number: u64, pid: u32) {
        let seen = crate::platform::descendant_sessions(pid);
        if seen.is_empty() {
            return;
        }
        let new: Vec<u32> = {
            let registry = lock(&self.shared.registry);
            let Some(entry) = registry.workers.get(&number) else {
                return;
            };
            seen.into_iter()
                .filter(|session| !entry.status.tool_sessions.contains_key(session))
                .collect()
        };
        if new.is_empty() {
            return;
        }
        // A session id is its leader's pid; the leader's start token tells
        // it apart from a later process that gets the same pid, so `kill`
        // can check it is still the same session.
        let sessions: Vec<Value> = new
            .into_iter()
            .map(
                |session| match crate::platform::process_start_token(session) {
                    Some(start) => json!({"session": session, "leader_start": start}),
                    None => json!({"session": session}),
                },
            )
            .collect();
        self.record(
            number,
            Direction::Herdr,
            &json!({"type": "tool_sessions", "sessions": sessions}),
        );
    }
}

/// Which processes of the recorded tool `sessions` (each with its leader's
/// start token) `kill` may signal. A session id is its leader's pid, which a
/// new session leader, such as a pane's shell, can get once the session has
/// ended, so a session counts only while its leader still has the recorded
/// start token, and of its members only those that did not start before
/// that leader: every process of a session is its leader or was forked
/// after the leader started. A session without a recorded token is not
/// verified and not signalled. On Windows nothing records sessions, so the
/// plan is empty there.
pub(in crate::workers) fn plan_session_kill(
    sessions: &BTreeMap<u32, Option<u64>>,
    start_token: impl Fn(u32) -> Option<u64>,
    members: impl Fn(u32) -> Vec<u32>,
) -> WorkerKillReport {
    let mut report = WorkerKillReport {
        pids: Vec::new(),
        unverified_sessions: Vec::new(),
        stale_sessions: Vec::new(),
    };
    for (&session, &leader_start) in sessions {
        let Some(leader_start) = leader_start else {
            report.unverified_sessions.push(session);
            continue;
        };
        if start_token(session) != Some(leader_start) {
            report.stale_sessions.push(session);
            continue;
        }
        report.pids.extend(
            members(session)
                .into_iter()
                .filter(|pid| start_token(*pid).is_some_and(|start| start >= leader_start)),
        );
    }
    report
}

/// `kill`'s plan in words, for the refusal that asks for force.
pub(in crate::workers) fn describe_kill(plan: &WorkerKillReport) -> String {
    let list = |ids: &[u32]| {
        ids.iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut text = if plan.pids.is_empty() {
        "no process".to_owned()
    } else {
        format!("pids {}", list(&plan.pids))
    };
    if !plan.unverified_sessions.is_empty() {
        text.push_str(&format!(
            "; sessions not verified, not killed: {}",
            list(&plan.unverified_sessions)
        ));
    }
    if !plan.stale_sessions.is_empty() {
        text.push_str(&format!(
            "; sessions whose leader is gone or another process, skipped: {}",
            list(&plan.stale_sessions)
        ));
    }
    text
}

/// Where a worker's temp dir lives, beside the journals.
pub(in crate::workers) fn temp_dir_path(dir: &Path, worker_id: &str) -> PathBuf {
    dir.join("tmp").join(worker_id)
}

/// Where a pi worker's extension is ([`pi`]): beside its journal, until its
/// exit is stored, which also tells a re-attaching server the worker is pi.
pub(in crate::workers) fn pi_extension_path(dir: &Path, worker_id: &str) -> PathBuf {
    dir.join(format!("{worker_id}.pi-gate.js"))
}

/// Fails unless `path` itself (not a symlink's target) is a directory.
pub(in crate::workers) fn require_real_dir(path: &Path) -> std::io::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_dir() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "{} is not a real directory",
            path.display()
        )))
    }
}

pub(in crate::workers) fn remove_temp_dir(path: &Path) {
    match std::fs::remove_dir_all(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => warn!(%error, path = %path.display(), "worker temp dir not removed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_plan_keeps_members_that_started_with_or_after_the_leader() {
        let starts = BTreeMap::from([(10, 100), (11, 150), (12, 50), (20, 300)]);
        let sessions = BTreeMap::from([(10, Some(100)), (20, Some(200)), (30, None)]);
        let plan = plan_session_kill(
            &sessions,
            |pid| starts.get(&pid).copied(),
            |session| match session {
                // 13 has no start token any more: it is gone.
                10 => vec![10, 11, 12, 13],
                20 => vec![20, 21],
                _ => vec![30],
            },
        );
        assert_eq!(plan.pids, vec![10, 11]);
        assert_eq!(plan.stale_sessions, vec![20]);
        assert_eq!(plan.unverified_sessions, vec![30]);
    }
}
