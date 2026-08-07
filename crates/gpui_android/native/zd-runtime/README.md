# zd-runtime

Runtime swap for Zdroid: route every Zed-spawned subprocess (LSPs, git,
formatters, terminal shells, debug adapters, anything) into a
user-configured Linux rootfs (chroot for root users, proot for non-root).

## Files

- **`zd-exec`** — the per-call wrapper. Symlinks at
  `$PREFIX/zd-runtime/<name>` point at this. Reads its argv[0] basename,
  reads `runtime.conf`, dispatches into the rootfs.
- **`zd-runtime-sync`** — populator. Walks the rootfs's PATH dirs
  (`/usr/local/sbin`, `/usr/local/bin`, `/usr/sbin`, `/usr/bin`,
  `/sbin`, `/bin`) and creates one symlink per binary in
  `$PREFIX/zd-runtime/`.
- **`zd-runtime.conf.example`** — config template. Lives at
  `$PREFIX/etc/zd-runtime.conf` once installed.

## Install (manual, until the bootstrap rebuild ships)

```bash
# from inside Zdroid's terminal:
cp zd-exec          $PREFIX/bin/zd-exec
cp zd-runtime-sync  $PREFIX/bin/zd-runtime-sync
chmod +x $PREFIX/bin/zd-exec $PREFIX/bin/zd-runtime-sync

mkdir -p $PREFIX/etc
cp zd-runtime.conf.example $PREFIX/etc/zd-runtime.conf
# edit $PREFIX/etc/zd-runtime.conf to match your setup

zd-runtime-sync
# wait for symlinks to appear in $PREFIX/zd-runtime/
ls $PREFIX/zd-runtime/ | head
```

After this, invoking e.g. `$PREFIX/zd-runtime/git --version` runs the
rootfs's git inside the chroot, with stdio passed through.

## Architecture

See `memory/project_runtime_swap_architecture.md` for the load-bearing
design decision and migration sequence.

The next step after `zd-runtime-sync` is wiring `$PREFIX/zd-runtime/`
into Zed's PATH at app startup (in
`crates/gpui_android/examples/zed_android/src/lib.rs`). After that,
every `Command::new` inside Zed naturally resolves to the wrappers.

## Modes

### chroot

Native Linux chroot. Requires root via Magisk's su. The wrapper:

1. Re-execs itself via `su -c` if not already root.
2. Idempotently runs NetHunter's `bootkali_init` to ensure
   `/dev`, `/proc`, `/sys`, `/sdcard`, `/system` are mounted into
   the rootfs.
3. Bind-mounts Zdroid-B's home (`/data/data/com.zdroid/files/home`) at
   `RUNTIME_HOME_BIND` (default `/zed`) inside the rootfs.
4. Translates the host cwd to a path inside the rootfs.
5. Chroots in via `busybox_nh chroot`.
6. Exec's `/usr/bin/sudo -E PATH=... /bin/bash -c "cd ...; exec target"`.

Job control via `setsid -c` (with TIOCSCTTY force) when stdin is a tty.
Skipped for non-tty subprocess to avoid hijacking the parent's pty.

### proot

Userspace ptrace-based. No root needed.

```
exec proot -r ROOTFS -b host_home:/zed -b /storage/emulated/0:/sdcard -- /bin/bash -c "..."
```

Costs ~15-25% syscall overhead but works on non-rooted devices.
