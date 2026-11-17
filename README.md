# NuttX Console for Corvus GCS

A plugin for [Corvus GCS](https://github.com/M-BSquared/CorvusGCS), the
offline ground control station for PX4 and ArduPilot.

It opens PX4's own shell, NSH (the NuttShell of NuttX), over the MAVLink
link Corvus is already connected with. No cable to the flight controller, and
every builtin is there, also the ones with no button anywhere: `top`,
`listener`, `dmesg`, `free`, `param`, `ver all`.

Two views of the one shell, switched at the top of the plugin:

- **Console.** It works like the MAVLink console: filter, pause, copy, save,
  clear, and a command line with history. A program that redraws its screen,
  like `top`, replaces its lines instead of adding a screen a second.
- **Terminal.** The shell as a terminal, with `top` redrawing in place and
  Ctrl+C to stop a running program. The line is edited in Corvus and sent on
  Enter, so typing does not wait for a round trip over a telemetry radio.

The shell is opened when a PX4 autopilot is connected and the plugin is
showing, and closed when the plugin is closed. ArduPilot has no NSH, and the
plugin says so.

## What it is

A Rust crate (`corvus_plugin_nuttx_console`) built on `corvus_api`, the
plugin API of Corvus GCS. Corvus compiles its plugins in: there is no loader
and no JavaScript, so the plugin works the same on every platform Corvus is
built for. The web plugin this replaces (version 1) is in the history of this
repository, up to the commit before the Rust port.

- `src/model.rs`: the rules, free of the interface: the terminal's line
  editor, how the line is drawn after the prompt, how NSH's bytes are cut into
  lines, the history.
- `src/lib.rs`: the shell session (when it opens and closes, what is sent and
  in which order) and the lines of the console.
- `src/view.rs`: what is drawn.
- `lang/`: German, French and the dialect file.

## What it needs

A Corvus GCS whose backend has the `shell` topic of `/api/events` and whose
plugin API has `Host::events` and `corvus_api::ui::term` (newer than
2026.11.16).

## Build

Corvus GCS carries this repository as a submodule at `plugins/nuttx-console`:

```bash
git clone --recurse-submodules https://github.com/M-BSquared/CorvusGCS.git
```

In a checkout that was cloned without it:

```bash
git submodule update --init plugins/nuttx-console
```

Then `./run.sh` in the Corvus checkout builds and runs Corvus with the plugin.
It is on the PLUGINS tab, and its gear under Settings, Plugins gives it a tab
of its own.

## Tests

From the Corvus checkout:

```bash
cargo test -p corvus_plugin_nuttx_console
```

## Licence

Sustainable Use License, see [LICENSE.md](LICENSE.md). Copyright (c) 2026
Maximilian Böck.
