//! NuttX Console: the PX4 NuttShell (NSH), as a console and as a terminal
//! (`nuttx-console.js` of the web plugin).
//!
//! PX4 runs NSH for a ground station over MAVLink (SERIAL_CONTROL, device
//! SHELL); it is how `top`, `listener`, `dmesg`, `free`, `param` and the other
//! builtins with no MAVLink command are reached without a cable.
//!
//! Two views of one shell, switched at the top of the plugin:
//!
//! * the console: the MAVLink console's look, with filter, pause, copy, save,
//!   clear and a command line with history. NSH's output arrives as bytes and
//!   is cut into lines; a program that redraws its screen, like `top`,
//!   replaces its lines instead of adding a screen a second.
//! * the terminal: the escapes drawn rather than cut out. The line is edited
//!   here and sent on Enter, as PX4's own `mavlink_shell.py` and
//!   QGroundControl do it: a packet per keystroke would wait a round trip
//!   over a telemetry radio, and NSH echoes the line it receives.
//!
//! Both type into the same shell and both show what it prints. The shell is
//! opened when a PX4 autopilot is connected and the plugin is showing, and
//! closed when it goes: a `top` left running would otherwise keep streaming
//! over the telemetry link for nobody. ArduPilot has no NSH, and the plugin
//! says so instead of offering a prompt that cannot answer.
//!
//! Transport, all in the backend:
//!
//! * `POST /api/mavlink/shell/send {data}`: typed as given
//! * `POST /api/mavlink/shell/close`: ends the shell, frees the link
//! * the `shell` topic of `/api/events`: the output; the first event is a
//!   replay of what the shell printed so far

pub mod model;
mod view;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use eframe::egui;
use serde_json::{json, Value};

use corvus_api::net::bg::Pending;
use corvus_api::net::stream::Stream;
use corvus_api::plugin::{Host, Info, Plugin, Reply};
use corvus_api::ui::term::emu::Emulator;

use model::{remember, Assembler, Editor};

const SEND_URL: &str = "/api/mavlink/shell/send";
const CLOSE_URL: &str = "/api/mavlink/shell/close";
const SAVE_URL: &str = "/api/console/save";
const CAPS_URL: &str = "/api/mavlink/capabilities";
/// The most the backend takes in one write; a longer paste goes in several.
pub const SEND_MAX_CHARS: usize = 4096;
/// Lines the console keeps, as the MAVLink console does.
pub const MAX_LINES: usize = 2000;
/// Shell output waiting for the next frame; beyond this the oldest goes.
const INBOX_MAX: usize = 4096;
const HISTORY_SAVE_S: f64 = 0.8;

/// What the shell is to the operator right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Kind {
    #[default]
    Loading,
    Offline,
    Unsupported,
    Live,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Mode {
    pub kind: Kind,
    /// The autopilot's name, for the hint.
    pub label: String,
    /// Why there is no shell, in a sentence.
    pub detail: String,
}

const OFFLINE: &str = "No vehicle connected. The shell opens when a PX4 autopilot connects.";

/// One line of the console.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rec {
    pub level: &'static str,
    pub text: String,
    /// NSH printed it (as opposed to a note of the console's own).
    pub from_shell: bool,
}

/// The console's lines (`mountConsole`): what is shown, what is held while
/// paused, and where the running program's screen starts.
#[derive(Debug, Default)]
pub struct Lines {
    pub lines: Vec<Rec>,
    pub held: Vec<Rec>,
    pub paused: bool,
    /// Where the current program's screen starts in `lines`: a clear from the
    /// shell (`top` redrawing) takes everything after it away.
    screen_start: usize,
    /// Counts every line shown, for the "new lines" chip.
    pub added: u64,
    assembler: Assembler,
}

impl Lines {
    fn push(&mut self, rec: Rec) {
        let cmd = rec.level == "cmd";
        self.lines.push(rec);
        self.added += 1;
        if self.lines.len() > MAX_LINES {
            let drop = self.lines.len() - MAX_LINES;
            self.lines.drain(..drop);
            self.screen_start = self.screen_start.saturating_sub(drop);
        }
        if cmd {
            self.screen_start = self.lines.len();
        }
    }

