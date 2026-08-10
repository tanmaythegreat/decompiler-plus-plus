// gui.rs — native viewer for decompiler++.
//
// Left to right: function list, pseudocode or CFG, disassembly. Along the
// bottom: registers, stack, memory, program output.
//
// The views are kept in step by address. Every pseudocode line carries the
// address of the instruction that produced it, so selecting in one view
// selects in all of them, and the stepper drives all three at once.

use fltk::enums::{Align, Color, Event, Font, FrameType, Key, Shortcut};
use fltk::prelude::*;
use fltk::{app, browser, button, dialog, draw, frame, group, input, menu, text, window};
use mini_decompiler::analysis::{self, Analyzed, Program};
use std::collections::HashSet;
use mini_decompiler::cfg::Cfg;
use mini_decompiler::emu::{Emu, SHOWN_REGS};
use mini_decompiler::ir::{Field, StructDef, StructTable, Type};
use mini_decompiler::rename;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

const BG: Color = Color::from_rgb(0x16, 0x18, 0x1d);
const PANEL: Color = Color::from_rgb(0x1c, 0x1f, 0x26);
const PANEL2: Color = Color::from_rgb(0x22, 0x26, 0x2f);
const LINE: Color = Color::from_rgb(0x2e, 0x33, 0x3d);
const FG: Color = Color::from_rgb(0xd7, 0xda, 0xe0);
const DIM: Color = Color::from_rgb(0x7d, 0x85, 0x92);
const ACCENT: Color = Color::from_rgb(0x82, 0xaa, 0xff);
const HOT: Color = Color::from_rgb(0x3a, 0x41, 0x52);
const PCBG: Color = Color::from_rgb(0x2d, 0x3a, 0x2a);
const KEY: Color = Color::from_rgb(0xc7, 0x92, 0xea);
const NUMC: Color = Color::from_rgb(0xf7, 0x8c, 0x6c);
const STRC: Color = Color::from_rgb(0xc3, 0xe8, 0x8d);
const VARC: Color = Color::from_rgb(0xff, 0xcb, 0x6b);
const WARN: Color = Color::from_rgb(0xf0, 0x71, 0x78);
const HEADER: Color = Color::from_rgb(0x2a, 0x2f, 0x3a);
const SHADOW: Color = Color::from_rgb(0x10, 0x12, 0x16);
const EDGE_T: Color = Color::from_rgb(0x7f, 0xbf, 0x8f);
const EDGE_F: Color = Color::from_rgb(0xd0, 0x7f, 0x7f);

struct Ui {
    prog: Option<Program>,
    cur: usize,
    no_cast: bool,
    fn_names: HashMap<String, String>,
    var_names: HashMap<(String, usize), String>,
    var_types: HashMap<(String, usize), String>,
    user_structs: Vec<StructDef>,
    code: Vec<(Option<u64>, String)>,
    asm: Vec<(u64, String)>,
    sel_addr: Option<u64>,
    sel_var: Option<usize>,
    emu: Option<Emu>,
    bottom: Bottom,
    breakpoints: HashSet<u64>,
    graph_pseudo: bool,
    /// blocks the user dragged, offset from the layout origin
    node_pos: HashMap<u64, (i32, i32)>,
    drag: Option<(u64, i32, i32)>,
    xrefs: Vec<(String, u64, String)>,
    xref_target: String,
    string_filter: String,
    /// address the Memory pane is showing, independent of the code selection
    mem_focus: Option<u64>,
    history: Vec<usize>,
    history_idx: usize,
    /// when true, library/FLIRT-matched functions are hidden from the function list
    hide_lib_fns: bool,
    /// address of the function the AI naming pass is currently asking about,
    /// if any — used to highlight that row in the function list while its
    /// request is in flight.
    ai_active_addr: Option<u64>,
    /// function name -> (variable name -> index), snapshotted when an AI
    /// naming pass starts so incoming renames can be applied without
    /// re-borrowing `prog` for every message off the channel.
    ai_var_index: HashMap<String, HashMap<String, usize>>,
}

impl Ui {
    fn new() -> Ui {
        Ui {
            prog: None,
            cur: 0,
            no_cast: false,
            fn_names: HashMap::new(),
            var_names: HashMap::new(),
            var_types: HashMap::new(),
            user_structs: Vec::new(),
            code: Vec::new(),
            asm: Vec::new(),
            sel_addr: None,
            sel_var: None,
            emu: None,
            bottom: Bottom::Registers,
            breakpoints: HashSet::new(),
            graph_pseudo: false,
            node_pos: HashMap::new(),
            drag: None,
            xrefs: Vec::new(),
            xref_target: String::new(),
            string_filter: String::new(),
            mem_focus: None,
            history: Vec::new(),
            history_idx: 0,
            hide_lib_fns: false,
            ai_active_addr: None,
            ai_var_index: HashMap::new(),
        }
    }

    fn func(&self) -> Option<&Analyzed> {
        self.prog.as_ref().and_then(|p| p.funcs.get(self.cur))
    }

    fn fname(&self, raw: &str) -> String {
        self.fn_names.get(raw).cloned().unwrap_or_else(|| raw.to_string())
    }

    fn var_label(&self, f: &Analyzed, i: usize) -> String {
        self.var_names
            .get(&(f.name.clone(), i))
            .cloned()
            .unwrap_or_else(|| f.frame.vars[i].name.clone())
    }

    fn structs(&self) -> StructTable {
        let mut st = self.prog.as_ref().map(|p| p.structs.clone()).unwrap_or_default();
        for d in &self.user_structs {
            if !st.defs.iter().any(|x| x.name == d.name) {
                st.defs.push(d.clone());
            }
        }
        st
    }

    /// Renames are applied to the rendered text rather than baked into the
    /// analysis, so renaming is instant and never re-runs the decompiler.
    fn apply_names(&self, text: &str) -> String {
        let Some(f) = self.func() else { return text.to_string() };
        let mut pairs: Vec<(String, String)> = Vec::new();
        for (i, v) in f.frame.vars.iter().enumerate() {
            if let Some(n) = self.var_names.get(&(f.name.clone(), i)) {
                pairs.push((v.name.clone(), n.clone()));
            }
        }
        for (old, new) in &self.fn_names {
            pairs.push((old.clone(), new.clone()));
        }
        if pairs.is_empty() {
            text.to_string()
        } else {
            replace_identifiers(text, &pairs)
        }
    }

    fn rebuild(&mut self) {
        self.code.clear();
        self.asm.clear();
        let st = self.structs();
        let (lines, raw) = {
            let Some(f) = self.func() else { return };
            let mut lines = analysis::render_lines(f, &st, self.no_cast);
            for (_, textline) in lines.iter_mut() {
                for (i, v) in f.frame.vars.iter().enumerate() {
                    if let Some(ty) = self.var_types.get(&(f.name.clone(), i)) {
                        let old = format!("    {};", v.ty.declare(&v.name, &st));
                        if textline.starts_with(&old) {
                            let sep = if ty.ends_with('*') { "" } else { " " };
                            let rest = &textline[old.len()..];
                            *textline = format!("    {}{}{};{}", ty, sep, v.name, rest);
                        }
                    }
                }
            }
            let raw: Vec<(u64, String)> =
                f.raw.iter().map(|i| (i.addr, i.asm_text.clone())).collect();
            (lines, raw)
        };
        self.code = lines.into_iter().map(|(a, t)| (a, self.apply_names(&t))).collect();
        self.asm = raw;
    }

    /// The variable a line refers to, for "click the line, press F2".
    fn var_on_line(&self, idx: usize) -> Option<usize> {
        let f = self.func()?;
        let text = &self.code.get(idx)?.1;
        (0..f.frame.vars.len()).find(|&i| mentions(text, &self.var_label(f, i)))
    }
}

fn mentions(text: &str, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let (b, n) = (text.as_bytes(), name.as_bytes());
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    (0..=b.len().saturating_sub(n.len())).any(|i| {
        &b[i..i + n.len()] == n
            && (i == 0 || !ident(b[i - 1]))
            && (i + n.len() == b.len() || !ident(b[i + n.len()]))
    })
}

fn replace_identifiers(text: &str, pairs: &[(String, String)]) -> String {
    let mut out = String::with_capacity(text.len());
    let b = text.as_bytes();
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut i = 0;
    'outer: while i < b.len() {
        if ident(b[i]) && (i == 0 || !ident(b[i - 1])) {
            let mut j = i;
            while j < b.len() && ident(b[j]) {
                j += 1;
            }
            let word = &text[i..j];
            for (old, new) in pairs {
                if word == old {
                    out.push_str(new);
                    i = j;
                    continue 'outer;
                }
            }
            out.push_str(word);
            i = j;
            continue;
        }
        out.push(b[i] as char);
        i += 1;
    }
    out
}

const KEYWORDS: [&str; 17] = [
    "if", "else", "while", "do", "for", "return", "goto", "break", "continue", "struct",
    "unsigned", "signed", "void", "char", "short", "int", "long",
];

/// FLTK colours text through a parallel buffer of style letters, one per
/// character.
fn style_line(text: &str, vars: &[String], fns: &[String]) -> String {
    let b = text.as_bytes();
    let mut out = vec![b'A'; b.len()];
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'"' {
            let start = i;
            i += 1;
            while i < b.len() && b[i] != b'"' {
                if b[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            i = (i + 1).min(b.len());
            out[start..i].fill(b'D');
            continue;
        }
        if b[i] == b'/' && i + 1 < b.len() && (b[i + 1] == b'*' || b[i + 1] == b'/') {
            out[i..].fill(b'G');
            break;
        }
        if b[i].is_ascii_digit() && (i == 0 || !ident(b[i - 1])) {
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'x') {
                i += 1;
            }
            out[start..i].fill(b'C');
            continue;
        }
        if ident(b[i]) && (i == 0 || !ident(b[i - 1])) {
            let start = i;
            while i < b.len() && ident(b[i]) {
                i += 1;
            }
            let w = &text[start..i];
            let s = if KEYWORDS.contains(&w) {
                b'B'
            } else if vars.iter().any(|v| v == w) {
                b'F'
            } else if fns.iter().any(|f| f == w) {
                b'E'
            } else {
                b'A'
            };
            out[start..i].fill(s);
            continue;
        }
        i += 1;
    }
    String::from_utf8(out).unwrap_or_default()
}

fn code_styles() -> Vec<text::StyleTableEntry> {
    let mk = |c: Color, f: Font| text::StyleTableEntry { color: c, font: f, size: 13 };
    vec![
        mk(FG, Font::Courier),
        mk(KEY, Font::CourierBold),
        mk(NUMC, Font::Courier),
        mk(STRC, Font::Courier),
        mk(ACCENT, Font::Courier),
        mk(VARC, Font::Courier),
        mk(DIM, Font::CourierItalic),
        mk(WARN, Font::CourierBold),
    ]
}

// ---------------------------------------------------------------- state ----
#[derive(Clone, Copy, PartialEq, Debug)]
enum Bottom {
    Registers,
    Stack,
    Memory,
    Vmmap,
    Strings,
    Xrefs,
    Output,
}

