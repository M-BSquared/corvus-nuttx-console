//! The rules of the NuttX console, free of the interface: the line editor of
//! the terminal, how the line is drawn after the prompt, how NSH's bytes are
//! cut into the console's lines, and the command history. Each is the twin of
//! a function of `nuttx-console.js` and is held to what that one answered.

/// Commands kept in the history.
pub const HISTORY_MAX: usize = 50;
/// A line with no newline in sight is shown once it is this long.
pub const LINE_MAX_CHARS: usize = 8192;

/// A key the line editor knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Left,
    Right,
    Home,
    End,
    WordLeft,
    WordRight,
    Backspace,
    Delete,
    WordDelete,
    KillBefore,
    KillAfter,
    Up,
    Down,
}

fn control_key(ch: char) -> Option<Key> {
    Some(match ch {
        '\x7f' | '\x08' => Key::Backspace,
        '\x01' => Key::Home,
        '\x05' => Key::End,
        '\x02' => Key::Left,
        '\x06' => Key::Right,
        '\x10' => Key::Up,
        '\x0e' => Key::Down,
        '\x15' => Key::KillBefore,
        '\x0b' => Key::KillAfter,
        '\x17' => Key::WordDelete,
        _ => return None,
    })
}

fn csi_key(seq: &str) -> Option<Key> {
    Some(match seq {
        "A" => Key::Up,
        "B" => Key::Down,
        "C" => Key::Right,
        "D" => Key::Left,
        "H" | "1~" | "7~" => Key::Home,
        "F" | "4~" | "8~" => Key::End,
        "3~" => Key::Delete,
        "1;5C" | "1;3C" => Key::WordRight,
        "1;5D" | "1;3D" => Key::WordLeft,
        _ => return None,
    })
}

fn ss3_key(ch: char) -> Option<Key> {
    Some(match ch {
        'A' => Key::Up,
        'B' => Key::Down,
        'C' => Key::Right,
        'D' => Key::Left,
        'H' => Key::Home,
        'F' => Key::End,
        _ => return None,
    })
}

/// The key an escape sequence at `s[i]` stands for, and how long it is
/// (`readEscape`).
pub fn read_escape(s: &[char], i: usize) -> (Option<Key>, usize) {
    match s.get(i + 1) {
        Some('[') => {
            let mut j = i + 2;
            while j < s.len() && !('\x40'..='\x7e').contains(&s[j]) {
                j += 1;
            }
            let end = j.min(s.len().saturating_sub(1));
            let seq: String = s.get(i + 2..=end).unwrap_or(&[]).iter().collect();
            (csi_key(&seq), end + 1 - i)
        }
        Some('O') if i + 2 < s.len() => (ss3_key(s[i + 2]), 3),
        Some('b') => (Some(Key::WordLeft), 2),
        Some('f') => (Some(Key::WordRight), 2),
        Some('\x7f') => (Some(Key::WordDelete), 2),
        _ => (None, 1),
    }
}

/// Earlier commands as saved: text only, no blanks, the newest `HISTORY_MAX`.
pub fn clean_history(saved: &[String]) -> Vec<String> {
    let kept: Vec<String> = saved.iter().filter(|h| !h.trim().is_empty()).cloned().collect();
    kept[kept.len().saturating_sub(HISTORY_MAX)..].to_vec()
}

/// Adds `text` at the end of `history`: no blanks, no repeat of the last.
/// True when the history changed.
pub fn remember(history: &mut Vec<String>, text: &str) -> bool {
    if text.trim().is_empty() || history.last().is_some_and(|l| l == text) {
        return false;
    }
    history.push(text.to_string());
    let over = history.len().saturating_sub(HISTORY_MAX);
    history.drain(..over);
    true
}

/// What the terminal has to do after keys were fed to the editor, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// The line or the cursor moved: the state then.
    Render { line: String, cursor: usize },
    /// Enter: send the line and a newline.
    Submit(String),
    /// Ctrl+C on a line: drop it and ask for a prompt.
    Cancel(String),
    /// Ctrl+C on an empty line: send it to NSH.
    Interrupt,
    /// Ctrl+L.
    Clear,
}

