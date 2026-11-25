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
plugin API of Corvus GCS. It is not part of the program: it is published as a shared
module from this repository and installed from Settings, Plugins. There is no JavaScript. The web plugin this replaces (version 1) is in the history of this
repository, up to the commit before the Rust port.

- `src/model.rs`: the rules, free of the interface: the terminal's line
  editor, how the line is drawn after the prompt, how NSH's bytes are cut into
  lines, the history.
- `src/mcp.rs`: the tools for an AI client, off until the gear allows them.
- `src/lib.rs`: the shell session (when it opens and closes, what is sent and
  in which order) and the lines of the console.
- `src/view.rs`: what is drawn.
- `lang/`: German, French and the dialect file.

## What it needs

A Corvus GCS whose backend has the `shell` topic of `/api/events` and whose
plugin API has `Host::events` and `corvus_api::ui::term` (newer than
2026.11.16).

## Change it

Corvus GCS carries this repository as a submodule at `plugins/nuttx-console`, and the
crate is a member of its workspace:

```bash
git clone --recurse-submodules https://github.com/M-BSquared/CorvusGCS.git
cd CorvusGCS
./run.sh
```

Settings, Plugins then offers the copy built there. A dependency must be one
that `Cargo.lock` of Corvus GCS already holds.

## Publish it

Commit, then:

```bash
./release.sh
```

It runs the tests and pushes `main`. The workflow builds the plugin once, with the oldest Corvus
GCS that `plugin.json` names (`min_corvus`) or the first published one after it, lets that program
load it, and attaches it to the release `plugin` of this repository. Every later Corvus GCS takes
it as it is. Raise `min_corvus` when the plugin starts to need something newer: older stations
then go on getting the last version that fits them. `plugin.json` also names the permissions the
plugin uses, which the operator sees before installing it. `--watch` waits for the result, `--help`
lists the rest.

## Tests

From the Corvus checkout:

```bash
cargo test -p corvus_plugin_nuttx_console
```

## Licence

Sustainable Use License, see [LICENSE.md](LICENSE.md). Copyright (c) 2026
Maximilian Böck.
