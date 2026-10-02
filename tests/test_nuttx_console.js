"use strict";

/**
 * Frontend tests for the NuttX Console plugin (plugins/nuttx-console).
 *
 *   PART A  the terminal's line editor: typing, moving, deleting, history,
 *           Ctrl+C, pastes with several lines.
 *   PART B  drawing the terminal's line: cursor moves across wrapped rows.
 *   PART C  cutting NSH's bytes into the console's lines: escapes, carriage
 *           returns, a `top` redrawing its screen.
 *   PART D  the console view: the MAVLink console's parts, opening the shell
 *           on a PX4, saying why not on ArduPilot or with nothing connected,
 *           the command line, filter, pause, copy, save, closing.
 *   PART E  the terminal window: one per shell, typing and output, and the
 *           shell kept while either view is open and closed with the last.
 *
 * Run:
 *   node plugins/nuttx-console/tests/test_nuttx_console.js
 */

const assert = require("node:assert/strict");
const path = require("node:path");

// The Corvus checkout whose src/ this plugin runs against: the one around
// plugins/nuttx-console/, unless CORVUS_ROOT names another (the plugin kept in its own
// repository, say). tools/frontend_tests.js sets it.
const CORVUS = process.env.CORVUS_ROOT
  ? path.resolve(process.env.CORVUS_ROOT)
  : path.join(__dirname, "..", "..", "..");

global.window = global;
global.Corvus = {};
global.CustomEvent = class CustomEvent {
  constructor(type, options = {}) { this.type = type; this.detail = options.detail; }
};
const windowListeners = {};
window.addEventListener = (type, fn) => { (windowListeners[type] = windowListeners[type] || new Set()).add(fn); };
window.removeEventListener = (type, fn) => { if (windowListeners[type]) windowListeners[type].delete(fn); };
window.dispatchEvent = () => true;
window.matchMedia = () => ({ matches: false, addEventListener() {}, removeEventListener() {} });

function makeEl(tag) {
  const e = {
    tagName: String(tag || "div").toUpperCase(),
    className: "", children: [], dataset: {},
    type: "", hidden: false, disabled: false, value: "", id: "", title: "", placeholder: "",
    tabIndex: 0, _attrs: {}, _listeners: {}, _isEl: true, parentNode: null, _text: "",
    scrollTop: 0, scrollHeight: 0, clientHeight: 0, selectionStart: 0, selectionEnd: 0,
    offsetParent: {},
  };
  Object.defineProperty(e, "textContent", {
    get() { return e._text + e.children.map((c) => c.textContent || "").join(""); },
    set(v) { e._text = String(v); e.children.length = 0; },
  });
  let html = "";
  Object.defineProperty(e, "innerHTML", {
    get() { return html; },
    set(v) { html = String(v); e.children.length = 0; },
  });
  e.style = { _p: {}, setProperty(k, v) { e.style._p[k] = v; }, getPropertyValue: (k) => e.style._p[k] || "" };
  e.classList = {
    add(c) { const s = e.className.split(/\s+/).filter(Boolean); if (!s.includes(c)) s.push(c); e.className = s.join(" "); },
    remove(c) { e.className = e.className.split(/\s+/).filter((x) => x !== c).join(" "); },
    toggle(c, force) {
      const next = force === undefined ? !e.classList.contains(c) : !!force;
      if (next) e.classList.add(c); else e.classList.remove(c);
      return next;
    },
    contains(c) { return e.className.split(/\s+/).includes(c); },
  };
  e.appendChild = (c) => { c.parentNode = e; e.children.push(c); return c; };
  e.append = (...n) => n.forEach((x) => e.appendChild(x));
  e.removeChild = (c) => { const i = e.children.indexOf(c); if (i >= 0) e.children.splice(i, 1); c.parentNode = null; return c; };
  e.remove = () => { if (e.parentNode) e.parentNode.removeChild(e); };
  e.setAttribute = (k, v) => { e._attrs[k] = String(v); };
  e.getAttribute = (k) => (k in e._attrs ? e._attrs[k] : null);
  e.removeAttribute = (k) => { delete e._attrs[k]; };
  e.addEventListener = (t, cb) => { (e._listeners[t] = e._listeners[t] || []).push(cb); };
  e.removeEventListener = (t, cb) => { e._listeners[t] = (e._listeners[t] || []).filter((x) => x !== cb); };
  e.fire = (t, ev) => (e._listeners[t] || []).slice().forEach((cb) => cb(Object.assign({ target: e, preventDefault() {} }, ev)));
  e.click = () => { if (!e.disabled) e.fire("click"); };
  e.focus = () => { e.focused = (e.focused || 0) + 1; };
  e.querySelector = (sel) => all(e, sel)[0] || null;
  e.querySelectorAll = (sel) => all(e, sel);
  Object.defineProperty(e, "firstChild", { get: () => e.children[0] || null });
  return e;
}