fn main() {
    let app = app::App::default().with_scheme(app::Scheme::Gtk);
    app::background(0x14, 0x16, 0x1a);
    app::background2(0x1b, 0x1e, 0x25);
    app::foreground(0xd7, 0xda, 0xe0);
    app::set_font(Font::Courier);
    app::set_font_size(13);
    app::set_frame_type(FrameType::FlatBox);
    app::set_menu_linespacing(6);

    let ui = Rc::new(RefCell::new(Ui::new()));
    // Delivers progress from the background AI-renaming thread (see
    // `run_ai_renaming`) back to this thread's event loop, below.
    let (ai_tx, ai_rx) = app::channel::<AiEvent>();

    let mut win = window::Window::default().with_size(1620, 1000).with_label("decompiler++");
    win.set_color(BG);

    let mut menubar = menu::MenuBar::new(0, 0, 1200, 26, None);
    let mut btn_start = button::Button::new(1200, 0, 80, 26, "▶ Start");
    btn_start.set_color(PANEL2);
    btn_start.set_label_color(FG);
    btn_start.set_frame(FrameType::FlatBox);
    let mut btn_step = button::Button::new(1280, 0, 80, 26, "Step");
    btn_step.set_color(PANEL2);
    btn_step.set_label_color(FG);
    btn_step.set_frame(FrameType::FlatBox);
    let mut btn_over = button::Button::new(1360, 0, 100, 26, "Step Over");
    btn_over.set_color(PANEL2);
    btn_over.set_label_color(FG);
    btn_over.set_frame(FrameType::FlatBox);
    let mut btn_cont = button::Button::new(1460, 0, 100, 26, "Continue");
    btn_cont.set_color(PANEL2);
    btn_cont.set_label_color(FG);
    btn_cont.set_frame(FrameType::FlatBox);
    menubar.set_color(PANEL);
    menubar.set_text_color(FG);
    menubar.set_selection_color(ACCENT);
    menubar.set_frame(FrameType::FlatBox);
    menubar.set_text_size(12);

    // ---- panes ------------------------------------------------------------
    let body = group::Tile::new(0, 26, 1620, 664, None);

    let left = pane(0, 26, 240, 664, "Functions");
    let mut filter = input::Input::new(4, 46, 232, 22, None);
    filter.set_tooltip("filter functions");
    filter.set_color(PANEL2);
    filter.set_text_color(FG);
    filter.set_text_size(12);
    filter.set_frame(FrameType::FlatBox);
    let mut hide_lib_cb = button::CheckButton::new(4, 70, 232, 20, "Hide library functions");
    hide_lib_cb.set_value(false);
    hide_lib_cb.set_label_color(FG);
    hide_lib_cb.set_label_size(11);
    hide_lib_cb.set_color(PANEL);
    hide_lib_cb.set_selection_color(ACCENT);
    hide_lib_cb.set_frame(FrameType::FlatBox);
    let mut fn_list = browser::HoldBrowser::new(2, 96, 236, 590, None);
    fn_list.set_color(PANEL);
    fn_list.set_selection_color(HOT);
    fn_list.set_text_size(12);
    fn_list.set_frame(FrameType::FlatBox);
    left.end();

    let centre = pane(240, 26, 780, 664, "");
    let mut t_code = tab_button(244, 28, 104, "Pseudocode");
    let mut t_graph = tab_button(350, 28, 74, "Graph");
    let mut g_mode = tab_button(426, 28, 132, "Graph: assembly");
    g_mode.hide();
    let mut code_view = text::TextEditor::new(242, 56, 776, 632, None);
    let code_buf = text::TextBuffer::default();
    let code_style = text::TextBuffer::default();
    code_view.set_buffer(code_buf.clone());
    code_view.set_highlight_data(code_style.clone(), code_styles());
    style_display(&mut code_view);

    let mut graph_scroll = group::Scroll::new(242, 56, 776, 632, None);
    graph_scroll.set_color(PANEL);
    graph_scroll.set_frame(FrameType::FlatBox);
    let mut graph = frame::Frame::new(242, 56, 4000, 4000, None);
    graph_scroll.end();
    graph_scroll.hide();
    centre.end();

    let right = pane(1020, 26, 600, 664, "Disassembly");
    let mut asm_view = text::TextEditor::new(1022, 48, 596, 640, None);
    let asm_buf = text::TextBuffer::default();
    let asm_style = text::TextBuffer::default();
    asm_view.set_buffer(asm_buf.clone());
    asm_view.set_highlight_data(asm_style.clone(), code_styles());
    style_display(&mut asm_view);
    right.end();
    body.end();

    // ---- bottom -----------------------------------------------------------
    let mut lower = group::Group::new(0, 690, 1620, 288, None);
    lower.set_color(PANEL);
    lower.set_frame(FrameType::FlatBox);
    let names = [
        ("Registers", Bottom::Registers),
        ("Stack", Bottom::Stack),
        ("Memory", Bottom::Memory),
        ("Map", Bottom::Vmmap),
        ("Strings", Bottom::Strings),
        ("Xrefs", Bottom::Xrefs),
        ("Output", Bottom::Output),
    ];
    let mut btabs: Vec<(button::Button, Bottom)> = Vec::new();
    let mut x = 6;
    for (label, kind) in names {
        let w = label.len() as i32 * 8 + 22;
        btabs.push((tab_button(x, 694, w, label), kind));
        x += w + 2;
    }
    let mut detach = tab_button(x + 12, 694, 90, "Detach");

    let mut lower_view = text::TextEditor::new(2, 720, 1616, 226, None);
    let lower_buf = text::TextBuffer::default();
    let lower_style = text::TextBuffer::default();
    lower_view.set_buffer(lower_buf.clone());
    lower_view.set_highlight_data(lower_style.clone(), code_styles());
    style_display(&mut lower_view);
    lower_view.handle(|_, ev| is_readonly_event(ev));

    let mut entry = input::Input::new(2, 950, 1616, 24, None);
    entry.set_color(PANEL2);
    entry.set_text_color(STRC);
    entry.set_text_size(12);
    entry.set_frame(FrameType::FlatBox);
    lower.end();

    let mut status = frame::Frame::new(0, 978, 1620, 22, None);
    status.set_label_color(DIM);
    status.set_label_size(11);
    status.set_align(Align::Inside | Align::Left);
    status.set_frame(FrameType::FlatBox);
    status.set_color(PANEL2);

    win.end();
    win.make_resizable(true);
    win.show();

    // ---------------------------------------------------------------- paint --
    let redraw: Rc<dyn Fn()> = {
        let ui = ui.clone();
        let bufs = (
            code_buf.clone(),
            code_style.clone(),
            asm_buf.clone(),
            asm_style.clone(),
            lower_buf.clone(),
            lower_style.clone(),
        );
        let views = (code_view.clone(), asm_view.clone(), lower_view.clone());
        let graph = graph.clone();
        let status = status.clone();
        let entry = entry.clone();
        let btabs: Vec<(button::Button, Bottom)> = btabs.clone();
        Rc::new(move || {
            let (mut code_buf, mut code_style, mut asm_buf, mut asm_style, mut low_buf, mut low_style) =
                (bufs.0.clone(), bufs.1.clone(), bufs.2.clone(), bufs.3.clone(), bufs.4.clone(), bufs.5.clone());
            let (mut code_view, mut asm_view, mut low_view) =
                (views.0.clone(), views.1.clone(), views.2.clone());
            let mut graph = graph.clone();
            let mut status = status.clone();
            let mut entry = entry.clone();

            let u = ui.borrow();
            for (b, kind) in &btabs {
                let mut b = b.clone();
                b.set_color(if *kind == u.bottom { HOT } else { PANEL2 });
            }
            entry.set_readonly(u.bottom != Bottom::Output);

            let Some(p) = &u.prog else {
                status.set_label("  no file loaded — File ▸ Open");
                return;
            };
            let Some(f) = u.func() else { return };

            let vars: Vec<String> = (0..f.frame.vars.len()).map(|i| u.var_label(f, i)).collect();
            let fns: Vec<String> = p.funcs.iter().map(|g| u.fname(&g.name)).collect();

            let pc = u.emu.as_ref().and_then(|e| e.pc);

            // pseudocode
            let (mut txt, mut sty) = (String::new(), String::new());
            for (addr, line) in &u.code {
                let bp = addr.map_or(false, |a| u.breakpoints.contains(&a));
                let is_pc = pc.is_some() && addr == &pc;
                let gutter = match addr {
                    Some(a) => format!("{} {:08x}  ", if is_pc { ">" } else if bp { "*" } else { " " }, a),
                    None => " ".repeat(12),
                };
                let style_char = if is_pc { "C" } else if bp { "H" } else { "G" };
                sty.push_str(&style_char.repeat(gutter.len()));
                sty.push_str(&style_line(line, &vars, &fns));
                sty.push('\n');
                txt.push_str(&gutter);
                txt.push_str(line);
                txt.push('\n');
            }
            code_buf.set_text(&txt);
            code_style.set_text(&sty);

            // disassembly, with breakpoint marks and jump arrows
            let arrows = jump_arrows(&u.asm);
            let (mut atxt, mut asty) = (String::new(), String::new());
            for (i, (a, t)) in u.asm.iter().enumerate() {
                let bp = u.breakpoints.contains(a);
                let is_pc = pc == Some(*a);
                let head = format!("{} {:08x} ", if is_pc { ">" } else if bp { "*" } else { " " }, a);
                let arrow = arrows.get(i).cloned().unwrap_or_else(|| "    ".into());
                let style_char = if is_pc { "C" } else if bp { "H" } else { "G" };
                asty.push_str(&style_char.repeat(head.len()));
                asty.push_str(&"E".repeat(arrow.chars().count()));
                asty.push_str(&style_asm(t));
                asty.push('\n');
                atxt.push_str(&head);
                atxt.push_str(&arrow);
                atxt.push_str(t);
                atxt.push('\n');
            }
            asm_buf.set_text(&atxt);
            asm_style.set_text(&asty);

            let focus = pc.or(u.sel_addr);
            scroll_to(&mut code_view, &u.code.iter().map(|(a, _)| *a).collect::<Vec<_>>(), focus);
            scroll_to(&mut asm_view, &u.asm.iter().map(|(a, _)| Some(*a)).collect::<Vec<_>>(), focus);

            // bottom panel
            let (lt, ls) = u.bottom_text(p, f);
            low_buf.set_text(&lt);
            low_style.set_text(&ls);
            if u.bottom == Bottom::Output {
                low_view.set_insert_position(low_buf.length());
                low_view.show_insert_position();
            }

            let st = match &u.emu {
                Some(e) if e.halted => format!("  stopped — {}   {} steps", e.reason, e.steps),
                Some(e) if e.hit_breakpoint.is_some() => format!(
                    "  breakpoint at {:#x}   {} steps",
                    e.hit_breakpoint.unwrap(),
                    e.steps
                ),
                Some(e) if e.started() => format!("  running   pc {:#x}   {} steps", e.pc.unwrap_or(0), e.steps),
                _ => format!(
                    "  {}   {} functions   {} strings   F2 rename · F3 type · F4 breakpoint · Shift+F12 xrefs",
                    short(&p.path),
                    p.funcs.len(),
                    p.strings.len()
                ),
            };
            status.set_label(&st);
            graph.redraw();
        })
    };

    // ---------------------------------------------------------------- graph --
    {
        let ui = ui.clone();
        graph.draw(move |w| {
            draw::set_draw_color(PANEL);
            draw::draw_rectf(w.x(), w.y(), w.w(), w.h());
            let u = ui.borrow();
            let Some(f) = u.func() else { return };
            let boxes = u.graph_boxes(f, w.x() + 24, w.y() + 24);
            let pc = u.emu.as_ref().and_then(|e| e.pc);

            for b in &boxes {
                for (i, s) in b.succs.iter().enumerate() {
                    let Some(t) = boxes.iter().find(|x| x.addr == *s) else { continue };
                    draw::set_draw_color(if b.succs.len() == 2 {
                        if i == 0 { EDGE_T } else { EDGE_F }
                    } else {
                        DIM
                    });
                    draw::set_line_style(draw::LineStyle::Solid, 2);
                    let (x1, y1) = (b.x + b.w / 2, b.y + b.h);
                    let (x2, y2) = (t.x + t.w / 2, t.y);
                    let mid = (y1 + y2) / 2;
                    draw::draw_line(x1, y1, x1, mid);
                    draw::draw_line(x1, mid, x2, mid);
                    draw::draw_line(x2, mid, x2, y2 - 6);
                    draw::draw_polygon(x2 - 4, y2 - 7, x2 + 4, y2 - 7, x2, y2);
                    draw::set_line_style(draw::LineStyle::Solid, 1);
                }
            }
            for b in &boxes {
                let hot = u.sel_addr.map_or(false, |a| b.insns.contains(&a));
                let is_pc = pc.map_or(false, |a| b.insns.contains(&a));
                draw::set_draw_color(SHADOW);
                draw::draw_rectf(b.x + 3, b.y + 3, b.w, b.h);
                draw::set_draw_color(if is_pc { PCBG } else { PANEL2 });
                draw::draw_rectf(b.x, b.y, b.w, b.h);
                draw::set_draw_color(if is_pc { STRC } else if hot { ACCENT } else { LINE });
                draw::draw_rect(b.x, b.y, b.w, b.h);
                draw::set_draw_color(HEADER);
                draw::draw_rectf(b.x + 1, b.y + 1, b.w - 2, 18);
                draw::set_font(Font::CourierBold, 11);
                draw::set_draw_color(ACCENT);
                draw::draw_text(&format!("{:#x}", b.addr), b.x + 8, b.y + 14);
                draw::set_font(Font::Courier, 11);
                for (i, t) in b.text.iter().enumerate() {
                    draw::set_draw_color(if u.graph_pseudo { FG } else { DIM });
                    draw::draw_text(t, b.x + 8, b.y + 32 + 13 * i as i32);
                }
            }
        });
    }
    {
        let ui = ui.clone();
        let redraw = redraw.clone();
        let mut scroll = graph_scroll.clone();
        graph.handle(move |w, ev| match ev {
            Event::Push => {
                let (mx, my) = app::event_coords();
                let hit = {
                    let u = ui.borrow();
                    u.func().and_then(|f| {
                        u.graph_boxes(f, w.x() + 24, w.y() + 24)
                            .into_iter()
                            .find(|b| mx >= b.x && mx <= b.x + b.w && my >= b.y && my <= b.y + b.h)
                    })
                };
                if let Some(b) = hit {
                    let mut u = ui.borrow_mut();
                    u.sel_addr = b.insns.first().copied();
                    u.drag = Some((b.addr, mx - b.x, my - b.y));
                    drop(u);
                    redraw();
                    return true;
                }
                false
            }
            Event::Drag => {
                let (mx, my) = app::event_coords();
                let mut u = ui.borrow_mut();
                if let Some((addr, dx, dy)) = u.drag {
                    let base = (w.x() + 24, w.y() + 24);
                    u.node_pos.insert(addr, (mx - dx - base.0, my - dy - base.1));
                    drop(u);
                    scroll.redraw();
                    return true;
                }
                false
            }
            Event::Released => {
                ui.borrow_mut().drag = None;
                true
            }
            _ => false,
        });
    }

    // ------------------------------------------------------------- actions --
    let reload: Rc<dyn Fn()> = {
        let ui = ui.clone();
        let fn_list = fn_list.clone();
        let filter = filter.clone();
        let hide_lib_cb = hide_lib_cb.clone();
        let redraw = redraw.clone();
        Rc::new(move || {
            let mut fn_list = fn_list.clone();
            {
                let mut u = ui.borrow_mut();
                u.rebuild();
                // Removed automatic emulator start on file load
            }
            fn_list.clear();
            {
                let u = ui.borrow();
                let pat = filter.value().to_lowercase();
                let hide_lib = hide_lib_cb.value();
                if let Some(p) = &u.prog {
                    for (i, f) in p.funcs.iter().enumerate() {
                        let name = u.fname(&f.name);
                        // Use the authoritative is_lib flag set by FLIRT at analysis time.
                        // This is correct even after the user renames the function.
                        if hide_lib && f.is_lib && i != u.cur {
                            continue;
                        }
                        if !pat.is_empty() && !name.to_lowercase().contains(&pat) {
                            continue;
                        }
                        if name == "_start" {
                            fn_list.add(&format!("@B@C4@{}\t{:x}", name, f.addr));
                        } else if u.ai_active_addr == Some(f.addr) {
                            // Currently being asked about by the AI naming
                            // pass — italic + accent-colored so it's easy
                            // to spot scrolling past as the pass works
                            // through the list.
                            fn_list.add(&format!("@i@C4@» {}\t{:x}", name, f.addr));
                        } else {
                            fn_list.add(&format!("{}\t{:x}", name, f.addr));
                        }
                        if i == u.cur {
                            fn_list.select(fn_list.size());
                        }
                        if u.ai_active_addr == Some(f.addr) {
                            fn_list.middle_line(fn_list.size());
                        }
                    }
                }
            }
            redraw();
        })
    };

    let open_file: Rc<dyn Fn(Option<String>)> = {
        let ui = ui.clone();
        let reload = reload.clone();
        let ai_tx = ai_tx.clone();
        Rc::new(move |path: Option<String>| {
            let path = match path {
                Some(p) => p,
                None => {
                    let mut c =
                        dialog::NativeFileChooser::new(dialog::NativeFileChooserType::BrowseFile);
                    c.show();
                    let f = c.filename();
                    if f.as_os_str().is_empty() {
                        return;
                    }
                    f.to_string_lossy().to_string()
                }
            };

            let bytes = match std::fs::read(&path) {
                Ok(b) => b,
                Err(e) => {
                    dialog::alert_default(&format!("failed to read {}: {}", path, e));
                    return;
                }
            };

            // Stripped or statically-linked binaries are exactly the case
            // where a plain symbol table won't have named much of anything.
            // Ask whether it's worth the extra time on FLIRT and/or AI
            // naming before running the (slower) full analysis.
            let (use_flirt, use_ai) = match analysis::probe_binary(&path, &bytes) {
                Ok(profile) if profile.stripped || profile.static_linked => {
                    ask_rename_options(&path, &profile)
                }
                _ => (false, false),
            };

            let custom_sigs = if use_flirt {
                match mini_decompiler::flirt::auto_generate_libc_signatures() {
                    Ok(sigs) => Some(sigs),
                    Err(e) => {
                        dialog::alert_default(&format!(
                            "FLIRT auto-signature generation failed: {}\n\nContinuing without it.",
                            e
                        ));
                        None
                    }
                }
            } else {
                None
            };

            let prog = match analysis::analyze_bytes(&path, &bytes, None, custom_sigs.as_deref()) {
                Ok(p) => p,
                Err(e) => {
                    dialog::alert_default(&format!("{}\n\nOnly ELF objects are supported.", e));
                    return;
                }
            };

            {
                let mut u = ui.borrow_mut();
                u.cur = prog.funcs.iter().position(|f| f.name == "main").unwrap_or(0);
                u.prog = Some(prog);
                u.sel_addr = None;
                u.sel_var = None;
                u.emu = None;
                u.node_pos.clear();
                u.ai_active_addr = None;
            }

            // Show the function list — with its plain deterministic names —
            // right away, instead of leaving the GUI blank while AI naming
            // runs. AI renaming (if requested) is kicked off after, and
            // streams names in live as they arrive; see `run_ai_renaming`.
            reload();

            if use_ai {
                run_ai_renaming(&ui, ai_tx.clone());
            }
        })
    };

    let restart: Rc<dyn Fn()> = {
        let ui = ui.clone();
        let redraw = redraw.clone();
        Rc::new(move || {
            {
                let mut u = ui.borrow_mut();
                let bps = u.breakpoints.clone();
                let stdin = u.emu.as_ref().map(|e| e.stdin.clone()).unwrap_or_default();
                let cur = u.cur;
                if let Some(p) = &u.prog {
                    let mut e = Emu::new(p, cur);
                    e.breakpoints = bps.into_iter().collect();
                    e.stdin = stdin;
                    u.emu = Some(e);
                }
                u.sel_addr = u.emu.as_ref().and_then(|e| e.pc);
                u.bottom = Bottom::Registers;
            }
            redraw();
        })
    };

    let step: Rc<dyn Fn()> = {
        let ui = ui.clone();
        let redraw = redraw.clone();
        Rc::new(move || {
            {
                let mut u = ui.borrow_mut();
                let Ui { prog: Some(p), emu: Some(e), .. } = &mut *u else { return };
                e.exec(p);
                let pc = e.pc;
                u.sel_addr = pc;
            }
            redraw();
        })
    };

    let cont: Rc<dyn Fn()> = {
        let ui = ui.clone();
        let redraw = redraw.clone();
        Rc::new(move || {
            {
                let mut u = ui.borrow_mut();
                let bps = u.breakpoints.clone();
                let Ui { prog: Some(p), emu: Some(e), .. } = &mut *u else { return };
                e.breakpoints = bps.into_iter().collect();
                e.run(p, 20_000_000);
                let pc = e.pc;
                u.sel_addr = pc;
                if u.emu.as_ref().map_or(false, |e| !e.out.is_empty()) {
                    u.bottom = Bottom::Output;
                }
            }
            redraw();
        })
    };

    let rename: Rc<dyn Fn()> = {
        let ui = ui.clone();
        let reload = reload.clone();
        Rc::new(move || {
            let (title, current, is_fn, id, key) = {
                let u = ui.borrow();
                let Some(f) = u.func() else { return };
                match u.sel_var {
                    Some(i) => ("Rename variable", u.var_label(f, i), false, i, f.name.clone()),
                    None => ("Rename function", u.fname(&f.name), true, 0, f.name.clone()),
                }
            };
            let Some(new) = dialog::input_default(title, &current) else { return };
            let new = new.trim().to_string();
            if !valid_identifier(&new) {
                dialog::alert_default("not a valid C identifier");
                return;
            }
            {
                let mut u = ui.borrow_mut();
                if is_fn {
                    u.fn_names.insert(key, new);
                } else {
                    u.var_names.insert((key, id), new);
                }
            }
            reload();
        })
    };

    let retype: Rc<dyn Fn()> = {
        let ui = ui.clone();
        let reload = reload.clone();
        Rc::new(move || {
            let (current, id, key) = {
                let u = ui.borrow();
                let Some(f) = u.func() else { return };
                let Some(i) = u.sel_var else {
                    dialog::alert_default("select a line that mentions a variable first");
                    return;
                };
                let st = u.structs();
                let cur = u.var_types.get(&(f.name.clone(), i)).cloned().unwrap_or_else(|| {
                    f.frame.vars[i].ty.declare("", &st).trim().to_string()
                });
                (cur, i, f.name.clone())
            };
            let Some(new) = dialog::input_default("Type", &current) else { return };
            let new = new.trim().to_string();
            if new.is_empty() {
                return;
            }
            ui.borrow_mut().var_types.insert((key, id), new);
            reload();
        })
    };

    let toggle_bp: Rc<dyn Fn()> = {
        let ui = ui.clone();
        let redraw = redraw.clone();
        Rc::new(move || {
            {
                let mut u = ui.borrow_mut();
                let Some(a) = u.sel_addr else { return };
                if !u.breakpoints.remove(&a) {
                    u.breakpoints.insert(a);
                }
                let bps = u.breakpoints.clone();
                if let Some(e) = &mut u.emu {
                    e.breakpoints = bps.into_iter().collect();
                }
            }
            redraw();
        })
    };

    let xrefs: Rc<dyn Fn()> = {
        let ui = ui.clone();
        let redraw = redraw.clone();
        Rc::new(move || {
            {
                let mut u = ui.borrow_mut();
                let Some(p) = &u.prog else { return };
                let (target, name) = match u.sel_addr {
                    Some(a) => {
                        let f = p.funcs.iter().find(|f| f.raw.iter().any(|i| i.addr == a));
                        // a call site refers to its target, so prefer that
                        let called = f
                            .and_then(|f| f.raw.iter().find(|i| i.addr == a))
                            .and_then(|i| i.targets.first().copied())
                            .filter(|t| p.funcs.iter().any(|g| g.addr == *t));
                        match called {
                            Some(t) => (
                                t,
                                p.funcs.iter().find(|g| g.addr == t).map(|g| g.name.clone()).unwrap_or_default(),
                            ),
                            None => (u.func().map(|f| f.addr).unwrap_or(0), u.func().map(|f| f.name.clone()).unwrap_or_default()),
                        }
                    }
                    None => (
                        u.func().map(|f| f.addr).unwrap_or(0),
                        u.func().map(|f| f.name.clone()).unwrap_or_default(),
                    ),
                };
                u.xrefs = analysis::references_to(p, target, &name);
                u.xref_target = format!("{} ({:#x})", name, target);
                u.bottom = Bottom::Xrefs;
            }
            redraw();
        })
    };

    let find_string: Rc<dyn Fn()> = {
        let ui = ui.clone();
        let redraw = redraw.clone();
        Rc::new(move || {
            let Some(q) = dialog::input_default("Search strings for", "") else { return };
            {
                let mut u = ui.borrow_mut();
                u.string_filter = q.trim().to_lowercase();
                u.bottom = Bottom::Strings;
            }
            redraw();
        })
    };

    // ------------------------------------------------------------- menu bar --
    {
        let o = open_file.clone();
        menubar.add("&File/&Open binary…\t", Shortcut::Ctrl | 'o', menu::MenuFlag::Normal, move |_| o(None));
    }
    {
        let ui = ui.clone();
        let reload = reload.clone();
        menubar.add(
            "&File/Load &FLIRT Signature…\t",
            Shortcut::None,
            menu::MenuFlag::Normal,
            move |_| {
                let mut c = dialog::NativeFileChooser::new(dialog::NativeFileChooserType::BrowseFile);
                c.set_filter("FLIRT Signatures\t*.{sig,pat}");
                c.show();
                let f = c.filename();
                if f.as_os_str().is_empty() {
                    return;
                }
                let sig_path = f.to_string_lossy().to_string();
                let u = ui.borrow_mut();
                if let Some(path) = u.prog.as_ref().map(|p| p.path.clone()) {
                    drop(u);
                    match analysis::analyze_file(&path, Some(&sig_path), None) {
                        Ok(p) => {
                            let mut u = ui.borrow_mut();
                            u.prog = Some(p);
                            u.emu = None;
                            drop(u);
                            reload();
                        }
                        Err(e) => dialog::alert_default(&format!("Failed: {}", e)),
                    }
                } else {
                    dialog::alert_default("Please open a binary first.");
                }
            },
        );
    }
    {
        let ui = ui.clone();
        let reload = reload.clone();
        menubar.add(
            "&File/Auto-Generate &libc Signatures\t",
            Shortcut::None,
            menu::MenuFlag::Normal,
            move |_| {
                let u = ui.borrow_mut();
                if let Some(path) = u.prog.as_ref().map(|p| p.path.clone()) {
                    drop(u);
                    match mini_decompiler::flirt::auto_generate_libc_signatures() {
                        Ok(sigs) => {
                            match analysis::analyze_file(&path, None, Some(&sigs)) {
                                Ok(p) => {
                                    let mut u = ui.borrow_mut();
                                    u.prog = Some(p);
                                    u.emu = None;
                                    drop(u);
                                    reload();
                                }
                                Err(e) => dialog::alert_default(&format!("Failed to analyze: {}", e)),
                            }
                        }
                        Err(e) => dialog::alert_default(&format!("Auto-generate failed: {}", e)),
                    }
                } else {
                    dialog::alert_default("Please open a binary first.");
                }
            },
        );
    }
    {
        let ui = ui.clone();
        let ai_tx = ai_tx.clone();
        menubar.add(
            "&File/&AI-Assisted Renaming…\t",
            Shortcut::None,
            menu::MenuFlag::Normal,
            move |_| {
                if ui.borrow().prog.is_none() {
                    dialog::alert_default("Please open a binary first.");
                    return;
                }
                run_ai_renaming(&ui, ai_tx.clone());
            },
        );
    }
    menubar.add(
        "&File/&AI Settings…\t",
        Shortcut::None,
        menu::MenuFlag::Normal,
        move |_| show_api_key_dialog(),
    );
    menubar.add("&File/&Quit\t", Shortcut::Ctrl | 'q', menu::MenuFlag::Normal, |_| app::quit());
    {
        let ui = ui.clone();
        let redraw = redraw.clone();
        menubar.add("&View/&Casts\t", Shortcut::Ctrl | 't', menu::MenuFlag::Toggle, move |_| {
            {
                let mut u = ui.borrow_mut();
                u.no_cast = !u.no_cast;
                u.rebuild();
            }
            redraw();
        });
    }
    {
        let ui = ui.clone();
        let reload = reload.clone();
        menubar.add("&View/&Back\t", Shortcut::Alt | '[', menu::MenuFlag::Normal, move |_| {
            let mut u = ui.borrow_mut();
            if u.history_idx > 0 {
                if u.history_idx == u.history.len() {
                    let cur = u.cur;
                    u.history.push(cur);
                }
                u.history_idx -= 1;
                u.cur = u.history[u.history_idx];
                u.sel_addr = None;
                u.sel_var = None;
                drop(u);
                reload();
            }
        });
    }
    {
        let ui = ui.clone();
        let reload = reload.clone();
        menubar.add("&View/&Forward\t", Shortcut::Alt | ']', menu::MenuFlag::Normal, move |_| {
            let mut u = ui.borrow_mut();
            if u.history_idx + 1 < u.history.len() {
                u.history_idx += 1;
                u.cur = u.history[u.history_idx];
                u.sel_addr = None;
                u.sel_var = None;
                drop(u);
                reload();
            }
        });
    }
    for (label, kind) in [
        ("&View/Registers\t", Bottom::Registers),
        ("&View/Stack\t", Bottom::Stack),
        ("&View/Memory map\t", Bottom::Vmmap),
        ("&View/Strings\t", Bottom::Strings),
        ("&View/Output\t", Bottom::Output),
    ] {
        let ui = ui.clone();
        let redraw = redraw.clone();
        menubar.add(label, Shortcut::None, menu::MenuFlag::Normal, move |_| {
            ui.borrow_mut().bottom = kind;
            redraw();
        });
    }
    {
        let f = find_string.clone();
        menubar.add("&Search/&Strings…\t", Shortcut::Ctrl | 'f', menu::MenuFlag::Normal, move |_| f());
    }
    {
        let x = xrefs.clone();
        menubar.add(
            "&Search/&References\t",
            Shortcut::Shift | Key::F12,
            menu::MenuFlag::Normal,
            move |_| x(),
        );
    }
    {
        let r = rename.clone();
        menubar.add("&Edit/&Rename\t", Shortcut::from_key(Key::F2), menu::MenuFlag::Normal, move |_| r());
    }
    {
        let t = retype.clone();
        menubar.add("&Edit/Change &type\t", Shortcut::from_key(Key::F3), menu::MenuFlag::Normal, move |_| t());
    }
    {
        let ui = ui.clone();
        let reload = reload.clone();
        menubar.add("&Edit/&Structures…\t", Shortcut::None, menu::MenuFlag::Normal, move |_| {
            define_struct(&ui);
            reload();
        });
    }
    {
        let r = restart.clone();
        menubar.add("&Debug/&Start\t", Shortcut::from_key(Key::F5), menu::MenuFlag::Normal, move |_| r());
    }
    {
        let s = step.clone();
        menubar.add("&Debug/Ste&p\t", Shortcut::from_key(Key::F7), menu::MenuFlag::Normal, move |_| s());
    }
    {
        let ui = ui.clone();
        let redraw = redraw.clone();
        menubar.add(
            "&Debug/Step &over\t",
            Shortcut::from_key(Key::F8),
            menu::MenuFlag::Normal,
            move |_| {
                {
                    let mut u = ui.borrow_mut();
                    let Ui { prog: Some(p), emu: Some(e), .. } = &mut *u else { return };
                    e.step_over(p);
                    let pc = e.pc;
                    u.sel_addr = pc;
                }
                redraw();
            },
        );
    }
    {
        let c = cont.clone();
        menubar.add("&Debug/&Continue\t", Shortcut::from_key(Key::F9), menu::MenuFlag::Normal, move |_| c());
    }
    {
        let b = toggle_bp.clone();
        menubar.add(
            "&Debug/Toggle &breakpoint\t",
            Shortcut::from_key(Key::F4),
            menu::MenuFlag::Normal,
            move |_| b(),
        );
    }
    menubar.add("&Help/&Keys\t", Shortcut::None, menu::MenuFlag::Normal, |_| {
        dialog::message_default(
            "F2 rename    F3 change type    F4 breakpoint\n\
             F5 start     F7 step           F9 continue\n\
             Ctrl+F strings    Shift+F12 references    Ctrl+T casts\n\n\
             Right-click the code for the same actions.\n\
             Drag graph blocks to rearrange them.",
        );
    });

    {
        let r = restart.clone();
        btn_start.set_callback(move |_| r());
    }
    {
        let s = step.clone();
        btn_step.set_callback(move |_| s());
    }
    {
        let ui = ui.clone();
        let redraw = redraw.clone();
        btn_over.set_callback(move |_| {
            {
                let mut u = ui.borrow_mut();
                let Ui { prog: Some(p), emu: Some(e), .. } = &mut *u else { return };
                e.step_over(p);
                let pc = e.pc;
                u.sel_addr = pc;
            }
            redraw();
        });
    }
    {
        let c = cont.clone();
        btn_cont.set_callback(move |_| c());
    }

    // ------------------------------------------------------- context menus --
    let context: Rc<dyn Fn(i32, i32)> = {
        let (r, t, b, x) = (rename.clone(), retype.clone(), toggle_bp.clone(), xrefs.clone());
        let ui = ui.clone();
        let redraw = redraw.clone();
        Rc::new(move |mx: i32, my: i32| {
            let m = menu::MenuItem::new(&[
                "Rename\tF2",
                "Change type\tF3",
                "Toggle breakpoint\tF4",
                "Find references\tShift+F12",
                "Copy address",
            ]);
            match m.popup(mx, my).and_then(|i| i.label()) {
                Some(l) if l.starts_with("Rename") => r(),
                Some(l) if l.starts_with("Change") => t(),
                Some(l) if l.starts_with("Toggle") => b(),
                Some(l) if l.starts_with("Find") => x(),
                Some(l) if l.starts_with("Copy") => {
                    let a = ui.borrow().sel_addr;
                    if let Some(a) = a {
                        app::copy(&format!("{:#x}", a));
                    }
                    redraw();
                }
                _ => {}
            }
        })
    };

    for (view, is_asm) in [(code_view.clone(), false), (asm_view.clone(), true)] {
        let ui = ui.clone();
        let redraw = redraw.clone();
        let context = context.clone();
        let reload = reload.clone();
        let mut view = view;
        view.handle(move |v, ev| match ev {
            Event::Push | Event::Released => {
                let row = row_at(v, app::event_y());
                {
                    let mut u = ui.borrow_mut();
                    if is_asm {
                        if let Some((a, _)) = u.asm.get(row) {
                            u.sel_addr = Some(*a);
                        }
                    } else if let Some((a, line)) = u.code.get(row).cloned() {
                        u.sel_addr = a;
                        u.sel_var = u.var_on_line(row);
                        
                        if ev == Event::Released && app::event_mouse_button() == app::MouseButton::Left {
                            if let Some(p) = &u.prog {
                                if let Some(target) = p.funcs.iter().position(|f| mentions(&line, &u.fname(&f.name))) {
                                    if target != u.cur {
                                        let cur = u.cur;
                                        let idx = u.history_idx;
                                        u.history.truncate(idx);
                                        u.history.push(cur);
                                        u.history_idx = u.history.len();
                                        u.cur = target;
                                        u.sel_addr = None;
                                        u.sel_var = None;
                                        drop(u);
                                        reload();
                                        return true;
                                    }
                                }
                            }
                        }
                    }
                }
                if ev == Event::Push && app::event_mouse_button() == app::MouseButton::Right {
                    let (mx, my) = app::event_coords();
                    context(mx, my);
                    return true;
                }
                redraw();
                false
            }
            _ => is_readonly_event(ev),
        });
    }

    // ------------------------------------------------------------ wiring ----
    {
        let ui = ui.clone();
        let reload = reload.clone();
        fn_list.set_callback(move |b| {
            let i = b.value();
            if i > 0 {
                let name = b.text(i).unwrap_or_default();
                let mut name = name.split('\t').next().unwrap_or("");
                if name.starts_with("@B@C4@") {
                    name = &name[6..];
                }
                let name = name.to_string();
                let mut u = ui.borrow_mut();
                if let Some(p) = &u.prog {
                    if let Some(k) = p.funcs.iter().position(|f| u.fname(&f.name) == name) {
                        u.cur = k;
                    }
                }
                u.sel_addr = None;
                u.sel_var = None;
                u.node_pos.clear();
                drop(u);
                reload();
            }
        });
    }
    {
        let reload = reload.clone();
        filter.set_callback(move |_| reload());
    }
    for (b, kind) in btabs.clone() {
        let ui = ui.clone();
        let redraw = redraw.clone();
        let mut b = b;
        b.set_callback(move |_| {
            ui.borrow_mut().bottom = kind;
            redraw();
        });
    }
    {
        let ui = ui.clone();
        detach.set_callback(move |_| detach_panel(&ui));
    }
    {
        // clicking a row that shows an address follows it in the Memory pane
        let ui = ui.clone();
        let redraw = redraw.clone();
        let mut lv = lower_view.clone();
        lv.handle(move |v, ev| {
            if ev == Event::Released {
                let row = row_at(v, app::event_y());
                let pos = v.skip_lines(0, row as i32, true);
                let line = v.buffer().map(|b| b.line_text(pos));
                if let Some(line) = line {
                    if let Some(a) = first_address(&line) {
                        let mut u = ui.borrow_mut();
                        u.mem_focus = Some(a);
                        if u.bottom != Bottom::Memory {
                            u.bottom = Bottom::Memory;
                        }
                        drop(u);
                        redraw();
                    }
                }
            }
            false
        });
    }
    {
        let ui = ui.clone();
        let redraw = redraw.clone();
        entry.set_callback(move |i| {
            let line = i.value();
            {
                let mut u = ui.borrow_mut();
                if u.bottom == Bottom::Output {
                    if let Some(e) = &mut u.emu {
                        e.stdin.push_str(&line);
                        e.stdin.push('\n');
                        e.out.push_str(&format!("{}\n", line));
                    }
                } else if u.bottom == Bottom::Strings {
                    u.string_filter = line.to_lowercase();
                }
            }
            i.set_value("");
            redraw();
        });
    }
    {
        let ui = ui.clone();
        let redraw = redraw.clone();
        let mut cv = code_view.clone();
        let mut gs = graph_scroll.clone();
        let mut gm = g_mode.clone();
        let mut a = t_code.clone();
        let mut b = t_graph.clone();
        t_code.set_callback(move |_| {
            cv.show();
            gs.hide();
            gm.hide();
            a.set_color(HOT);
            b.set_color(PANEL2);
            ui.borrow_mut().sel_var = None;
            redraw();
            app::redraw();
        });
    }
    {
        let mut cv = code_view.clone();
        let mut gs = graph_scroll.clone();
        let mut gm = g_mode.clone();
        let mut a = t_code.clone();
        let mut b = t_graph.clone();
        t_graph.set_callback(move |_| {
            cv.hide();
            gs.show();
            gm.show();
            b.set_color(HOT);
            a.set_color(PANEL2);
            app::redraw();
        });
    }
    {
        let ui = ui.clone();
        let redraw = redraw.clone();
        g_mode.set_callback(move |b| {
            {
                let mut u = ui.borrow_mut();
                u.graph_pseudo = !u.graph_pseudo;
                b.set_label(if u.graph_pseudo { "Graph: pseudocode" } else { "Graph: assembly" });
                u.node_pos.clear();
            }
            redraw();
            app::redraw();
        });
    }

    t_code.set_color(HOT);

    if let Some(path) = std::env::args().nth(1) {
        open_file(Some(path));
        if let Some(name) = std::env::args().nth(2) {
            {
                let mut u = ui.borrow_mut();
                if let Some(p) = &u.prog {
                    if let Some(k) = p.funcs.iter().position(|f| f.name == name) {
                        u.cur = k;
                    }
                }
            }
            reload();
        }
    }

    // Standard fltk-rs pattern for draining a background thread's messages:
    // `app::wait()` blocks until there's something to do — including a
    // background thread calling `Sender::send`, which wakes it via
    // `app::awake()` — so this isn't a busy loop.
    let mut status_loop = status.clone();
    while app.wait() {
        if let Some(ev) = ai_rx.recv() {
            handle_ai_event(ev, &ui, &mut status_loop, &reload);
        }
    }
}

