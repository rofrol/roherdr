# Compiling the Linux-only code

`src/platform/linux.rs` (a pidfd exit watch, `/proc` reads, socket peer
credentials) is compiled only for the Linux target. A change there passes
`cargo fmt` and `clippy` on macOS or Windows without ever being compiled.

## Where it is compiled

`.github/workflows/ci.yml` runs its `check (ubuntu-latest)` job — `just check`,
which is `cargo fmt --check`, clippy for every target, the whole nextest suite,
the maintenance, UI hot-path, integration-asset and docs-contract tests — on
every push to `master` and every pull request. That job is the Linux compile
check, and it fails on Linux-only code that does not build.

Pushes and pull requests on this fork do not start workflows: dispatch them with
`scripts/fork_ci_dispatch.sh <name>`, which pushes a throwaway branch whose
`ci.yml` also has `workflow_dispatch:` and runs the same job.

## Before pushing, from a macOS host

```sh
cargo install cargo-zigbuild   # once
rustup target add aarch64-unknown-linux-gnu   # once, if it is not installed
cargo zigbuild --locked --all-targets --target aarch64-unknown-linux-gnu.2.34
```

`cargo zigbuild` links with Zig, so the bundled SQLite and the vendored
terminal library are built for Linux too; plain
`cargo clippy --target aarch64-unknown-linux-gnu` stops at the bundled C
sources (`failed to find tool "aarch64-linux-gnu-gcc"`).

This is deliberately not part of `just check`: it needs Zig and
`cargo-zigbuild`, which not every local environment has, and the Ubuntu CI job
above compiles the Linux-only code on every branch. Run it as its own step
before a release, or on any Linux machine with `just check`.

In the headless worker sandbox that prepared this note the command could not
be compiled at all: the vendored Zig build needs a writable temporary
directory, and its build step is refused there. Run it outside the sandbox;
the result belongs in the release report.

`scripts/linux_vm/gnome_vm.sh` is for manual desktop checks only, not for
compiling.
