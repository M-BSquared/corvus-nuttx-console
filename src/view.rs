//! What the operator sees: the switch between the two views, the console
//! (the MAVLink console's parts: toolbar, stream, command row, footer) and the
//! terminal (`mountConsole` and `mountTerminal` of `nuttx-console.js`).

use std::sync::{Arc, Mutex};

use corvus_api::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};

use corvus_api::i18n::{tr, tr_fmt};
use corvus_api::plugin::Host;
use corvus_api::ui::button::icon_btn;
use corvus_api::ui::card::{self, Look};
use corvus_api::ui::field::{Surface, TextField};
use corvus_api::ui::segmented::{self, Opts, Segment};
use corvus_api::ui::style::{self, Weight};
use corvus_api::ui::term::emu::Emulator;
use corvus_api::ui::term::view::TermState;
use corvus_api::ui::term::{self, Shell};
use corvus_api::ui::tip::HoverTr;
use corvus_api::ui::{field, icons};

use crate::{Kind, Nuttx, Tab};

/// The least height the plugin takes, when the panel is short.
const MIN_HEIGHT: f32 = 360.0;

/// What the view remembers between frames.
#[derive(Default)]
pub struct ViewState {
    pub filter: String,
    input: String,
    /// None: a new line; otherwise the history entry shown.
    hist: Option<usize>,
    draft: String,
    term: TermState,
    /// Lines counted when the stream was last at its live end.
    seen: u64,
    missed: usize,
    jump: bool,
    focus_on_live: bool,
}

/// What the terminal view talks to: the keys go into `typed`, to be run
/// through the line editor once the frame is drawn.
struct Keys<'a> {
    emu: Arc<Mutex<Emulator>>,
    typed: &'a mut String,
    size: &'a mut Option<(usize, usize)>,
}

impl Shell for Keys<'_> {
    fn emulator(&self) -> Arc<Mutex<Emulator>> {
        self.emu.clone()
    }

    fn send(&mut self, data: String) {
        self.typed.push_str(&data);
    }

    fn resize(&mut self, cols: usize, rows: usize) {
        *self.size = Some((cols, rows));
    }
}

fn status_of(n: &Nuttx) -> (&'static str, Color32) {
    match n.mode().kind {
        Kind::Live => ("CONNECTED", style::healthy()),
        Kind::Unsupported => ("NOT AVAILABLE", style::warning()),
        Kind::Loading => ("WAITING", style::text_3()),
        Kind::Offline => ("OFFLINE", style::text_3()),
    }
}

pub fn show(n: &mut Nuttx, ui: &mut egui::Ui, area: Rect, h: &mut Host) -> f32 {
    let height = (ui.clip_rect().bottom() - area.top() - 14.0).max(MIN_HEIGHT);
    let rect = Rect::from_min_size(area.min, Vec2::new(area.width(), height));
    let painter = ui.painter().clone();

    // the switch, the state and the new shell button
    let segs = [
        Segment::new(tr("Console").into_owned()).icon("list"),
        Segment::new(tr("Terminal").into_owned()).icon("square-terminal"),
    ];
    let selected = usize::from(n.tab == Tab::Terminal);
    let (strip, pressed) = segmented::show(ui, rect.min, &segs, selected, "nx-tab", Opts::FILTER);
    if let Some(i) = pressed {
        n.tab = if i == 1 { Tab::Terminal } else { Tab::Console };
    }
    let live = n.mode().kind == Kind::Live;
    let fresh = Rect::from_min_size(
        Pos2::new(rect.right() - 26.0, strip.center().y - 13.0),
        Vec2::splat(26.0),
    );
    if icon_btn(
        ui,
        fresh,
        "refresh-cw",
        "End the shell and what runs in it, and open a new one",
        false,
        "nx-new",
    ) && live
    {
        n.new_shell(h);
    }
    let (status, colour) = status_of(n);
    let g = style::galley(ui, &tr(status), 10.0, Weight::W600, colour, 0.6);
    painter.galley(
        Pos2::new(fresh.left() - 10.0 - g.size().x, strip.center().y - g.size().y / 2.0),
        g,
        colour,
    );

    let body = Rect::from_min_max(Pos2::new(rect.left(), strip.bottom() + 10.0), rect.max);
    match n.tab {
        Tab::Console => console(n, ui, body, h),
        Tab::Terminal => terminal(n, ui, body, h),
    }
    height
}

