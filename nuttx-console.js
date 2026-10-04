"use strict";
window.Corvus = window.Corvus || {};

/**
 * NuttX Console: the PX4 NuttShell (NSH), as a console and as a terminal.
 *
 * PX4 runs NSH for a ground station over MAVLink (SERIAL_CONTROL, device
 * SHELL); it is how `top`, `listener`, `dmesg`, `free`, `param` and the
 * other builtins with no MAVLink command are reached without a cable.
 *
 * Two views of one shell:
 *
 *   the console   the plugin's tab or card. It is built from the MAVLink
 *                 console's own parts (its toolbar, stream, input row and
 *                 footer, with their classes), so it looks and behaves like
 *                 it: filter, pause, copy, save, clear, a command line with
 *                 history. NSH's output arrives as bytes and is cut into lines
 *                 here; a program that redraws its screen, like `top`,
 *                 replaces its lines instead of adding a screen a second.
 *   the terminal  a window of its own (Corvus.floatWindows, the frame the SSH
 *                 terminals use), opened from the console's toolbar. xterm,
 *                 with the escapes drawn rather than cut out. The line is
 *                 edited here and sent on Enter, as PX4's own
 *                 Tools/mavlink_shell.py and QGroundControl do it: a packet
 *                 per keystroke would wait a round trip over a telemetry
 *                 radio, and NSH echoes the line it receives.
 *
 * Both type into the same shell and both show what it prints. The shell is
 * opened when a PX4 autopilot is connected and one of the two is showing, and
 * closed when the last of them goes (or the page does): a `top` left running
 * would otherwise keep streaming over the telemetry link for nobody.
 * ArduPilot has no NSH, so there both say so instead of offering a prompt
 * that cannot answer.
 *
 * Transport, all in the backend (corvus/mavlink_shell.py):
 *   POST /api/mavlink/shell/send {data}   typed as given
 *   POST /api/mavlink/shell/close         ends the shell, frees the link
 *   GET  /api/events?topics=shell          the output; the first event is a
 *                                         replay of what the shell printed so
 *                                         far
 *
 * Self-contained like every plugin: this script, its stylesheet
 * (nuttx-console.css, every class of its own prefixed nx-), the vendored
 * xterm the app loads on demand, Corvus.ui, Corvus.floatWindows and the
 * plugin api.
 */