function all(root, sel) {
  const out = [];
  const classes = sel.split(".").filter(Boolean);
  (function walk(list) {
    list.forEach((c) => {
      if (!c || !c._isEl) return;
      const own = c.className.split(/\s+/);
      if (classes.every((x) => own.includes(x))) out.push(c);
      walk(c.children);
    });
  })(root.children);
  return out;
}

global.document = {
  createElement: makeEl,
  createElementNS: makeEl,
  createTextNode: (t) => ({ nodeType: 3, textContent: String(t), _isText: true }),
  createDocumentFragment: () => makeEl("fragment"),
  getElementById: () => null,
  querySelector: () => null,
  querySelectorAll: () => [],
  addEventListener() {},
  removeEventListener() {},
  body: makeEl("body"),
  head: makeEl("head"),
};

// ---- a fake xterm: records what is written, hands keys in -----------------

const terminals = [];
class FakeTerminal {
  constructor(options) {
    this.options = Object.assign({}, options);
    this.cols = 80;
    this.rows = 24;
    this.out = [];
    this.disposed = false;
    this.cleared = 0;
    this.onKey = null;
    // The prompt NSH prints is "nsh> ": the line typed after it starts at 5.
    this.buffer = { active: { cursorX: 5 } };
    terminals.push(this);
  }
  loadAddon() {}
  open() {}
  write(text, cb) { this.out.push(text); if (cb) cb(); }
  onData(fn) { this.onKey = fn; return { dispose: () => { this.onKey = null; } }; }
  attachCustomKeyEventHandler() {}
  hasSelection() { return false; }
  getSelection() { return ""; }
  focus() {}
  clear() { this.cleared += 1; }
  dispose() { this.disposed = true; }
  type(data) { if (this.onKey) this.onKey(data); }
  text() { return this.out.join(""); }
}
window.Terminal = FakeTerminal;
window.FitAddon = { FitAddon: class { fit() {} proposeDimensions() { return { cols: 80, rows: 24 }; } } };

// ---- the frame the terminal windows open in --------------------------------

const frames = new Map();
Corvus.floatWindows = {
  open(spec) {
    const rec = { key: spec.key, spec, bodyEl: makeEl("div"), raised: 0, status: null };
    frames.set(spec.key, rec);
    spec.mount(rec);
    return rec;
  },
  get: (key) => frames.get(key) || null,
  raise(rec) { rec.raised += 1; },
  close(key) {
    const rec = frames.get(key);
    if (!rec) return false;
    frames.delete(key);
    rec.spec.onClose(rec);
    return true;
  },
  setStatus(rec, text, tone, title) { rec.status = { text, tone, title }; },
};

// ---- the shell topic of /api/events ---------------------------------------

const shellSubs = new Set();
Corvus.events = {
  subscribe(topic, fn) {
    assert.equal(topic, "shell");
    shellSubs.add(fn);
    return () => shellSubs.delete(fn);
  },
};
function shellSays(data) { shellSubs.forEach((fn) => fn(data)); }

let caps = { shell: true, label: "PX4" };
Corvus.capabilities = { get: () => Promise.resolve(caps) };

const clipboard = [];
Object.defineProperty(global, "navigator", {
  value: { clipboard: { writeText: (t) => { clipboard.push(t); return Promise.resolve(); } } },
  configurable: true,
});
const beacons = [];
global.fetch = (url, opts) => { beacons.push([url, opts]); return Promise.resolve({ ok: true }); };

require(path.join(CORVUS, "src", "js", "ui.js"));
require(path.join(CORVUS, "src", "js", "plugins.js"));
require("../nuttx-console.js");

const nx = Corvus.pluginNuttx;

const tick = () => new Promise((resolve) => setImmediate(resolve));
async function settle() { for (let i = 0; i < 10; i++) await tick(); }

// ===========================================================================
// PART A: the line editor
// ===========================================================================

function typeAll(editor, ...keys) {
  return keys.flatMap((k) => editor.feed(k));
}