fn terminal(n: &mut Nuttx, ui: &mut egui::Ui, rect: Rect, h: &mut Host) {
    let hint_h = 18.0;
    let screen = Rect::from_min_max(rect.min, Pos2::new(rect.right(), rect.bottom() - hint_h));
    let (mut typed, mut size) = (String::new(), None);
    let mut io = Keys {
        emu: n.term.emu.clone(),
        typed: &mut typed,
        size: &mut size,
    };
    term::view::show(ui, screen, &mut io, &mut n.ui.term, "nx-term", true);
    if let Some((cols, rows)) = size {
        n.term.resize(cols, rows);
    }
    if !typed.is_empty() {
        n.terminal_keys(&typed, h.now);
    }
    let hint = hint_text(n, "The line is sent on Enter. Ctrl+C stops a running program.");
    let g = style::galley(ui, &hint, 10.0, Weight::W400, style::text_3(), 0.0);
    ui.painter()
        .galley(Pos2::new(rect.left(), rect.bottom() - 13.0), g, style::text_3());
}

/// The footer's words: what the shell is, or why there is none.
fn hint_text(n: &Nuttx, live_tail: &str) -> String {
    let mode = n.mode();
    match mode.kind {
        Kind::Live => {
            let name = if mode.label.is_empty() { "PX4" } else { &mode.label };
            format!(
                "{} {}",
                tr_fmt("{a} shell over MAVLink.", &[("a", name)]),
                tr(live_tail)
            )
        }
        Kind::Loading => tr("Waiting for the vehicle").into_owned(),
        _ => tr(&mode.detail).into_owned(),
    }
}