/// The line being typed in the terminal, its cursor and the command history
/// (`createEditor`). The line is edited here and sent on Enter: a packet per
/// keystroke would wait a round trip over a telemetry radio.
#[derive(Debug, Default, Clone)]
pub struct Editor {
    line: Vec<char>,
    cursor: usize,
    history: Vec<String>,
    /// None: a new line; otherwise the history entry shown.
    index: Option<usize>,
    /// The new line, kept while the history is browsed.
    draft: Vec<char>,
}

impl Editor {
    pub fn new(saved: &[String]) -> Editor {
        Editor {
            history: clean_history(saved),
            ..Editor::default()
        }
    }

    pub fn line(&self) -> String {
        self.line.iter().collect()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn history(&self) -> &[String] {
        &self.history
    }

    /// Takes the history the console and the terminal share.
    pub fn set_history(&mut self, history: &[String]) {
        self.history = clean_history(history);
        self.index = None;
    }

    pub fn reset(&mut self) {
        self.line.clear();
        self.cursor = 0;
        self.index = None;
        self.draft.clear();
    }

    fn word_start_before(&self, pos: usize) -> usize {
        let mut p = pos;
        while p > 0 && self.line[p - 1] == ' ' {
            p -= 1;
        }
        while p > 0 && self.line[p - 1] != ' ' {
            p -= 1;
        }
        p
    }

    fn word_end_after(&self, pos: usize) -> usize {
        let mut p = pos;
        while p < self.line.len() && self.line[p] == ' ' {
            p += 1;
        }
        while p < self.line.len() && self.line[p] != ' ' {
            p += 1;
        }
        p
    }

    fn show(&mut self, text: Vec<char>) -> bool {
        self.cursor = text.len();
        self.line = text;
        true
    }

    /// Applies one key. True when the line or the cursor changed.
    pub fn key(&mut self, key: Key) -> bool {
        let len = self.line.len();
        match key {
            Key::Left if self.cursor > 0 => self.cursor -= 1,
            Key::Right if self.cursor < len => self.cursor += 1,
            Key::Home if self.cursor > 0 => self.cursor = 0,
            Key::End if self.cursor < len => self.cursor = len,
            Key::WordLeft => {
                let p = self.word_start_before(self.cursor);
                if p == self.cursor {
                    return false;
                }
                self.cursor = p;
            }
            Key::WordRight => {
                let p = self.word_end_after(self.cursor);
                if p == self.cursor {
                    return false;
                }
                self.cursor = p;
            }
            Key::Backspace if self.cursor > 0 => {
                self.line.remove(self.cursor - 1);
                self.cursor -= 1;
            }
            Key::Delete if self.cursor < len => {
                self.line.remove(self.cursor);
            }
            Key::WordDelete => {
                let p = self.word_start_before(self.cursor);
                if p == self.cursor {
                    return false;
                }
                self.line.drain(p..self.cursor);
                self.cursor = p;
            }
            Key::KillBefore if self.cursor > 0 => {
                self.line.drain(..self.cursor);
                self.cursor = 0;
            }
            Key::KillAfter if self.cursor < len => self.line.truncate(self.cursor),
            Key::Up => {
                if self.history.is_empty() || self.index == Some(0) {
                    return false;
                }
                let next = match self.index {
                    None => {
                        self.draft = self.line.clone();
                        self.history.len() - 1
                    }
                    Some(i) => i - 1,
                };
                self.index = Some(next);
                return self.show(self.history[next].chars().collect());
            }
            Key::Down => {
                let Some(i) = self.index else { return false };
                if i + 1 < self.history.len() {
                    self.index = Some(i + 1);
                    return self.show(self.history[i + 1].chars().collect());
                }
                self.index = None;
                let draft = self.draft.clone();
                return self.show(draft);
            }
            _ => return false,
        }
        true
    }

    /// Takes what the keyboard sent (a key, or a whole paste) and says what
    /// the view has to do, in order.
    pub fn feed(&mut self, data: &str) -> Vec<Effect> {
        let s: Vec<char> = data.chars().collect();
        let mut effects = Vec::new();
        let mut changed = false;
        let mut after_cr = false;
        let mut i = 0;
        while i < s.len() {
            let ch = s[i];
            if ch == '\x1b' {
                let (key, length) = read_escape(&s, i);
                i += length;
                after_cr = false;
                if key.is_some_and(|k| self.key(k)) {
                    changed = true;
                }
                continue;
            }
            i += 1;
            // A pasted CRLF is one line break, not an empty command after it.
            if ch == '\n' && after_cr {
                after_cr = false;
                continue;
            }
            after_cr = ch == '\r';
            let flush = |me: &Editor, effects: &mut Vec<Effect>, changed: &mut bool| {
                if *changed {
                    effects.push(Effect::Render {
                        line: me.line(),
                        cursor: me.cursor,
                    });
                }
                *changed = false;
            };
            match ch {
                '\r' | '\n' => {
                    flush(self, &mut effects, &mut changed);
                    let text = self.line();
                    remember(&mut self.history, &text);
                    self.reset();
                    effects.push(Effect::Submit(text));
                }
                '\x03' => {
                    flush(self, &mut effects, &mut changed);
                    effects.push(if self.line.is_empty() {
                        Effect::Interrupt
                    } else {
                        Effect::Cancel(self.line())
                    });
                    self.reset();
                }
                '\x0c' => {
                    flush(self, &mut effects, &mut changed);
                    effects.push(Effect::Clear);
                    changed = true;
                }
                _ => {
                    if let Some(k) = control_key(ch) {
                        if self.key(k) {
                            changed = true;
                        }
                    } else if ch >= ' ' {
                        // Tab and the rest are dropped: NSH completes nothing.
                        self.line.insert(self.cursor, ch);
                        self.cursor += 1;
                        changed = true;
                    }
                }
            }
        }
        if changed {
            effects.push(Effect::Render {
                line: self.line(),
                cursor: self.cursor,
            });
        }
        effects
    }
}

/// The escape sequence that moves the cursor from one index of the line to
/// another (`cursorMove`). The line starts at column `start_col`, after
/// whatever the shell printed last, and wraps at `cols`.
pub fn cursor_move(cols: usize, start_col: usize, from: usize, to: usize) -> String {
    let width = cols.max(1);
    let (a, b) = (start_col + from, start_col + to);
    let (ra, rb) = (a / width, b / width);
    let mut out = String::new();
    if rb < ra {
        out.push_str(&format!("\x1b[{}A", ra - rb));
    } else if rb > ra {
        out.push_str(&format!("\x1b[{}B", rb - ra));
    }
    out.push_str(&format!("\x1b[{}G", b % width + 1));
    out
}

/// Erases from the line's start, draws it, and puts the cursor where it
/// belongs (`paintLine`).
pub fn paint_line(cols: usize, start_col: usize, line: &str, cursor: usize) -> String {
    let len = line.chars().count();
    let mut out = format!("\x1b[J{line}");
    // Ending exactly on the last column leaves the terminal waiting to wrap, a
    // row short of where the arithmetic puts the cursor. A space and a
    // backspace take it there.
    if len > 0 && (start_col + len).is_multiple_of(cols.max(1)) {
        out.push_str(" \x08");
    }
    out + &cursor_move(cols, start_col, len, cursor)
}

/// What the assembler found in NSH's bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    /// A line that ended.
    Line(String),
    /// The shell cleared its screen or sent the cursor home: how `top` starts
    /// each redraw.
    Clear,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Parse {
    #[default]
    Text,
    Esc,
    Csi,
}

/// NSH's bytes as the console's lines (`createAssembler`). Other escapes
/// (colour, erase to the end of the line) are dropped, a backspace takes the
/// character before it, and a carriage return on its own starts the line
/// again, as it would on a screen. What has not ended yet is `pending`.
#[derive(Debug, Default, Clone)]
pub struct Assembler {
    partial: String,
    cr: bool,
    state: Parse,
    params: String,
}

impl Assembler {
    pub fn feed(&mut self, text: &str) -> Vec<Piece> {
        let mut out = Vec::new();
        for ch in text.chars() {
            match self.state {
                Parse::Esc => {
                    if ch == '[' {
                        self.state = Parse::Csi;
                        self.params.clear();
                    } else {
                        self.state = Parse::Text;
                        if ch == 'c' {
                            out.push(Piece::Clear);
                        }
                    }
                    continue;
                }
                Parse::Csi => {
                    if ('\x40'..='\x7e').contains(&ch) {
                        self.state = Parse::Text;
                        let home = (ch == 'H' || ch == 'f') && matches!(self.params.as_str(), "" | "1" | "1;1");
                        if home || (ch == 'J' && self.params == "2") {
                            out.push(Piece::Clear);
                        }
                    } else if self.params.len() < 32 {
                        self.params.push(ch);
                    }
                    continue;
                }
                Parse::Text => {}
            }
            match ch {
                '\x1b' => self.state = Parse::Esc,
                '\r' => self.cr = true,
                '\n' => {
                    out.push(Piece::Line(std::mem::take(&mut self.partial)));
                    self.cr = false;
                }
                _ => {
                    if self.cr {
                        self.partial.clear();
                        self.cr = false;
                    }
                    if ch == '\x08' {
                        self.partial.pop();
                    } else if ch == '\t' || (ch >= ' ' && ch != '\x7f') {
                        self.partial.push(ch);
                        if self.partial.chars().count() >= LINE_MAX_CHARS {
                            out.push(Piece::Line(std::mem::take(&mut self.partial)));
                        }
                    }
                }
            }
        }
        out
    }

