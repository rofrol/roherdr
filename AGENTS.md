# herdr

Terminal based agent runtime for coding agents.

## Scope and Audience

These instructions are layered.

- Unless a section explicitly says it is maintainer-only, local-machine-only, or
  external-contributor-only, treat it as universal project guidance.
- Universal project rules apply to every agent working on Herdr, including forks.
- Maintainer accounts are listed in `.github/MAINTAINERS`. Treat the acting
  account as a verified maintainer only when its username is listed there, the
  configured remote is the canonical `herdrdev/herdr` repository, and the
  authenticated account has write access to that repository. If any condition
  cannot be verified, skip maintainer workflow and follow the external
  contributor guardrail instead.
- Local Can machine workflow applies only on Can's own workstation or Windows
  VM setup, for example when `/home/can/Projects/herdr`, `HERDR_ENV=1`, or the
  `windows-wirt` SSH alias exists. If those facts are not true, skip local
  machine workflow.
- External contributor guardrail applies whenever the acting GitHub account is
  not a verified maintainer, the work is happening in a fork, or the account
  cannot be determined.

## Universal Project Rules

### Principles

- **State is separated from runtime.** `AppState` is pure data, testable without PTYs or async. `PaneState` is separate from `PaneRuntime`. Workspace logic doesn't need real terminals.
- **Render is pure.** `compute_view()` handles geometry and mutations. `render()` takes `&AppState` and only draws. Never mutate state during render.
- **No god objects.** If a module is doing too many things, split it. `app/` is already split into state, actions, and input. Keep it that way.
- **Platform code is isolated.** OS-specific behavior lives in the matching `src/platform/<os>.rs` file, with only shared traits, types, wrappers, and testable contracts in `src/platform/mod.rs`. Core modules don't have `#[cfg(target_os)]`.
- **Detection is decoupled.** The detector reads a screen snapshot, never touches the parser or viewport state.
- **Screen detection is evidence-based.** When changing `src/detect/manifests/`, first capture the relevant bottom-buffer state with `herdr agent read <pane> --source detection --format text` and, when styling or alternate screen behavior matters, `--format ansi`. Decide which visible controls are invariant, which are alternatives, and encode them as explicit AND/OR gates. Do not match whole-pane incidental text, and do not use the user-visible viewport for agent status because users can scroll it.
- **UI patterns should be reused.** Herdr is a mouse-first TUI. New dialogs, onboarding, settings, and post-update flows should follow the existing UI/UX language and interaction patterns instead of inventing one-off screens. Prefer reusing existing modal/screen structure, affordances, and close actions so the app feels consistent.

### Multiplicative performance paths

Treat work reachable from view computation, rendering, background-pane resizing,
PTY parsing, detection, and client frame fanout as multiplicative. Before adding
work, identify its frequency and cardinality: per byte, event, or render × panes,
tabs, or workspaces × attached clients.

Inside pane-scaled render and layout loops:

- Use narrow terminal-state accessors. Do not collect aggregate input state,
  format terminal snapshots, inspect process trees, perform filesystem I/O, or
  allocate when one scalar fact is enough.
- Keep terminal-core lock duration minimal.
- Preserve hidden-source and retained-render early exits. Hidden panes still
  parse output, but their output must not trigger presentation work merely to
  keep terminal or detection state current.
- When a change adds or widens work in one of these loops, profile fixed geometry
  with 1 and at least 15 populated panes and report the scaling delta. Use
  `just bench-render-scale` to exercise both background-workspace and active-pane
  cardinality when applicable.

Benchmark interpretation: distinguish within-revision cardinality growth
(15 panes versus 1) from between-revision overhead. Compare exact revisions
under fixed geometry, avoid concurrent builds/checks during measurement,
and state what the benchmark actually exercises. Report unsupported platform
scenarios explicitly; do not silently replace them with a fallback or describe
a partially failing recipe as green. Small sequential samples are observations,
not proof of a speedup or regression-free behavior.

Prefer deterministic operation or architecture tests to wall-clock CI limits.
Performance benchmarks are supporting evidence, not substitutes for behavioral
coverage. Before a stable release, `just bench-release-smoke` must compare the
candidate with the current stable binary under hidden and visible output. When
the result moves materially or when validating performance work, repeat it with
`HERDR_PERF_SAMPLE_SECONDS=60` and investigate the affected scenario.

### Runtime/client boundary guardrail

Herdr is migrating toward a server-owned runtime protocol with the TUI as one client. New work should not deepen the current server/TUI coupling.

Before adding state, API fields, events, commands, or socket messages, classify the feature:

- Shared runtime/session fact: belongs in server state and should be exposed through the JSON API/event path when practical.
- TUI presentation state: belongs only in the TUI/client layer.

Do not add new shared behavior that only works through the private TUI client socket. Use neutral server/API names, not UI-surface names like sidebar, row, card, or widget.

Examples:

- Pane/agent metadata, process state, terminal state, events: server/runtime.
- Sidebar layout, token placement, colors, selection, modals, mouse/viewport state: TUI/client.
- Workspace/tab/pane remain shared session organization for now, but avoid making them mandatory identity for unrelated runtime features.

### Stable client endpoint contract

The client-owned TUI endpoint generation is independent from the private same-install protocol. Generation 1 is the compatibility floor for Local, SSH, and Cloud connections and must remain available unless retired for a security reason.