function testEnterSubmitsTheLineAndRemembersIt() {
  const ed = nx.createEditor();
  const fx = typeAll(ed, "v", "e", "r", " ", "a", "l", "l", "\r");
  assert.deepEqual(fx[fx.length - 1], { type: "submit", line: "ver all" });
  assert.deepEqual(ed.state(), { line: "", cursor: 0 });
  assert.deepEqual(ed.history(), ["ver all"]);
}

function testRenderCarriesTheStateItWasFor() {
  const ed = nx.createEditor();
  assert.deepEqual(ed.feed("ls\rfree"), [
    { type: "render", line: "ls", cursor: 2 },
    { type: "submit", line: "ls" },
    { type: "render", line: "free", cursor: 4 },
  ]);
}

function testAPastedCrlfIsOneLineBreak() {
  const ed = nx.createEditor();
  const submits = ed.feed("top\r\nfree\r\n").filter((f) => f.type === "submit");
  assert.deepEqual(submits.map((f) => f.line), ["top", "free"], "no empty command between them");
}

function testTheCursorMovesAndTextGoesInWhereItIs() {
  const ed = nx.createEditor();
  ed.feed("lisener");
  ed.feed("\x1b[D\x1b[D\x1b[D\x1b[D");
  ed.feed("t");
  assert.deepEqual(ed.state(), { line: "listener", cursor: 4 });
  ed.feed("\x1b[H");
  assert.equal(ed.state().cursor, 0);
  ed.feed("\x05");
  assert.equal(ed.state().cursor, 8);
  ed.feed("\x1bOD");
  assert.equal(ed.state().cursor, 7);
}

function testDeletingByCharacterWordAndLine() {
  const ed = nx.createEditor();
  ed.feed("param show MPC_XY");
  ed.feed("\x7f");
  assert.equal(ed.state().line, "param show MPC_X");
  ed.feed("\x17");
  assert.equal(ed.state().line, "param show ");
  ed.feed("\x01\x1b[3~");
  assert.equal(ed.state().line, "aram show ");
  ed.feed("\x1b[1;5C");
  assert.equal(ed.state().cursor, 4);
  ed.feed("\x0b");
  assert.equal(ed.state().line, "aram");
  ed.feed("\x15");
  assert.deepEqual(ed.state(), { line: "", cursor: 0 });
  assert.deepEqual(ed.feed("\x7f"), [], "nothing to delete is nothing to draw");
}

function testHistoryGoesBackAndReturnsToTheDraft() {
  const ed = nx.createEditor(["ver all", "top"]);
  ed.feed("fr");
  ed.feed("\x1b[A");
  assert.equal(ed.state().line, "top");
  ed.feed("\x1b[A");
  assert.equal(ed.state().line, "ver all");
  assert.deepEqual(ed.feed("\x1b[A"), [], "the oldest entry is the end");
  ed.feed("\x1b[B\x1b[B");
  assert.deepEqual(ed.state(), { line: "fr", cursor: 2 }, "the draft comes back");
}

function testHistoryIsCappedAndSkipsRepeats() {
  const ed = nx.createEditor();
  ed.feed("top\rtop\r\r");
  assert.deepEqual(ed.history(), ["top"]);
  for (let i = 0; i < nx.HISTORY_MAX + 5; i++) ed.feed(`cmd${i}\r`);
  assert.equal(ed.history().length, nx.HISTORY_MAX);
  assert.deepEqual(nx.createEditor([3, "", " ", "ls"]).history(), ["ls"], "saved junk is dropped");
}

function testCtrlCStopsAProgramOrDropsTheLine() {
  const ed = nx.createEditor();
  assert.deepEqual(ed.feed("\x03"), [{ type: "interrupt" }]);
  ed.feed("listener sensor_accel");
  assert.deepEqual(ed.feed("\x03"), [{ type: "cancel", line: "listener sensor_accel" }]);
  assert.deepEqual(ed.history(), [], "a dropped line is not history");
}

function testCtrlLClearsAndTabDoesNothing() {
  const ed = nx.createEditor();
  ed.feed("ls");
  assert.deepEqual(ed.feed("\x0c"), [{ type: "clear" }, { type: "render", line: "ls", cursor: 2 }]);
  assert.deepEqual(ed.feed("\t"), []);
  assert.deepEqual(ed.feed("\x1b[15~"), [], "an unknown key is ignored, all of it");
}

// ===========================================================================
// PART B: drawing the terminal's line
// ===========================================================================

function testCursorMovesWithinAndAcrossRows() {
  assert.equal(nx.cursorMove(80, 5, 3, 0), "\x1b[6G");
  assert.equal(nx.cursorMove(20, 5, 17, 2), "\x1b[1A\x1b[8G");
  assert.equal(nx.cursorMove(20, 5, 2, 17), "\x1b[1B\x1b[3G");
}