    pub fn add(&mut self, level: &'static str, text: &str, from_shell: bool) {
        let rec = Rec {
            level,
            text: text.to_string(),
            from_shell,
        };
        if self.paused {
            self.held.push(rec);
            if self.held.len() > MAX_LINES {
                self.held.remove(0);
            }
        } else {
            self.push(rec);
        }
    }

    /// The shell cleared its screen: the lines of the screen before go. Held
    /// lines are the new screen's too; the shown ones are kept, so a paused
    /// console stays what the operator is reading.
    fn clear_screen(&mut self) {
        if self.paused {
            self.held.clear();
        } else if self.screen_start < self.lines.len() {
            self.lines.truncate(self.screen_start);
        }
    }

    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
        if !paused {
            for rec in std::mem::take(&mut self.held) {
                self.push(rec);
            }
        }
    }

    pub fn clear(&mut self) {
        self.lines.clear();
        self.held.clear();
        self.screen_start = 0;
    }

    /// Shell output. A replay is what the shell printed so far: its lines are
    /// rebuilt from it, so a stream that reconnected does not show the session
    /// twice. The console's own lines were never in it and stay.
    pub fn take_text(&mut self, text: &str, replay: bool) {
        if replay {
            self.assembler.reset();
            self.lines.retain(|r| !r.from_shell);
            self.held.retain(|r| !r.from_shell);
            self.screen_start = self.lines.len();
        }
        for piece in self.assembler.feed(text) {
            match piece {
                model::Piece::Clear => self.clear_screen(),
                model::Piece::Line(t) if t.trim().is_empty() => {}
                model::Piece::Line(t) => self.add(model::line_level(&t), &t, true),
            }
        }
    }

    pub fn new_shell(&mut self) {
        self.assembler.reset();
        self.add("info", "New shell.", false);
    }

    /// The lines that pass `filter` (lower case), as text.
    pub fn transcript(&self, filter: &str) -> (usize, String) {
        let shown: Vec<&str> = self
            .lines
            .iter()
            .filter(|r| filter.is_empty() || r.text.to_lowercase().contains(filter))
            .map(|r| r.text.as_str())
            .collect();
        (shown.len(), shown.join("\n"))
    }

    /// The footer count: lines (or shown / all under a filter), held, paused.
    pub fn count_text(&self, filter: &str) -> String {
        let mut parts = vec![if filter.is_empty() {
            self.lines.len().to_string()
        } else {
            format!("{} / {}", self.transcript(filter).0, self.lines.len())
        }];
        if !self.held.is_empty() {
            parts.push(format!("{} held", self.held.len()));
        }
        if self.paused {
            parts.push("paused".into());
        }
        parts.join("  \u{00b7}  ")
    }
}

/// The terminal's screen and the line being typed on it (`mountTerminal`).
pub struct Term {
    pub emu: Arc<Mutex<Emulator>>,
    pub editor: Editor,
    /// Whether the line being typed is on screen, and where.
    shown: bool,
    start_col: usize,
    drawn_cursor: usize,
    drawn_len: usize,
}

impl Default for Term {
    fn default() -> Term {
        Term {
            emu: Arc::new(Mutex::new(Emulator::new(80, 24))),
            editor: Editor::default(),
            shown: false,
            start_col: 0,
            drawn_cursor: 0,
            drawn_len: 0,
        }
    }
}

