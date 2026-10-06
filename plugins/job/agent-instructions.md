# Long-running work

Run work that takes more than a minute (builds, exports, VM or remote jobs,
long test suites) with
`herdr-job run --name "<short description>" --why "<what it is for>" -- <command>`,
then wait for it in the background with `herdr-job wait <id>`. The command must
block until the work is really done: if it only starts work elsewhere (a VM,
a remote host, a detached process), make it wait for that work, e.g. by polling
its status file. Do not detach it with `nohup` or `&`.

The same goes for any wait your next step depends on, however short, and for
processes you did not start (a scheduled run, another session's build): wait
for one with `herdr-job watch --pid <pid> --why "<what you do after it>"`, then
`herdr-job wait <id>`, not with a loop in a background shell of your own. A job
shows in the user's sidebar; your own background shell does not. `watch` only
sees the process end, not its exit status: check its result yourself.

When `git status` shows changes that are not yours (another session works in
the same checkout), build and test your change in a clean tree:
`herdr-job clean-tree <your paths> -- <build or test command>` runs it on
`HEAD` plus only those paths.