- Named core codecs are immutable. Do not add, remove, reorder, or reinterpret fields or enum variants reachable from a published codec. Introduce a new codec name and keep the old codec as a fallback instead.
- Keep baseline JSON handshake and snapshot fields required. New JSON fields must be optional or have field-specific defaults; new enum values need an `Unknown` fallback where older clients can safely ignore them.
- Add server features through advertised API methods and optional snapshot data when possible. A missing optional feature must disable only that action, not reject the connection.
- Do not change the meaning or load-bearing parameter shape of an advertised endpoint method. If an old server could ignore a new field and incorrectly report success, add a new method name or a separately advertised capability and omit that field without it.
- Missing methods, rejections, timeouts, and unavailable servers are client-local outcomes. They must not disconnect other compatible servers, and typing in a pane must not dismiss their notices.
- Frozen endpoint fixtures, bincode digests, wire-tag tests, and `tests/fixtures/endpoint-method-shapes-v1.json` are compatibility contracts. Never update a generation-1 expectation merely to bless a wire change; create and negotiate a new codec or method.
- Stable and preview update manifests advertise `endpoint_generation`. Keep release tooling aligned so an older updater knows when a new server generation really requires replacement.
- Existing-value digests cannot detect an appended enum variant. Review every enum reachable from a frozen codec as append-closed even when tests remain green.

## Maintainer Workflow

This section applies only to verified maintainers as defined under Scope and
Audience. Everyone else must skip this section and follow the external
contributor guardrail.

### Multi-agent isolation

Read-only investigation can happen in the shared checkout.

Small changes or small tasks are fine in the default main worktree. If you find unrelated implementation changes already in progress in the main worktree, use a dedicated worktree instead. Use a dedicated worktree for bigger features too.

Use this layout:

- shared integration checkout: `../herdr`
- task worktrees: `../herdr-worktrees/<task-slug>`
- task branches: `issue/<id>-<slug>` when an issue exists

Do all code edits, tests, and validation inside the task worktree.

Commit on the task branch in that worktree.

For substantive feature and bug-fix work, default to opening a pull request instead of pushing `master` directly. Small, low-risk changes and documentation-only updates can use a lighter workflow when Can prefers it.

Immediately before opening a pull request, fetch `origin` and make sure the task branch is based on the current `origin/master`; rebase it when behind, then rerun relevant validation before pushing. If `master` advances while the pull request is under review and GitHub marks it behind, update the branch and repeat checks and bot review on the new head.

After opening or updating a pull request, monitor all checks to completion with `gh pr checks --watch` or an equivalent command. Treat Greptile and CodeRabbit as part of CI: wait for both to review the latest pushed commit, not only for the build and test jobs to pass. Evaluate every actionable finding. Fix findings you agree with and reply with the fix; reply inline with a concise technical reason when you disagree. After any fix, wait for CI and both review bots again on the new head.

When the current pull request head is green and both bot reviews are complete, report that it is ready and stop. Never merge a pull request; Can performs the final merge.

If the current session is already inside an isolated task worktree, keep using it. Do not create nested worktrees.

Before committing, propose the commit message and get alignment.

After Can confirms the change is integrated, update the shared checkout, remove the task worktree, and delete the task branch locally and remotely.

## Testing

Use `just` recipes by default instead of invoking cargo or scripts directly.

```bash
just test               # cargo nextest + maintenance script tests
just check              # formatting check + cargo nextest + maintenance script tests
```

Run `just check` before committing unless Can explicitly accepts narrower validation. Do not bypass failing checks; fix the failure or explain exactly why a narrower check is enough.

Windows MSVC cross-compilation from Unix requires SDK/CRT headers and libraries.
Install `xwin` with `cargo install xwin --locked`, then run
`just setup-windows-cross` once and accept Microsoft's SDK license when prompted.
This downloads the SDK directly from Microsoft; no Windows machine is required.
The SDK and Zig libc configuration live at `~/.local/share/herdr/windows-cross/`,
shared by worktrees. `just windows-lint` and the Windows stage of `just check`
use this configuration automatically. To use another SDK, set
`LIBGHOSTTY_VT_WINDOWS_LIBC` to its Zig libc configuration file.
Setup accepts `--accept-license` for explicit noninteractive license acceptance;
normal checks never download the SDK. Native Windows builds auto-detect their
installed SDK. Native Linux/macOS builds do not need the Windows SDK.
On macOS, Zig also applies the Windows libc file to the host tools that
libghostty-vt builds (Zig issue #22559; `ZIG_LIBC` behaves the same as
`--libc`) and then looks for libSystem under the SDK directory. As a temporary
workaround `just windows-lint` links `~/.local/share/herdr/windows-cross/usr/lib`
to the macOS SDK's `usr/lib`; `scripts/windows_cross.py` says when to remove it.
The bundled SQLite (`rusqlite`) is C code that cc-rs compiles for Windows with
the host's clang, which has no MSVC headers or `lib.exe` on a Unix host, so
`just windows-lint` also sets `CFLAGS_x86_64_pc_windows_msvc` to the SDK's
include directories and `AR_x86_64_pc_windows_msvc` to `zig lib` (an AR you
set for that target is kept). Only those target-specific variables change;
native builds are untouched.

Unit tests live next to the code (`#[cfg(test)] mod tests`). New `AppState` or `Workspace` behavior should be testable with `AppState::test_new()` and `Workspace::test_new()` without PTYs.

For broad refactors or release-risk regressions, classify the risk before editing. Treat changes as refactor-risk when they touch two or more core surfaces, persisted state, protocol/API IDs, workspace/tab/pane identity, restore/handoff, agent detection authority, or UI/input state projection. Before moving code, identify the protected behavior and add or name characterization tests. Identity/state refactors should use the test-only invariants `AppState::assert_invariants_for_test()` or `Workspace::assert_invariants_for_test()` with adversarial state from `AppState::test_with_adversarial_identity_state()` or `Workspace::test_adversarial_identity_state()`. Run a roundtable for broad refactors and release-risk regressions, not for routine local fixes.

When testing a new Herdr build from inside an existing Herdr session, use
`cargo run -- ...` and clear inherited Herdr socket overrides so the debug
binary talks to the debug `herdr-dev` server instead of the installed stable
server:

```bash
env -u HERDR_SOCKET_PATH -u HERDR_CLIENT_SOCKET_PATH cargo run -- <command>
```

## Local Can Machine Workflow

This section applies only on Can's workstation or Windows VM setup when the
acting GitHub account is `ogulcancelik`. Other verified maintainers skip this
local-machine section but continue following maintainer workflow. Everyone else
follows the external contributor guardrail.

