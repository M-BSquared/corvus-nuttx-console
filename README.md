# NuttX Console for Corvus GCS

A plugin for [Corvus GCS](https://github.com/M-BSquared/CorvusGCS), the
offline ground control station for PX4 and ArduPilot.

It opens PX4's own shell, NSH (the NuttShell of NuttX), over the MAVLink
link Corvus is already connected with. No cable to the flight controller, and
every builtin is there, also the ones with no button anywhere: `top`,
`listener`, `dmesg`, `free`, `param`, `ver all`.

It works like the MAVLink console, and the same shell opens in a terminal
window too, with `top` redrawing in place and Ctrl+C to stop a running
program.

The full description is in the
[Corvus guide](https://m-bsquared.github.io/CorvusGCS/guide/plugins.html#nuttx-console).

## What it needs

- Corvus GCS newer than 2026.10.03, the first whose backend opens the shell
  for a terminal (`/api/mavlink/shell/send`, `/api/mavlink/shell/close` and
  the `shell` topic of `/api/events`).
- A PX4 autopilot. ArduPilot has no NSH, and the plugin says so.

## Install

1. Download this repository: **Code, Download ZIP**, or `git clone`.
2. In Corvus, open **Settings, Plugins, Open plugin folder**.
3. Put the folder there and name it `nuttx-console`. That is the folder its
   settings are kept in.
4. Restart Corvus. The plugin is on the PLUGINS tab.

With git, in one step:

```bash
git clone https://github.com/M-BSquared/corvus-nuttx-console.git ~/.corvus/plugins/nuttx-console
```

On Windows the plugin folder is `%USERPROFILE%\.corvus\plugins`.

## Update

Replace the folder's files with the new ones and restart Corvus, or run
`git pull` inside it. Keep `config.json`: it holds the plugin's settings.

## Tests

The tests in `tests/` run against Corvus's own scripts, so they need a Corvus
checkout. Inside one, with this plugin in `plugins/nuttx-console/`, Corvus runs them
with its own:

```bash
node tools/frontend_tests.js
```

Anywhere else, point them at the checkout:

```bash
CORVUS_ROOT=/path/to/CorvusGCS node tests/test_nuttx_console.js
```

## Licence

Sustainable Use License, see [LICENSE.md](LICENSE.md). Copyright (c) 2026
Maximilian Böck.