fn console(n: &mut Nuttx, ui: &mut egui::Ui, rect: Rect, h: &mut Host) {
    let painter = ui.painter().clone();
    let live = n.mode().kind == Kind::Live;

    // the toolbar: the filter, then the tools
    let field_h: f32 = 2.0 + 10.0 + 11.5 * 1.21;
    let bar = Rect::from_min_size(rect.min, Vec2::new(rect.width(), field_h.max(26.0) + 8.0));
    let tools_w = 5.0 * 26.0 + 4.0 * 2.0;
    let search = Rect::from_min_size(
        Pos2::new(rect.left(), bar.top() + (26.0 - field_h).max(0.0) / 2.0),
        Vec2::new(rect.width() - 8.0 - tools_w, field_h),
    );
    TextField::new(&mut n.ui.filter)
        .surface(Surface::Glass)
        .hint("Filter")
        .font(11.5, Weight::W400)
        .icon("search", 12.0, 7.0, 6.0)
        .pad(25.0, 8.0)
        .show(ui, search, "nx-filter");
    let filter = n.ui.filter.trim().to_lowercase();
    let paused = n.lines.paused;
    let tools: [(&'static str, &str); 5] = [
        ("octagon-x", "Stop the running program (Ctrl+C)"),
        if paused {
            ("play", "Resume the stream")
        } else {
            ("pause", "Pause the stream")
        },
        ("copy", "Copy visible lines"),
        ("download", "Save log to a file"),
        ("rotate-ccw", "Clear console"),
    ];
    let mut tx = search.right() + 8.0;
    for (i, (icon, title)) in tools.into_iter().enumerate() {
        let r = Rect::from_min_size(Pos2::new(tx, bar.top()), Vec2::splat(26.0));
        let resp = ui
            .interact(r, ui.scope_id().with(("nx-tool", i)), egui::Sense::click())
            .on_hover_tr(title);
        let dim = i == 0 && !live;
        let (fill, colour) = if i == 1 && paused {
            (style::warning_dark(), style::warning())
        } else if resp.hovered() && !dim {
            (style::surface_hover(), style::text_1())
        } else {
            (Color32::TRANSPARENT, style::text_3())
        };
        painter.rect_filled(r, 5, fill);
        icons::paint(
            ui,
            icon,
            Rect::from_center_size(r.center(), Vec2::splat(13.0)),
            if dim { colour.gamma_multiply(0.45) } else { colour },
        );
        if resp.clicked() {
            match i {
                0 if live => n.send("\x03"),
                1 => n.lines.set_paused(!paused),
                2 => {
                    let (count, text) = n.lines.transcript(&filter);
                    if !text.is_empty() {
                        ui.ctx().copy_text(text);
                        let said = tr_fmt("Copied {a} lines to the clipboard.", &[("a", &count.to_string())]);
                        n.lines.add("info", &said, false);
                    }
                }
                3 => n.save_log(h),
                4 => {
                    n.lines.clear();
                    n.ui.missed = 0;
                }
                _ => {}
            }
        }
        tx += 28.0;
    }

    // the footer and the command row from the bottom up
    let footer_h = 5.0 + 15.0;
    let row_h = 34.0 + 2.0 * 6.0;
    let footer = Rect::from_min_max(Pos2::new(rect.left(), rect.bottom() - footer_h), rect.max);
    let row = Rect::from_min_max(
        Pos2::new(rect.left(), footer.top() - row_h),
        Pos2::new(rect.right(), footer.top()),
    );
    let stream = Rect::from_min_max(Pos2::new(rect.left(), bar.bottom()), Pos2::new(rect.right(), row.top()));
    paint_stream(n, ui, stream, &filter);

    let prompt = style::galley_mono(ui, "nsh>", 13.0, style::terminal_cmd());
    painter.galley(
        Pos2::new(row.left(), row.center().y - prompt.size().y / 2.0),
        prompt.clone(),
        style::terminal_cmd(),
    );
    let send = Rect::from_min_size(Pos2::new(row.right() - 34.0, row.center().y - 17.0), Vec2::splat(34.0));
    let input = Rect::from_min_max(
        Pos2::new(row.left() + prompt.size().x + 8.0, row.center().y - 16.5),
        Pos2::new(send.left() - 8.0, row.center().y + 16.5),
    );
    let placeholder = match n.mode().kind {
        Kind::Live => "NSH command (\u{2191} history, Ctrl+C stops a program)",
        Kind::Loading => "Waiting for the vehicle",
        _ => "No shell",
    };
    let resp = field::text_field(ui, input, &mut n.ui.input, placeholder, true, "nx-input");
    if live && !n.ui.focus_on_live {
        n.ui.focus_on_live = true;
        resp.request_focus();
    }
    if resp.has_focus() {
        let (up, down, esc, ctrl_c) = ui.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::Escape),
                // Ctrl+C on an empty command line is the shell's, as in the
                // terminal; with text in it, it copies as everywhere.
                i.events.iter().any(|e| matches!(e, egui::Event::Copy)),
            )
        });
        if up {
            browse(n, -1);
        }
        if down {
            browse(n, 1);
        }
        if esc {
            n.ui.input.clear();
            n.ui.hist = None;
        }
        if ctrl_c && n.ui.input.is_empty() && live {
            n.send("\x03");
        }
    }
    let mut submit = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    if submit {
        resp.request_focus();
    }
    let sresp = ui
        .interact(send, ui.scope_id().with("nx-send"), egui::Sense::click())
        .on_hover_tr("Send");
    let fill = if sresp.hovered() && live {
        style::accent_hover()
    } else {
        style::accent()
    };
    painter.circle_filled(send.center(), 17.0, fill.gamma_multiply(if live { 1.0 } else { 0.45 }));
    icons::paint(
        ui,
        "send",
        Rect::from_center_size(send.center(), Vec2::splat(15.0)),
        style::text_inverted(),
    );
    submit |= sresp.clicked();
    if submit && live {
        // Enter on an empty line is sent too: it asks for a prompt, and it is
        // what ends a `top`.
        let text = std::mem::take(&mut n.ui.input);
        n.send(&format!("{text}\n"));
        n.remember_command(&text, h.now);
        n.ui.hist = None;
        n.ui.draft.clear();
    }

    let count = n.lines.count_text(&filter);
    let colour = if n.lines.paused {
        style::warning()
    } else {
        style::text_3()
    };
    let weight = if n.lines.paused { Weight::W600 } else { Weight::W400 };
    let cg = style::galley(ui, &count, 10.0, weight, colour, 0.0);
    let count_w = cg.size().x;
    painter.galley(Pos2::new(footer.left(), footer.top() + 5.0), cg, colour);
    let hint = hint_text(n, "The Terminal view draws programs like top in place.");
    let hg = corvus_api::ui::text::clamped(
        ui,
        &hint,
        10.0,
        Weight::W400,
        style::text_3(),
        0.0,
        footer.width() - count_w - 16.0,
        1,
        None,
    );
    painter.galley(
        Pos2::new(footer.right() - hg.size().x, footer.top() + 5.0),
        hg,
        style::text_3(),
    );
}

/// Walks the history from the command line (`browse`): up from a new line
/// keeps it as the draft, down past the newest brings it back.
fn browse(n: &mut Nuttx, step: i32) {
    let history = n.history().to_vec();
    if history.is_empty() {
        return;
    }
    let next = match n.ui.hist {
        None if step > 0 => return,
        None => {
            n.ui.draft = n.ui.input.clone();
            history.len() - 1
        }
        Some(i) if step > 0 && i + 1 >= history.len() => {
            n.ui.hist = None;
            n.ui.input = std::mem::take(&mut n.ui.draft);
            return;
        }
        Some(i) if step > 0 => i + 1,
        Some(i) => i.saturating_sub(1),
    };
    n.ui.hist = Some(next);
    n.ui.input = history[next].clone();
}