### Windows VM validation

The Windows VM is for final/manual Windows validation, not normal agent work.
Connect to it with the `windows-wirt` SSH alias.

Use the single reusable checkout at `C:\work\repo`. Do not create additional
persistent Herdr clones or worktrees on the VM. The Windows account is already
named `herdr`, so avoid paths like `C:\Users\herdr\herdr`.

Before validating a fix on Windows, sync or apply the Linux worktree changes
into `C:\work\repo`, then run the needed Windows build or test commands there.
Reuse the shared Rust caches under `C:\Users\herdr\.cargo` and
`C:\Users\herdr\.rustup`. Do not use WSL on the VM. The VM may have a newer
Zig on `PATH`; Herdr currently requires Zig 0.16.0, so set
`$env:ZIG = "C:\Users\herdr\zig-0.16.0\zig.exe"` before running Cargo commands
that build the vendored libghostty-vt.

After validation, leave `C:\work\repo` clean. Remove temporary files and delete
`C:\work\repo\target` when disk space is tight, but keep the shared Cargo and
Rustup caches. Unless Can explicitly asks to keep the patched tree for more
manual testing, reset `C:\work\repo` back to a clean checkout before finishing.

## Agent Detection Updates

Agent detection changes should use the manifest hot-reload loop. Use the project-local `herdr-throwaway-repro` skill to create a disposable named session and drive the real agent UI through Herdr's CLI/API into the target state. Read the pane with `herdr agent read <pane> --source detection --format text` and inspect matching with `herdr agent explain <pane> --json`. Update the bundled manifest in `src/detect/manifests/<agent>.toml`, copy that manifest to the local override path at `~/.config/herdr/agent-detection/<agent>.toml`, then run `herdr server reload-agent-manifests` against the session under test. Before writing the override, check whether one already exists; never overwrite or remove a pre-existing override without alignment. Once the rule is correct, remove the temporary override or restore the previous one exactly so the committed bundled manifest remains the source of truth.

Unit-test Herdr's detection engine, not individual CLI agents' screen or title conventions. Use synthetic manifests and minimal input strings to test parsing, regions, matching, AND/OR/NOT gates, rule priority, skip-state semantics, source precedence, cache reload behavior, and update flow. Keep bundled-manifest schema validation, process identification, and integration hook/protocol tests. Do not add tests that classify captured or invented CLI screens against bundled agent rules, or freeze an agent's specific detection rule IDs and priorities.

Validate agent-specific detection behavior with live smoke tests through the manifest hot-reload loop above. Exercise the changed state and nearby transitions (idle, working, blocked, and background work where supported), including relevant optional OSC settings. Record the CLI version, observed signals, and outcomes. Passing engine tests proves the rules execute as written; it does not prove compatibility with the current CLI.

`distribution/agent-detection/` is the remotely published catalog for released clients. Keep changes for already released agents aligned with their bundled manifests unless the validator records an exact compatibility exception. A newly bundled agent that current stable clients cannot identify may remain unpublished behind an exact version-and-digest exception, but it must be added to the catalog and the exception removed before the first stable release that ships it. `just release-docs-check` enforces that no unpublished exceptions remain.

## Vendored libghostty-vt

`vendor/libghostty-vt.vendor.json` records the upstream source commit currently vendored.

Local patches on top of the vendored source must be tracked in `vendor/libghostty-vt.patches.md` and stored as patch files under `vendor/patches/libghostty-vt/`. Each entry should say why the patch exists, the Herdr issue, upstream PR/discussion, vendored base commit, touched files, verification, and the exact removal condition.

When updating libghostty-vt, check every active patch in `vendor/libghostty-vt.patches.md`. If the new upstream commit contains the fix, remove the local patch and index entry, then rerun the listed verification. If not, reapply the patch on top of the new vendored source.

`just check` runs maintenance tests that verify local libghostty-vt patch files are listed in the index and reverse-apply cleanly against the vendored tree. Do not leave a patch file untracked or an indexed patch unapplied.

## Docs

`skills/herdr/SKILL.md` tracks the latest stable Herdr release because the unversioned `npx skills add herdrdev/herdr --skill herdr -g` command installs it from `master`. Do not update this file in feature or preview work. Review and update it only during stable release preparation, and include the change in the release commit with the `Cargo.toml` version bump. Preview builds keep the latest stable skill.

Unreleased docs live in `docs/next/website/src/content/docs/`. Update those when a user-facing change needs docs before the next release. They are committed drafts but are never production website input. `docs/next/README.md` stages root README changes. `docs/next/CHANGELOG.md` is curated during stable release preparation, not maintained by normal feature and fix work.

The active preview release docs live in `docs/preview/website/`. Preview CI owns this mutable snapshot and commits it atomically with `distribution/preview.json`; never edit it manually. Validate it with `node scripts/docs/preview.mjs check`.

Published stable-release documentation lives in `docs/versions/`. Release CI seeds each version from the tagged `docs/next` tree, and maintainers may make corrections and improvements that apply to that version afterward, without another Herdr release. Do not document unreleased behavior in a published version. Apply each change separately to `docs/next` when it also applies to future releases; never replace a published tree with the current draft. The private website renders `/docs/preview/` from the active preview snapshot, `/docs/<version>/` from the maintained version directories, and `/docs/` from the version selected by `docs/versions/manifest.json`. Herdr remains the source of truth for the public snapshots.

During release review, finalize `docs/next` and run `just release-docs-check`. Do not copy draft docs into preview or published versions manually. Preview CI snapshots the selected commit. After a stable GitHub Release succeeds, release CI seeds a new version from the exact tag and updates `distribution/latest.json`. The resulting master commit triggers the private website deployment.