function testAFullRowLeavesTheCursorWhereTheArithmeticSays() {
  assert.equal(nx.paintLine(20, 5, "abc", 3), "\x1b[Jabc\x1b[9G");
  assert.equal(nx.paintLine(20, 5, "x".repeat(15), 15), "\x1b[J" + "x".repeat(15) + " \b\x1b[1G");
}

// ===========================================================================
// PART C: NSH's bytes as lines
// ===========================================================================

const linesOf = (events) => events.filter((e) => e.type === "line").map((e) => e.text);

function testLinesEndAtNewlinesAndThePromptWaits() {
  const a = nx.createAssembler();
  assert.deepEqual(linesOf(a.feed("\nnsh> ")), [""]);
  assert.equal(a.pending(), "nsh> ");
  assert.deepEqual(linesOf(a.feed("ver all\r\nHW arch: PX4_FMU_V6X\r\nnsh> ")),
    ["nsh> ver all", "HW arch: PX4_FMU_V6X"]);
  assert.equal(a.pending(), "nsh> ");
}

function testEscapesGoAndALoneCarriageReturnStartsTheLineAgain() {
  const a = nx.createAssembler();
  assert.deepEqual(linesOf(a.feed("\x1b[33mWARN  [ekf2] gps lost\x1b[0m\x1b[K\n")), ["WARN  [ekf2] gps lost"]);
  assert.deepEqual(linesOf(a.feed("erasing 10%\rerasing 100%\n")), ["erasing 100%"]);
  assert.deepEqual(linesOf(a.feed("ab\bc\x07\n")), ["ac"]);
  assert.deepEqual(linesOf(a.feed("a\x1b[")), []);
  assert.deepEqual(linesOf(a.feed("31mb\n")), ["ab"], "an escape split between two packets");
}

function testAScreenClearIsReported() {
  const a = nx.createAssembler();
  const types = (text) => a.feed(text).map((e) => e.type);
  assert.deepEqual(types("\x1b[H"), ["clear"], "cursor home: top starts a redraw");
  assert.deepEqual(types("\x1b[2J"), ["clear"]);
  assert.deepEqual(types("\x1bc"), ["clear"]);
  assert.deepEqual(types("\x1b[5;1H\x1b[J\x1b[K"), [], "moving elsewhere is not a new screen");
}

function testLinesAreColouredByWhatTheyAre() {
  assert.equal(nx.lineLevel("nsh> ver all"), "cmd");
  assert.equal(nx.lineLevel("pxh> ver all"), "cmd");
  assert.equal(nx.lineLevel("ERROR [commander] Arming denied"), "error");
  assert.equal(nx.lineLevel("WARN  [health_and_arming_checks] Preflight Fail"), "warning");
  assert.equal(nx.lineLevel("HW arch: PX4_FMU_V6X"), "shell");
}

// ===========================================================================
// PART D: the console view
// ===========================================================================

function fakeApi(state) {
  const api = {
    posts: [],
    saved: [],
    subs: new Set(),
    state: Object.assign({ connected: true, autopilot_stack: "px4" }, state),
    reply: () => Promise.resolve({ ok: true }),
    postJson(url, body) { api.posts.push([url, body]); return api.reply(url, body); },
    getState: () => api.state,
    subscribe(fn) { api.subs.add(fn); return () => api.subs.delete(fn); },
    getSettings: () => ({ history: ["ver all"] }),
    saveSettings(patch) { api.saved.push(patch); return Promise.resolve(patch); },
    push(next) { api.state = Object.assign({}, api.state, next); api.subs.forEach((fn) => fn(api.state)); },
  };
  return api;
}

const sent = (api) => api.posts.filter(([url]) => url === "/api/mavlink/shell/send").map(([, b]) => b.data);
// What reached the shell, in order. Writes made while one is in flight go
// out together, so the stream is what a test can rely on, not the packets.
const typed = (api) => sent(api).join("");
const opened = [];
const closes = (api) => api.posts.filter(([url]) => url === "/api/mavlink/shell/close").length;

async function openConsole(state, api) {
  const container = makeEl("div");
  opened.push(container);
  const a = api || fakeApi(state);
  nx.init(container, a);
  await settle();
  const q = (sel) => container.querySelector(sel);
  const shown = () => container.querySelectorAll(".con-line").map((l) => ({
    level: l.className.replace("con-line", "").trim(),
    text: l.querySelector(".con-msg").textContent,
  }));
  return { container, api: a, q, shown };
}