fn short(p: &str) -> String {
    p.rsplit('/').next().unwrap_or(p).to_string()
}

/// Modal "how should we handle this binary" prompt, shown right after
/// opening a binary that `analysis::probe_binary` flagged as stripped or
/// statically linked — the two cases where a plain symbol table won't have
/// named much of anything. Returns (use_flirt, use_ai); clicking Skip or
/// closing the window is the same as leaving both boxes unchecked, since
/// either way it means "just do the normal, symbol-table-only analysis".
fn ask_rename_options(filename: &str, profile: &analysis::BinaryProfile) -> (bool, bool) {
    let kind = match (profile.stripped, profile.static_linked) {
        (true, true) => "stripped and statically linked",
        (true, false) => "stripped",
        (false, true) => "statically linked",
        (false, false) => "",
    };

    let mut w = window::Window::default().with_size(440, 234).with_label("Name recovery");
    w.set_color(PANEL);

    let mut head = frame::Frame::new(16, 14, 408, 44, None);
    head.set_label(&format!(
        "{} looks {}.\nFunction names may be missing — recover some before analysing?",
        short(filename),
        kind
    ));
    head.set_label_color(FG);
    head.set_label_size(12);
    head.set_align(Align::Inside | Align::Left | Align::Wrap);

    let mut flirt_cb = button::CheckButton::new(
        16,
        66,
        408,
        22,
        "Use FLIRT signatures (identify statically-linked library functions)",
    );
    flirt_cb.set_value(true);
    flirt_cb.set_label_color(FG);
    flirt_cb.set_label_size(12);
    flirt_cb.set_color(PANEL);
    flirt_cb.set_selection_color(ACCENT);
    flirt_cb.set_frame(FrameType::FlatBox);

    let mut ai_cb = button::CheckButton::new(
        16,
        92,
        408,
        22,
        "Use AI renaming (see File > AI Settings…)",
    );
    ai_cb.set_value(false);
    ai_cb.set_label_color(FG);
    ai_cb.set_label_size(12);
    ai_cb.set_color(PANEL);
    ai_cb.set_selection_color(ACCENT);
    ai_cb.set_frame(FrameType::FlatBox);

    let mut note = frame::Frame::new(16, 120, 408, 50, None);
    note.set_label(
        "FLIRT runs first and is local/instant. AI renaming only\n\
         targets functions still unnamed afterwards, and sends\n\
         their decompiled C to Model Studio.",
    );
    note.set_label_color(DIM);
    note.set_label_size(11);
    note.set_align(Align::Inside | Align::Left | Align::Wrap);

    let mut skip_btn = button::Button::new(228, 186, 90, 30, "Skip");
    skip_btn.set_color(PANEL2);
    skip_btn.set_label_color(FG);
    skip_btn.set_frame(FrameType::FlatBox);

    let mut ok_btn = button::Button::new(326, 186, 98, 30, "Analyse");
    ok_btn.set_color(PANEL2);
    ok_btn.set_label_color(FG);
    ok_btn.set_frame(FrameType::FlatBox);

    w.end();
    w.make_modal(true);
    w.show();

    let result: Rc<RefCell<(bool, bool)>> = Rc::new(RefCell::new((false, false)));

    {
        let mut w2 = w.clone();
        skip_btn.set_callback(move |_| w2.hide());
    }
    {
        let result = result.clone();
        let mut w2 = w.clone();
        let flirt_cb = flirt_cb.clone();
        let ai_cb = ai_cb.clone();
        ok_btn.set_callback(move |_| {
            *result.borrow_mut() = (flirt_cb.value(), ai_cb.value());
            w2.hide();
        });
    }
    w.set_callback(move |w| w.hide());

    while w.shown() {
        app::wait();
    }

    let picked = *result.borrow();
    picked
}