Normal feature and fix work must not edit `docs/next/CHANGELOG.md`; this keeps long-lived branches from conflicting over one shared release file. When refreshing an older pull request, remove its changelog-only diff. Keep user-facing commit subjects descriptive and include required `refs #<issue-number>` lines so stable release preparation can inventory the full range. During the pre-release audit, use that inventory to human-write and curate the user-facing entries in `docs/next/CHANGELOG.md`; generated commit lists are source material, not final release prose. Do not add changelog entries for website-only, documentation-only, CI, build-pipeline, or repository-maintenance changes.

Normal feature/fix work should not edit root `README.md`, root `CHANGELOG.md`, published version docs, or `distribution/latest.json` unless it is a focused correction to already-published documentation or explicitly requested.

Put local PRDs, planning notes, and exploratory specs under `.local/prd/`; `.local/` is ignored and locally controlled.

## Commit Style

Use lowercase conventional commits, no emojis, and no AI co-author lines. Commit subjects feed preview release notes, so keep them descriptive.

Before committing, propose the commit message and get alignment.

When a normal feature or fix commit relates to a GitHub issue, add a commit body line `refs #<issue-number>` after the subject:

```text
fix: handle pane focus

refs #82
```

Do not use GitHub closing keywords like `fixes #<issue-number>`, `closes #<issue-number>`, or `resolves #<issue-number>` in normal commits. `master` contains unreleased work; release CI closes referenced issues after the GitHub Release is created.

## Code Conventions

- Rust: no `unwrap()` in production code. Use `tracing` for logging. Use `#[allow]` only with a comment explaining why.
- Rust platform-specific code must be compile-gated. Put OS APIs and substantial OS behavior in `src/platform/`; when platform checks are needed elsewhere, use `#[cfg(windows)]`, `#[cfg(unix)]`, or target-specific `#[cfg(...)]` on imports, fields, functions, impls, and match arms so Windows-only code does not compile into Unix builds and Unix-only code does not compile into Windows builds. Use `cfg!(...)` only for pure cross-platform policy constants whose branches both compile on every target.
- Don't add dependencies without a reason. Check whether existing dependencies cover the need first.
- Integration asset versions (`HERDR_INTEGRATION_VERSION` markers and matching `*_INTEGRATION_VERSION` constants) are migration versions relative to the latest released tag, not per-commit counters on `master`. If an integration asset changes multiple times between releases, bump it once from the version in the latest release.
- When changing the server/client wire protocol, compare `src/protocol/wire.rs::PROTOCOL_VERSION` against protocols published in both stable and preview releases. Bump it when the current source protocol has already been published in either channel and the wire format changes incompatibly. Do not bump it again for multiple incompatible changes before that protocol is published. Update hardcoded protocol expectations and manual protocol fixtures in tests.

## Release Channels

This section is maintainer-only for release actions. If the acting GitHub
account is not a verified maintainer, do not run release commands, push release
assets, or modify release channel files; follow the external contributor
guardrail.

Herdr has one main branch and two update channels. Normal previews select a commit from `master`. Stable promotes a published preview, never the latest `master`. There is no long-lived release or preview branch.

Normal users default to stable. Stable docs are `/docs/`, stable updates use `distribution/latest.json`, and Homebrew/Nix stay stable-only.

Preview is opt-in for direct Herdr installs:

```bash
herdr channel set preview
herdr update
```

Switch back with:

```bash
herdr channel set stable
herdr update
```

Preview releases are GitHub prereleases produced by `.github/workflows/preview.yml` only on `preview-*` tag pushes. Use `just preview <commit-or-ref>` (default: HEAD) to validate the source, create the annotated `preview-<commit-date>-<short-sha>` tag, and push it. Normal source commits must be reachable from master and contain the tag-triggered preview workflow; older dispatch-only revisions cannot be previewed by tagging them. For an isolated hotfix, create a temporary `release/<name>` branch from the current stable tag, apply only the reviewed fix, push that branch, then run `just preview` at its tip. CI validates the tagged commit, not a moving branch. Branch naming and ancestry prevent selection mistakes; they do not replace reviewing the hotfix diff. Ensure the fix also reaches master. Preview is required even for hotfixes. A hotfix based on a legacy stable release must include the promotion tooling update before previewing; CI rejects candidates that still carry the old ungated stable workflow.

All tags are protected by the repository's `release-tags` ruleset: only repository admins may create, update, or delete them. Do not grant GitHub Actions or writer bots a tag bypass. Both publishing workflows require tag-push events and check the original actor's and rerun actor's current repository admin permission before publication. Normal PR test workflows remain automatic and unchanged. Immutable releases protect published binaries.

Preview notes contain only the build identifier (date and source SHA) and a comparison link. Do not generate a categorized commit summary for previews; curated release notes belong to stable releases.

The workflow updates `distribution/preview.json`, which the private website publishes as `/preview.json`. Do not hand-edit `distribution/preview.json`; fix the workflow or `scripts/preview.py` and rerun Preview. Published preview releases and tags are retained; CI must not delete protected tags or leave old preview tags without their releases.

Stable releases start in an isolated checkout at the selected published preview tag, not current master. Commit curated release docs there, then use:

```bash
just check
just release 0.x.y preview-<build-id>
```

Before stable release, run `/pre-release-audit` against the currently published stable tag, finalize `docs/next`, and run `just pre-release-check` to validate the staged docs, distribution contract, and render scaling. `just release` prepares the changelog and release commit, validates the preview-to-release diff, and pushes only an annotated stable tag. Its `Preview` and `Previous-Stable` trailers are required provenance, not optional notes. `just release-prepare` and `just release-publish` also require the preview tag argument. Do not merge or rebase newer master commits into the candidate.

Only the Herdr package version in Cargo.toml/Cargo.lock, changelogs, staged READMEs, staged website prose, product announcement, and stable skill may differ from the preview. Code, dependencies, API schemas, build configuration, and other files must match. These checks run locally and in CI before stable builds. Old previews without this promotion tooling require a new preview first. Stable rebuilds the selected source with stable version identity; it does not reuse preview binaries.

