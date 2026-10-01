#!/bin/sh
# A small Fedora GNOME Shell (Wayland) VM on an Apple Silicon Mac, for manual
# Linux desktop checks such as clicking herdr's notifications.
# See scripts/linux_vm/README.md.
set -eu

VM_DIR=${HERDR_GNOME_VM_DIR:-$HOME/VMs/herdr-gnome}
SSH_PORT=${HERDR_GNOME_VM_SSH_PORT:-2223}
GUEST_USER=herdr
GUEST_PASSWORD=herdr
FEDORA_RELEASE=44
FEDORA_IMAGE=Fedora-Cloud-Base-Generic-44-1.7.aarch64.qcow2
FEDORA_SHA256=55c60a3b80d3616a08705afd0459e75fe9f03c54aba7a46e4002a41a72fa0d5b
FEDORA_URL=https://download.fedoraproject.org/pub/fedora/linux/releases/$FEDORA_RELEASE/Cloud/aarch64/images/$FEDORA_IMAGE
DISK_SIZE=20G
PACKAGES="gnome-shell gnome-session-wayland-session gdm ptyxis libnotify mesa-dri-drivers"
FIRMWARE=$(brew --prefix qemu 2>/dev/null || echo /opt/homebrew)/share/qemu

usage() {
  cat <<EOF
usage: $(basename "$0") <command>

  create    download Fedora Cloud $FEDORA_RELEASE, install GNOME Shell, then power off
  run       start the VM in a window (GNOME logs in as $GUEST_USER automatically)
  ssh [CMD] ssh into the running VM
  push FILE copy FILE into the VM's ~/bin (e.g. a cross-built herdr)
  destroy   delete the VM ($VM_DIR)

VM directory: $VM_DIR (HERDR_GNOME_VM_DIR), ssh port: $SSH_PORT (HERDR_GNOME_VM_SSH_PORT)
EOF
}

die() {
  echo "error: $*" >&2
  exit 1
}

ssh_opts() {
  echo "-i $VM_DIR/id_ed25519 -p $SSH_PORT -o UserKnownHostsFile=$VM_DIR/known_hosts -o StrictHostKeyChecking=accept-new -o ConnectTimeout=5"
}

qemu() {
  display=$1
  shift
  [ -f "$VM_DIR/vars.fd" ] || cp "$FIRMWARE/edk2-arm-vars.fd" "$VM_DIR/vars.fd"
  qemu-system-aarch64 \
    -name "Fedora GNOME (herdr test)" \
    -machine virt,highmem=on -accel hvf -cpu host -smp 4 -m 4G \
    -drive if=pflash,format=raw,readonly=on,file="$FIRMWARE/edk2-aarch64-code.fd" \
    -drive if=pflash,format=raw,file="$VM_DIR/vars.fd" \
    -drive if=virtio,format=qcow2,file="$VM_DIR/disk.qcow2",discard=unmap,detect-zeroes=unmap \
    -drive if=virtio,format=raw,readonly=on,file="$VM_DIR/seed.iso" \
    -device virtio-gpu-pci,xres=1920,yres=1200 \
    -device qemu-xhci -device usb-kbd -device usb-tablet \
    -nic "user,model=virtio-net-pci,hostfwd=tcp:127.0.0.1:$SSH_PORT-:22" \
    -device virtio-rng-pci \
    -display "$display" "$@"
}

make_seed() {
  seed=$VM_DIR/seed
  rm -rf "$seed" "$VM_DIR/seed.iso"
  mkdir -p "$seed"
  printf 'instance-id: herdr-gnome\nlocal-hostname: herdr-gnome\n' >"$seed/meta-data"
  cat >"$seed/user-data" <<EOF
#cloud-config
users:
  - name: $GUEST_USER
    groups: [wheel]
    sudo: ALL=(ALL) NOPASSWD:ALL
    shell: /bin/bash
    lock_passwd: false
    plain_text_passwd: $GUEST_PASSWORD
    ssh_authorized_keys:
      - $(cat "$VM_DIR/id_ed25519.pub")
write_files:
  - path: /etc/gdm/custom.conf
    content: |
      [daemon]
      AutomaticLoginEnable=True
      AutomaticLogin=$GUEST_USER
runcmd:
  - dnf install -y --setopt=install_weak_deps=False $PACKAGES
  - dnf clean all
  - systemctl set-default graphical.target
  - fstrim -av
EOF
  hdiutil makehybrid -quiet -o "$VM_DIR/seed.iso" -iso -joliet -default-volume-name cidata "$seed"
}