impl Term {
    fn with<R>(&self, f: impl FnOnce(&mut Emulator) -> R) -> R {
        let mut emu = self.emu.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut emu)
    }

    fn write(&self, text: &str) {
        self.with(|e| e.feed(text.as_bytes()));
    }

    fn cols(&self) -> usize {
        self.with(|e| e.screen.cols)
    }

    /// Draws the line being typed: `line` and `cursor`, or the editor's now.
    /// Its start is read off the screen the first time.
    fn render(&mut self, snapshot: Option<(&str, usize)>) {
        let (line, cursor) = match snapshot {
            Some((l, c)) => (l.to_string(), c),
            None => (self.editor.line(), self.editor.cursor()),
        };
        let cols = self.cols();
        if !self.shown {
            if line.is_empty() {
                return;
            }
            self.start_col = self.with(|e| e.screen.x);
            self.shown = true;
            self.write(&model::paint_line(cols, self.start_col, &line, cursor));
        } else {
            self.write(&format!(
                "{}{}",
                model::cursor_move(cols, self.start_col, self.drawn_cursor, 0),
                model::paint_line(cols, self.start_col, &line, cursor)
            ));
        }
        self.drawn_cursor = cursor;
        self.drawn_len = line.chars().count();
    }

    fn hide_line(&mut self) {
        if !self.shown {
            return;
        }
        self.shown = false;
        if self.drawn_len > 0 {
            let cols = self.cols();
            self.write(&format!(
                "{}\x1b[J",
                model::cursor_move(cols, self.start_col, self.drawn_cursor, 0)
            ));
        }
    }

    /// Text for the screen (the shell's, or a note): under the line being typed.
    pub fn output(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.hide_line();
        self.write(text);
        if !self.editor.line().is_empty() {
            self.render(None);
        }
    }

    /// The terminal's own notes are dim, so they never read as the autopilot's.
    pub fn note(&mut self, text: &str) {
        self.output(&format!("\r\n\x1b[2m{text}\x1b[0m\r\n"));
    }

    /// What the shell printed so far, on a clean screen.
    fn replay(&mut self, text: &str) {
        self.shown = false;
        self.with(Emulator::reset);
        self.write(text);
        if !self.editor.line().is_empty() {
            self.render(None);
        }
    }

    fn reset(&mut self) {
        self.editor.reset();
        self.shown = false;
        self.with(Emulator::reset);
    }

    /// The grid the view shows now. The line is erased at the width it was
    /// drawn at, and only then is the screen resized.
    pub fn resize(&mut self, cols: usize, rows: usize) {
        if cols < 20 || rows < 4 || self.with(|e| (e.screen.cols, e.screen.rows)) == (cols, rows) {
            return;
        }
        self.hide_line();
        self.with(|e| e.screen.resize(cols, rows));
        self.render(None);
    }
}

/// Which of the two views is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Console,
    Terminal,
}

type Inbox = Arc<Mutex<VecDeque<(String, bool)>>>;

#[derive(Default)]
pub struct Nuttx {
    live: bool,
    mode: Mode,
    /// `(connected, stack)` the mode was worked out for.
    link: Option<(bool, String)>,
    caps: Option<Reply>,
    inbox: Inbox,
    stream: Option<Stream>,
    /// Whether a write has started the shell on the autopilot.
    opened: bool,
    pending_input: String,
    sending: Option<Pending<Result<(u16, Value), String>>>,
    closing: Option<Reply>,
    history: Vec<String>,
    history_due: Option<f64>,
    saving_log: Option<Reply>,
    pub(crate) lines: Lines,
    pub(crate) term: Term,
    pub(crate) tab: Tab,
    pub(crate) ui: view::ViewState,
}

impl Nuttx {
    fn set_mode(&mut self, kind: Kind, label: &str, detail: &str) {
        let next = Mode {
            kind,
            label: label.to_string(),
            detail: detail.to_string(),
        };
        if next == self.mode {
            return;
        }
        let previous = std::mem::replace(&mut self.mode, next);
        let name = if self.mode.label.is_empty() {
            "PX4"
        } else {
            &self.mode.label
        };
        // what each view says about the change
        match (self.mode.kind, previous.kind) {
            (Kind::Live, _) => self.lines.add("success", &format!("{name} shell open."), false),
            (Kind::Offline, Kind::Live) => {
                self.lines.add("warning", "Vehicle disconnected.", false);
                self.term.note("Vehicle disconnected.");
            }
            (Kind::Unsupported, _) => {
                let detail = self.mode.detail.clone();
                self.lines.add("warning", &detail, false);
                self.term.note(&detail);
            }
            (Kind::Offline, Kind::Loading) => {
                let detail = self.mode.detail.clone();
                self.lines.add("info", &detail, false);
                self.term.note(&detail);
            }
            _ => {}
        }
    }

