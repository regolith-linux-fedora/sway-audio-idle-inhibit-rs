# sway-audio-idle-inhibit-rs

A Rust port of [sway-audio-idle-inhibit](https://github.com/regolith-linux/SwayAudioIdleInhibit),
the program that keeps the [Regolith](https://regolith-desktop.com/) desktop
from going idle (dimming, locking, suspending) while any application plays
or records audio. It can also print whether audio is active, for status
bars. Regolith's sway-audio-idle-inhibit is a fork of
[ErikReider/SwayAudioIdleInhibit](https://github.com/ErikReider/SwayAudioIdleInhibit).

## Status

**Experimental port; not a drop-in replacement yet.**

- **A specific moment in time.** The port reproduces sway-audio-idle-inhibit
  as of [regolith-linux/SwayAudioIdleInhibit@4521b18](https://github.com/regolith-linux/SwayAudioIdleInhibit/commit/4521b18c5d8ca922bbbc67a7dc459059e0ff3279)
  (October 2026, Debian package 2.0.3): ErikReider's `main` plus Regolith's
  packaging and fixes up to #9. The C++ program and its meson build are
  replaced by a Rust crate; the systemd user unit and the Debian maintainer
  scripts are kept.
- **Upstream fixes since then.** Two fixes were merged on
  regolith-linux/SwayAudioIdleInhibit `main` after `4521b18`:
  - #11 (re-enable the user unit when the stale `default.target.wants` link
    was removed by hand before the 26.04 upgrade) is ported here; the
    maintainer script is the same in the port.
  - #10 (don't abort when the first logind `Inhibit` comes after the system
    bus authentication timeout; released as 2.0.4) fixes C++ code the port
    does not have. The port takes no logind lock (see below), its zbus
    connection to the session bus authenticates when it connects and stays
    in use, and a failed D-Bus call is logged instead of aborting. A run
    whose first inhibit came 100 s after start inhibited normally.

  On ErikReider/SwayAudioIdleInhibit, `main` has nothing that the snapshot
  lacks. Its open pull requests are not ported: #44 (uncorked silent
  streams do not inhibit), #47 (ignore playback streams by application
  name) and #27 (fall back to `media.name` for the ignore list). #42, #48
  and #49 only touch the C++ build or the C++ D-Bus code (#49 is #10).
  This list was made on 2026-11-09 and may not be complete: check upstream
  before relying on it.
- **Very deep testing is needed** before anyone considers this port as a
  replacement for the C++ sway-audio-idle-inhibit.
- **No deep code reviews yet.** The port has not yet been reviewed in depth
  by the major AI coding agents and code reviewers. Those reviews are
  planned before any replacement decision.
- **Upstreaming is undecided.** This repository is not a pull request to
  regolith-linux/SwayAudioIdleInhibit; whether, how and when any of it goes
  upstream will be decided later.

## What it ports

One binary, `sway-audio-idle-inhibit`, with one PulseAudio client
connection (PulseAudio, or PipeWire through `pipewire-pulse`):

- The same options as the C++ program: `-h`/`--help`, `--dry-print-both`,
  `--dry-print-both-waybar`, `--dry-print-sink`, `--dry-print-source` and
  `--ignore-source-outputs NAMES`. They are read left to right, with the
  same precedence when several dry-print flags are given, and an unknown
  argument prints the help.
- The same idea of "active": a running (uncorked) playback stream that
  belongs to a client, or a running recording stream that belongs to a
  client and whose application name is not on the ignore list. Streams
  owned by modules, such as loopbacks, do not count.
- The dry-print modes print the same lines as the C++ program
  (`RUNNING`/`NOT RUNNING`, and the waybar JSON line), and use no D-Bus.
- In idle mode it prints `IDLE INHIBITED`/`NOT IDLE INHIBITED` as before.

## Differences from upstream

Deliberate changes in behaviour:

- **Idle is inhibited through gnome-session, not logind.** The port calls
  `org.gnome.SessionManager.Inhibit` on the session bus (app id
  `sway-audio-idle-inhibit`, reason "Audio is playing", flags 8 = idle) and
  `Uninhibit` when audio stops. Both Regolith sessions run under
  gnome-session, so the X11 session honours it as well. In the Wayland
  session, regolith-powerd mirrors gnome-session's idle inhibition to a
  logind idle lock, which swayidle honours. The C++ program took a logind
  idle lock directly. **On a sway setup without gnome-session (outside
  Regolith) the port therefore does not keep the session awake**; it waits
  for a session manager to appear.
