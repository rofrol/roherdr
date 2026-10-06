# Long-running work

Run work that takes more than a minute (builds, exports, VM or remote jobs,
long test suites) with
`herdr-job run --name "<short description>" --why "<what it is for>" -- <command>`,
then wait for it in the background with `herdr-job wait <id>`. The command must
block until the work is really done: if it only starts work elsewhere (a VM,
a remote host, a detached process), make it wait for that work, e.g. by polling
its status file. Do not detach it with `nohup` or `&`.

When `git status` shows changes that are not yours (another session works in
the same checkout), build and test your change in a clean tree:
`herdr-job clean-tree <your paths> -- <build or test command>` runs it on
`HEAD` plus only those paths.
