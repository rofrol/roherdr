# Linux GNOME test VM

`gnome_vm.sh` builds a small Fedora VM with GNOME Shell on Wayland on an
Apple Silicon Mac (QEMU with HVF), for manual checks that need a real Linux
desktop, such as clicking herdr's desktop notifications
(`plugins/job/README.md`, "Clickable notifications on Linux").

It starts from the Fedora Cloud image (about 0.5 GB) and installs only GNOME
Shell, GDM with automatic login, the Ptyxis terminal and `libnotify`, not the
whole Workstation. The VM takes about 2 GB, lives outside the repo
(`~/VMs/herdr-gnome`) and is cheap to rebuild, so delete it when you are done.

```sh
brew install qemu
scripts/linux_vm/gnome_vm.sh create    # several minutes; run it with herdr-job
scripts/linux_vm/gnome_vm.sh run       # window with GNOME, logged in as herdr
scripts/linux_vm/gnome_vm.sh ssh       # while it runs
scripts/linux_vm/gnome_vm.sh destroy
```

The guest user is `herdr` with password `herdr` (sudo without a password).
SSH is forwarded to `127.0.0.1` only, with a key generated into the VM
directory; neither is committed.

## Trying herdr in the VM

Cross-build on the Mac and copy the binary in:

```sh
cargo zigbuild --release --target aarch64-unknown-linux-gnu.2.34
scripts/linux_vm/gnome_vm.sh push target/aarch64-unknown-linux-gnu/release/herdr
```

`cargo install cargo-zigbuild` and `brew install zig` provide the linker.
In the GNOME window open Ptyxis and run `~/bin/herdr`. The notification click
needs the desktop's session bus, so start the herdr client inside the VM's
terminal, not over ssh.

## Limits

- The click is a real mouse click in the QEMU window. There is no headless or
  CI variant: a headless GNOME Shell can only fake the click through its
  private `org.gnome.Shell.Eval` API, which changes between versions.
- The display has no 3D acceleration; GNOME Shell renders with llvmpipe,
  which is enough for notifications.
- When the Fedora release changes, update `FEDORA_IMAGE` and
  `FEDORA_SHA256` from the release's `CHECKSUM` file.