/// Lets the user pick an AI provider (OpenAI/ChatGPT, Anthropic/Claude,
/// Google/Gemini, Alibaba/Qwen, or a custom OpenAI-compatible endpoint),
/// then paste in that provider's API key (masked) and optionally override
/// its model. Everything is saved to `~/.config/dpp-gui/config.toml` via
/// `mini_decompiler::config`. Saved values are only ever used as a
/// fallback when the matching provider-specific env var (e.g.
/// OPENAI_API_KEY) isn't already set — see `AiClient::from_env`.
fn show_api_key_dialog() {
    use mini_decompiler::ai::Provider;

    let current = Provider::active();
    let options: Vec<String> =
        Provider::ALL.iter().map(|p| format!("{} ({})", p.key(), p.display_name())).collect();
    let provider_prompt = format!(
        "Which AI provider? Type one of: {}\n(current default: {})",
        Provider::ALL.iter().map(|p| p.key()).collect::<Vec<_>>().join(", "),
        current.key()
    );
    let Some(picked) = dialog::input_default(&provider_prompt, current.key()) else { return };
    let Some(provider) = Provider::parse(&picked) else {
        dialog::alert_default(&format!(
            "\"{}\" isn't a provider I recognise. Choose one of: {}",
            picked.trim(),
            options.join(", ")
        ));
        return;
    };

    if let Err(e) = mini_decompiler::config::save_active_provider(provider.key()) {
        dialog::alert_default(&format!("Failed to save active provider: {e}"));
        return;
    }

    let env_var = match provider {
        Provider::OpenAI => "OPENAI_API_KEY",
        Provider::Anthropic => "ANTHROPIC_API_KEY",
        Provider::Gemini => "GEMINI_API_KEY",
        Provider::Qwen => "DASHSCOPE_API_KEY",
        Provider::Custom => "CUSTOM_API_KEY",
    };
    let existing_key = mini_decompiler::config::load_api_key(provider.key());
    let key_prompt = if std::env::var(env_var).is_ok() {
        format!(
            "{env_var} is currently set in your environment and will be\n\
             used instead of any key saved here. Enter a key anyway to\n\
             save it as a fallback for when the env var isn't set:"
        )
    } else if existing_key.is_some() {
        format!(
            "Enter a new {} API key to replace the one currently saved\n\
             (leave blank and press OK, then Remove, to clear it):",
            provider.display_name()
        )
    } else {
        format!("Enter your {} API key:", provider.display_name())
    };

    let Some(input) = dialog::password_default(&key_prompt, "") else { return };
    let key = input.trim().to_string();

    if key.is_empty() {
        if existing_key.is_some()
            && dialog::choice2_default("No key entered. Remove the saved key?", "Cancel", "Remove", "")
                == Some(1)
        {
            match mini_decompiler::config::clear_api_key(provider.key()) {
                Ok(()) => dialog::message_default("Saved API key removed."),
                Err(e) => dialog::alert_default(&format!("Failed to remove key: {e}")),
            }
        }
    } else if let Err(e) = mini_decompiler::config::save_api_key(provider.key(), &key) {
        dialog::alert_default(&format!("Failed to save key: {e}"));
        return;
    }

    // Optional model override — most people can skip this and get the
    // built-in default (see `Provider::default_model`), but "custom"
    // endpoints and anyone chasing a newer model need it.
    let existing_model = mini_decompiler::config::load_model(provider.key()).unwrap_or_default();
    let model_prompt = format!(
        "Model override for {} (leave blank to use the default):",
        provider.display_name()
    );
    if let Some(model_input) = dialog::input_default(&model_prompt, &existing_model) {
        if let Err(e) = mini_decompiler::config::save_model(provider.key(), model_input.trim()) {
            dialog::alert_default(&format!("Failed to save model: {e}"));
            return;
        }
    }

    // "custom" also needs a base URL, since there's no sensible built-in
    // default for an arbitrary OpenAI-compatible endpoint.
    if provider == Provider::Custom {
        let existing_url = mini_decompiler::config::load_base_url(provider.key()).unwrap_or_default();
        if let Some(url_input) = dialog::input_default(
            "Base URL for the custom OpenAI-compatible endpoint\n(e.g. http://localhost:11434/v1):",
            &existing_url,
        ) {
            if let Err(e) = mini_decompiler::config::save_base_url(provider.key(), url_input.trim()) {
                dialog::alert_default(&format!("Failed to save base URL: {e}"));
                return;
            }
        }
    }

    dialog::message_default(&format!(
        "Saved to {}\n\nAI renaming will use {} whenever its API key\nisn't already set via {env_var}.",
        mini_decompiler::config::config_file_display(),
        provider.display_name(),
    ));
}