fn paint_stream(n: &mut Nuttx, ui: &mut egui::Ui, r: Rect, filter: &str) {
    card::paint(ui.painter(), r, Look::Console);
    let inner = r.shrink2(Vec2::new(12.0, 8.0));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner));
    let mut area = egui::ScrollArea::vertical()
        .id_salt("nx-stream")
        .stick_to_bottom(true)
        .auto_shrink([false, false]);
    if n.ui.jump {
        area = area.vertical_scroll_offset(f32::INFINITY);
    }
    let lines = &n.lines.lines;
    let out = area.show(&mut child, |ui| {
        let width = inner.width() - 9.0;
        for l in lines
            .iter()
            .filter(|l| filter.is_empty() || l.text.to_lowercase().contains(filter))
        {
            let (msg, rail) = match l.level {
                "success" => (style::healthy(), Some(style::healthy())),
                "warning" => (style::warning(), Some(style::warning())),
                "error" => (style::critical(), Some(style::critical())),
                "cmd" => (style::terminal_cmd(), Some(style::terminal_cmd())),
                "info" => (style::terminal_muted(), None),
                _ => (style::terminal_text(), None),
            };
            let mut job = egui::text::LayoutJob::default();
            let fmt = |c: Color32| egui::TextFormat {
                font_id: style::mono(12.0),
                color: c,
                line_height: Some(12.0 * 1.55),
                ..Default::default()
            };
            job.append(">>", 0.0, fmt(style::terminal_cmd()));
            job.append(" ", 0.0, fmt(msg));
            job.append(&l.text, 0.0, fmt(msg));
            job.wrap.max_width = width;
            job.wrap.break_anywhere = true;
            let g = ui.painter().layout_job(job);
            let (rect, _) = ui.allocate_exact_size(Vec2::new(inner.width(), g.size().y + 2.0), egui::Sense::hover());
            if let Some(c) = rail {
                ui.painter()
                    .vline(rect.left() + 1.0, rect.y_range(), Stroke::new(2.0, c));
            }
            ui.painter()
                .galley(Pos2::new(rect.left() + 9.0, rect.top() + 1.0), g, msg);
        }
    });
    let at_end = n.ui.jump || out.state.clamped_offset().y + out.inner_rect.height() >= out.content_size.y - 1.0;
    let added = n.lines.added;
    n.ui.missed = if at_end {
        0
    } else {
        n.ui.missed + added.saturating_sub(n.ui.seen) as usize
    };
    n.ui.seen = added;
    n.ui.jump = !at_end && jump_chip(ui, r, n.ui.missed);
}

/// The chip that takes the stream back to its live end, with how much has
/// come since. True when pressed.
fn jump_chip(ui: &mut egui::Ui, r: Rect, missed: usize) -> bool {
    let label = match missed {
        0 => tr("Jump to live").into_owned(),
        1 => tr_fmt("{a} new line", &[("a", "1")]),
        n => tr_fmt("{a} new lines", &[("a", &n.to_string())]),
    };
    let g = style::galley(ui, &label, 10.5, Weight::W600, style::text_inverted(), 0.3);
    let size = Vec2::new(10.0 + 12.0 + 5.0 + g.size().x + 10.0, 4.0 + g.size().y.max(12.0) + 4.0);
    let rect = Rect::from_min_size(Pos2::new(r.center().x - size.x / 2.0, r.bottom() - 10.0 - size.y), size);
    let resp = ui
        .interact(rect, ui.scope_id().with("nx-jump"), egui::Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    let fill = if resp.hovered() {
        style::accent_hover()
    } else {
        style::accent()
    };
    let painter = ui.painter();
    painter.add(style::shadow_pop().as_shape(rect, 255u8));
    painter.rect_filled(rect, 255u8, fill);
    icons::paint(
        ui,
        "arrow-down",
        Rect::from_center_size(Pos2::new(rect.left() + 10.0 + 6.0, rect.center().y), Vec2::splat(12.0)),
        style::text_inverted(),
    );
    painter.galley(
        Pos2::new(rect.left() + 10.0 + 12.0 + 5.0, rect.center().y - g.size().y / 2.0),
        g,
        style::text_inverted(),
    );
    resp.clicked()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_line_walks_the_history_and_keeps_the_draft() {
        let mut n = Nuttx::default();
        browse(&mut n, -1);
        assert_eq!(n.ui.input, "", "no history, nothing happens");
        n.remember_command("ver all", 0.0);
        n.remember_command("free", 0.0);
        n.ui.input = "to".into();
        browse(&mut n, 1);
        assert_eq!(n.ui.input, "to", "down on a new line does nothing");
        browse(&mut n, -1);
        assert_eq!(n.ui.input, "free");
        browse(&mut n, -1);
        browse(&mut n, -1);
        assert_eq!(n.ui.input, "ver all", "the oldest is the end");
        browse(&mut n, 1);
        browse(&mut n, 1);
        assert_eq!((n.ui.input.as_str(), n.ui.hist), ("to", None), "the draft comes back");
    }
}