Corvus.pluginNuttx = (function () {
  const SEND_URL = "/api/mavlink/shell/send";
  const CLOSE_URL = "/api/mavlink/shell/close";
  const SAVE_URL = "/api/console/save";
  const WINDOW_KEY = "nsh:terminal";
  const HISTORY_MAX = 50;
  const HISTORY_SAVE_MS = 800;
  // The most the backend takes in one write; a longer paste goes in several.
  const SEND_MAX_CHARS = 4096;
  // What the session keeps of the shell's output for a view that opens late:
  // as much as the backend replays.
  const RAW_MAX_CHARS = 64 * 1024;
  // Lines the console keeps, as the MAVLink console does.
  const MAX_LINES = 2000;
  // A line with no newline in sight is shown once it is this long.
  const LINE_MAX_CHARS = 8192;

  // The terminal's own notes are dim, so they never read as the autopilot's.
  const NOTE_ON = "\x1b[2m";
  const NOTE_OFF = "\x1b[0m";

  const PROMPT_RE = /^(nsh|pxh)>( |$)/;

  // ---- the line editor (the terminal) -------------------------------------

  const CONTROL_KEYS = {
    "\x7f": "backspace", "\b": "backspace",
    "\x01": "home", "\x05": "end", "\x02": "left", "\x06": "right",
    "\x10": "up", "\x0e": "down",
    "\x15": "killBefore", "\x0b": "killAfter", "\x17": "wordDelete",
  };
  const CSI_KEYS = {
    A: "up", B: "down", C: "right", D: "left", H: "home", F: "end",
    "1~": "home", "7~": "home", "4~": "end", "8~": "end", "3~": "delete",
    "1;5C": "wordRight", "1;3C": "wordRight", "1;5D": "wordLeft", "1;3D": "wordLeft",
  };
  const SS3_KEYS = { A: "up", B: "down", C: "right", D: "left", H: "home", F: "end" };

  /** The key an escape sequence at `s[i]` stands for, and how long it is. */
  function readEscape(s, i) {
    const next = s[i + 1];
    if (next === "[") {
      let j = i + 2;
      while (j < s.length) {
        const code = s.charCodeAt(j);
        if (code >= 0x40 && code <= 0x7e) break;
        j += 1;
      }
      const end = Math.min(j, s.length - 1);
      return { key: CSI_KEYS[s.slice(i + 2, end + 1)] || "", length: end + 1 - i };
    }
    if (next === "O" && i + 2 < s.length) return { key: SS3_KEYS[s[i + 2]] || "", length: 3 };
    if (next === "b") return { key: "wordLeft", length: 2 };
    if (next === "f") return { key: "wordRight", length: 2 };
    if (next === "\x7f") return { key: "wordDelete", length: 2 };
    return { key: "", length: 1 };
  }

  /**
   * The line being typed in the terminal, its cursor and the command history.
   *
   * feed(data) takes what xterm reports (a key, or a whole paste) and returns
   * what the view has to do, in order:
   *   {type: "render", line, cursor} the line or the cursor moved; the
   *                                 state then, which a later key in the
   *                                 same paste may already have changed
   *   {type: "submit", line}        Enter: send `line` and a newline
   *   {type: "cancel", line}        Ctrl+C on a line: drop it, ask for a prompt
   *   {type: "interrupt"}           Ctrl+C on an empty line: send it to NSH
   *   {type: "clear"}               Ctrl+L
   *
   * Pure, and exported for the test suite.
   * @param {string[]} [saved] earlier commands, oldest first
   */
  function createEditor(saved) {
    let line = "";
    let cursor = 0;
    let history = cleanHistory(saved);
    let index = -1;       // -1: a new line; otherwise the history entry shown
    let draft = "";       // the new line, kept while the history is browsed

    function wordStartBefore(pos) {
      let p = pos;
      while (p > 0 && line[p - 1] === " ") p -= 1;
      while (p > 0 && line[p - 1] !== " ") p -= 1;
      return p;
    }

    function wordEndAfter(pos) {
      let p = pos;
      while (p < line.length && line[p] === " ") p += 1;
      while (p < line.length && line[p] !== " ") p += 1;
      return p;
    }

    function show(text) {
      line = text;
      cursor = text.length;
      return true;
    }

    /** Apply one named key. True when the line or the cursor changed. */
    function key(name) {
      switch (name) {
        case "left":
          if (!cursor) return false;
          cursor -= 1; return true;
        case "right":
          if (cursor >= line.length) return false;
          cursor += 1; return true;
        case "home":
          if (!cursor) return false;
          cursor = 0; return true;
        case "end":
          if (cursor >= line.length) return false;
          cursor = line.length; return true;
        case "wordLeft": {
          const p = wordStartBefore(cursor);
          if (p === cursor) return false;
          cursor = p; return true;
        }
        case "wordRight": {
          const p = wordEndAfter(cursor);
          if (p === cursor) return false;
          cursor = p; return true;
        }
        case "backspace":
          if (!cursor) return false;
          line = line.slice(0, cursor - 1) + line.slice(cursor);
          cursor -= 1; return true;
        case "delete":
          if (cursor >= line.length) return false;
          line = line.slice(0, cursor) + line.slice(cursor + 1);
          return true;
        case "wordDelete": {
          const p = wordStartBefore(cursor);
          if (p === cursor) return false;
          line = line.slice(0, p) + line.slice(cursor);
          cursor = p; return true;
        }
        case "killBefore":
          if (!cursor) return false;
          line = line.slice(cursor);
          cursor = 0; return true;
        case "killAfter":
          if (cursor >= line.length) return false;
          line = line.slice(0, cursor);
          return true;
        case "up":
          if (!history.length || index === 0) return false;
          if (index === -1) { draft = line; index = history.length - 1; } else index -= 1;
          return show(history[index]);
        case "down":
          if (index === -1) return false;
          if (index < history.length - 1) { index += 1; return show(history[index]); }
          index = -1;
          return show(draft);
        default:
          return false;
      }
    }

    function startOver() {
      line = "";
      cursor = 0;
      index = -1;
      draft = "";
    }

    function feed(data) {
      const effects = [];
      let changed = false;
      const flush = () => {
        if (changed) effects.push({ type: "render", line, cursor });
        changed = false;
      };
      const s = String(data == null ? "" : data);
      let afterCR = false;
      let i = 0;
      while (i < s.length) {
        const ch = s[i];
        if (ch === "\x1b") {
          const esc = readEscape(s, i);
          i += esc.length;
          afterCR = false;
          if (key(esc.key)) changed = true;
          continue;
        }
        i += 1;
        // A pasted CRLF is one line break, not an empty command after it.
        if (ch === "\n" && afterCR) { afterCR = false; continue; }
        afterCR = ch === "\r";
        if (ch === "\r" || ch === "\n") {
          flush();
          const text = line;
          history = remember(history, text);
          startOver();
          effects.push({ type: "submit", line: text });
          continue;
        }
        if (ch === "\x03") {
          flush();
          effects.push(line ? { type: "cancel", line } : { type: "interrupt" });
          startOver();
          continue;
        }
        if (ch === "\x0c") {
          flush();
          effects.push({ type: "clear" });
          changed = true;
          continue;
        }
        const named = CONTROL_KEYS[ch];
        if (named) {
          if (key(named)) changed = true;
          continue;
        }
        if (ch < " ") continue;            // Tab and the rest: NSH completes nothing
        line = line.slice(0, cursor) + ch + line.slice(cursor);
        cursor += 1;
        changed = true;
      }
      flush();
      return effects;
    }

    return {
      feed,
      state: () => ({ line, cursor }),
      history: () => history.slice(),
      reset: startOver,
    };
  }

  function cleanHistory(saved) {
    return (Array.isArray(saved) ? saved : [])
      .filter((h) => typeof h === "string" && h.trim())
      .slice(-HISTORY_MAX);
  }

  /** `history` with `text` added at the end: no blanks, no repeat of the last. */
  function remember(history, text) {
    if (typeof text !== "string" || !text.trim()) return history;
    const next = history[history.length - 1] === text ? history.slice() : history.concat([text]);
    return next.slice(-HISTORY_MAX);
  }

  // ---- drawing the line (the terminal) ------------------------------------
  //
  // The line is drawn after whatever the shell printed last (its prompt), at
  // the column the cursor was in then. Positions are counted from there, so a
  // line longer than the terminal is wide wraps and is still erased and
  // redrawn whole.

  /** The escape sequence that moves the cursor from one index of the line to another. */
  function cursorMove(cols, startCol, from, to) {
    const width = Math.max(1, cols | 0);
    const a = startCol + from;
    const b = startCol + to;
    const dy = Math.floor(b / width) - Math.floor(a / width);
    let out = "";
    if (dy < 0) out += `\x1b[${-dy}A`;
    else if (dy > 0) out += `\x1b[${dy}B`;
    return out + `\x1b[${(b % width) + 1}G`;
  }

  /** Erase from the line's start, draw it, and put the cursor where it belongs. */
  function paintLine(cols, startCol, line, cursor) {
    let out = "\x1b[J" + line;
    // Ending exactly on the last column leaves xterm waiting to wrap, a row
    // short of where the arithmetic above puts the cursor. A space and a
    // backspace take it there.
    if (line.length && (startCol + line.length) % Math.max(1, cols | 0) === 0) out += " \b";
    return out + cursorMove(cols, startCol, line.length, cursor);
  }

  // ---- cutting the output into lines (the console) ------------------------

  /**
   * NSH's bytes, as the console's lines.
   *
   * feed(text) returns, in order, {type: "line", text} for every line that
   * ended and {type: "clear"} where the shell cleared its screen or sent the
   * cursor home: that is how `top` starts each redraw, and the console
   * replaces the screen it showed rather than adding one a second. Other
   * escapes (colour, erase to the end of the line) are dropped, a backspace
   * takes the character before it, and a carriage return on its own starts
   * the line again, as it would on a screen. What has not ended yet (the
   * prompt, waiting) is pending() and is kept for the next feed.
   *
   * Pure, and exported for the test suite.
   */
  function createAssembler() {
    let partial = "";
    let cr = false;
    let state = "text";          // text | esc | csi
    let params = "";

    function feed(text) {
      const out = [];
      const s = String(text == null ? "" : text);
      for (let i = 0; i < s.length; i++) {
        const ch = s[i];
        if (state === "esc") {
          if (ch === "[") { state = "csi"; params = ""; continue; }
          state = "text";
          if (ch === "c") out.push({ type: "clear" });       // full reset
          continue;
        }
        if (state === "csi") {
          const code = s.charCodeAt(i);
          if (code >= 0x40 && code <= 0x7e) {
            state = "text";
            const home = (ch === "H" || ch === "f") && (params === "" || params === "1" || params === "1;1");
            if (home || (ch === "J" && params === "2")) out.push({ type: "clear" });
          } else {
            params += ch;
          }
          continue;
        }
        if (ch === "\x1b") { state = "esc"; continue; }
        if (ch === "\r") { cr = true; continue; }
        if (ch === "\n") {
          out.push({ type: "line", text: partial });
          partial = "";
          cr = false;
          continue;
        }
        if (cr) { partial = ""; cr = false; }
        if (ch === "\b") { partial = partial.slice(0, -1); continue; }
        if (ch !== "\t" && (ch < " " || ch === "\x7f")) continue;
        partial += ch;
        if (partial.length >= LINE_MAX_CHARS) {
          out.push({ type: "line", text: partial });
          partial = "";
        }
      }
      return out;
    }

    function reset() {
      partial = "";
      cr = false;
      state = "text";
      params = "";
    }

    return { feed, reset, pending: () => partial };
  }

  /** The console level a line of NSH output is shown at. Pure. */
  function lineLevel(text) {
    if (PROMPT_RE.test(text)) return "cmd";
    if (/^ERROR\b/.test(text)) return "error";
    if (/^WARN\b/.test(text)) return "warning";
    return "shell";
  }

  // ---- shared bits ---------------------------------------------------------

  function el(tag, className, text) {
    const e = document.createElement(tag);
    if (className) e.className = className;
    if (text != null) e.textContent = text;
    return e;
  }

  function ensureXterm() {
    const term = Corvus.sshTerm;
    if (term && typeof term.ensure === "function") return term.ensure();
    const lazy = Corvus.lazy;
    if (lazy && typeof lazy.terminal === "function") return lazy.terminal();
    return typeof window.Terminal === "function"
      ? Promise.resolve() : Promise.reject(new Error("no terminal"));
  }

  function xtermTheme() {
    try {
      if (Corvus.sshTerm && typeof Corvus.sshTerm.themeFromTokens === "function") {
        return Corvus.sshTerm.themeFromTokens();
      }
    } catch (_e) { /* the default theme then */ }
    return undefined;
  }

  function copyToClipboard(text, refocus) {
    if (Corvus.sshTerm && typeof Corvus.sshTerm.copyText === "function") {
      Corvus.sshTerm.copyText(text, refocus);
      return Promise.resolve();
    }
    if (navigator.clipboard && typeof navigator.clipboard.writeText === "function") {
      return navigator.clipboard.writeText(text);
    }
    return Promise.reject(new Error("no clipboard"));
  }

  // ---- the shell, shared by the console and the terminal --------------------

  /**
   * One shell for both views. A view attaches with
   * {onMode(mode, previous), onText(text, replay), onNote(text, level),
   * onReset()} and is told everything from then on; the first to attach
   * wires the output stream and the telemetry, the last to go closes the
   * shell and lets them go.
   *
   * mode is {kind, label, detail}: kind "loading", "offline", "unsupported"
   * or "live"; label the stack's name; detail the sentence to show.
   */
  function createSession() {
    let api = null;
    const views = new Set();
    let mode = { kind: "loading", label: "", detail: "" };
    let raw = "";
    let history = [];
    let link = null;
    let capsSeq = 0;
    let opened = false;
    let pendingInput = "";
    let sending = false;
    let lastClose = Promise.resolve();
    let saveTimer = null;
    const unwire = [];

    function tell(method, ...args) {
      views.forEach((view) => {
        if (typeof view[method] !== "function") return;
        try { view[method](...args); } catch (err) { console.error("NuttX console view failed:", err); }
      });
    }

    function setMode(kind, label, detail) {
      const previous = mode;
      if (previous.kind === kind && previous.label === label && previous.detail === detail) return;
      mode = { kind, label: label || "", detail: detail || "" };
      tell("onMode", mode, previous);
    }

    // ---- what is connected ----

    function capabilities() {
      const caps = Corvus.capabilities;
      if (!caps || typeof caps.get !== "function") return Promise.resolve(null);
      return Promise.resolve().then(() => caps.get()).catch(() => null);
    }

    function evaluate(state) {
      if (!views.size) return;
      const connected = !!(state && state.connected);
      const stack = (state && state.autopilot_stack) || "";
      if (link && link.connected === connected && link.stack === stack) return;
      link = { connected, stack };
      const seq = ++capsSeq;
      if (!connected) {
        setMode("offline", "", "No vehicle connected. The shell opens when a PX4 autopilot connects.");
        return;
      }
      capabilities().then((caps) => {
        if (!views.size || seq !== capsSeq) return;
        const label = (caps && caps.label) || "";
        if (caps && caps.shell === false) {
          setMode("unsupported", label,
            `Not available on this autopilot. ${label || "It"} has no NuttX shell: the shell is part of PX4.`);
          return;
        }
        if (mode.kind === "live") return;
        setMode("live", label || "PX4", "");
        // A newline is how NSH is asked for a prompt; the first write also
        // starts the shell on the autopilot.
        send("\n");
      });
    }

    // ---- the output ----

    function onShell(data) {
      if (!data || typeof data.text !== "string") return;
      if (data.replay) {
        raw = data.text.slice(-RAW_MAX_CHARS);
        tell("onText", data.text, true);
        return;
      }
      raw += data.text;
      if (raw.length > RAW_MAX_CHARS) raw = raw.slice(-RAW_MAX_CHARS);
      tell("onText", data.text, false);
    }

    // ---- typing ----

    function send(data) {
      if (!views.size || !data || !api) return;
      pendingInput += data;
      if (!sending) drain();
    }

    async function drain() {
      sending = true;
      while (pendingInput && views.size) {
        await lastClose;
        if (!views.size) break;
        const chunk = pendingInput.slice(0, SEND_MAX_CHARS);
        pendingInput = pendingInput.slice(chunk.length);
        let reply;
        try {
          reply = await api.postJson(SEND_URL, { data: chunk });
        } catch (err) {
          const body = (err && err.body) || {};
          reply = Object.assign({ ok: false }, body,
            { status: err && err.status, error: body.error || (err && err.message) });
        }
        if (reply && reply.ok) { opened = true; continue; }
        // What was typed after a refusal would be refused as well.
        pendingInput = "";
        refused(reply || {});
      }
      pendingInput = "";
      sending = false;
    }

    function refused(reply) {
      const error = String(reply.error || "no answer from Corvus");
      if (reply.status === 409) {
        setMode("unsupported", mode.label, `Not available on this autopilot. ${error}`);
        return;
      }
      if (mode.kind === "live") tell("onNote", `Not sent: ${error}`, "error");
    }

    function closeShell() {
      if (!opened) return lastClose;
      opened = false;
      const post = api ? api.postJson.bind(api) : null;
      lastClose = Promise.resolve()
        .then(() => (post ? post(CLOSE_URL, {}) : null))
        .catch(() => {});
      return lastClose;
    }

    /** End the shell, and whatever runs in it, and open a fresh one. */
    function newShell() {
      if (mode.kind !== "live") return;
      raw = "";
      tell("onReset");
      closeShell();
      send("\n");
    }

    // ---- history, shared by both views and kept in the plugin's settings ----

    function rememberCommand(text) {
      const next = remember(history, text);
      if (next === history) return;
      history = next;
      if (!api || typeof api.saveSettings !== "function") return;
      if (saveTimer) clearTimeout(saveTimer);
      saveTimer = setTimeout(saveHistory, HISTORY_SAVE_MS);
    }

    function saveHistory() {
      if (saveTimer) { clearTimeout(saveTimer); saveTimer = null; }
      if (!api || typeof api.saveSettings !== "function") return;
      Promise.resolve(api.saveSettings({ history: history.slice() })).catch(() => {});
    }

    // ---- views coming and going ----

    // The page going away (a reload, the window closed) takes its views with
    // it without destroying them; the shell on the autopilot is closed here,
    // with a request the browser lets finish.
    function onPageHide() {
      if (!opened) return;
      opened = false;
      try {
        fetch(CLOSE_URL, {
          method: "POST", keepalive: true,
          headers: { "Content-Type": "application/json" }, body: "{}",
        }).catch(() => {});
      } catch (_e) { /* the page is going anyway */ }
    }

    function wire() {
      const settings = typeof api.getSettings === "function" ? api.getSettings() : {};
      history = cleanHistory(settings && settings.history);
      const events = Corvus.events;
      if (events && typeof events.subscribe === "function") {
        unwire.push(events.subscribe("shell", onShell));
      }
      if (typeof api.subscribe === "function") {
        const unsub = api.subscribe(evaluate);
        if (typeof unsub === "function") unwire.push(unsub);
      }
      window.addEventListener("pagehide", onPageHide);
      unwire.push(() => window.removeEventListener("pagehide", onPageHide));
      if (!events || typeof events.subscribe !== "function") {
        setMode("unsupported", "", "This version of Corvus cannot stream the shell.");
        return;
      }
      evaluate(typeof api.getState === "function" ? api.getState() : null);
    }

    function attach(view, viewApi) {
      if (viewApi) api = viewApi;
      if (!api) return () => {};
      views.add(view);
      if (views.size === 1) wire();
      else if (typeof view.onMode === "function") view.onMode(mode, null);
      return () => detach(view);
    }

    function detach(view) {
      if (!views.delete(view) || views.size) return;
      unwire.splice(0).forEach((fn) => { try { fn(); } catch (_e) { /* keep going */ } });
      if (saveTimer) saveHistory();
      closeShell();
      raw = "";
      link = null;
      capsSeq += 1;
      mode = { kind: "loading", label: "", detail: "" };
    }

    return {
      attach,
      send,
      newShell,
      rememberCommand,
      history: () => history.slice(),
      mode: () => mode,
      raw: () => raw,
      viewCount: () => views.size,
    };
  }

  const session = createSession();

  // ---- the console (tab or card) ---------------------------------------------

  /**
   * The console view, inside the plugin's container. Built from the MAVLink
   * console's classes (console-toolbar, console-output, con-line, ...), so
   * the two cannot drift apart in looks.
   */
  function mountConsole(containerEl, api) {
    const ui = Corvus.ui;
    let disposed = false;
    let mode = session.mode();

    const root = el("div", "nx nx-console");

    // Toolbar: the filter, then the tools.
    const toolbar = el("div", "console-toolbar");
    const search = el("div", "console-search");
    const searchIcon = el("i", "console-search-icon");
    searchIcon.setAttribute("data-lucide", "search");
    const filterInput = el("input", "console-filter");
    filterInput.type = "text";
    filterInput.placeholder = "Filter";
    filterInput.setAttribute("autocomplete", "off");
    filterInput.setAttribute("spellcheck", "false");
    filterInput.setAttribute("aria-label", "Filter shell lines");
    search.append(searchIcon, filterInput);
    const tools = el("div", "console-tools");
    const terminalBtn = ui.iconButton("square-terminal", {
      title: "Open the shell in a terminal window",
      onClick: () => openWindow(api),
    });
    const stopBtn = ui.iconButton("octagon-x", {
      title: "Stop the running program (Ctrl+C)",
      onClick: () => { if (mode.kind === "live") session.send("\x03"); },
    });
    const pauseBtn = ui.iconButton("pause", { title: "Pause the stream", onClick: () => setPaused(!paused) });
    const copyBtn = ui.iconButton("copy", { title: "Copy visible lines", onClick: copyVisible });
    const saveBtn = ui.iconButton("download", { title: "Save log to a file", onClick: saveLog });
    const clearBtn = ui.iconButton("rotate-ccw", { title: "Clear console", onClick: clearLines });
    tools.append(terminalBtn, stopBtn, pauseBtn, copyBtn, saveBtn, clearBtn);
    toolbar.append(search, tools);

    // The stream, and the chip that takes it back to the live end.
    const stream = el("div", "console-stream");
    const output = el("div", "console-output");
    output.setAttribute("role", "log");
    output.setAttribute("aria-live", "polite");
    const jumpBtn = el("button", "console-jump");
    jumpBtn.type = "button";
    jumpBtn.hidden = true;
    const jumpIcon = el("i");
    jumpIcon.setAttribute("data-lucide", "arrow-down");
    const jumpLabel = el("span", "", "Jump to live");
    jumpBtn.append(jumpIcon, jumpLabel);
    stream.append(output, jumpBtn);

    // The command line.
    const inputRow = el("div", "console-input-row");
    const prompt = el("span", "console-prompt", "nsh>");
    const input = el("input", "console-input");
    input.type = "text";
    input.setAttribute("autocomplete", "off");
    input.setAttribute("spellcheck", "false");
    input.setAttribute("aria-label", "NSH command");
    const sendBtn = ui.button({
      variant: "primary", shape: "round", icon: "send", className: "console-send", title: "Send",
      onClick: () => submit(),
    });
    inputRow.append(prompt, input, sendBtn);

    const footer = el("div", "console-footer");
    const countEl = el("span", "console-count");
    const hintEl = el("span", "console-hint");
    footer.append(countEl, hintEl);

    root.append(toolbar, stream, inputRow, footer);
    containerEl.appendChild(root);
    if (ui.refreshIcons) ui.refreshIcons();

    // ---- the lines ----

    let lines = [];
    let held = [];
    let paused = false;
    let filterText = "";
    let autoscroll = true;
    let missed = 0;
    // Where the current program's screen starts in `lines`: a clear from the
    // shell (`top` redrawing) takes everything after it away.
    let screenStart = 0;
    const assembler = createAssembler();

    function passes(rec) {
      return !filterText || rec.text.toLowerCase().indexOf(filterText) !== -1;
    }

    function lineEl(rec) {
      const line = el("div", "con-line " + rec.level);
      line.append(
        el("span", "con-arrow", ">>"), document.createTextNode(" "),
        el("span", "con-msg", rec.text));
      return line;
    }

    function updateCount() {
      const parts = [filterText ? `${lines.filter(passes).length} / ${lines.length}` : `${lines.length}`];
      if (held.length) parts.push(`${held.length} held`);
      if (paused) parts.push("paused");
      countEl.textContent = parts.join("  ·  ");
      countEl.classList.toggle("is-paused", paused);
      jumpBtn.hidden = autoscroll;
      jumpLabel.textContent = missed > 0 ? `${missed} new line${missed === 1 ? "" : "s"}` : "Jump to live";
    }

    function renderLines() {
      ui.clear(output);
      lines.filter(passes).forEach((rec) => output.appendChild(lineEl(rec)));
      if (autoscroll) output.scrollTop = output.scrollHeight;
      updateCount();
    }

    function pushLine(rec) {
      lines.push(rec);
      if (lines.length > MAX_LINES) {
        const drop = lines.length - MAX_LINES;
        lines = lines.slice(drop);
        screenStart = Math.max(0, screenStart - drop);
      }
      if (rec.level === "cmd") screenStart = lines.length;
      if (passes(rec)) {
        output.appendChild(lineEl(rec));
        while (output.children.length > MAX_LINES) output.removeChild(output.firstChild);
        if (autoscroll) output.scrollTop = output.scrollHeight;
        else missed += 1;
      }
      updateCount();
    }

    function addLine(level, text, fromShell) {
      const rec = { level, text: String(text), fromShell: !!fromShell };
      if (paused) {
        held.push(rec);
        if (held.length > MAX_LINES) held.shift();
        updateCount();
        return;
      }
      pushLine(rec);
    }

    /** The shell cleared its screen: the lines of the screen before go. */
    function clearScreen() {
      if (paused) {
        // Held lines are the new screen's too; the shown ones are kept, so a
        // paused console stays what the operator is reading.
        held = [];
        updateCount();
        return;
      }
      if (screenStart >= lines.length) return;
      lines = lines.slice(0, screenStart);
      renderLines();
    }

    function setPaused(next) {
      paused = !!next;
      if (!paused) {
        const flush = held;
        held = [];
        flush.forEach(pushLine);
      }
      pauseBtn.classList.toggle("active", paused);
      pauseBtn.title = paused ? "Resume the stream" : "Pause the stream";
      pauseBtn.setAttribute("aria-label", pauseBtn.title);
      ui.clear(pauseBtn).appendChild(ui.icon(paused ? "play" : "pause", 15));
      if (ui.refreshIcons) ui.refreshIcons();
      updateCount();
    }

    function clearLines() {
      lines = [];
      held = [];
      screenStart = 0;
      missed = 0;
      renderLines();
    }

    function transcript() {
      return lines.filter(passes).map((r) => r.text).join("\n");
    }

    function copyVisible() {
      const text = transcript();
      if (!text) return;
      const count = lines.filter(passes).length;
      copyToClipboard(text).then(
        () => addLine("info", `Copied ${count} lines to the clipboard.`),
        () => addLine("error", "Clipboard unavailable. Use Save instead."));
    }

    function saveLog() {
      const text = transcript();
      if (!text) return;
      const stamp = new Date().toISOString().replace(/[:.]/g, "-").slice(0, 19);
      Promise.resolve()
        .then(() => api.postJson(SAVE_URL, { filename: `corvus-nsh_${stamp}.log`, text }))
        .then((res) => {
          if (res && res.path) addLine("success", `NSH log written to ${res.path}`);
          else addLine("error", (res && res.error) || "Could not save the log");
        })
        .catch((err) => addLine("error", (err && err.message) || "Could not save the log"));
    }

    function take(events) {
      events.forEach((ev) => {
        if (ev.type === "clear") { clearScreen(); return; }
        if (!ev.text.trim()) return;
        addLine(lineLevel(ev.text), ev.text, true);
      });
    }

    // ---- the command line ----

    let historyIndex = -1;
    let draft = "";

    function submit() {
      if (mode.kind !== "live") return;
      const text = input.value;
      // Enter on an empty line is sent too: it asks for a prompt, and it is
      // what ends a `top`.
      session.send(text + "\n");
      session.rememberCommand(text);
      input.value = "";
      historyIndex = -1;
      draft = "";
    }

    function browse(step) {
      const history = session.history();
      if (!history.length) return;
      if (historyIndex === -1) {
        if (step > 0) return;
        draft = input.value;
        historyIndex = history.length - 1;
      } else {
        historyIndex += step;
        if (historyIndex >= history.length) {
          historyIndex = -1;
          input.value = draft;
          return;
        }
        historyIndex = Math.max(0, historyIndex);
      }
      input.value = history[historyIndex];
    }

    function onKeyDown(e) {
      if (e.key === "Enter") { e.preventDefault(); submit(); return; }
      if (e.key === "ArrowUp") { e.preventDefault(); browse(-1); return; }
      if (e.key === "ArrowDown") { e.preventDefault(); browse(1); return; }
      if (e.key === "Escape") { input.value = ""; historyIndex = -1; return; }
      // Ctrl+C with nothing selected is the shell's, as in the terminal; with
      // a selection it copies, as everywhere.
      const selected = input.selectionStart !== input.selectionEnd;
      if (e.ctrlKey && !e.shiftKey && !e.metaKey && (e.key === "c" || e.key === "C") && !selected) {
        e.preventDefault();
        if (mode.kind === "live") session.send("\x03");
      }
    }

    function onScroll() {
      const atEnd = output.scrollHeight - output.scrollTop - output.clientHeight < 24;
      if (atEnd !== autoscroll) {
        autoscroll = atEnd;
        if (atEnd) missed = 0;
        updateCount();
      }
    }

    function jumpToLive() {
      autoscroll = true;
      missed = 0;
      output.scrollTop = output.scrollHeight;
      updateCount();
    }

    input.addEventListener("keydown", onKeyDown);
    output.addEventListener("scroll", onScroll);
    jumpBtn.addEventListener("click", jumpToLive);
    filterInput.addEventListener("input", () => {
      filterText = filterInput.value.trim().toLowerCase();
      renderLines();
    });

    // ---- the shell ----

    let focusOnLive = true;

    function showMode(next, previous) {
      mode = next;
      const live = next.kind === "live";
      input.disabled = !live;
      sendBtn.disabled = !live;
      stopBtn.disabled = !live;
      input.placeholder = live
        ? "NSH command (↑ history, Ctrl+C stops a program)"
        : next.kind === "loading" ? "Waiting for the vehicle" : "No shell";
      hintEl.textContent = live
        ? `${next.label || "PX4"} shell over MAVLink. The terminal button opens it in a window.`
        : next.detail;
      if (!previous) return;
      if (live) {
        addLine("success", `${next.label || "PX4"} shell open.`);
        if (focusOnLive && root.offsetParent !== null) { try { input.focus(); } catch (_e) { /* not yet */ } }
        focusOnLive = false;
      } else if (next.kind === "offline" && previous.kind === "live") {
        addLine("warning", "Vehicle disconnected.");
      } else if (next.kind === "unsupported") {
        addLine("warning", next.detail);
      } else if (next.kind === "offline" && previous.kind === "loading") {
        addLine("info", next.detail);
      }
    }

    const view = {
      onMode: (next, previous) => { if (!disposed) showMode(next, previous || null); },
      onText: (text, replay) => {
        if (disposed) return;
        if (replay) {
          // What the shell printed so far: its lines are rebuilt from it, so
          // a stream that reconnected does not show the session twice. The
          // console's own lines ("PX4 shell open.") were never in it and stay.
          assembler.reset();
          lines = lines.filter((rec) => !rec.fromShell);
          held = held.filter((rec) => !rec.fromShell);
          screenStart = lines.length;
          renderLines();
        }
        take(assembler.feed(text));
      },
      onNote: (text, level) => { if (!disposed) addLine(level || "info", text); },
      onReset: () => {
        if (disposed) return;
        assembler.reset();
        addLine("info", "New shell.");
      },
    };

    showMode(session.mode(), null);
    take(assembler.feed(session.raw()));
    const detach = session.attach(view, api);

    containerEl._nxDestroy = function () {
      if (disposed) return;
      disposed = true;
      containerEl._nxDestroy = null;
      input.removeEventListener("keydown", onKeyDown);
      output.removeEventListener("scroll", onScroll);
      detach();
      if (root.parentNode) root.parentNode.removeChild(root);
    };

    return view;
  }

  // ---- the terminal (a window of its own) -----------------------------------

  /**
   * xterm on the shell, in `host`. `status(text, tone, title)` is the window's
   * pill. Returns {fit, focus, dispose}.
   */
  function mountTerminal(host, status) {
    let disposed = false;
    let term = null;
    let fit = null;
    let mode = session.mode();
    const cleanups = [];
    const editor = createEditor(session.history());
    let detach = () => {};

    // What is on screen of the line being typed.
    let shown = false;
    let startCol = 0;
    let drawnCursor = 0;
    let drawnLen = 0;
    let writesPending = 0;
    let renderQueued = false;

    function write(text, done) {
      if (!term || disposed) return;
      writesPending += 1;
      term.write(text, () => {
        writesPending -= 1;
        if (typeof done === "function") done();
        if (!writesPending && renderQueued && !disposed) {
          renderQueued = false;
          render();
        }
      });
    }

    /** Draw the line being typed (`snapshot`, or the editor's state now).
     *  Its start is read off the screen, so not while something written
     *  before it is still on its way there. */
    function render(snapshot) {
      if (!term || disposed) return;
      const { line, cursor } = snapshot || editor.state();
      if (!shown) {
        if (writesPending) { renderQueued = true; return; }
        if (!line) return;
        startCol = term.buffer.active.cursorX;
        shown = true;
        write(paintLine(term.cols, startCol, line, cursor));
      } else {
        write(cursorMove(term.cols, startCol, drawnCursor, 0)
          + paintLine(term.cols, startCol, line, cursor));
      }
      drawnCursor = cursor;
      drawnLen = line.length;
    }

    function hideLine() {
      if (!shown) return;
      shown = false;
      if (drawnLen) write(cursorMove(term.cols, startCol, drawnCursor, 0) + "\x1b[J");
    }

    /** Text for the screen (the shell's, or a note): under the line being typed. */
    function output(text) {
      if (!term || !text) return;
      hideLine();
      write(text);
      if (editor.state().line) render();
    }

    function note(text) {
      output(`\r\n${NOTE_ON}${text}${NOTE_OFF}\r\n`);
    }

    function onKeys(data) {
      if (disposed || mode.kind !== "live") return;
      editor.feed(data).forEach((fx) => {
        if (fx.type === "render") {
          render(fx);
        } else if (fx.type === "submit") {
          // NSH echoes the line it receives; that echo replaces this copy.
          hideLine();
          session.send(fx.line + "\n");
          session.rememberCommand(fx.line);
        } else if (fx.type === "cancel") {
          // Left on screen, marked, and a fresh prompt asked for.
          if (shown) write(cursorMove(term.cols, startCol, drawnCursor, drawnLen) + "^C");
          shown = false;
          session.send("\n");
        } else if (fx.type === "interrupt") {
          session.send("\x03");
        } else if (fx.type === "clear") {
          hideLine();
          write("", () => { if (term && !disposed) term.clear(); });
          render();
        }
      });
    }

    function showMode(next, previous) {
      mode = next;
      if (term) term.options.disableStdin = next.kind !== "live";
      if (next.kind === "live") status("CONNECTED", "on");
      else if (next.kind === "unsupported") status("NOT AVAILABLE", "", next.detail);
      else if (next.kind === "loading") status("WAITING", "wait");
      else status("OFFLINE", "", next.detail);
      if (!previous || !term) return;
      if (next.kind === "offline" && previous.kind === "live") note("Vehicle disconnected.");
      else if (next.kind !== "live" && next.detail && next.kind !== previous.kind) note(next.detail);
    }

    function refit() {
      if (!term || !fit || disposed) return;
      // A frame that is not laid out measures nothing; fitted to that, the
      // terminal would be two columns wide and rewrap everything in it.
      if (!host.offsetWidth || !host.offsetHeight) return;
      let dims = null;
      try { dims = fit.proposeDimensions(); } catch (_e) { /* fit anyway */ }
      if (dims && dims.cols === term.cols && dims.rows === term.rows) return;
      const resize = () => {
        if (disposed) return;
        try { fit.fit(); } catch (_e) { /* the next resize tries again */ }
        render();
      };
      // The line is erased at the width it was drawn at, and only then is the
      // terminal resized: the erase was worked out for the old width.
      if (shown && drawnLen) {
        hideLine();
        write("", resize);
      } else {
        shown = false;
        resize();
      }
    }

    function focus() {
      if (term) { try { term.focus(); } catch (_e) { /* not focusable yet */ } }
    }

    const view = {
      onMode: (next, previous) => { if (!disposed) showMode(next, previous || null); },
      onText: (text, replay) => {
        if (disposed || !term) return;
        if (replay) {
          // What the shell printed so far, on a clean screen.
          hideLine();
          write("\x1bc" + text);
          if (editor.state().line) render();
          return;
        }
        output(text);
      },
      onNote: (text) => { if (!disposed) note(text); },
      onReset: () => {
        if (disposed || !term) return;
        editor.reset();
        hideLine();
        write("\x1bc");
      },
    };

    function mount() {
      if (disposed) return;
      host.textContent = "";
      term = new window.Terminal({
        fontFamily: "JetBrains Mono, ui-monospace, monospace",
        fontSize: 12,
        lineHeight: 1.2,
        cursorBlink: true,
        scrollback: 5000,
        // NSH ends its lines with CRLF, the shells PX4 runs in SITL with LF.
        convertEol: true,
        disableStdin: true,
        theme: xtermTheme(),
      });
      fit = new window.FitAddon.FitAddon();
      term.loadAddon(fit);
      term.open(host);
      refit();

      const dataSub = term.onData(onKeys);
      cleanups.push(() => dataSub.dispose());

      // Cmd+C, or Ctrl+Shift+C, with a selection copies; Ctrl+C without one
      // is the shell's. Ctrl+Shift+V is left to the browser, which pastes.
      term.attachCustomKeyEventHandler((e) => {
        if (e.type !== "keydown") return true;
        const copy = (e.metaKey && e.key === "c")
          || (e.ctrlKey && e.shiftKey && (e.key === "C" || e.key === "c"));
        if (copy && term.hasSelection()) {
          copyToClipboard(term.getSelection(), focus).catch(() => {});
          return false;
        }
        if (e.ctrlKey && e.shiftKey && !e.metaKey && (e.key === "V" || e.key === "v")) return false;
        return true;
      });

      if (typeof window.ResizeObserver === "function") {
        const observer = new window.ResizeObserver(refit);
        observer.observe(host);
        cleanups.push(() => observer.disconnect());
      }
      if (typeof Corvus.ui.onThemeChange === "function") {
        const unTheme = Corvus.ui.onThemeChange(() => {
          if (term && !disposed) term.options.theme = xtermTheme();
        });
        if (typeof unTheme === "function") cleanups.push(unTheme);
      }

      showMode(session.mode(), null);
      const sofar = session.raw();
      if (sofar) write(sofar);
      detach = session.attach(view);
      focus();
    }

    host.textContent = "Loading terminal…";
    status("WAITING", "wait");
    ensureXterm().then(mount, () => {
      if (disposed) return;
      host.textContent = "Terminal component unavailable. The xterm bundle did not load.";
      status("", "");
    });

    return {
      fit: refit,
      focus,
      dispose() {
        if (disposed) return;
        disposed = true;
        cleanups.splice(0).forEach((fn) => { try { fn(); } catch (_e) { /* keep tearing down */ } });
        detach();
        if (term) { try { term.dispose(); } catch (_e) { /* already gone */ } }
        term = null;
      },
    };
  }

  /**
   * Open the terminal window, or bring it forward. The frame is the one the
   * SSH terminals open in: dragged by its bar, sized from its corner,
   * maximized from the bar, put away with ×. Putting it away while the
   * console is open leaves the shell running; with neither open, it closes.
   * @returns {boolean} whether a window is showing the shell
   */
  function openWindow(api) {
    const F = Corvus.floatWindows;
    if (!F || typeof F.open !== "function") return false;
    const existing = F.get(WINDOW_KEY);
    if (existing) {
      F.raise(existing);
      if (existing.nx) existing.nx.focus();
      return true;
    }
    // The shell is held by the window from the moment it opens, so closing
    // the console under it does not end the shell the window shows.
    let hold = session.attach({}, api);
    const rec = F.open({
      key: WINDOW_KEY,
      kind: "nsh-terminal",
      title: "NuttX Console",
      subtitle: "PX4 shell over MAVLink",
      icon: "square-terminal",
      noun: "terminal",
      ariaLabel: "Terminal: NuttX Console",
      className: "term-win--terminal",
      bodyClass: "ssh-term",
      closeTitle: "Close the window",
      tools: [Corvus.ui.iconButton("rotate-ccw", {
        size: 13,
        className: "icon-btn term-win-disconnect",
        title: "New shell. Ends this one, and what runs in it",
        ariaLabel: "End this shell and open a new one",
        onClick: () => session.newShell(),
      })],
      onResize: (r) => { if (r.nx) r.nx.fit(); },
      onFocus: (r) => { if (r.nx) r.nx.focus(); },
      onClose: (r) => {
        if (r.nx) r.nx.dispose();
        r.nx = null;
        hold();
        hold = () => {};
      },
      mount: (r) => {
        r.nx = mountTerminal(r.bodyEl, (text, tone, title) => F.setStatus(r, text, tone, title));
      },
    });
    if (!rec) { hold(); return false; }
    return true;
  }

  // ---- the plugin ------------------------------------------------------------

  function init(containerEl, api) {
    mountConsole(containerEl, api);
  }

  function destroy(containerEl) {
    if (containerEl && typeof containerEl._nxDestroy === "function") containerEl._nxDestroy();
  }

  return {
    init, destroy, openWindow,
    createEditor, readEscape, cursorMove, paintLine, createAssembler, lineLevel,
    session,
    HISTORY_MAX, WINDOW_KEY,
  };
})();

// Registered as this script runs; the grid re-renders on every register().
if (window.Corvus && Corvus.plugins && typeof Corvus.plugins.register === "function") {
  Corvus.plugins.register("nuttx-console", {
    name: "NuttX Console",
    icon: "square-terminal",
    description: "The PX4 NuttShell (NSH) over MAVLink, as a console and in a terminal window",
    // A shell is reached for again and again on the bench: it can have a
    // tab of its own, switched on from its gear in Settings > Plugins, and is
    // a card under PLUGINS otherwise.
    tab: true,
    init: function (containerEl, api) { Corvus.pluginNuttx.init(containerEl, api); },
    destroy: function (containerEl) { Corvus.pluginNuttx.destroy(containerEl); },
  });
}