/// Progress messages from the background AI-renaming thread (see
/// `run_ai_renaming`) back to the GUI thread, delivered through an
/// `app::channel`. Kept to plain owned data (`String`/`u64`/`HashMap`) so
/// it's `Send + Sync` with no extra work.
enum AiEvent {
    /// About to ask the model about this function — `index`/`total` are
    /// 1-based progress for the status line.
    Started { addr: u64, name: String, index: usize, total: usize },
    /// The model answered (or the request failed) for one function.
    /// `new_name`/`vars` are already validated and deduped — see
    /// `rename::RenamePlan::accept` — so they can be applied as-is.
    Result {
        old_name: String,
        addr: u64,
        new_name: Option<String>,
        vars: HashMap<String, String>,
        error: Option<String>,
        index: usize,
        total: usize,
    },
    /// The whole pass is done.
    Finished { total: usize },
}

/// Kicks off the Stage-3 AI naming pass (see `rename.rs`) over every
/// function in the current program that still has its deterministic
/// `sub_XXXXXX` name. Unlike the earlier synchronous version, this returns
/// immediately: the actual network round-trips run on a background thread,
/// which streams an `AiEvent` per function back through `ai_tx` as it goes.
/// The GUI thread's `app::wait()` loop (see `main`) applies each one to the
/// UI's rename maps and calls `reload()` — the same maps manual F2 renaming
/// writes to, so a bad AI guess is exactly as harmless, and as undoable, as
/// a bad manual rename — as soon as it arrives, instead of waiting for the
/// whole pass to finish. This is also why opening a file no longer blocks
/// on AI renaming: the caller populates the function list with its
/// deterministic names first, then calls this, and names update live as
/// replies come back.
fn run_ai_renaming(ui: &Rc<RefCell<Ui>>, ai_tx: app::Sender<AiEvent>) {
    let client = match mini_decompiler::ai::AiClient::from_env() {
        Ok(c) => c,
        Err(e) => {
            dialog::alert_default(&format!("AI renaming skipped: {}", e));
            return;
        }
    };

    // Everything the background thread needs is pulled out as plain owned
    // data right here, while we still hold the borrow — `Analyzed` itself
    // stays behind the GUI's `RefCell` and never crosses the thread
    // boundary. This is also where the var-name index gets snapshotted for
    // later, since it stays valid for the life of this pass (the analysis
    // itself never changes, only which display names are attached to it).
    let targets = {
        let mut u = ui.borrow_mut();
        let Some(prog) = &u.prog else { return };
        let structs = prog.structs.clone();
        let targets: Vec<rename::AiTarget> = prog
            .funcs
            .iter()
            .filter(|a| rename::needs_naming(a))
            .map(|a| rename::make_target(a, analysis::render(a, &structs, false)))
            .collect();
        u.ai_var_index = prog
            .funcs
            .iter()
            .map(|f| {
                let vars =
                    f.frame.vars.iter().enumerate().map(|(i, v)| (v.name.clone(), i)).collect();
                (f.name.clone(), vars)
            })
            .collect();
        targets
    };

    let total = targets.len();
    if total == 0 {
        dialog::message_default("Every function already has a name — nothing for AI renaming to do.");
        return;
    }

    std::thread::spawn(move || {
        rename::build_plan(
            &client,
            &targets,
            |t, index, total| {
                ai_tx.send(AiEvent::Started { addr: t.addr, name: t.name.clone(), index, total });
            },
            |t, index, total, res| {
                let (new_name, vars, error) = match res {
                    Ok((new_name, vars)) => (new_name, vars, None),
                    Err(e) => (None, HashMap::new(), Some(e.to_string())),
                };
                ai_tx.send(AiEvent::Result {
                    old_name: t.name.clone(),
                    addr: t.addr,
                    new_name,
                    vars,
                    error,
                    index,
                    total,
                });
            },
        );
        ai_tx.send(AiEvent::Finished { total });
    });
}