    pub fn reset(&mut self) {
        *self = Assembler::default();
    }

    pub fn pending(&self) -> &str {
        &self.partial
    }
}

/// Whether a line is NSH's prompt (`nsh> ` or `pxh> `, with or without text).
pub fn is_prompt(text: &str) -> bool {
    ["nsh>", "pxh>"].iter().any(|p| {
        text.strip_prefix(p)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
    })
}

fn starts_with_word(text: &str, word: &str) -> bool {
    text.strip_prefix(word)
        .is_some_and(|rest| !rest.chars().next().is_some_and(|c| c.is_alphanumeric() || c == '_'))
}

/// The console level a line of NSH output is shown at (`lineLevel`).
pub fn line_level(text: &str) -> &'static str {
    if is_prompt(text) {
        "cmd"
    } else if starts_with_word(text, "ERROR") {
        "error"
    } else if starts_with_word(text, "WARN") {
        "warning"
    } else {
        "shell"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(line: &str, cursor: usize) -> Effect {
        Effect::Render {
            line: line.into(),
            cursor,
        }
    }

    #[test]
    fn typing_moving_and_deleting_edit_the_line() {
        let mut e = Editor::new(&[]);
        assert_eq!(e.feed("top"), vec![render("top", 3)]);
        assert_eq!(e.feed("\x1b[D\x1b[D"), vec![render("top", 1)]);
        assert_eq!(e.feed("x"), vec![render("txop", 2)]);
        assert_eq!(e.feed("\x7f"), vec![render("top", 1)]);
        assert_eq!(e.feed("\x1b[3~"), vec![render("tp", 1)]);
        assert_eq!(e.feed("\x01"), vec![render("tp", 0)]);
        assert_eq!(e.feed("\x01"), vec![], "nothing moved, nothing to draw");
        assert_eq!(e.feed("\x05"), vec![render("tp", 2)]);
        assert_eq!(e.feed("\t"), vec![], "Tab is dropped: NSH completes nothing");
    }

    #[test]
    fn words_are_jumped_over_and_killed() {
        let mut e = Editor::new(&[]);
        e.feed("listener  sensor_accel");
        assert_eq!(e.feed("\x1bb"), vec![render("listener  sensor_accel", 10)]);
        assert_eq!(e.feed("\x1b[1;5D"), vec![render("listener  sensor_accel", 0)]);
        assert_eq!(e.feed("\x1bf"), vec![render("listener  sensor_accel", 8)]);
        assert_eq!(e.feed("\x0b"), vec![render("listener", 8)]);
        assert_eq!(e.feed("\x17"), vec![render("", 0)]);
        e.feed("free now");
        assert_eq!(e.feed("\x1b[D\x1b[D\x1b[D\x15"), vec![render("now", 0)]);
    }

    #[test]
    fn enter_submits_and_the_history_is_browsed_with_the_draft_kept() {
        let mut e = Editor::new(&["ver all".into(), " ".into(), "free".into()]);
        assert_eq!(e.history(), ["ver all", "free"], "blanks are not history");
        assert_eq!(e.feed("top\r"), vec![render("top", 3), Effect::Submit("top".into())]);
        assert_eq!(e.history(), ["ver all", "free", "top"]);
        e.feed("dm");
        assert_eq!(e.feed("\x1b[A"), vec![render("top", 3)]);
        assert_eq!(e.feed("\x1b[A\x1b[A"), vec![render("ver all", 7)]);
        assert_eq!(e.feed("\x1b[A"), vec![], "the oldest is the end");
        assert_eq!(
            e.feed("\x1bOB\x1bOB\x1bOB"),
            vec![render("dm", 2)],
            "the draft comes back"
        );
        assert_eq!(e.feed("\x1b[B"), vec![]);
        // The same command twice is remembered once, an empty Enter not at all.
        e.reset();
        e.feed("top\r");
        e.feed("\r");
        assert_eq!(e.history(), ["ver all", "free", "top"]);
    }

    #[test]
    fn a_paste_is_several_lines_and_crlf_is_one_break() {
        let mut e = Editor::new(&[]);
        assert_eq!(
            e.feed("ver all\r\nfree\nto"),
            vec![
                render("ver all", 7),
                Effect::Submit("ver all".into()),
                render("free", 4),
                Effect::Submit("free".into()),
                render("to", 2),
            ]
        );
    }

    #[test]
    fn ctrl_c_cancels_a_line_or_interrupts_and_ctrl_l_clears() {
        let mut e = Editor::new(&[]);
        assert_eq!(e.feed("\x03"), vec![Effect::Interrupt]);
        assert_eq!(e.feed("top\x03"), vec![render("top", 3), Effect::Cancel("top".into())]);
        assert_eq!(e.line(), "");
        assert_eq!(e.feed("a\x0c"), vec![render("a", 1), Effect::Clear, render("a", 1)]);
    }

    #[test]
    fn escapes_are_read_whole_even_when_unknown_or_cut_short() {
        let s: Vec<char> = "\x1b[1;5C".chars().collect();
        assert_eq!(read_escape(&s, 0), (Some(Key::WordRight), 6));
        let s: Vec<char> = "\x1b[99zab".chars().collect();
        assert_eq!(read_escape(&s, 0), (None, 5));
        let s: Vec<char> = "\x1b[".chars().collect();
        assert_eq!(read_escape(&s, 0), (None, 2));
        let s: Vec<char> = "\x1b".chars().collect();
        assert_eq!(read_escape(&s, 0), (None, 1));
        let s: Vec<char> = "\x1bOH".chars().collect();
        assert_eq!(read_escape(&s, 0), (Some(Key::Home), 3));
        let mut e = Editor::new(&[]);
        assert_eq!(e.feed("a\x1b[99zb\x1b"), vec![render("ab", 2)]);
    }

    #[test]
    fn the_line_is_drawn_after_the_prompt_and_wraps() {
        assert_eq!(cursor_move(80, 5, 3, 0), "\x1b[6G");
        assert_eq!(cursor_move(10, 5, 0, 7), "\x1b[1B\x1b[3G");
        assert_eq!(cursor_move(10, 5, 17, 2), "\x1b[2A\x1b[8G");
        assert_eq!(cursor_move(0, 0, 0, 0), "\x1b[1G", "a width of nothing is one column");
        assert_eq!(paint_line(80, 5, "top", 3), "\x1b[Jtop\x1b[9G");
        assert_eq!(paint_line(80, 5, "top", 1), "\x1b[Jtop\x1b[7G");
        assert_eq!(
            paint_line(10, 5, "abcde", 5),
            "\x1b[Jabcde \x08\x1b[1G",
            "a line that ends on the last column is pushed onto the next row"
        );
        assert_eq!(paint_line(10, 5, "", 0), "\x1b[J\x1b[6G");
    }

    #[test]
    fn output_is_cut_into_lines_and_a_redraw_is_a_clear() {
        let mut a = Assembler::default();
        assert_eq!(a.feed("nsh> ver\r\nHW arch: PX4"), vec![Piece::Line("nsh> ver".into())]);
        assert_eq!(a.pending(), "HW arch: PX4");
        assert_eq!(a.feed("_FMU\r\n"), vec![Piece::Line("HW arch: PX4_FMU".into())]);
        // top: home, then the screen; colour and erase to end of line are dropped.
        assert_eq!(
            a.feed("\x1b[H\x1b[1;32mPID\x1b[0m COMMAND\x1b[K\n"),
            vec![Piece::Clear, Piece::Line("PID COMMAND".into())]
        );
        assert_eq!(a.feed("\x1b[2J"), vec![Piece::Clear]);
        assert_eq!(a.feed("\x1bc"), vec![Piece::Clear]);
        assert_eq!(a.feed("\x1b[5;3H"), vec![], "a cursor move elsewhere is not a clear");
        // A carriage return on its own starts the line again; a backspace takes one back.
        assert_eq!(a.feed("10%\r20%\rdone\n"), vec![Piece::Line("done".into())]);
        assert_eq!(a.feed("abx\x08c\x07\n"), vec![Piece::Line("abc".into())]);
        // An escape cut in two by a packet boundary is still one escape.
        assert_eq!(a.feed("a\x1b["), vec![]);
        assert_eq!(a.feed("31mb\n"), vec![Piece::Line("ab".into())]);
        a.feed("half");
        a.reset();
        assert_eq!(a.pending(), "");
    }

    #[test]
    fn a_line_with_no_end_is_shown_once_it_is_long() {
        let mut a = Assembler::default();
        let out = a.feed(&"x".repeat(LINE_MAX_CHARS + 3));
        assert_eq!(out, vec![Piece::Line("x".repeat(LINE_MAX_CHARS))]);
        assert_eq!(a.pending(), "xxx");
    }

    #[test]
    fn lines_get_the_level_of_what_they_are() {
        assert_eq!(line_level("nsh> top"), "cmd");
        assert_eq!(line_level("pxh>"), "cmd");
        assert_eq!(line_level("nsh>x"), "shell");
        assert_eq!(line_level("ERROR [mavlink] no link"), "error");
        assert_eq!(line_level("ERRORS: 0"), "shell");
        assert_eq!(line_level("WARN  [commander] low battery"), "warning");
        assert_eq!(line_level(" ERROR indented"), "shell");
    }

    #[test]
    fn the_history_keeps_the_newest_fifty() {
        let saved: Vec<String> = (0..60).map(|i| format!("cmd {i}")).collect();
        let mut h = clean_history(&saved);
        assert_eq!((h.len(), h[0].as_str()), (50, "cmd 10"));
        assert!(remember(&mut h, "new"));
        assert_eq!((h.len(), h[0].as_str(), h[49].as_str()), (50, "cmd 11", "new"));
        assert!(!remember(&mut h, "new"));
        assert!(!remember(&mut h, "   "));
    }
}
