# studio-display-brightness

**Brightness control for the Apple Studio Display on Linux — and for every other
monitor on the same machine, from the same command.**

The Studio Display does not speak DDC/CI, so the usual tools cannot see it.
Everything else does, and needs a completely different protocol. This handles
both, per monitor, and puts the one you are looking at in your status bar.

```
$ studio-display-brightness list
DP-3    60%  DDC/CI   Dell Inc. U2720Q
DP-1   100%  USB HID  Apple Computer Inc StudioDisplay
```

Scroll on the bar to change the screen you are pointing at. Nothing runs as
root, and nothing needs to: one udev rule hands the display to whoever is
logged in.

## Requirements

- **Hyprland.** `hyprctl` is how the tool knows which screen you are looking at.
  Nothing else here is compositor-specific, but without it there is no focused
  screen to act on, so `--display DP-1` becomes mandatory.
- **`ddcutil`**, for every monitor that is not a Studio Display.
- A Studio Display needs its **USB or Thunderbolt cable** to the host, not just
  DisplayPort. Brightness is a USB control; over video alone there is nothing to
  talk to.

## Why the Studio Display needs its own path

`ddcutil detect` lists it as **Invalid display**, which is what sends people
looking for a vendor tool. There is no need for one: the display describes the
control itself, in its own HID report descriptor.

```
05 80              Usage Page (Monitor)
09 01              Usage (Monitor Control)
a1 01              Collection (Application)
85 01                Report ID 1
06 82 00             Usage Page (Monitor Enumerated Values)
09 10                Usage 0x10 — the VESA VCP code for Brightness
16 90 01             Logical Minimum 400
27 60 ea 00 00       Logical Maximum 60000
55 0e                Unit Exponent -2      → 4.00 … 600.00 nits
75 20  95 01         one 32-bit field
b1 42                Feature
```

So it is a seven-byte feature report — id, a little-endian `u32` of hundredths
of a nit, and a trailing `u16` the display leaves at zero — and the range is the
panel's own 600-nit spec rather than an arbitrary scale. That is why the tooltip
can say `421 nits` and mean it.

Three things that cost an evening if you rediscover them:

- **The display presents five hidraw nodes and only one answers.** They are told
  apart here by their report descriptor — the one that opens with usage page
  `Monitor` — rather than by number, because `/dev/hidraw11` is correct right up
  until something is replugged.
- **The udev rule wants `TAG+="uaccess"`, not a group.** hidraw nodes are
  root-only by default, so a rule is needed either way — but tagging the device
  `uaccess` makes logind give an ACL to whoever is actually logged in
  (`user:you:rw-`), which follows the session. Granting a group instead hands
  the display to that group on every seat, including the ones nobody is sitting
  at. The NixOS module below installs the tagging version.
- **0% is not off.** The display refuses anything below 400 (4 nits), so the
  percentage is mapped onto 400–60000 rather than 0–60000.

Everything that is not a Studio Display goes through `ddcutil`, which carries
years of per-model quirks and retries that are not worth rediscovering one
monitor at a time.

## Which screen it acts on

A status bar module is not told which bar it is drawn on, so it follows the
**focused** screen. Under `input:follow_mouse = 1` — the Hyprland default — the
focused screen is the one under the pointer, which is the one whose bar you just
scrolled on. So the obvious thing happens without naming a display:

| | |
| --- | --- |
| Scroll on a bar | ±5% on that bar's screen |
| Left click | next preset: 25 → 50 → 75 → 100 |
| Right click | full brightness |

Name one explicitly with `--display DP-1` when you want to be sure, which is
what a keybinding tied to one monitor wants.

## Commands

| | |
| --- | --- |
| `studio-display-brightness list` | Every display that can be controlled, and how |
| `studio-display-brightness get` | The focused screen's level |
| `studio-display-brightness set 70` | Set it |
| `studio-display-brightness up` / `down` | ±5%, `--step N` to change that |
| `studio-display-brightness cycle` | Next preset |
| `studio-display-brightness watch` | JSON for the status bar, one line per change |