/// Applies one `AiEvent` to the UI state and status line, then reloads the
/// function list so a rename (or the in-progress highlight) shows up
/// immediately. Called from the `app::wait()` loop in `main` as messages
/// arrive off the AI-renaming background thread.
fn handle_ai_event(
    ev: AiEvent,
    ui: &Rc<RefCell<Ui>>,
    status: &mut frame::Frame,
    reload: &Rc<dyn Fn()>,
) {
    match ev {
        AiEvent::Started { addr, name, index, total } => {
            ui.borrow_mut().ai_active_addr = Some(addr);
            // reload() (via redraw()) sets its own default status text, so
            // the progress message has to go on *after* it or it's
            // overwritten instantly.
            reload();
            status.set_label(&format!("  AI renaming: {}/{} — {}", index, total, name));
        }
        AiEvent::Result { old_name, addr, new_name, vars, error, index, total } => {
            {
                let mut u = ui.borrow_mut();
                if let Some(new) = &new_name {
                    u.fn_names.insert(old_name.clone(), new.clone());
                }
                if let Some(idx) = u.ai_var_index.get(&old_name).cloned() {
                    for (old_var, new_var) in &vars {
                        if let Some(&i) = idx.get(old_var) {
                            u.var_names.insert((old_name.clone(), i), new_var.clone());
                        }
                    }
                }
                if u.ai_active_addr == Some(addr) {
                    u.ai_active_addr = None;
                }
            }
            if let Some(err) = &error {
                eprintln!("AI renaming: {} skipped ({err})", old_name);
            }
            reload();
            status.set_label(&format!("  AI renaming: {}/{} done", index, total));
        }
        AiEvent::Finished { total } => {
            ui.borrow_mut().ai_active_addr = None;
            reload();
            status.set_label(&format!("  AI renaming finished — {} function(s)", total));
        }
    }
}