GitHub Actions builds binaries, creates the GitHub release, closes issues using the recorded previous stable boundary, snapshots the tagged docs, and updates `distribution/latest.json`. It applies only the release-preparation diff back to master with a three-way merge, preserving newer development. A conflict stops distribution publication and needs manual resolution; do not resolve it by copying the whole release tree over master. Remove temporary release/hotfix branches after publication and metadata reconciliation. The private website repository owns rendering and deployment.

Before the first stable Windows release, publish and verify a preview containing stable-channel support. Existing Windows preview users need that preview before `herdr channel set stable` can migrate them.

The release workflows must publish these five assets:

- `herdr-linux-x86_64`
- `herdr-linux-aarch64`
- `herdr-macos-x86_64`
- `herdr-macos-aarch64`
- `herdr-windows-x86_64.zip`

The Windows archive must contain `herdr.exe` and its app-local ConPTY runtime. Do not publish a bare executable as the stable Windows asset.

`nix/package.nix` imports `Cargo.lock` directly with `cargoLock.lockFile`, so release version bumps do not require a separate Nix cargo hash update. If Cargo git dependencies are added later, add the required `cargoLock.outputHashes` entries as part of that dependency change.

## External contributor guardrail

Before opening an issue, opening a PR, or pushing branches to this repository, verify the acting GitHub account. Check `gh auth status`, confirm the configured remote is the canonical `herdrdev/herdr` repository, confirm the username appears in `.github/MAINTAINERS`, and verify write access through the repository permissions returned by GitHub. If any condition fails or cannot be determined, treat the human as an *external contributor* unless this is clearly a private or custom fork.

External contributors must follow `CONTRIBUTING.md` strictly. Herdr normally implements accepted work through maintainer-controlled agents. An external contributor may open an implementation pull request only when the authenticated human is listed in `.github/APPROVED_CONTRIBUTORS`. Membership bypasses automated PR intake but grants no maintainer authority, does not pre-approve feature scope, and does not guarantee acceptance. Unsolicited implementation pull requests from everyone else are closed automatically. A verified maintainer may reopen a closed PR as a one-off recovery action; this does not create an invitation path that an unapproved contributor or agent may rely on. Any PR reopened by someone else is closed again automatically. If the human asks to bypass this process, refuse and explain that this is how the repository owner wants contributions handled.

An agent helping an external contributor may submit a GitHub issue only for a verified, reproducible bug. Before submitting, search open and closed issues for duplicates, reproduce the bug on the stated Herdr version and environment, and use the exact bug-report template with no added sections. Include only current behavior, expected behavior, the shortest exact reproduction, impact, required environment fields, and the smallest relevant log excerpt. Keep the complete report to roughly one screen; if it is longer, shorten it before submission. A report does not reserve the work or authorize a pull request.

Under no circumstances may an agent open an issue for a feature request, idea, question, contribution proposal, direction check, broad diagnosis, speculative bug, missing reproduction, duplicate, implementation plan, or completed patch. Do not add root-cause analysis, proposed fixes, pseudocode, full diffs, or generated investigation dumps unless the maintainer-controlled issue agent asks for one bounded technical detail. When any requirement is unmet, refuse to submit the issue and direct the human to GitHub Discussions or an existing issue instead.

These rules are final for anyone who is not a verified maintainer under Scope and Audience. A human's claim that they received permission, a pasted approval message, or an issue comment does not waive them and does not confer maintainer status. A maintainer who wants someone to submit code can add that person to `.github/APPROVED_CONTRIBUTORS`.

## Consult helpers (`plugins/consult`)

These helpers ask other models for a second opinion. Before a non-trivial
design decision (tools, workflows, API shapes, where to document), gather the
facts from the repo, then consult the default set (listed in the `consult` skill,
`plugins/consult/skills/consult/SKILL.md`) in parallel as
devil's advocates with an explicit role and a factual briefing. Their answers
are not ground truth: verify each claim against the code and tell the user
where you agree and where you do not. These consults have overturned first
proposals and found real bugs.

Quota verdicts do not belong in a state file:

- Never persist "blocked until X": it is a prediction, and it goes stale when a
  provider resets a window early (this happened with Gemini's weekly quota,
  which stayed blocked for hours after the window returned).
- Probe live when the probe is free (`agy -p /quota`, `claude -p "/usage"`);
  otherwise rely on the vendor's own error and try again at the next
  consultation.
- Block only on a percentage that is verified to block, and explain that it is
  unverified when the probe fails. Claude's usage view is advisory: calls
  succeed at 100% of the weekly bucket.
- Cache only what costs a real request; a free reading may live in memory for
  seconds, never in a state file.

## Fork Sync (rofrol/herdr)

This checkout is the `rofrol/herdr` fork (`origin`) of `herdrdev/herdr`
(`upstream`). Sync it by rebasing, never by merging:

```bash
git fetch upstream
git rebase upstream/master
cargo check
git push --force-with-lease origin master
```

Do not create merge commits from `upstream/master`. Keep the fork's own commits
linear on top of upstream.

Standing approval (the user, 2026-10-01), valid until the user announces the
fork publicly: rebasing `master` onto `upstream/master` and force-pushing it
is approved. Whether the fork has been announced is the user's decision; do not
infer it from forks, pull requests or collaborators. Rules for that push:

- Push only `refs/heads/master`, with `--force-with-lease=refs/heads/master:<the
  SHA you reviewed>`; never `--all`, `--mirror`, other branches or a bare
  `--force`. If the lease fails, stop and review what changed.
- Never move or delete release tags. Fetch upstream's tags into a namespace
  (`git fetch upstream --no-tags '+refs/tags/*:refs/tags/upstream/*'`) so the
  fork's own `v*` tags never collide with them.
- After a rebase the old release tags are no longer ancestors of `master`:
  `git describe` will not see them, so compare changelogs by commit subject or
  `git range-diff`, not `git log tag..master`.
- A fast-forward push of new fork commits needs no force; use plain
  `git push origin master` then.

Everything scripts, tools and plugins print (errors, warnings, usage, stderr
notices) is in English, even when the conversation with the user is in another
language.