function key(input, k, extra) {
  input.fire("keydown", Object.assign({ key: k }, extra));
}

async function finish(...containers) {
  containers.forEach((c) => nx.destroy(c));
  frames.forEach((_rec, k) => Corvus.floatWindows.close(k));
  await settle();
  assert.equal(nx.session.viewCount(), 0, "every view let go of the shell");
}

async function testTheConsoleIsTheMavlinkConsolesParts() {
  caps = { shell: true, label: "PX4" };
  const { container, q } = await openConsole();
  for (const cls of [".console-toolbar", ".console-filter", ".console-tools", ".console-stream",
    ".console-output", ".console-jump", ".console-input-row", ".console-prompt", ".console-input",
    ".console-send", ".console-footer", ".console-count", ".console-hint"]) {
    assert.ok(q(cls), `has ${cls}`);
  }
  assert.equal(q(".console-prompt").textContent, "nsh>");
  shellSays({ text: "free\n" });
  assert.equal(container.querySelectorAll(".con-time").length, 0, "no times in the NuttX console");
  await finish(container);
}

async function testOnAPx4TheShellOpensAndSaysSo() {
  const { container, api, q, shown } = await openConsole();
  assert.deepEqual(sent(api), ["\n"], "a newline asks NSH for a prompt");
  assert.deepEqual(shown(), [{ level: "success", text: "PX4 shell open." }]);
  assert.equal(q(".console-input").disabled, false);
  assert.match(q(".console-hint").textContent, /PX4 shell over MAVLink/);
  await finish(container);
}

async function testACommandIsSentAndItsEchoAndOutputAreLines() {
  const { container, api, q, shown } = await openConsole();
  const input = q(".console-input");
  input.value = "ver all";
  key(input, "Enter");
  await settle();
  assert.deepEqual(sent(api), ["\n", "ver all\n"]);
  assert.equal(input.value, "");
  shellSays({ text: "\nnsh> ver all\r\nHW arch: PX4_FMU_V6X\r\nWARN  [x] old firmware\r\nnsh> " });
  assert.deepEqual(shown().slice(1), [
    { level: "cmd", text: "nsh> ver all" },
    { level: "shell", text: "HW arch: PX4_FMU_V6X" },
    { level: "warning", text: "WARN  [x] old firmware" },
  ], "blank lines and the waiting prompt are not shown");
  await finish(container);
}

async function testTheSendButtonAndAnEmptyEnterAreSentToo() {
  const { container, api, q } = await openConsole();
  q(".console-input").value = "free";
  q(".console-send").click();
  key(q(".console-input"), "Enter");
  await settle();
  assert.equal(typed(api), "\nfree\n\n", "Enter alone asks for a prompt, and ends a top");
  await finish(container);
}

async function testAReplayRedrawsTheShellsLinesAndKeepsTheConsoles() {
  const { container, shown } = await openConsole();
  shellSays({ text: "nsh> free\nUmem: 977632\n" });
  shellSays({ text: "nsh> free\nUmem: 977632\nnsh> ", replay: true });
  assert.deepEqual(shown().map((l) => l.text), ["PX4 shell open.", "nsh> free", "Umem: 977632"],
    "a stream that reconnected shows the session once, and the console's own line stays");
  await finish(container);
}

async function testUpAndDownBringBackEarlierCommands() {
  const { container, q } = await openConsole();
  const input = q(".console-input");
  input.value = "top";
  key(input, "Enter");
  input.value = "fr";
  key(input, "ArrowUp");
  assert.equal(input.value, "top");
  key(input, "ArrowUp");
  assert.equal(input.value, "ver all", "the saved history is there too");
  key(input, "ArrowDown");
  key(input, "ArrowDown");
  assert.equal(input.value, "fr", "the draft comes back");
  await finish(container);
}

async function testATopRedrawReplacesItsScreen() {
  const { container, shown } = await openConsole();
  shellSays({ text: "nsh> top\n" });
  shellSays({ text: "\x1b[H\x1b[2JProcesses: 42 total\nCPU usage: 30%\n" });
  shellSays({ text: "\x1b[HProcesses: 42 total\nCPU usage: 47%\n" });
  assert.deepEqual(shown().slice(1).map((l) => l.text),
    ["nsh> top", "Processes: 42 total", "CPU usage: 47%"]);
  await finish(container);
}