    /// Works the mode out from the link: offline, or what the autopilot's
    /// capabilities say once they are in.
    fn evaluate(&mut self, h: &Host) {
        let connected = h.state.get("connected").and_then(Value::as_bool).unwrap_or(false);
        let stack = h
            .state
            .get("autopilot_stack")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let link = (connected, stack);
        if self.link.as_ref() == Some(&link) {
            return;
        }
        self.link = Some(link);
        self.caps = None;
        if !connected {
            self.opened = false;
            self.pending_input.clear();
            self.set_mode(Kind::Offline, "", OFFLINE);
            return;
        }
        self.caps = Some(h.get(CAPS_URL));
    }

    fn take_caps(&mut self) {
        let Some(answer) = self.caps.as_ref().and_then(Pending::take) else {
            return;
        };
        self.caps = None;
        let caps = answer.unwrap_or(Value::Null);
        let label = caps.get("label").and_then(Value::as_str).unwrap_or("").to_string();
        if caps.get("shell") == Some(&json!(false)) {
            let who = if label.is_empty() { "It" } else { &label };
            self.set_mode(
                Kind::Unsupported,
                &label,
                &format!("Not available on this autopilot. {who} has no NuttX shell: the shell is part of PX4."),
            );
            return;
        }
        if self.mode.kind == Kind::Live {
            return;
        }
        self.set_mode(Kind::Live, if label.is_empty() { "PX4" } else { &label }, "");
        // A newline is how NSH is asked for a prompt; the first write also
        // starts the shell on the autopilot.
        self.send("\n");
    }

    /// Queues `data` for the shell. One request is on its way at a time, so
    /// what was typed arrives in the order it was typed.
    pub(crate) fn send(&mut self, data: &str) {
        if self.live && !data.is_empty() {
            self.pending_input.push_str(data);
        }
    }

    fn drain(&mut self, h: &Host) {
        if let Some(answer) = self.sending.as_ref().and_then(Pending::take) {
            self.sending = None;
            let (status, body) = match answer {
                Ok((status, body)) => (status, body),
                Err(e) => (0, json!({"ok": false, "error": e})),
            };
            if body.get("ok") == Some(&json!(true)) {
                self.opened = true;
            } else {
                // What was typed after a refusal would be refused as well.
                self.pending_input.clear();
                let error = body
                    .get("error")
                    .and_then(Value::as_str)
                    .filter(|e| !e.is_empty())
                    .unwrap_or("no answer from Corvus")
                    .to_string();
                if status == 409 {
                    let label = self.mode.label.clone();
                    self.set_mode(
                        Kind::Unsupported,
                        &label,
                        &format!("Not available on this autopilot. {error}"),
                    );
                } else if self.mode.kind == Kind::Live {
                    self.lines.add("error", &format!("Not sent: {error}"), false);
                    self.term.note(&format!("Not sent: {error}"));
                }
            }
        }
        if self.closing.as_ref().is_some_and(|c| c.take().is_some()) {
            self.closing = None;
        }
        // A write waits for a close before it: the close would end the shell
        // the write just started.
        if self.sending.is_some() || self.closing.is_some() || self.pending_input.is_empty() {
            return;
        }
        let cut = self
            .pending_input
            .char_indices()
            .nth(SEND_MAX_CHARS)
            .map_or(self.pending_input.len(), |(i, _)| i);
        let chunk: String = self.pending_input.drain(..cut).collect();
        self.sending = Some(h.post_any(SEND_URL, json!({ "data": chunk })));
    }

    fn close_shell(&mut self, h: &Host) {
        if !self.opened {
            return;
        }
        self.opened = false;
        self.closing = Some(h.post(CLOSE_URL, json!({})));
    }

    /// Ends the shell, and whatever runs in it, and opens a fresh one.
    pub(crate) fn new_shell(&mut self, h: &Host) {
        if self.mode.kind != Kind::Live {
            return;
        }
        self.lines.new_shell();
        self.term.reset();
        self.pending_input.clear();
        self.close_shell(h);
        self.send("\n");
    }

    /// The history both views share, kept in the plugin's settings.
    pub(crate) fn remember_command(&mut self, text: &str, now: f64) {
        if remember(&mut self.history, text) {
            self.term.editor.set_history(&self.history);
            self.history_due = Some(now + HISTORY_SAVE_S);
        }
    }

    pub(crate) fn history(&self) -> &[String] {
        &self.history
    }

    pub(crate) fn mode(&self) -> &Mode {
        &self.mode
    }