- **The user unit runs in both Regolith sessions.** Its conditions accept
  `XDG_SESSION_DESKTOP=regolith-wayland` and `regolith-x11`. Upstream #9
  limited it to the Wayland session, because the logind lock had no effect
  in the X11 session. GNOME sessions and the login screen still do not
  start it.
- **It keeps running** when the session manager or the sound server is
  missing, and reconnects when either comes back. A new owner of the
  session-manager name gets its own `Inhibit`, and a cookie is never
  released on an owner that did not hand it out. While the sound server is
  unreachable nothing counts as active.
- **Level meters never count**: recording streams with resample method
  `peaks` or `stream.monitor=true`, such as the volume meters of mixers.
- **New options**: `--delay SECONDS` (inhibit only after audio has been
  active that long without a break), `--sink-only` and `--source-only`
  (idle mode only). Giving both of the last two, or an invalid `--delay`,
  is a usage error with exit status 2.
- **SIGTERM and SIGINT** release the inhibitor before exiting.
- The package no longer depends on `libsystemd0`, and the binary does not
  link `libstdc++`.

Leftovers: `build-scripts/` (the AUR `PKGBUILD` and the COPR spec) and
`.clang-format` still describe the C++ build.

## History

This repository keeps upstream's history up to `4521b18`, followed by:

1. `Port sway-audio-idle-inhibit from C++ to Rust`: the crate, the Debian
   build with cargo and the user unit's conditions. The C++ sources, the
   meson files and the release workflows are removed.
2. `fix: re-enable the service when the stale link was removed by hand
   (ports #11)`: upstream's commit, unchanged.
3. `Publish as sway-audio-idle-inhibit-rs`: this README and the package
   version `2.0.4+rs1`.

The Debian version is upstream's latest Debian version (2.0.4) plus
`+rs1`. It sorts above upstream's 2.0.4 and below any later upstream
release (2.0.5, 2.1.0), so an upstream package always replaces it.

## Usage

As before, Regolith starts it through the systemd user unit
`sway-audio-idle-inhibit.service`, which is part of
`graphical-session.target`. By hand:

```sh
# Keep the session awake while audio plays or records
sway-audio-idle-inhibit
# Only after 30 s of uninterrupted audio, and only for playback
sway-audio-idle-inhibit --delay 30 --sink-only
# Ignore some recorders (space-separated application names)
sway-audio-idle-inhibit --ignore-source-outputs "cava pavucontrol"
```

Monitor sources and sinks, printing `RUNNING` or `NOT RUNNING`:

```sh
sway-audio-idle-inhibit --dry-print-both
sway-audio-idle-inhibit --dry-print-source
sway-audio-idle-inhibit --dry-print-sink
```

### Waybar integration

Add `custom/audio_idle_inhibitor` to one of the module lists, and this
section to `~/.config/waybar/config` (the icons below use FontAwesome):

```json
	"custom/audio_idle_inhibitor": {
		"format": "{icon}",
		"exec": "sway-audio-idle-inhibit --dry-print-both-waybar",
		"exec-if": "which sway-audio-idle-inhibit",
		"return-type": "json",
		"format-icons": {
			"output": "",
			"input": "",
			"output-input": "  ",
			"none": ""
		}
	},
```

## Building

Needs Rust 1.89 or newer, `pkg-config` and `libpulse-dev`.

```sh
cargo build --release --locked
cargo test --locked
cargo fmt --check
```

The binary is `target/release/sway-audio-idle-inhibit`. To build without
network access, vendor the crates first:

```sh
mkdir -p .cargo
cargo vendor --locked vendor > .cargo/config.toml
cargo build --release --locked --offline
```

### Debian package

```sh
sudo apt-get install debhelper cargo rustc libpulse-dev pkgconf
dpkg-buildpackage -b -us -uc
```

`debian/rules` builds with `cargo build --release --locked`, runs the
tests, and installs `/usr/bin/sway-audio-idle-inhibit` and
`/usr/lib/systemd/user/sway-audio-idle-inhibit.service`. Vendor the
crates as above to build without network access.

## License

GPL-3.0-or-later, as upstream (`LICENSE`).