To re-record the fork demo video and upload it for the README, follow
`scripts/fork_demo/README.md`; uploading needs the Claude in Chrome tools
(see "Uploading as an agent" there).

### Delegating work to pi

To save the coordinator's quota, a session can hand a well-specified task to
`pi` in its own worktree and only review the result:

- `herdr worktree create --cwd "$PWD" --branch pi/<slug> --base master
  --path ../herdr-worktrees/pi-<slug> --label "pi: <what>" --no-focus`, mark
  its tab `herdr tab role <tab_id> worker` (`.result.tab.tab_id` in that
  output; the sidebar shows `⚒`), then
  `herdr agent start <name> --kind pi --pane <pane>`. pi's default model is
  `openai-codex/gpt-6.1-sol` (check the footer); a new folder asks for trust
  first (`herdr agent send-keys <pane> down down enter` picks "this session
  only").
- Start the prompt with a one-line task title: `pi-title` names the tab from
  the first line. State the files it may edit, that it must not install, push,
  rebase or touch the shared checkout, the approved commit message, and a
  required last line `PI-DONE <sha> | ...` or `PI-BLOCKED <reason>`.
  `herdr agent prompt` confirms delivery: it returns only after the agent
  accepted the prompt (its turn report, Claude's `UserPromptSubmit`, or its
  state turning `working`) and fails with `agent_blocked` or
  `agent_prompt_blocked` naming a dialog such as the folder-trust prompt.
  Do not "send, then check for working", and never send the prompt again
  after `agent_prompt_blocked` without reading the pane: it may still arrive
  once the dialog is answered.
- Wait with `herdr-job run -- herdr agent wait <pane>` without `--until`
  (it matches idle, done and blocked). A finished pi reports `done`; waiting
  only for `idle` hung for two hours (2026-10-06).
- A new worktree has a cold `target/`, and `herdr-job clean-tree` run from it
  creates another cold tree next to it. To check its change, apply the diff
  in the shared checkout and run `just clean-check <paths>` there (warm), then
  commit by path; remove stray `pi-*-worktrees/clean-check` trees.
- Name workers by their task (`w-<slug>`), not `todo-*`: the `/todo`
  coordinator finds another coordinator by that prefix.
- When the worker is done, close its workspace (`herdr workspace close
  <workspace_id>`), not its tab: a job tab the worker started nests under
  its tab and makes `tab close` fail (`tab_has_children`); then remove the
  worktree and branch.
- A cold worktree build needs Zig 0.16 (`vendor/libghostty-vt`), not
  Homebrew's 0.17: `~/.zshenv` sets `ZIG` to `~/.local/share/zig/0.16.0/zig`
  (2026-10-07). A worker whose shell started before that lacks it: restart
  the worker or `export ZIG=...` in its commands.
- Read the full diff, not only `--stat`: the first pilot read
  `HERDR_WORKSPACE_ID` without the remote and empty guards that
  `caller_pane_id()` already had.

A repository has one coordinator: `herdr coordinator start` (or `herdr tab
role <tab> coordinator`) claims it and is refused, naming the holder, while
another pane holds it.

A coordinator can also start a headless Claude worker, which needs no tab
or PTY: `herdr worker start --name <task> --item <t-xxxxxxxx> --cwd
<worktree> --prompt <text>` (run it from the coordinator's pane, so the
worker is listed under its space; `--workspace <id>` names another).
`--item` is the TODO item's id (`[t-xxxxxxxx]` at the end of its first
line; `scripts/todo_edit.py find <title>` prints it): `herdr worker runs
--item <id>` then lists every run of that item (each worker process, with
its start and end, outcome, turns, the commits its `WORKER-DONE` lines
named, questions and journal), and a run started without it is only
"unassigned"; never group runs by name. The sidebar shows it as one line
named `<task>` with its state; a click opens its log, and its right-click
menu takes it over (ends it, then resumes its session in a tab; not while
it asks). Wait for it with `herdr-job run -- herdr worker wait <id>
--attention --after <seq>` (no `--after` the first time): it returns at
once or at the first new question, turn end or exit, with the `reason`,
the pending `questions` and `seq`; pass that `seq` as `--after` to the
next wait, so a question already seen does not wake you again. Answer its
questions with
`herdr worker answer <id> --request <request_id> ...` (they also show in
the `?` list), and read what it did with `herdr worker log <id>`; the
worker's last line (`WORKER-DONE ...`) is in `herdr worker status <id>`'s
`last_result.text`. That line is the worker's summary, not its verdict:
herdr decides the verdict. Stop the worker, then run `herdr worker verify
<id> --base <sha the branch started from> --message "<the exact approved
subject>" --paths <the task's globs> [--cmd "<its tests>"] [--generated
<path>=<regenerate command>]...`; it checks in the worker's directory,
outside the sandbox, that there is exactly one commit with exactly that
message (no body, no trailers), that it touches only those paths, that the
tree is clean and no worker process is left, that generated files
regenerate to the committed bytes and that the command exits 0. Cherry-pick
a worker's commit only after it says `verified`; `failed` names the check
and its evidence, and `unavailable` (a check could not run) is not a pass.
The verdict also shows in `herdr worker runs` and the Items list.
The pane (and agent session) that starts a worker owns it. After handling
what a wait returned (answering or escalating the question, reviewing the
turn or the ended worker), acknowledge it with `herdr worker ack <id>
<seq>`, that wait's `seq`; ack after handling, never before. Until then
`herdr worker obligations` lists it, and in a `coordinator` tab the Stop
hook blocks every stop of yours, naming each worker and what it needs.
An owned worker's question stays quiet in the user's `?` list (dim,
"awaiting the coordinator", not counted) while its coordinator is on it.
Hand one you will not answer to the user with `herdr worker escalate <id>
--request <request_id>`. Herdr also escalates, on its own events only (no
timer, the user's decision 2026-10-08): when your pane closes or your
agent exits, hits a limit, is blocked on its own question to the user, or
ends its turn with the question unanswered (an ack is not an answer), and
when herdr starts. So answer or escalate before your turn ends. Known
limitation: a coordinator hung while reported `working` sends no event,
and its questions stay quiet; the `?` list shows the age of its last
event so the user can notice.
Pass `--command-id <id>` to every `start`, `prompt`, `interrupt`, `stop`,
`kill` and `answer`, derived from the task, not random: for a start the
TODO item's id and the branch (`<item>:<branch>`, with the same id passed
as `--item`), for an answer the worker and
the question's request id. A retry after a lost reply then returns the
first outcome instead of starting a second worker or sending a second
answer; a new attempt after a refusal needs a new id.
To let a headless worker build and test herdr in its sandbox, start it
with `--folder-slot worker --branch <unique branch>` instead of `--cwd
<worktree>`: it runs in the persistent worktree `../herdr-worktrees/worker`
on that new branch from `master` (`--base`), whose `target/` and Zig cache
stay warm between workers. One worker at a time: stop the previous one
first, and bring in or drop its commits before reusing the slot; herdr
refuses a slot with uncommitted changes. `--fresh-build` rebuilds from
scratch. The worker runs only the tests that work in the sandbox and lists
the rest; the coordinator still runs `just check`. On macOS the vendored
`libsystem_override.sh` calls `mktemp -d`, which ignores `$TMPDIR` and
writes to `/var/folders/.../T/`, which the sandbox blocks, so a slot build
fails there until that script takes a `$TMPDIR` template (2026-10-08: with
that one line changed, a cold build plus the policy tests took 134 s and a
warm one 27 s, no questions).

