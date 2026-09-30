# display-brightness

Brightness for external displays on Wayland, as a status bar module and a
command. Two displays, two entirely different protocols, one interface:

```
$ display-brightness list
DP-4    60%  DDC/CI   ASUSTek COMPUTER INC PG27UCDM T1LMAS012351
DP-1   100%  USB HID  Apple Computer Inc StudioDisplay 0xEB2958CF
```

## The Studio Display does not speak DDC/CI

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

Three details that cost time if you rediscover them:

- **The display presents five hidraw nodes and only one answers.** They are told
  apart by their report descriptor — the one that opens with usage page
  `Monitor` — rather than by number, because `/dev/hidraw11` is right until
  something is replugged.
- **No root, and no udev rule.** `logind` already grants the active session an
  ACL on the node (`user:you:rw-`). Every guide that tells you to write a udev
  rule is describing a machine where that is not set up.
- **0% is not off.** The display refuses anything below 400 (4 nits), so the
  percentage is mapped onto 400–60000 rather than 0–60000.

Everything else goes through `ddcutil`, which carries years of per-model quirks
and retries that are not worth rediscovering one monitor at a time.

## Which screen it acts on

A wayle custom module is not told which bar it is drawn on, so it follows the
**focused** screen instead. Under `input:follow_mouse = 1` — the default across
most of the wlroots family — the focused screen is the one under the pointer,
which is the one whose bar you just scrolled on. So the obvious thing happens
without naming a display:

| | |
| --- | --- |
| Scroll on a bar | ±5% on that bar's screen |
| Left click | next preset: 25 → 50 → 75 → 100 |
| Right click | full brightness |

Name one explicitly with `--display DP-1` when you want to be sure, which is
what a keybinding on a specific monitor wants.

## Commands

| | |
| --- | --- |
| `display-brightness list` | Every display that can be controlled, and how |
| `display-brightness get` | The focused screen's level |
| `display-brightness set 70` | Set it |
| `display-brightness up` / `down` | ±5%, `--step N` to change that |
| `display-brightness cycle` | Next preset |
| `display-brightness watch` | JSON for the bar, one line per change |

All of them take `--display DP-1`.

Stepping **clamps rather than wraps**. Scrolling past the bottom stops at the
minimum; it does not jump to full brightness in a dark room.

## What `watch` is careful about

Finding out which displays answer DDC probes every I²C bus and costs the best
part of a second, so the set of displays is worked out once and kept, rescanned
every thirty seconds — or immediately when focus lands on a screen it has never
heard of, which is a display being plugged in.

Reading a level over DDC is slow too, so a level once read is remembered for ten
seconds. And a level this tool just set is written to a small file under
`$XDG_RUNTIME_DIR`, which `watch` notices within 250 ms — so scrolling the wheel
moves the number at once rather than at the next hardware read.

## Installing

`wayle-module.toml` is the bar module. On a home-manager machine the wayle
config is a read-only store symlink, so the block belongs in
`services.wayle.settings` instead — `home-nix.patch` has the Nix version,
including the optional `XF86MonBrightness` keybindings.

The flake exposes a NixOS module that installs `ddcutil`, loads `i2c-dev` and
puts the users you name in the `i2c` group. The Studio Display needs none of
that; every other monitor needs all of it.

```nix
services.display-brightness = {
  enable = true;
  users = [ "r3" ];
};
```
