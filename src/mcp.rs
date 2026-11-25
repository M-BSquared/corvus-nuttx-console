//! What the NuttX Console offers an AI client through Corvus' MCP server, when
//! the operator has allowed it twice: Plugin tools in Settings, Connections,
//! MCP, and one or both of the switches in this plugin's gear.
//!
//! "May read" runs the commands of [`reads_only`], a short list that shows
//! something and changes nothing. "May run any command" takes any one line, and
//! the operator is asked before each, with the command in the question. It is
//! refused while the aircraft is armed: a shell can disarm, move an actuator or
//! reboot the autopilot past every other permission.
//!
//! A call types its command into the shell as the console does, through the same
//! endpoints, and waits for the prompt to come back. With the plugin closed it
//! opens a shell of its own and ends it again, so nothing keeps streaming over
//! the telemetry link.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use corvus_api::net::bg::Pending;
use corvus_api::net::stream::Stream;
use corvus_api::plugin::{Host, McpTool};
use serde_json::{json, Value};

use super::model::{is_prompt, Assembler, Piece};
use super::{Kind, Nuttx, CLOSE_URL, INBOX_MAX, SEND_URL};

pub const MCP_READ: &str = "mcp_read";
pub const MCP_RUN: &str = "mcp_run";
/// Short enough to stand whole in the question to the operator.
pub const COMMAND_MAX: usize = 200;
/// How long a command may take; the MCP server gives a plugin 30 s.
const RUN_LIMIT: Duration = Duration::from_secs(15);
/// How long the shell's stream may take to come up.
const STREAM_LIMIT: Duration = Duration::from_secs(5);
const OUTPUT_MAX: usize = 16 * 1024;
const DONE_MAX: usize = 32;

/// Modules whose `status` only prints.
const STATUS: [&str; 16] = [
    "battery_status",
    "commander",
    "control_allocator",
    "dataman",
    "ekf2",
    "fw_att_control",
    "gps",
    "land_detector",
    "logger",
    "mavlink",
    "mc_att_control",
    "mc_pos_control",
    "navigator",
    "rc_update",
    "sensors",
    "uorb",
];

/// Whole commands that only print.
const WHOLE: [&str; 13] = [
    "df",
    "dmesg",
    "free",
    "mount",
    "param status",
    "perf",
    "ps",
    "top once",
    "uorb top -1",
    "uptime",
    "ver all",
    "work_queue status",
    "param show",
];

fn word_ok(w: &str) -> bool {
    !w.is_empty() && !w.starts_with('-') && w.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '*'))
}

/// Whether `command` is one of the few that show something and change nothing.
/// What is not known is not on the list: many modules act on a word they do not know.
pub fn reads_only(command: &str) -> bool {
    let words: Vec<&str> = command.split(' ').collect();
    if WHOLE.contains(&command) {
        return true;
    }
    match words.as_slice() {
        [m, "status"] => STATUS.contains(m),
        ["ver", what] => [
            "hw", "hwcmp", "git", "bdate", "gcc", "uid", "mcu", "px4guid", "uri", "sw",
        ]
        .contains(what),
        ["param", "show", name] => word_ok(name),
        ["listener", topic] => word_ok(topic) && !topic.contains('*'),
        ["listener", topic, "-n", n] => {
            word_ok(topic) && !topic.contains('*') && n.parse::<u32>().is_ok_and(|n| (1..=10).contains(&n))
        }
        ["ls", path] => path.starts_with('/') && path_ok(path),
        ["cat", path] => ["/proc/", "/etc/"].iter().any(|r| path.starts_with(r)) && path_ok(path),
        _ => false,
    }
}

fn path_ok(p: &str) -> bool {
    !p.contains("..")
        && p.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '.' | '-'))
}

/// One command line out of an AI client's arguments: printable ASCII, so what the
/// operator reads in the question is what the shell gets.
pub fn command_of(args: &Value) -> Result<String, String> {
    let c = match args.get("command") {
        Some(Value::String(s)) => s.trim(),
        _ => return Err("command is required".into()),
    };
    if c.is_empty() {
        return Err("command is required".into());
    }
    if c.len() > COMMAND_MAX {
        return Err(format!("command may be at most {COMMAND_MAX} characters long"));
    }
    if !c.chars().all(|c| (' '..='~').contains(&c)) {
        return Err("command must be one line of plain ASCII characters".into());
    }
    Ok(c.to_string())
}