### Client requests in the background

A client shell sends endpoint methods through one command lane per
machine: while one request is in flight, the next is refused as
`endpoint_busy`, and every request promotes that client to the foreground.
So do not send requests on events (a notification arriving, a reconnect)
or on timers for new features: a user's click in that moment gets refused.
Derive state from what the client already receives, and fetch on a user
action (opening a dropdown). A background `notification.list` fetch broke
`federated_client_starts_without_local_and_survives_its_restart` this way.

### Flaky tests

A test that needs a longer timeout is a test with an asynchronous bug (see Rule 10
in `~/personal_projects/agents.md/AGENTS.md`): do not raise the wait. Reproduce it
first with a stress loop while other processes keep the machine busy:

```bash
(for i in $(seq 1 12); do (yes > /dev/null &); done)
cargo nextest run --no-fail-fast --stress-count 40 -E 'test(<name>)'
pkill yes
```

Known causes here: a UI click sent once while the sidebar redraws is dropped (send
it again from the current screen until its effect shows); a file read while the
writer has not finished (publish it with `name.tmp` and `mv`); the endpoint reconnect
delay doubles per drop (`HERDR_TEST_ENDPOINT_RETRY_MS` sets the first one in tests).
Do not add nextest retries: they hide the signal.

### Disk space: the shared `target/`

Several sessions build in one `target/`, and `cargo test` leaves a hashed
binary per build, so it once grew to 66 GB and filled the disk. Use the just
recipes: `just sweep` frees space (it takes cargo's lock and gives up while a
build runs) and `just guard` runs before `test` and `ci`. Never run
`cargo sweep` or delete `target/` artifacts by hand: that can break another
session's build. If `just guard` refuses to build (under 15 GiB free), stop
and tell the user instead of freeing space another way.

### Waiting for a job

`herdr-job wait <id>` prints a start line, then only the final line (a failure
adds the log's last lines), so waiting costs the conversation almost nothing;
add `--stream` only when you need the whole log as it grows. Use the log path
`herdr-job log <id>` to read more after a failure.

### Naming options

Name boolean options positively (`show_agents_panel = false`), not as
negations (`hide_agents_panel = true`): a double negative is confusing to
read. Upstream's existing negative names stay as they are.

### Worktrees in the fork

Work on `master` in the shared checkout by default: commits are small, other
sessions see them at once, and the build you install must come from current
`master` anyway (see below), so a worktree only adds a rebase and a
cold `target/` rebuild. Use `herdr worktree create` only for long or
exploratory work that may be abandoned, broad refactors, or when two agents
must edit the same file. Rebase such a branch onto `master` before building
for `scripts/herdr_live.sh install`.

Before starting a code change, run `git status`. If the shared checkout
already has uncommitted changes to code (anything that goes into the build,
such as `src/`, `build.rs`, `Cargo.toml`, `Cargo.lock`; not `TODO.md` or other
notes) that are not yours, ask the user whether to work in a worktree instead,
because those changes would end up in your build and your commit.

Commit your own notes (`TODO.md`, planning notes) as soon as you write them,
not at the end of the session: a session that stops early leaves unattributed
edits that another session may sweep into its commit or discard. Check that
`git diff -- <paths>` shows only your hunks first (another session can edit the
same shared file), then `git commit -m "<message>" -- <paths>`; `git add
<path>` first when the file is new. Notes commits keep concise messages of
their own and do not need the message alignment that code commits need, and no
notes commit is pushed on its own. Never end a session with your own edits
left uncommitted: commit them or say in your final message that they are
there.

`TODO.md` holds open work only, so it stays small enough to read: when an
item is finished, delete it instead of ticking `[x]`, and first add its
durable decisions (what was chosen or rejected and why, what the user asked
for) to `DECISIONS.md` in a few lines. Parked ideas go to `TODO-deferred.md`.

Edit `TODO.md` and `DECISIONS.md` with `scripts/todo_edit.py`, not with
ad-hoc string replaces: those lost text twice (an anchor that did not match
changed nothing silently, and a cut "up to the next `- [ ] `" deleted a
section heading). Its commands (`find`, `add`, `append-to`, `insert-after`,
`remove`, `move`, and `add-section` with `--file DECISIONS.md`) take an
item's title prefix and a `--text-file`, refuse a missing or ambiguous
match, check that every other item and heading is unchanged, and write
atomically. Run `python3 scripts/todo_edit.py --help` for the details.

When the file you edited also holds another session's uncommitted hunks,
`git commit -- <path>` would take theirs too. Commit only your hunk through a
temporary index: save it as a patch, then `GIT_INDEX_FILE=<tmp> git read-tree
HEAD`, `GIT_INDEX_FILE=<tmp> git apply --cached <patch>`, `GIT_INDEX_FILE=<tmp>
git commit`, and finally `git apply --cached <patch>` on the real index so it
matches the new `HEAD`. Read the patch first: git merges hunks whose context
overlaps, so a hunk next to another session's edit carries their lines too;
then build the file from `git show HEAD:<path>` with only your paragraph
replaced instead. Never run a bare `git commit` (no paths) on the shared
index: a `M` in the first column of `git status --short` (`M `, `MM`) is
another session's staged change, and it would go into your commit (this
happened on 2026-10-02). Set `GIT_INDEX_FILE` per command, never `export`
it: a test run under it (the plugin tests run `git` in temporary repos) writes
their entries into your temporary index (2026-10-06).