async function testCtrlCAndTheStopButtonStopAProgram() {
  const { container, api, q } = await openConsole();
  const input = q(".console-input");
  key(input, "c", { ctrlKey: true });
  container.querySelectorAll(".icon-btn").find((b) => b.title.startsWith("Stop")).click();
  input.value = "ab";
  input.selectionStart = 0;
  input.selectionEnd = 2;
  key(input, "c", { ctrlKey: true });
  await settle();
  assert.equal(typed(api), "\n\x03\x03", "with text selected, Ctrl+C copies instead");
  await finish(container);
}

async function testFilterPauseCopyAndSave() {
  const { container, api, q, shown } = await openConsole();
  shellSays({ text: "alpha\nbeta\n" });
  const filter = q(".console-filter");
  filter.value = "BET";
  filter.fire("input");
  assert.deepEqual(shown().map((l) => l.text), ["beta"]);
  assert.match(q(".console-count").textContent, /^1 \/ 3/);
  filter.value = "";
  filter.fire("input");

  const pause = container.querySelectorAll(".icon-btn").find((b) => b.title === "Pause the stream");
  pause.click();
  shellSays({ text: "gamma\n" });
  assert.equal(shown().length, 3, "held while paused");
  assert.match(q(".console-count").textContent, /1 held/);
  pause.click();
  assert.equal(shown()[3].text, "gamma", "and shown on resume, in order");

  container.querySelectorAll(".icon-btn").find((b) => b.title === "Copy visible lines").click();
  await settle();
  assert.match(clipboard[clipboard.length - 1], /alpha\n.*beta\n.*gamma$/);
  container.querySelectorAll(".icon-btn").find((b) => b.title === "Save log to a file").click();
  await settle();
  const save = api.posts.find(([url]) => url === "/api/console/save");
  assert.match(save[1].filename, /^corvus-nsh_.*\.log$/);
  container.querySelectorAll(".icon-btn").find((b) => b.title === "Clear console").click();
  assert.deepEqual(shown(), []);
  await finish(container);
}

async function testOnArduPilotItSaysWhyAndSendsNothing() {
  caps = { shell: false, label: "ArduPilot" };
  const { container, api, q, shown } = await openConsole({ autopilot_stack: "ardupilot" });
  caps = { shell: true, label: "PX4" };
  assert.deepEqual(sent(api), []);
  const said = shown();
  assert.equal(said[said.length - 1].level, "warning");
  assert.match(said[said.length - 1].text, /Not available on this autopilot\. ArduPilot has no NuttX shell/);
  assert.match(q(".console-hint").textContent, /ArduPilot has no NuttX shell/);
  [...said.map((l) => l.text), q(".console-hint").textContent, q(".console-input").placeholder]
    .forEach((t) => assert.doesNotMatch(t, /—|–| - /, "no dashes in what the operator reads"));
  assert.equal(q(".console-input").disabled, true);
  q(".console-input").value = "ls";
  key(q(".console-input"), "Enter");
  await finish(container);
  assert.equal(api.posts.length, 0, "nothing sent, and a shell never opened is not closed");
}

async function testWithNothingConnectedItWaitsThenOpens() {
  const { container, api, q, shown } = await openConsole({ connected: false, autopilot_stack: "" });
  assert.deepEqual(sent(api), []);
  assert.match(shown()[0].text, /opens when a PX4 autopilot connects/);
  assert.equal(q(".console-input").disabled, true);
  api.push({ connected: true, autopilot_stack: "px4" });
  await settle();
  assert.deepEqual(sent(api), ["\n"]);
  assert.equal(q(".console-input").disabled, false);
  api.push({ connected: false });
  await settle();
  assert.equal(shown()[shown().length - 1].text, "Vehicle disconnected.");
  await finish(container);
}

async function testARefusalByTheStackSwitchesToNotAvailable() {
  const api = fakeApi({ autopilot_stack: "" });
  api.reply = () => {
    const err = new Error("Generic has no NuttX shell. It is a PX4 feature (NSH over SERIAL_CONTROL)");
    err.status = 409;
    err.body = { ok: false, error: err.message };
    return Promise.reject(err);
  };
  const { container, q } = await openConsole(null, api);
  assert.equal(q(".console-input").disabled, true);
  assert.match(q(".console-hint").textContent, /Generic has no NuttX shell/);
  await finish(container);
}