/// The tools the two switches allow.
pub fn tools(read: bool, run: bool) -> Vec<McpTool> {
    let schema = json!({"type": "object", "properties": {"command": {"type": "string",
        "description": format!("One line, at most {COMMAND_MAX} characters")}}, "required": ["command"]});
    let mut out = Vec::new();
    if read {
        out.push(McpTool {
            name: "read_shell".into(),
            title: "Read from the PX4 shell".into(),
            description: format!(
                "Run one read only command in the PX4 NuttX shell (NSH) and return what it prints. Allowed: {}; ver with hw, sw, git, uid, mcu, bdate, gcc, uri, hwcmp or px4guid; param show NAME; listener TOPIC [-n 1 to 10]; ls /PATH; cat under /proc or /etc; and status of {}.",
                WHOLE.join(", "),
                STATUS.join(", ")
            ),
            input_schema: schema.clone(),
            confirm: false,
            read_only: true,
        });
    }
    if run {
        out.push(McpTool {
            name: "run_shell".into(),
            title: "Run in the PX4 shell".into(),
            description: "Run any one line in the PX4 NuttX shell (NSH) and return what it prints. The operator is asked before each. Refused while the aircraft is armed. A command that does not end is stopped after 15 s.".into(),
            input_schema: schema,
            confirm: true,
            read_only: false,
        });
    }
    out
}

/// Puts together what a command printed.
#[derive(Debug, Default)]
pub struct Reader {
    command: String,
    asm: Assembler,
    lines: Vec<String>,
    chars: usize,
    echoed: bool,
    cut: bool,
}

impl Reader {
    pub fn new(command: &str) -> Reader {
        Reader {
            command: command.to_string(),
            ..Reader::default()
        }
    }

    /// NSH repeats the line it was sent, after its prompt.
    fn is_echo(&self, line: &str) -> bool {
        let line = line.trim_end();
        line == self.command || (is_prompt(line) && line[4..].trim() == self.command)
    }

    pub fn feed(&mut self, text: &str) {
        for piece in self.asm.feed(text) {
            match piece {
                // A program that draws its screen again: only the last one counts.
                Piece::Clear if self.echoed => self.forget(),
                Piece::Clear => {}
                Piece::Line(l) if !self.echoed && self.is_echo(&l) => {
                    self.echoed = true;
                    self.forget();
                }
                Piece::Line(l) if l.trim().is_empty() => {}
                Piece::Line(l) => {
                    let n = l.chars().count() + 1;
                    if self.chars + n > OUTPUT_MAX {
                        self.cut = true;
                    } else {
                        self.chars += n;
                        self.lines.push(l);
                    }
                }
            }
        }
    }

    fn forget(&mut self) {
        self.lines.clear();
        self.chars = 0;
    }

    /// The prompt is back.
    pub fn done(&self) -> bool {
        self.echoed && at_prompt(&self.asm)
    }

    pub fn answer(&self, complete: bool) -> Value {
        json!({"output": self.lines.join("\n"), "complete": complete, "cut": self.cut})
    }
}

/// Whether the shell stands at an empty prompt.
fn at_prompt(asm: &Assembler) -> bool {
    let p = asm.pending().trim_end();
    is_prompt(p) && p.len() == 4
}

type Sent = Pending<Result<(u16, Value), String>>;

enum Phase {
    /// The stream is coming up; its first event is what was printed before.
    Waiting,
    Sending(Sent),
    Reading,
}

struct Call {
    token: u64,
    command: String,
    /// Any command, so not while armed.
    any: bool,
    inbox: Arc<Mutex<VecDeque<(String, bool)>>>,
    _stream: Stream,
    phase: Phase,
    reader: Reader,
    since: Instant,
    /// There was no shell before, so it is ended after.
    owns: bool,
}

#[derive(Default)]
pub struct State {
    call: Option<Call>,
    done: Vec<(u64, Result<Value, String>)>,
    /// A Ctrl-C or a close on its way out.
    ending: Vec<Sent>,
}