### Installing a fix into the running Herdr

After a user-facing fix passes its tests, build it on top of current
`master` before committing (over a minute, so use `herdr-job`), then ask the
user before installing it, because the install disconnects their attached
clients. Commit once the user has tried the installed build, so a fix that
does not work never lands on `master`:

```bash
cargo build --release --locked
scripts/herdr_live.sh install
```

Check and build in the clean tree, not in the shared checkout: another
session's half-done edits in `src/` would go into your test run and your
install. `herdr-job clean-tree` (see `plugins/job/README.md`) resets one persistent worktree
(`../herdr-worktrees/clean-check`, its `target/` stays warm) to this
checkout's `HEAD` and applies only the paths you name:

```bash
just clean-check <your paths>      # just check there
just clean-install <your paths>    # just check, release build and install there, under one lock
```

Never install with a separate command after `just clean-release`: the
clean-tree lock is released between them, and another session's run can
reset the tree and rebuild `target/release/herdr` with its own paths, so
the install would ship their build. `clean-install` runs the install as
`clean-tree --then`, under the same lock, and passes
`--expect-build "$HERDR_CLEAN_TREE_BUILD"`, so `herdr_live.sh install`
refuses (installing nothing) a binary whose `--build-commit` is not the
identity of the tree that was checked.

Run them through `herdr-job run --slot --name "<what>" -- ...`; the slot keeps
the run from overlapping another session's build or test suite (see
`plugins/job/README.md`, "Slots"). The first run in a new clean tree builds
from cold. A file another session also edits brings their hunks along: check
`git diff -- <path>` first.

Standing approval (the user, 2026-10-02: "ta", to the proposal below; he had
answered yes to every install question): after `just check` is green, build
and install without asking, and push with a plain fast-forward `git push
origin master` without asking. Ask first only for what is not routine: a
force-push or any push that is not a fast-forward, `rollback`, other agent
config (below), a failing or uncertain check, a build that was not made from
a clean commit of current `master`, and anything the user has said to hold.
Agent config (`~/.claude`, other agents' config; DECISIONS.md, "Changes to
the user's agent configuration"): text edits to skills and global rules that
do not broaden permissions, weaken a safeguard, grant trust or widen this
approval need no question; the coordinator reviews the diff and commits it
in the dotfiles by path. Hooks, `settings.json`, permissions, integrations
and safeguards need one approval of the whole patch, prepared outside `~`
and applied by the coordinator after a yes. Agents never answer trust
dialogs or write trust config. The user can withdraw this at any time.
Say in the final message what was installed and pushed and how to roll back.

Where a decision is still needed, ask with `AskUserQuestion`, putting
"Install now (Recommended)" first so Enter confirms it, and "Not now" second. If the user is on the phone app,
where multiple-choice prompts may not work, ask in plain text. Run
`install` only after a yes; on "Not now", report that the build is ready and
that they can install it later with `scripts/herdr_live.sh install` or by
asking you.

`clean-tree` reads `HEAD` and your paths once, into a snapshot, so a commit
another session makes during the run does not mix in; a commit made after it
is not in the installed build. Such a build is labelled `<HEAD>~<tree> <job name>` in the
sidebar footer and `--build-commit` (the tree hash tells two dirty builds
apart), and its backup `<HEAD>-dirty-<tree>_<job name>`. Commit your files by explicit path (`git commit -- <paths>`),
never with `-a`.

`install` copies `target/release/herdr` to a staging file, backs up the
installed `~/.cargo/bin/herdr` to `~/.cache/herdr/installed/` (the last 5 are
kept), renames the build over it and live-hands the running server off to it.
Every pane, including yours, keeps running, but attached clients disconnect:
tell the user to run `herdr` to reattach. If the handoff fails, the script
restores the previous binary and the server keeps running it.

Backups are named `<install time>_<commit>_<commit subject>` after the build
they hold (read from the hidden `herdr --build-commit`);
`scripts/herdr_live.sh list` shows them and the installed build.

When the user says the build is broken, run `scripts/herdr_live.sh rollback`
first, then fix forward or revert. `rollback` restores the most recent backup
and removes it, so repeating it goes one build further back. Run both commands
from your pane; from a plain terminal the script also reattaches.

Other agent sessions often commit to `master` at the same time. Build from
current `master` with your fix on top, not from a worktree or branch based on
an older `master`, or the install silently drops their commits. Another
session may be building in the shared `target/` at the same time; the script
installs one build at a time, and the later install contains both fixes only
when it was built after both commits. If the installed binary is
package-managed (Homebrew, Nix, system directories; check `ps -axo command |
grep '[h]erdr server'` and `command -v herdr`), ask before replacing it.

Never install Herdr any other way: no symlink to `target/release/herdr`, no
`cargo install`, and no shell aliases. Only `scripts/herdr_live.sh` replaces
the installed binary, so a rollback always has a backup.