async function testClosingTheConsoleEndsTheShellAndLetsEverythingGo() {
  const { container, api, q } = await openConsole();
  q(".console-input").value = "top";
  key(q(".console-input"), "Enter");
  await settle();
  nx.destroy(container);
  await settle();
  assert.deepEqual(api.posts[api.posts.length - 1], ["/api/mavlink/shell/close", {}]);
  assert.equal(shellSubs.size, 0, "the shell topic is let go");
  assert.equal(api.subs.size, 0, "and the telemetry");
  assert.equal(container.children.length, 0);
  assert.deepEqual(api.saved, [{ history: ["ver all", "top"] }], "the history is saved on the way out");
  assert.equal((windowListeners.pagehide || new Set()).size, 0);
  await finish();
}

async function testANewShellWaitsForTheLastClose() {
  const first = await openConsole();
  let release;
  first.api.reply = (url) => (url.endsWith("/close")
    ? new Promise((resolve) => { release = () => resolve({ ok: true }); })
    : Promise.resolve({ ok: true }));
  nx.destroy(first.container);
  const second = await openConsole();
  assert.deepEqual(sent(second.api), [], "the new prompt waits for the close");
  release();
  await settle();
  assert.deepEqual(sent(second.api), ["\n"]);
  await finish(second.container);
}

async function testAPageGoingAwayClosesTheShell() {
  const { container } = await openConsole();
  beacons.length = 0;
  windowListeners.pagehide.forEach((fn) => fn());
  assert.equal(beacons.length, 1);
  assert.equal(beacons[0][0], "/api/mavlink/shell/close");
  assert.equal(beacons[0][1].keepalive, true, "a request the browser lets finish");
  await finish(container);
}

// ===========================================================================
// PART E: the terminal window
// ===========================================================================

async function openTerminal(c) {
  terminals.length = 0;
  c.container.querySelectorAll(".icon-btn").find((b) => b.title === "Open the shell in a terminal window").click();
  await settle();
  return { rec: frames.get(nx.WINDOW_KEY), term: terminals[0] };
}

async function testTheTerminalButtonOpensAWindowOnTheSameShell() {
  const c = await openConsole();
  shellSays({ text: "\nnsh> " });
  const { rec, term } = await openTerminal(c);
  assert.ok(rec, "a window");
  assert.equal(rec.spec.title, "NuttX Console");
  assert.equal(rec.spec.className, "term-win--terminal", "the SSH terminals' frame");
  assert.deepEqual(rec.status, { text: "CONNECTED", tone: "on", title: undefined });
  assert.ok(term.text().includes("\nnsh> "), "what the shell printed so far");
  assert.equal(term.options.convertEol, true);
  assert.deepEqual(sent(c.api), ["\n"], "the open shell is shared, not opened again");

  term.type("free");
  term.type("\r");
  await settle();
  assert.deepEqual(sent(c.api), ["\n", "free\n"]);
  shellSays({ text: "free\r\n  total used\r\nnsh> " });
  assert.ok(term.text().endsWith("free\r\n  total used\r\nnsh> "), "the terminal draws it as it came");
  assert.ok(c.shown().some((l) => l.text === "  total used"), "and the console shows it as lines");
  await finish(c.container);
}

async function testOutputArrivingMidLineKeepsTheTerminalsLine() {
  const c = await openConsole();
  const { term } = await openTerminal(c);
  term.type("dmes");
  term.out.length = 0;
  shellSays({ text: "INFO  [commander] Ready\r\n" });
  const text = term.text();
  const at = text.indexOf("INFO");
  assert.ok(text.lastIndexOf("\x1b[J", at) !== -1, "the line is erased before the output");
  assert.ok(text.indexOf("dmes", at) > at, "and drawn again after it");
  await finish(c.container);
}

async function testOpeningAgainRaisesTheOneWindow() {
  const c = await openConsole();
  const { rec } = await openTerminal(c);
  await openTerminal(c);
  assert.equal(frames.size, 1);
  assert.equal(rec.raised, 1);
  await finish(c.container);
}

async function testTheShellLivesWhileEitherViewIsOpen() {
  const c = await openConsole();
  const { term } = await openTerminal(c);
  nx.destroy(c.container);
  await settle();
  assert.equal(closes(c.api), 0, "the window still shows the shell");
  shellSays({ text: "still here\r\n" });
  assert.ok(term.text().includes("still here"));
  Corvus.floatWindows.close(nx.WINDOW_KEY);
  await settle();
  assert.equal(closes(c.api), 1, "closed with the last view");
  assert.equal(term.disposed, true);
  await finish();
}

async function testClosingTheWindowLeavesTheConsolesShell() {
  const c = await openConsole();
  await openTerminal(c);
  Corvus.floatWindows.close(nx.WINDOW_KEY);
  await settle();
  assert.equal(closes(c.api), 0);
  assert.equal(nx.session.viewCount(), 1);
  await finish(c.container);
}