    fn save_history(&mut self, h: &mut Host) {
        self.history_due = None;
        // The answer is not waited for: a history that was not saved is not
        // worth a message.
        let _ = h.save_settings(&json!({ "history": self.history }), false);
    }

    pub(crate) fn save_log(&mut self, h: &Host) {
        let (_, text) = self.lines.transcript(&self.ui.filter.trim().to_lowercase());
        if text.is_empty() || self.saving_log.is_some() {
            return;
        }
        let (y, mo, d, hh, mi, s) = corvus_api::data::clock::civil(h.now as i64);
        let name = format!("corvus-nsh_{y:04}-{mo:02}-{d:02}T{hh:02}-{mi:02}-{s:02}.log");
        self.saving_log = Some(h.post(SAVE_URL, json!({"filename": name, "text": text})));
    }

    fn take_saved_log(&mut self) {
        let Some(answer) = self.saving_log.as_ref().and_then(Pending::take) else {
            return;
        };
        self.saving_log = None;
        match answer {
            Ok(v) => match v.get("path").and_then(Value::as_str) {
                Some(path) => self.lines.add("success", &format!("NSH log written to {path}"), false),
                None => {
                    let e = v
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("Could not save the log");
                    self.lines.add("error", e, false);
                }
            },
            Err(e) if e.is_empty() => self.lines.add("error", "Could not save the log", false),
            Err(e) => self.lines.add("error", &e, false),
        }
    }

    /// Takes in what the shell printed since the last frame.
    fn take_output(&mut self) {
        let batch: Vec<(String, bool)> = {
            let mut inbox = self.inbox.lock().unwrap_or_else(|e| e.into_inner());
            inbox.drain(..).collect()
        };
        for (text, replay) in batch {
            self.lines.take_text(&text, replay);
            if replay {
                self.term.replay(&text);
            } else {
                self.term.output(&text);
            }
        }
    }

    /// What the terminal's keyboard sent: the editor says what to do with it.
    pub(crate) fn terminal_keys(&mut self, data: &str, now: f64) {
        if self.mode.kind != Kind::Live {
            return;
        }
        for fx in self.term.editor.feed(data) {
            match fx {
                model::Effect::Render { line, cursor } => self.term.render(Some((&line, cursor))),
                model::Effect::Submit(line) => {
                    // NSH echoes the line it receives; that echo replaces this copy.
                    self.term.hide_line();
                    self.send(&format!("{line}\n"));
                    self.remember_command(&line, now);
                }
                model::Effect::Cancel(_) => {
                    // Left on screen, marked, and a fresh prompt asked for.
                    if self.term.shown {
                        let cols = self.term.cols();
                        let t = &self.term;
                        t.write(&format!(
                            "{}^C",
                            model::cursor_move(cols, t.start_col, t.drawn_cursor, t.drawn_len)
                        ));
                    }
                    self.term.shown = false;
                    self.send("\n");
                }
                model::Effect::Interrupt => self.send("\x03"),
                model::Effect::Clear => {
                    self.term.shown = false;
                    self.term.with(Emulator::reset);
                    self.send("\n");
                }
            }
        }
    }
}

impl Plugin for Nuttx {
    fn info(&self) -> Info {
        Info {
            id: "nuttx-console",
            name: "NuttX Console",
            icon: "square-terminal",
            description: "The PX4 NuttShell (NSH) over MAVLink, as a console and as a terminal",
            version: "2.0.0",
            order: 15,
            // A shell is reached for again and again on the bench: it can have
            // a tab of its own, switched on from its gear in Settings.
            can_tab: true,
        }
    }

    fn open(&mut self, h: &mut Host) {
        self.live = true;
        self.history = model::clean_history(
            &h.settings()
                .get("history")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
        );
        self.term.editor = Editor::new(&self.history);
        let inbox = self.inbox.clone();
        self.stream = Some(h.events("shell", move |topic, data| {
            if topic != "shell" {
                return;
            }
            let Some(text) = data.get("text").and_then(Value::as_str) else {
                return;
            };
            let replay = data.get("replay") == Some(&json!(true));
            let mut inbox = inbox.lock().unwrap_or_else(|e| e.into_inner());
            if replay {
                inbox.clear();
            } else if inbox.len() >= INBOX_MAX {
                inbox.pop_front();
            }
            inbox.push_back((text.to_string(), replay));
        }));
        self.evaluate(h);
    }