All of them take `--display DP-1`.

Stepping **clamps rather than wraps**. Scrolling past the bottom stops at the
minimum; it does not jump to full brightness in a dark room.

Keyboard, in your Hyprland config:

```
bind = , XF86MonBrightnessUp,   exec, studio-display-brightness up
bind = , XF86MonBrightnessDown, exec, studio-display-brightness down
```

## What `watch` is careful about

Reading over DDC is slow — a third of a second — and finding out which displays
answer DDC at all probes every I²C bus. Naïvely polling that is where the
five-to-ten-second lag people report with `ddcutil` bar modules comes from.

So the set of displays is worked out once and kept, rescanned every thirty
seconds — or immediately when focus lands on a screen it has never heard of,
which is a display being plugged in. A level once read is remembered for ten
seconds. And a level this tool just set is written to a small file under
`$XDG_RUNTIME_DIR`, which `watch` notices within 250 ms, so scrolling the wheel
moves the number at once rather than at the next hardware read.

## Status bar

`wayle-module.toml` is the module definition for [wayle](https://wayle.app). On
a home-manager machine the wayle config is a read-only store symlink, so the
block belongs in `services.wayle.settings` instead — `home-nix.patch` has the
Nix version.

The same `watch` output works anywhere that takes a JSON line: `text`, `class`
(`dim`/`mid`/`bright`), `tooltip`, `percent` and `display`.

## Installing on NixOS

Add the flake as an input, then the NixOS module. It installs the binary, writes
the udev rule the Studio Display needs, and — for DDC monitors — pulls in
`ddcutil`, loads `i2c-dev` and puts the users you name in the `i2c` group.

```nix
# flake.nix
inputs.studio-display-brightness.url = "github:you/studio-display-brightness";

# configuration.nix
imports = [ inputs.studio-display-brightness.nixosModules.default ];

services.studio-display-brightness = {
  enable = true;
  users = [ "you" ];   # only needed for DDC monitors
};
```

Only ever using a Studio Display? `ddc = false` skips `ddcutil`, the `i2c`
group and opening the I2C buses at all:

```nix
services.studio-display-brightness = { enable = true; ddc = false; };
```

The status bar module is a home-manager one, because that is where a bar's
config lives:

```nix
imports = [ inputs.studio-display-brightness.homeManagerModules.default ];

programs.studio-display-brightness = {
  enable = true;
  wayle.enable = true;      # defines the module; step = 5 by default
};

# then put it in a bar, which is yours to place:
services.wayle.settings.bar.layout = [{
  monitor = "*";
  center = [ "clock" "custom-studio-display-brightness" ];
}];
```

There is an `overlays.default` too, if you would rather have it in `pkgs`.

### Without the modules

`wayle-module.toml` is the bar module on its own, and `home-nix.patch` spells
out the same edits by hand. The udev rule, if you are writing it yourself:

```
SUBSYSTEM=="hidraw", KERNEL=="hidraw*", ATTRS{idVendor}=="05ac", \
  ATTRS{idProduct}=="1114", MODE="0660", TAG+="uaccess"
```

`1116` and `1118` are the Pro Display XDR variants and take the same line.

## Prior art

- [`asdbctl`](https://github.com/juliuszint/asdbctl) — Studio Display only, and
  the tool to use if that is all you need.
- [`asdcontrol`](https://github.com/nikosdion/asdcontrol) — the original, via
  hiddev.
- [`hid-apple-studio-display`](https://github.com/michaljach/hid-apple-studio-display)
  — a kernel driver exposing the display as `/sys/class/backlight`, so
  `brightnessctl` works. A good route if you would rather not run a daemon,
  though it does not help with your other monitors.
- [`ddcci-driver-linux`](https://gitlab.com/ddcci-driver-linux/ddcci-driver-linux)
  — the same idea for DDC monitors.

What none of them do is cover both kinds of display at once, per monitor,
following the screen you are looking at. That is the only reason this exists.

## Licence

MIT.

---

Made by [Eclypsys](https://eclypsys.ch).