impl State {
    fn answer(&mut self, token: u64, r: Result<Value, String>) {
        self.done.push((token, r));
        if self.done.len() > DONE_MAX {
            drop(self.done.remove(0));
        }
    }

    pub fn take(&mut self, token: u64) -> Option<Result<Value, String>> {
        let i = self.done.iter().position(|d| d.0 == token)?;
        Some(self.done.remove(i).1)
    }
}

/// Why a command may not run now, going by the telemetry.
fn refusal(state: &Value, any: bool) -> Option<&'static str> {
    if state.get("connected") != Some(&json!(true)) {
        return Some("No vehicle is connected.");
    }
    // Not known to be disarmed is as good as armed.
    if any && state.get("armed") != Some(&json!(false)) {
        return Some("The aircraft is armed. Only the read only commands run then.");
    }
    None
}

impl Nuttx {
    pub(crate) fn mcp_tools_for(h: &Host) -> Vec<McpTool> {
        let on = |k: &str| h.options().get(k) == Some(&Value::Bool(true));
        tools(on(MCP_READ), on(MCP_RUN))
    }

    /// Starts one tool call; the answer comes from `mcp.take`.
    pub(crate) fn mcp_start(&mut self, name: &str, args: &Value, token: u64, h: &mut Host) {
        match self.mcp_check(name, args, h) {
            Ok((command, any)) => {
                let inbox: Arc<Mutex<VecDeque<(String, bool)>>> = Arc::default();
                let sink = inbox.clone();
                let stream = h.events("shell", move |topic, data| {
                    let Some(text) = data.get("text").and_then(Value::as_str).filter(|_| topic == "shell") else {
                        return;
                    };
                    let mut q = sink.lock().unwrap_or_else(|e| e.into_inner());
                    if q.len() >= INBOX_MAX {
                        q.pop_front();
                    }
                    q.push_back((text.to_string(), data.get("replay") == Some(&json!(true))));
                });
                self.mcp.call = Some(Call {
                    token,
                    reader: Reader::new(&command),
                    command,
                    any,
                    inbox,
                    _stream: stream,
                    phase: Phase::Waiting,
                    since: Instant::now(),
                    owns: false,
                });
            }
            Err(e) => self.mcp.answer(token, Err(e)),
        }
    }

    fn mcp_check(&self, name: &str, args: &Value, h: &Host) -> Result<(String, bool), String> {
        if !Self::mcp_tools_for(h).iter().any(|t| t.name == name) {
            return Err(format!("The plugin offers no tool {name} right now."));
        }
        let any = name == "run_shell";
        let command = command_of(args)?;
        if !any && !reads_only(&command) {
            return Err("That is not one of the read only commands. The description of read_shell lists them.".into());
        }
        if let Some(why) = refusal(h.state, any) {
            return Err(why.into());
        }
        if self.mcp.call.is_some() {
            return Err("Another command is still running.".into());
        }
        if self.live && self.mode.kind == Kind::Live && (self.sending.is_some() || !self.pending_input.is_empty()) {
            return Err("The operator is using the shell.".into());
        }
        Ok((command, any))
    }

    /// Moves the call on. Every frame, whether or not the plugin is open.
    pub(crate) fn mcp_run(&mut self, h: &Host) {
        self.mcp.ending.retain(|p| p.take().is_none());
        let Some(mut c) = self.mcp.call.take() else { return };
        match self.mcp_step(&mut c, h) {
            None => self.mcp.call = Some(c),
            Some(r) => {
                // A shell that was opened for this is ended, unless the operator opened the
                // console meanwhile. In one that stays, what still runs is stopped.
                let sent = !matches!(c.phase, Phase::Waiting);
                let running = matches!(&r, Ok(v) if v["complete"] == json!(false));
                if sent && c.owns && !self.live {
                    self.mcp.ending.push(h.post_any(CLOSE_URL, json!({})));
                } else if running {
                    self.mcp.ending.push(h.post_any(SEND_URL, json!({"data": "\x03"})));
                }
                self.mcp.answer(c.token, r);
            }
        }
    }