fn define_struct(ui: &Rc<RefCell<Ui>>) {
    let existing: Vec<String> = ui.borrow().structs().defs.iter().map(|d| d.name.clone()).collect();
    let prompt = format!(
        "Existing: {}\nNew structure name:",
        if existing.is_empty() { "none".to_string() } else { existing.join(", ") }
    );
    let Some(name) = dialog::input_default(&prompt, "my_struct") else { return };
    let name = name.trim().to_string();
    if !valid_identifier(&name) {
        dialog::alert_default("not a valid identifier");
        return;
    }
    let Some(body) = dialog::input_default(
        "Fields, semicolon separated: offset type name",
        "0 int x; 4 int y; 8 long tag",
    ) else {
        return;
    };
    match parse_struct(&name, &body) {
        Some(d) => {
            let mut u = ui.borrow_mut();
            u.user_structs.retain(|x| x.name != d.name);
            u.user_structs.push(d);
        }
        None => dialog::alert_default("could not parse the field list"),
    }
}

/// Pop the current bottom panel into its own window, so it can be moved and
/// resized independently.
fn detach_panel(ui: &Rc<RefCell<Ui>>) {
    let (title, body) = {
        let u = ui.borrow();
        let Some(p) = &u.prog else { return };
        let Some(f) = u.func() else { return };
        (format!("{:?}", u.bottom), u.bottom_text(p, f).0)
    };
    let mut w = window::Window::default().with_size(760, 520).with_label(&title);
    w.set_color(PANEL);
    let mut d = text::TextEditor::new(0, 0, 760, 520, None);
    let mut b = text::TextBuffer::default();
    b.set_text(&body);
    d.set_buffer(b);
    style_display(&mut d);
    d.handle(|_, ev| is_readonly_event(ev));
    w.end();
    w.make_resizable(true);
    w.show();
    // FLTK keeps the window alive as long as it is shown; leaking the handle
    // here is what lets it outlive this call.
    std::mem::forget(w);
}

fn is_readonly_event(ev: Event) -> bool {
    if ev == Event::KeyDown || ev == Event::Paste {
        let state = app::event_state();
        let key = app::event_key();
        
        // Allow select all / copy
        if state.contains(Shortcut::Ctrl) && (key == Key::from_char('c') || key == Key::from_char('a')) {
            return false;
        }
        
        // Allow navigation keys
        let is_nav = matches!(
            key,
            Key::Left | Key::Right | Key::Up | Key::Down | Key::PageUp | Key::PageDown | Key::Home | Key::End
        );
        if is_nav || state.contains(Shortcut::Shift) {
            return false;
        }
        
        // Block printable characters
        if let Some(c) = app::event_text().chars().next() {
            if !c.is_control() && !state.contains(Shortcut::Ctrl) && !state.contains(Shortcut::Alt) {
                return true;
            }
        }
        
        // Block other editing keys
        if matches!(key, Key::BackSpace | Key::Delete | Key::Enter | Key::Tab) {
            return true;
        }
    }
    false
}

// ------------------------------------------------------------ Ui helpers ---
impl Ui {
    /// Text and style for whichever bottom panel is showing.
    fn bottom_text(&self, p: &Program, f: &Analyzed) -> (String, String) {
        let mut t = String::new();
        let mut s = String::new();
        let put = |line: &str, style: char, t: &mut String, s: &mut String| {
            t.push_str(line);
            t.push('\n');
            s.push_str(&style.to_string().repeat(line.chars().count()));
            s.push('\n');
        };

        match self.bottom {
            Bottom::Registers => {
                let Some(e) = &self.emu else { return ("Not running.\n".into(), "G".repeat(13)) };
                for r in SHOWN_REGS {
                    let v = e.reg(r);
                    let note = e.classify(v, p).describe();
                    let line = format!("{:<4} {:016x}  {}", r, v, note);
                    put(&line, if e.changed(r) { 'C' } else { 'A' }, &mut t, &mut s);
                }
                let line = format!(
                    "rip  {}",
                    e.pc.map(|v| format!("{:016x}", v)).unwrap_or_else(|| "--".into())
                );
                put(&line, 'E', &mut t, &mut s);
            }

            Bottom::Stack => {
                let Some(e) = &self.emu else { return ("Not running.\n".into(), "G".repeat(13)) };
                let (sp, bp) = (e.reg("rsp"), e.reg("rbp"));
                
                // Collect frame bases to color distinct frames
                let mut frames = vec![bp];
                let mut curr = bp;
                for _ in 0..5 {
                    if curr == 0 { break; }
                    let next = e.read(curr, 8);
                    if next == 0 || next == curr || next == 0xdeadbeefdeadbeef { break; }
                    frames.push(next);
                    curr = next;
                }
                
                let slots = self.slot_names(f);
                for i in 0..20u64 {
                    let a = sp.wrapping_add(i * 8);
                    
                    // Determine frame color
                    let mut frame_style = 'A'; // default color
                    for (fi, &fbase) in frames.iter().enumerate() {
                        if a <= fbase {
                            frame_style = if fi % 2 == 0 { 'B' } else { 'E' }; // toggle RED / BLUE
                            break;
                        }
                    }
                    let v = e.read(a, 8);
                    let mark = if a == sp {
                        "rsp>"
                    } else if a == bp {
                        "rbp>"
                    } else {
                        "    "
                    };
                    let off = a as i64 - bp as i64;
                    let named = slots
                        .iter()
                        .find(|(o, _)| *o == off)
                        .map(|(_, n)| format!("{:<10}", n))
                        .unwrap_or_else(|| " ".repeat(10));
                    let line = format!(
                        "{} {:012x} {} {:016x}  {}",
                        mark,
                        a,
                        named,
                        v,
                        e.classify(v, p).describe()
                    );
                    put(&line, frame_style, &mut t, &mut s);
                }
            }

            Bottom::Memory => {
                let Some(e) = &self.emu else { return ("Not running.\n".into(), "G".repeat(13)) };
                let base = self.mem_focus.unwrap_or_else(|| e.reg("rsp")) & !0xf;
                put(
                    &format!("{:#x}   {}", base, e.classify(base, p).describe()),
                    'G',
                    &mut t,
                    &mut s,
                );
                for r in 0..13u64 {
                    let a = base.wrapping_add(r * 16);
                    let (mut h, mut c) = (String::new(), String::new());
                    for col in 0..16u64 {
                        let b = e.read(a + col, 1) as u8;
                        h.push_str(&format!("{:02x} ", b));
                        c.push(if (0x20..0x7f).contains(&b) { b as char } else { '.' });
                    }
                    put(&format!("{:012x}  {} {}", a, h, c), 'A', &mut t, &mut s);
                }
            }

            Bottom::Vmmap => {
                let Some(e) = &self.emu else { return ("Not running.\n".into(), "G".repeat(13)) };
                put(
                    &format!("{:<18} {:<18} {:<5} {}", "START", "END", "PERM", "MAPPING"),
                    'G',
                    &mut t,
                    &mut s,
                );
                for r in &e.regions {
                    let here = e.pc.map_or(false, |pc| pc >= r.start && pc < r.end);
                    put(
                        &format!(
                            "{:#018x} {:#018x} {:<5} {}",
                            r.start, r.end, r.perm, r.name
                        ),
                        if here { 'E' } else { 'A' },
                        &mut t,
                        &mut s,
                    );
                }
            }

            Bottom::Strings => {
                let q = &self.string_filter;
                put(
                    &format!(
                        "{} strings{}",
                        p.strings.len(),
                        if q.is_empty() {
                            "   (Ctrl+F to search, or type below)".to_string()
                        } else {
                            format!("   matching \"{}\"", q)
                        }
                    ),
                    'G',
                    &mut t,
                    &mut s,
                );
                for (a, txt) in &p.strings {
                    if !q.is_empty() && !txt.to_lowercase().contains(q) {
                        continue;
                    }
                    put(&format!("{:#012x}  \"{}\"", a, txt), 'D', &mut t, &mut s);
                }
            }

            Bottom::Xrefs => {
                put(
                    &if self.xrefs.is_empty() {
                        "no references — put the caret on a call or a name, then Shift+F12".into()
                    } else {
                        format!("{} references to {}", self.xrefs.len(), self.xref_target)
                    },
                    'G',
                    &mut t,
                    &mut s,
                );
                for (func, addr, text) in &self.xrefs {
                    put(&format!("{:#012x}  {:<16} {}", addr, func, text), 'A', &mut t, &mut s);
                }
            }

            Bottom::Output => {
                let body = match &self.emu {
                    Some(e) if !e.out.is_empty() => e.out.clone(),
                    _ => "(no output yet — Debug ▸ Start, then Continue)\n".to_string(),
                };
                for line in body.lines() {
                    put(line, 'D', &mut t, &mut s);
                }
                put("", 'A', &mut t, &mut s);
                put("type below and press Enter to send to the program's stdin", 'G', &mut t, &mut s);
            }
        }
        (t, s)
    }

    /// Frame offsets of the recovered locals, relative to rbp, with the names
    /// the pseudocode uses.
    fn slot_names(&self, f: &Analyzed) -> Vec<(i64, String)> {
        f.frame
            .vars
            .iter()
            .enumerate()
            .filter(|(_, v)| !v.is_param || v.off != 0)
            .map(|(i, v)| (v.off + 8, self.var_label(f, i)))
            .collect()
    }

    fn graph_boxes(&self, f: &Analyzed, ox: i32, oy: i32) -> Vec<Box2> {
        let mut boxes = graph_layout(f, ox, oy, self.graph_pseudo.then_some(&self.code));
        for b in boxes.iter_mut() {
            if let Some((x, y)) = self.node_pos.get(&b.addr) {
                b.x = ox + x;
                b.y = oy + y;
            }
        }
        boxes
    }
}