    fn tick(&mut self, h: &mut Host) {
        if !self.live {
            // the close on its way out is still taken in
            if self.closing.as_ref().is_some_and(|c| c.take().is_some()) {
                self.closing = None;
            }
            return;
        }
        self.take_output();
        self.evaluate(h);
        self.take_caps();
        self.drain(h);
        self.take_saved_log();
        if self.history_due.is_some_and(|due| h.now >= due) {
            self.save_history(h);
        }
    }

    fn show(&mut self, ui: &mut egui::Ui, area: egui::Rect, h: &mut Host) -> f32 {
        view::show(self, ui, area, h)
    }

    fn close(&mut self, h: &mut Host) {
        if !self.live {
            return;
        }
        self.live = false;
        self.stream = None;
        if self.history_due.is_some() {
            self.save_history(h);
        }
        self.close_shell(h);
        self.sending = None;
        self.pending_input.clear();
        self.caps = None;
        self.link = None;
        self.mode = Mode::default();
        self.inbox.lock().unwrap_or_else(|e| e.into_inner()).clear();
        self.lines = Lines::default();
        self.term = Term::default();
    }
}

/// The language files of this plugin, for the translation layer.
pub const LANG: &[(&str, &str)] = &[
    ("de", include_str!("../lang/de.json")),
    ("fr", include_str!("../lang/fr.json")),
    ("dl", include_str!("../lang/dl.json")),
];