    fn mcp_step(&mut self, c: &mut Call, h: &Host) -> Option<Result<Value, String>> {
        let batch: Vec<(String, bool)> = c.inbox.lock().unwrap_or_else(|e| e.into_inner()).drain(..).collect();
        match &c.phase {
            Phase::Waiting => {
                let Some((backlog, _)) = batch.into_iter().find(|(_, replay)| *replay) else {
                    return (c.since.elapsed() > STREAM_LIMIT)
                        .then(|| Err("The shell's stream did not come up.".into()));
                };
                let mut before = Assembler::default();
                before.feed(&backlog);
                c.owns = backlog.is_empty();
                if !c.owns && !at_prompt(&before) {
                    return Some(Err("A program is running in the shell.".into()));
                }
                // The operator may have armed since the question was answered.
                if let Some(why) = refusal(h.state, c.any) {
                    return Some(Err(why.into()));
                }
                c.since = Instant::now();
                c.phase = Phase::Sending(h.post_any(SEND_URL, json!({"data": format!("{}\n", c.command)})));
            }
            Phase::Sending(sent) => {
                for (text, replay) in batch {
                    if !replay {
                        c.reader.feed(&text);
                    }
                }
                if let Some(answer) = sent.take() {
                    let body = match answer {
                        Ok((_, body)) => body,
                        Err(e) => json!({ "error": e }),
                    };
                    if body.get("ok") != Some(&json!(true)) {
                        let why = body.get("error").and_then(Value::as_str).filter(|e| !e.is_empty());
                        return Some(Err(why.unwrap_or("no answer from Corvus").to_string()));
                    }
                    c.phase = Phase::Reading;
                }
            }
            Phase::Reading => {
                for (text, replay) in batch {
                    if !replay {
                        c.reader.feed(&text);
                    }
                }
                if c.reader.done() {
                    return Some(Ok(c.reader.answer(true)));
                }
            }
        }
        (c.since.elapsed() > RUN_LIMIT).then(|| match c.phase {
            Phase::Reading => Ok(c.reader.answer(false)),
            _ => Err("The shell did not take the command.".into()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_switches_decide_which_tools_there_are() {
        assert!(tools(false, false).is_empty());
        let t = tools(true, true);
        assert_eq!(
            (t[0].name.as_str(), t[0].read_only, t[0].confirm),
            ("read_shell", true, false)
        );
        assert_eq!(
            (t[1].name.as_str(), t[1].read_only, t[1].confirm),
            ("run_shell", false, true)
        );
        assert!(
            t.iter().all(|t| t.description.chars().count() < 1000),
            "the server cuts a longer one"
        );
        assert_eq!(tools(false, true).len(), 1);
    }

    #[test]
    fn only_what_prints_is_read_only() {
        for c in [
            "ver all",
            "ver sw",
            "free",
            "dmesg",
            "top once",
            "param show",
            "param show MPC_XY_*",
            "listener vehicle_status",
            "listener sensor_accel -n 5",
            "commander status",
            "ekf2 status",
            "uorb top -1",
            "ls /fs/microsd/log",
            "cat /proc/meminfo",
        ] {
            assert!(reads_only(c), "{c}");
        }
        for c in [
            "",
            "reboot",
            "reboot status",
            "commander arm",
            "commander disarm -f",
            "param set MPC_XY_VEL_MAX 5",
            "param reset_all",
            "param show A; reboot",
            "ver all; reboot",
            "ver all && reboot",
            "ver  all",
            "free\nreboot",
            "top",
            "dmesg -f",
            "uorb top",
            "perf reset",
            "mount -t vfat /dev/x /y",
            "listener vehicle_status -n 100000",
            "listener x -r 100",
            "listener *",
            "cat /dev/ttyS1",
            "cat /etc/../dev/ttyS1",
            "cat /fs/microsd/x",
            "ls /$(reboot)",
            "ls `reboot`",
            "echo x > /fs/microsd/etc/extras.txt",
            "actuator_test set -m 1 -v 1",
            "mavlink stop-all",
            "unknown status",
            "param show -a",
        ] {
            assert!(!reads_only(c), "{c}");
        }
    }

    #[test]
    fn a_command_is_one_plain_line() {
        let c = |v: Value| command_of(&json!({ "command": v }));
        assert_eq!(c(json!("  ver all ")).unwrap(), "ver all");
        for bad in [
            json!(""),
            json!("  "),
            json!(null),
            json!(3),
            json!("a\nb"),
            json!("a\rb"),
            json!("a\u{3}"),
            json!("ver\u{202e}lla"),
            json!("x".repeat(COMMAND_MAX + 1)),
        ] {
            assert!(c(bad.clone()).is_err(), "{bad}");
        }
        assert!(command_of(&json!({})).is_err());
    }

    #[test]
    fn nothing_but_reading_runs_while_armed_or_when_that_is_not_known() {
        let s = |armed: Value| json!({"connected": true, "armed": armed});
        assert_eq!(refusal(&s(json!(false)), true), None);
        assert_eq!(refusal(&s(json!(true)), false), None);
        assert!(refusal(&s(json!(true)), true).is_some());
        assert!(refusal(&s(json!(null)), true).is_some());
        assert!(refusal(&json!({"armed": false}), false).is_some());
    }

    #[test]
    fn the_output_is_what_stands_between_the_echo_and_the_prompt() {
        let mut r = Reader::new("free");
        r.feed("\r\nNuttShell (NSH) NuttX-11.0.0\r\nnsh> ");
        assert!(!r.done(), "the prompt before the command is not the one after it");
        r.feed("free\r\n    total  used\r\n");
        r.feed("Umem: 1 2\r\n");
        assert!(!r.done());
        r.feed("nsh> ");
        assert!(r.done());
        assert_eq!(
            r.answer(true),
            json!({"output": "    total  used\nUmem: 1 2", "complete": true, "cut": false})
        );
        let mut plain = Reader::new("ver sw");
        plain.feed("ver sw\r\nPX4 1.17\r\npxh> ");
        assert!(
            plain.done(),
            "an echo without a prompt before it, and the simulator's prompt"
        );
    }

    #[test]
    fn a_screen_drawn_again_counts_once_and_output_has_an_end() {
        let mut r = Reader::new("top");
        r.feed("nsh> top\r\n\x1b[Hold\r\n\x1b[Hnew\r\n");
        assert_eq!(r.answer(false)["output"], "new");
        assert!(!r.done());
        let mut r = Reader::new("dmesg");
        r.feed("nsh> dmesg\r\n");
        for _ in 0..10_000 {
            r.feed("a line of the log\r\n");
        }
        let a = r.answer(false);
        assert!(a["output"].as_str().unwrap().len() <= OUTPUT_MAX);
        assert_eq!(a["cut"], true);
    }

    #[test]
    fn answers_are_taken_once_and_do_not_pile_up() {
        let mut s = State::default();
        for i in 0..100 {
            s.answer(i, Ok(json!(i)));
        }
        assert_eq!(s.done.len(), DONE_MAX);
        assert_eq!(s.take(99), Some(Ok(json!(99))));
        assert_eq!(s.take(99), None);
    }

    /// A station with a shell: streams it, and answers what is typed with `reply`.
    struct Station {
        port: u16,
        posts: Arc<Mutex<Vec<String>>>,
    }

    fn station(backlog: &'static str, reply: &'static str) -> Station {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let posts: Arc<Mutex<Vec<String>>> = Arc::default();
        let seen = posts.clone();
        let (say, hear) = std::sync::mpsc::channel::<String>();
        let hear = Arc::new(Mutex::new(hear));
        std::thread::spawn(move || {
            for mut conn in listener.incoming().flatten() {
                let (seen, say, hear) = (seen.clone(), say.clone(), hear.clone());
                std::thread::spawn(move || {
                    let mut r = BufReader::new(conn.try_clone().unwrap());
                    let (mut first, mut length) = (String::new(), 0usize);
                    let _ = r.read_line(&mut first);
                    loop {
                        let mut l = String::new();
                        if r.read_line(&mut l).unwrap_or(0) == 0 || l.trim().is_empty() {
                            break;
                        }
                        if let Some(n) = l.to_ascii_lowercase().strip_prefix("content-length:") {
                            length = n.trim().parse().unwrap_or(0);
                        }
                    }
                    if first.contains("/api/events") {
                        let event = |v: Value| format!("event: shell\ndata: {v}\n\n");
                        let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n";
                        let _ = conn.write_all(head.as_bytes());
                        let _ = conn.write_all(event(json!({"text": backlog, "replay": true})).as_bytes());
                        while let Ok(text) = hear.lock().unwrap().recv_timeout(Duration::from_secs(10)) {
                            if conn.write_all(event(json!({ "text": text })).as_bytes()).is_err() {
                                break;
                            }
                        }
                        return;
                    }
                    let mut body = vec![0; length];
                    let _ = r.read_exact(&mut body);
                    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                    let path = first.split_whitespace().nth(1).unwrap_or("").to_string();
                    let data = body["data"].as_str().unwrap_or("").to_string();
                    seen.lock().unwrap().push(format!("{path} {data:?}"));
                    let ok = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\nConnection: close\r\n\r\n{\"ok\":true}";
                    let _ = conn.write_all(ok);
                    if path == SEND_URL {
                        let _ = say.send(reply.to_string());
                    }
                });
            }
        });
        Station { port, posts }
    }

    /// Calls `tool` on a plugin that is closed and runs it until there is an answer.
    fn call(s: &Station, tool: &str, command: &str, armed: bool) -> Result<Value, String> {
        use corvus_api::plugin::{bus::Bus, shapes::ShapeSet, Outbox, Plugin};
        let mut p = Nuttx::default();
        let ctx = eframe::egui::Context::default();
        let server = corvus_api::net::http::Server::local(s.port);
        let state = json!({"connected": true, "armed": armed});
        let opts = json!({MCP_READ: true, MCP_RUN: true});
        let info = p.info();
        let (mut shapes, mut saved, mut out, mut bus) =
            (ShapeSet::default(), json!({}), Outbox::default(), Bus::default());
        let end = Instant::now() + Duration::from_secs(20);
        let mut started = false;
        while Instant::now() < end {
            let mut h = Host::new(
                &ctx,
                &server,
                &state,
                info,
                &mut shapes,
                &mut saved,
                opts.as_object().unwrap(),
                &mut out,
                &mut bus,
            );
            if !std::mem::replace(&mut started, true) {
                p.mcp_call(tool, &json!({ "command": command }), 7, &mut h);
            }
            p.tick(&mut h);
            if let Some(r) = p.mcp_take(7) {
                // What ends the shell is on its way out.
                let until = Instant::now() + Duration::from_secs(5);
                while !p.mcp.ending.is_empty() && Instant::now() < until {
                    p.tick(&mut h);
                    std::thread::sleep(Duration::from_millis(5));
                }
                return r;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("no answer");
    }

    #[test]
    fn a_command_is_typed_its_output_comes_back_and_the_shell_is_ended() {
        let s = station("", "NuttShell (NSH)\r\nnsh> ver sw\r\nPX4 1.17\r\nnsh> ");
        let r = call(&s, "read_shell", "ver sw", true).unwrap();
        assert_eq!(r, json!({"output": "PX4 1.17", "complete": true, "cut": false}));
        let posts = s.posts.lock().unwrap().clone();
        assert_eq!(
            posts,
            [format!("{SEND_URL} {:?}", "ver sw\n"), format!("{CLOSE_URL} {:?}", "")]
        );
    }

    #[test]
    fn a_shell_that_was_there_stays_and_one_in_use_is_left_alone() {
        let s = station("nsh> ", "nsh> reboot\r\nnsh> ");
        call(&s, "run_shell", "reboot", false).unwrap();
        assert_eq!(s.posts.lock().unwrap().len(), 1, "it is not ended");
        let s = station("nsh> top\r\n  PID COMMAND\r\n", "");
        let e = call(&s, "run_shell", "free", false).unwrap_err();
        assert!(e.contains("A program is running"), "{e}");
        assert!(s.posts.lock().unwrap().is_empty(), "nothing was typed into it");
    }

    #[test]
    fn what_is_not_allowed_never_reaches_the_shell() {
        let s = station("", "");
        for (tool, command, armed, why) in [
            ("read_shell", "reboot", false, "read only"),
            ("run_shell", "reboot", true, "armed"),
            ("run_shell", "a\nb", false, "one line"),
            ("other", "ver all", false, "no tool"),
        ] {
            let e = call(&s, tool, command, armed).unwrap_err();
            assert!(e.contains(why), "{e}");
        }
        assert!(s.posts.lock().unwrap().is_empty());
    }
}