async function testANewShellFromTheWindow() {
  const c = await openConsole();
  const { rec, term } = await openTerminal(c);
  rec.spec.tools[0].click();
  await settle();
  assert.equal(closes(c.api), 1);
  assert.deepEqual(sent(c.api), ["\n", "\n"], "and a prompt from the new one");
  assert.ok(term.text().endsWith("\x1bc"), "the terminal starts clean");
  assert.equal(c.shown()[c.shown().length - 1].text, "New shell.");
  await finish(c.container);
}

async function testTheWindowSaysWhyWhenThereIsNoShell() {
  const c = await openConsole({ connected: false });
  const { rec, term } = await openTerminal(c);
  assert.equal(rec.status.text, "OFFLINE");
  assert.match(rec.status.title, /No vehicle connected/);
  assert.equal(term.options.disableStdin, true);
  c.api.push({ connected: true });
  await settle();
  assert.equal(rec.status.text, "CONNECTED");
  assert.equal(term.options.disableStdin, false);
  await finish(c.container);
}

function testRegisteredWithATab() {
  const entry = Corvus.plugins.list().find((p) => p.id === "nuttx-console");
  assert.ok(entry, "registered");
  assert.equal(entry.name, "NuttX Console");
  assert.equal(entry.icon, "square-terminal");
  assert.equal(entry.tab, true, "asks for a tab of its own");
}

const tests = [
  testEnterSubmitsTheLineAndRemembersIt,
  testRenderCarriesTheStateItWasFor,
  testAPastedCrlfIsOneLineBreak,
  testTheCursorMovesAndTextGoesInWhereItIs,
  testDeletingByCharacterWordAndLine,
  testHistoryGoesBackAndReturnsToTheDraft,
  testHistoryIsCappedAndSkipsRepeats,
  testCtrlCStopsAProgramOrDropsTheLine,
  testCtrlLClearsAndTabDoesNothing,
  testCursorMovesWithinAndAcrossRows,
  testAFullRowLeavesTheCursorWhereTheArithmeticSays,
  testLinesEndAtNewlinesAndThePromptWaits,
  testEscapesGoAndALoneCarriageReturnStartsTheLineAgain,
  testAScreenClearIsReported,
  testLinesAreColouredByWhatTheyAre,
  testTheConsoleIsTheMavlinkConsolesParts,
  testOnAPx4TheShellOpensAndSaysSo,
  testACommandIsSentAndItsEchoAndOutputAreLines,
  testTheSendButtonAndAnEmptyEnterAreSentToo,
  testAReplayRedrawsTheShellsLinesAndKeepsTheConsoles,
  testUpAndDownBringBackEarlierCommands,
  testATopRedrawReplacesItsScreen,
  testCtrlCAndTheStopButtonStopAProgram,
  testFilterPauseCopyAndSave,
  testOnArduPilotItSaysWhyAndSendsNothing,
  testWithNothingConnectedItWaitsThenOpens,
  testARefusalByTheStackSwitchesToNotAvailable,
  testClosingTheConsoleEndsTheShellAndLetsEverythingGo,
  testANewShellWaitsForTheLastClose,
  testAPageGoingAwayClosesTheShell,
  testTheTerminalButtonOpensAWindowOnTheSameShell,
  testOutputArrivingMidLineKeepsTheTerminalsLine,
  testOpeningAgainRaisesTheOneWindow,
  testTheShellLivesWhileEitherViewIsOpen,
  testClosingTheWindowLeavesTheConsolesShell,
  testANewShellFromTheWindow,
  testTheWindowSaysWhyWhenThereIsNoShell,
  testRegisteredWithATab,
];

(async () => {
  let failed = 0;
  for (const t of tests) {
    try {
      await t();
      console.log(`ok   - ${t.name}`);
    } catch (err) {
      failed++;
      console.error(`FAIL - ${t.name}`);
      console.error(`      ${err && err.stack ? err.stack.split("\n").join("\n      ") : err}`);
      // A failed test can leave views attached; the next one starts clean.
      opened.splice(0).forEach((c) => nx.destroy(c));
      frames.forEach((_rec, k) => Corvus.floatWindows.close(k));
      await settle();
    }
  }
  if (failed) {
    console.error(`\n${failed}/${tests.length} NuttX console test(s) FAILED`);
    process.exit(1);
  }
  console.log(`\nAll ${tests.length} NuttX console tests passed.`);
  process.exit(0);
})();