/// ASCII jump arrows down the left of the disassembly, the way objdump and
/// Ghidra draw them: a branch and its target are joined by a rail so the loop
/// structure is visible without reading every address.
fn jump_arrows(asm: &[(u64, String)]) -> Vec<String> {
    let index: HashMap<u64, usize> = asm.iter().enumerate().map(|(i, (a, _))| (*a, i)).collect();
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for (i, (_, text)) in asm.iter().enumerate() {
        let t = text.to_lowercase();
        if !(t.starts_with('j') || t.starts_with("loop")) {
            continue;
        }
        let Some(tok) = t.split_whitespace().last() else { continue };
        let hex = tok.trim_end_matches('h');
        let Ok(target) = u64::from_str_radix(hex.trim_start_matches("0x"), 16) else { continue };
        let Some(&j) = index.get(&target) else { continue };
        spans.push((i, j));
    }
    // give each span its own column so overlapping jumps stay readable
    let mut lanes: Vec<Vec<(usize, usize)>> = Vec::new();
    for sp in spans {
        match lanes.iter_mut().find(|l| l.iter().all(|o| {
            let (min1, max1) = (sp.0.min(sp.1), sp.0.max(sp.1));
            let (min2, max2) = (o.0.min(o.1), o.0.max(o.1));
            max1 < min2 || min1 > max2
        })) {
            Some(l) => l.push(sp),
            None => lanes.push(vec![sp]),
        }
        if lanes.len() >= 3 {
            break;
        }
    }

    let width = lanes.len();
    let mut out = vec![String::new(); asm.len()];
    for (row, cell) in out.iter_mut().enumerate() {
        let mut line = vec![' '; width];
        let mut has_arrow = false;
        let mut has_line = false;
        for (li, lane) in lanes.iter().enumerate() {
            for &(src, tgt) in lane {
                let min = src.min(tgt);
                let max = src.max(tgt);
                if row == src {
                    if src < tgt {
                        line[li] = '┌';
                    } else {
                        line[li] = '└';
                    }
                    has_line = true;
                } else if row == tgt {
                    if src < tgt {
                        line[li] = '└';
                    } else {
                        line[li] = '┌';
                    }
                    has_arrow = true;
                } else if row > min && row < max {
                    line[li] = '│';
                }
            }
        }
        let tail = if has_arrow { "─▶ " } else if has_line { "── " } else { "   " };
        *cell = format!("{}{}", line.into_iter().collect::<String>(), tail);
    }
    out
}

const ASM_KEYWORDS: [&str; 24] = [
    "mov", "lea", "push", "pop", "call", "ret", "jmp", "je", "jne", "jl", "jle", "jg", "jge", "jb",
    "jbe", "ja", "jae", "js", "jns", "test", "cmp", "add", "sub", "leave",
];

/// Colour one disassembly line: mnemonic, registers, immediates.
fn style_asm(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = vec![b'A'; b.len()];
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut first = true;
    let mut i = 0;
    while i < b.len() {
        if ident(b[i]) && (i == 0 || !ident(b[i - 1])) {
            let start = i;
            while i < b.len() && ident(b[i]) {
                i += 1;
            }
            let w = &text[start..i];
            let style = if first {
                first = false;
                if ASM_KEYWORDS.contains(&w) || w.starts_with('j') {
                    b'B'
                } else {
                    b'E'
                }
            } else if w.chars().next().map_or(false, |c| c.is_ascii_digit()) {
                b'C'
            } else if is_register(w) {
                b'F'
            } else {
                b'A'
            };
            out[start..i].fill(style);
            continue;
        }
        i += 1;
    }
    String::from_utf8(out).unwrap_or_default()
}

fn is_register(w: &str) -> bool {
    const R: [&str; 20] = [
        "rax", "rbx", "rcx", "rdx", "rsi", "rdi", "rbp", "rsp", "eax", "ebx", "ecx", "edx", "esi",
        "edi", "ebp", "esp", "al", "bl", "cl", "dl",
    ];
    R.contains(&w) || (w.starts_with('r') && w[1..].chars().all(|c| c.is_ascii_digit()))
        || (w.starts_with("xmm") && w[3..].chars().all(|c| c.is_ascii_digit()))
}

// ------------------------------------------------------------------ helpers --
fn valid_identifier(s: &str) -> bool {
    let mut c = s.chars();
    matches!(c.next(), Some(f) if f.is_ascii_alphabetic() || f == '_')
        && c.all(|x| x.is_ascii_alphanumeric() || x == '_')
}

fn type_size(t: &str) -> usize {
    if t.ends_with('*') {
        return 8;
    }
    match t {
        "char" | "unsigned char" | "signed char" => 1,
        "short" | "unsigned short" => 2,
        "int" | "unsigned int" | "float" => 4,
        _ => 8,
    }
}

fn parse_type(t: &str) -> Type {
    if t.ends_with('*') {
        return Type::ptr(parse_type(t.trim_end_matches('*').trim()));
    }
    match t {
        "float" => Type::Float { bits: 32 },
        "double" => Type::Float { bits: 64 },
        _ => Type::Int { bits: (type_size(t) * 8) as u16, signed: !t.starts_with("unsigned") },
    }
}

fn parse_struct(name: &str, body: &str) -> Option<StructDef> {
    let mut fields = Vec::new();
    let mut end = 0i64;
    for part in body.split(|c| c == ';' || c == '\n') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (off_s, rest) = part.split_once(char::is_whitespace)?;
        let off: i64 = off_s.trim().parse().ok()?;
        let (ty, fname) = rest.trim().rsplit_once(char::is_whitespace)?;
        let (ty, fname) = (ty.trim(), fname.trim());
        fields.push(Field { off, ty: parse_type(ty), name: fname.to_string() });
        end = end.max(off + type_size(ty) as i64);
    }
    if fields.is_empty() {
        return None;
    }
    Some(StructDef { name: name.to_string(), fields, size: end.max(0) as usize })
}

fn tab_button(x: i32, y: i32, w: i32, label: &str) -> button::Button {
    let mut b = button::Button::new(x, y, w, 24, None).with_label(label);
    b.set_color(PANEL2);
    b.set_label_color(FG);
    b.set_frame(FrameType::FlatBox);
    b.set_label_size(11);
    b.clear_visible_focus();
    b
}

fn pane(x: i32, y: i32, w: i32, h: i32, title: &str) -> group::Group {
    let mut g = group::Group::new(x, y, w, h, None);
    g.set_color(PANEL);
    g.set_frame(FrameType::BorderBox);
    if !title.is_empty() {
        let mut t = frame::Frame::new(x + 6, y + 2, w - 10, 18, None).with_label(title);
        t.set_label_color(DIM);
        t.set_label_size(11);
        t.set_align(Align::Inside | Align::Left);
    }
    g
}

fn style_display(d: &mut text::TextEditor) {
    // a visible caret: without it there is no way to tell where the keyboard
    // is pointing, and every action that acts on "the current line" looks
    // like it does nothing
    d.set_cursor_style(text::Cursor::Block);
    d.show_cursor(true);
    d.set_cursor_color(ACCENT);
    d.set_color(PANEL);
    d.set_text_color(FG);
    d.set_text_font(Font::Courier);
    d.set_text_size(13);
    d.set_frame(FrameType::FlatBox);
    d.set_scrollbar_size(12);
    d.wrap_mode(text::WrapMode::None, 0);
}

/// Which buffer line a click landed on. Going through the widget's own
/// coordinate mapping rather than dividing by a line height keeps this
/// correct when the view is scrolled.
fn row_at(v: &text::TextEditor, y: i32) -> usize {
    let pos = v.xy_to_position(v.x() + 4, y, text::PositionType::Character);
    v.count_lines(0, pos, true).max(0) as usize
}

/// Bring the line for `addr` into view. TextDisplay has no per-row background,
/// so the current line is shown by moving the caret to it.
fn scroll_to(v: &mut text::TextEditor, addrs: &[Option<u64>], addr: Option<u64>) {
    let Some(t) = addr else { return };
    let Some(row) = addrs.iter().position(|a| *a == Some(t)) else { return };
    let pos = v.skip_lines(0, row as i32, true);
    v.set_insert_position(pos);
    v.show_insert_position();
}

struct Box2 {
    addr: u64,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    text: Vec<String>,
    insns: Vec<u64>,
    succs: Vec<u64>,
}

/// Layered layout: rank each block by its distance from the entry, then pack
/// each rank left to right.
fn graph_layout(
    f: &Analyzed,
    ox: i32,
    oy: i32,
    pseudo: Option<&Vec<(Option<u64>, String)>>,
) -> Vec<Box2> {
    let cfg = Cfg::build(&f.insns);
    let mut rank: HashMap<u64, usize> = HashMap::new();
    if let Some(b0) = cfg.blocks.first() {
        rank.insert(b0.addr, 0);
        let mut q = vec![b0.addr];
        while let Some(a) = q.pop() {
            let r = rank[&a];
            if let Some(b) = cfg.blocks.iter().find(|b| b.addr == a) {
                for s in &b.succs {
                    if !rank.contains_key(s) && cfg.blocks.iter().any(|x| x.addr == *s) {
                        rank.insert(*s, r + 1);
                        q.push(*s);
                    }
                }
            }
        }
    }

    let mut rows: Vec<Vec<usize>> = Vec::new();
    for (i, b) in cfg.blocks.iter().enumerate() {
        let r = *rank.get(&b.addr).unwrap_or(&0);
        while rows.len() <= r {
            rows.push(Vec::new());
        }
        rows[r].push(i);
    }

    let mut out = Vec::new();
    let mut y = oy;
    for row in &rows {
        let mut x = ox;
        let mut tallest = 0;
        for &bi in row {
            let b = &cfg.blocks[bi];
            // a block shows either its instructions or the pseudocode lines
            // that came from them
            let text: Vec<String> = match pseudo {
                Some(lines) => {
                    let addrs: Vec<u64> = b.instrs.iter().map(|&i| f.insns[i].addr).collect();
                    let picked: Vec<String> = lines
                        .iter()
                        .filter(|(a, _)| a.map_or(false, |a| addrs.contains(&a)))
                        .map(|(_, t)| t.trim().to_string())
                        .collect();
                    if picked.is_empty() {
                        // a block whose only job is the loop or branch test
                        // produced a structural line with no address; show
                        // the test itself rather than nothing
                        b.instrs
                            .iter()
                            .map(|&i| f.insns[i].asm_text.clone())
                            .filter(|t| {
                                let t = t.to_lowercase();
                                t.starts_with("cmp") || t.starts_with("test") || t.starts_with('j')
                            })
                            .collect::<Vec<_>>()
                            .into_iter()
                            .chain(std::iter::once("(condition)".to_string()))
                            .take(4)
                            .collect()
                    } else {
                        picked
                    }
                }
                None => b.instrs.iter().map(|&i| f.insns[i].asm_text.clone()).collect(),
            };
            let widest = text.iter().map(|t| t.len()).max().unwrap_or(10).max(12);
            let w = (widest as i32 * 7 + 24).min(500);
            let h = 26 + 13 * text.len() as i32;
            tallest = tallest.max(h);
            out.push(Box2 {
                addr: b.addr,
                x,
                y,
                w,
                h,
                text,
                insns: b.instrs.iter().map(|&i| f.insns[i].addr).collect(),
                succs: b.succs.clone(),
            });
            x += w + 40;
        }
        y += tallest + 50;
    }
    out
}

/// The first hex address on a line of a panel, so clicking a row can follow it.
fn first_address(line: &str) -> Option<u64> {
    let mut best: Option<u64> = None;
    for tok in line.split(|c: char| !(c.is_ascii_hexdigit() || c == 'x')) {
        let t = tok.trim_start_matches("0x");
        if t.len() >= 6 && t.chars().all(|c| c.is_ascii_hexdigit()) {
            if let Ok(v) = u64::from_str_radix(t, 16) {
                if v > 0x1000 {
                    best = Some(v);
                    break;
                }
            }
        }
    }
    best
}