wait_for_ssh() {
  printf 'waiting for ssh on port %s' "$SSH_PORT"
  i=0
  # shellcheck disable=SC2046 # ssh_opts is a list of options
  until ssh $(ssh_opts) -o BatchMode=yes "$GUEST_USER@127.0.0.1" true 2>/dev/null; do
    i=$((i + 1))
    [ "$i" -lt 120 ] || die "the VM did not answer on ssh; see $VM_DIR/console.log"
    printf .
    sleep 5
  done
  echo
}

create() {
  [ ! -e "$VM_DIR/disk.qcow2" ] || die "$VM_DIR/disk.qcow2 exists; run destroy first"
  command -v qemu-system-aarch64 >/dev/null || die "qemu-system-aarch64 not found (brew install qemu)"
  mkdir -p "$VM_DIR"
  echo "downloading $FEDORA_IMAGE"
  curl -fL --progress-bar -o "$VM_DIR/disk.qcow2.part" "$FEDORA_URL"
  echo "$FEDORA_SHA256  $VM_DIR/disk.qcow2.part" | shasum -a 256 -c - >/dev/null ||
    die "checksum mismatch for $FEDORA_IMAGE"
  mv "$VM_DIR/disk.qcow2.part" "$VM_DIR/disk.qcow2"
  qemu-img resize -q "$VM_DIR/disk.qcow2" "$DISK_SIZE"
  [ -f "$VM_DIR/id_ed25519" ] || ssh-keygen -q -t ed25519 -N '' -C herdr-gnome-vm -f "$VM_DIR/id_ed25519"
  make_seed

  echo "booting headless to install GNOME Shell (several minutes)"
  qemu none -serial "file:$VM_DIR/console.log" &
  qemu_pid=$!
  trap 'kill "$qemu_pid" 2>/dev/null || true' EXIT INT TERM
  wait_for_ssh
  # shellcheck disable=SC2046
  # "degraded done" (e.g. a hostname warning) exits non-zero, so check the packages instead.
  ssh $(ssh_opts) "$GUEST_USER@127.0.0.1" 'sudo cloud-init status --wait >/dev/null; rpm -q gnome-shell libnotify' ||
    die "provisioning failed; see /var/log/cloud-init-output.log in the VM ($0 ssh)"
  # shellcheck disable=SC2046
  ssh $(ssh_opts) "$GUEST_USER@127.0.0.1" 'sudo systemctl poweroff' || true
  wait "$qemu_pid" || true
  trap - EXIT INT TERM
  echo "done: $(du -sh "$VM_DIR" | cut -f1) in $VM_DIR; start it with: $0 run"
}

cmd=${1:-}
[ $# -eq 0 ] || shift
case $cmd in
  create) create ;;
  run)
    [ -f "$VM_DIR/disk.qcow2" ] || die "no VM in $VM_DIR; run create first"
    qemu cocoa,show-cursor=on,zoom-to-fit=on
    ;;
  ssh)
    # shellcheck disable=SC2046
    exec ssh $(ssh_opts) "$GUEST_USER@127.0.0.1" "$@"
    ;;
  push)
    [ $# -eq 1 ] || die "usage: $0 push FILE"
    # shellcheck disable=SC2046
    ssh $(ssh_opts) "$GUEST_USER@127.0.0.1" 'mkdir -p ~/bin'
    # shellcheck disable=SC2046
    scp -i "$VM_DIR/id_ed25519" -P "$SSH_PORT" -o UserKnownHostsFile="$VM_DIR/known_hosts" \
      "$1" "$GUEST_USER@127.0.0.1:bin/"
    ;;
  destroy)
    [ -d "$VM_DIR" ] || die "no VM in $VM_DIR"
    rm -rf "$VM_DIR"
    echo "deleted $VM_DIR"
    ;;
  -h | --help | help) usage ;;
  *)
    usage >&2
    exit 2
    ;;
esac