/// One instance of the plugin, for the registry.
pub fn plugin() -> Box<dyn Plugin> {
    Box::new(Nuttx::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(l: &Lines) -> Vec<&str> {
        l.lines.iter().map(|r| r.text.as_str()).collect()
    }

    #[test]
    fn output_becomes_lines_with_their_levels() {
        let mut l = Lines::default();
        l.take_text(
            "\nnsh> ver all\r\nHW arch: PX4_FMU_V6X\r\nWARN  [x] old firmware\r\nnsh> ",
            false,
        );
        assert_eq!(
            texts(&l),
            ["nsh> ver all", "HW arch: PX4_FMU_V6X", "WARN  [x] old firmware"]
        );
        let levels: Vec<_> = l.lines.iter().map(|r| r.level).collect();
        assert_eq!(levels, ["cmd", "shell", "warning"]);
        assert_eq!(l.count_text(""), "3");
        assert_eq!(l.count_text("arch"), "1 / 3");
    }

    #[test]
    fn a_program_that_redraws_replaces_its_screen() {
        let mut l = Lines::default();
        l.take_text("nsh> top\r\n", false);
        l.take_text("\x1b[HPID COMMAND\r\n  1 init\r\n", false);
        assert_eq!(texts(&l), ["nsh> top", "PID COMMAND", "  1 init"]);
        l.take_text("\x1b[HPID COMMAND\r\n  2 idle\r\n", false);
        assert_eq!(
            texts(&l),
            ["nsh> top", "PID COMMAND", "  2 idle"],
            "the screen before went, the command stays"
        );
    }

    #[test]
    fn pause_holds_lines_and_a_redraw_while_paused_keeps_what_is_read() {
        let mut l = Lines::default();
        l.take_text("nsh> top\r\nold screen\r\n", false);
        l.set_paused(true);
        l.take_text("held one\r\n", false);
        l.take_text("\x1b[Hnew screen\r\n", false);
        assert_eq!(texts(&l), ["nsh> top", "old screen"]);
        assert_eq!(l.count_text(""), "2  \u{00b7}  1 held  \u{00b7}  paused");
        l.set_paused(false);
        assert_eq!(texts(&l), ["nsh> top", "old screen", "new screen"]);
    }

    #[test]
    fn a_replay_rebuilds_the_shells_lines_and_keeps_the_consoles_own() {
        let mut l = Lines::default();
        l.add("success", "PX4 shell open.", false);
        l.take_text("nsh> free\r\n  total used\r\n", false);
        l.take_text("nsh> free\r\n  total used\r\nnsh> ver\r\n", true);
        assert_eq!(texts(&l), ["PX4 shell open.", "nsh> free", "  total used", "nsh> ver"]);
    }

    #[test]
    fn the_console_keeps_the_newest_lines() {
        let mut l = Lines::default();
        for i in 0..MAX_LINES + 10 {
            l.add("shell", &format!("line {i}"), true);
        }
        assert_eq!(l.lines.len(), MAX_LINES);
        assert_eq!(l.lines[0].text, "line 10");
        l.set_paused(true);
        for i in 0..MAX_LINES + 5 {
            l.add("shell", &format!("held {i}"), true);
        }
        assert_eq!(l.held.len(), MAX_LINES);
    }

    fn screen_text(t: &Term) -> String {
        t.with(|e| {
            (0..e.screen.rows)
                .map(|y| corvus_api::ui::term::emu::Screen::line_text(e.screen.line_at(0, y)))
                .collect::<Vec<_>>()
                .join("\n")
                .trim_end()
                .to_string()
        })
    }

    fn live() -> Nuttx {
        let mut n = Nuttx {
            live: true,
            ..Nuttx::default()
        };
        n.mode.kind = Kind::Live;
        n
    }

    #[test]
    fn the_terminal_draws_the_typed_line_after_the_prompt_and_sends_it_on_enter() {
        let mut n = live();
        n.term.output("nsh> ");
        n.terminal_keys("fre", 0.0);
        assert_eq!(screen_text(&n.term), "nsh> fre");
        n.terminal_keys("\x7f\x7f\x7fver", 0.0);
        assert_eq!(screen_text(&n.term), "nsh> ver");
        assert_eq!(n.pending_input, "", "nothing leaves before Enter");
        n.terminal_keys("\r", 0.0);
        assert_eq!(n.pending_input, "ver\n");
        assert_eq!(screen_text(&n.term), "nsh>", "the typed copy goes: NSH echoes the line");
        assert_eq!(n.history(), ["ver"]);
        // Output that arrives while a line is being typed goes above it.
        n.term.output("ver\r\nPX4\r\nnsh> ");
        n.terminal_keys("to", 0.0);
        n.term.output("\r\nINFO  [commander] Ready\r\nnsh> ");
        assert!(
            screen_text(&n.term).ends_with("INFO  [commander] Ready\nnsh> to"),
            "{}",
            screen_text(&n.term)
        );
    }

    #[test]
    fn ctrl_c_marks_a_cancelled_line_and_interrupts_on_an_empty_one() {
        let mut n = live();
        n.term.output("nsh> ");
        n.terminal_keys("top\x03", 0.0);
        assert_eq!(screen_text(&n.term), "nsh> top^C");
        assert_eq!(n.pending_input, "\n", "a fresh prompt is asked for");
        n.pending_input.clear();
        n.terminal_keys("\x03", 0.0);
        assert_eq!(n.pending_input, "\x03");
    }

    #[test]
    fn a_long_line_wraps_and_is_erased_whole() {
        let mut n = live();
        n.term.resize(20, 6);
        n.term.output("nsh> ");
        let long = "listener sensor_accel 5";
        n.terminal_keys(long, 0.0);
        assert_eq!(screen_text(&n.term), "nsh> listener sensor\n_accel 5");
        n.terminal_keys("\x15", 0.0);
        assert_eq!(screen_text(&n.term), "nsh>");
    }

    #[test]
    fn nothing_is_typed_into_a_shell_that_is_not_there() {
        let mut n = Nuttx {
            live: true,
            ..Nuttx::default()
        };
        n.terminal_keys("reboot\r", 0.0);
        assert_eq!(n.pending_input, "");
        assert_eq!(screen_text(&n.term), "");
    }

    #[test]
    fn the_mode_is_said_once_in_both_views() {
        let mut n = live();
        n.set_mode(Kind::Offline, "", OFFLINE);
        n.set_mode(Kind::Offline, "", OFFLINE);
        assert_eq!(texts(&n.lines), ["Vehicle disconnected."]);
        assert!(screen_text(&n.term).contains("Vehicle disconnected."));
        n.set_mode(Kind::Live, "PX4", "");
        assert_eq!(texts(&n.lines), ["Vehicle disconnected.", "PX4 shell open."]);
    }
}
