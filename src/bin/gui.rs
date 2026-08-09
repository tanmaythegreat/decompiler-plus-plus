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

    let mut win = window::Window::default().with_size(1620, 1000).with_label("decompiler++");
    win.set_color(BG);

    let mut menubar = menu::MenuBar::new(0, 0, 1620, 26, None);
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
    let mut fn_list = browser::HoldBrowser::new(2, 74, 236, 612, None);
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
    let mut code_view = text::TextDisplay::new(242, 56, 776, 632, None);
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
    let mut asm_view = text::TextDisplay::new(1022, 48, 596, 640, None);
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

    let mut lower_view = text::TextDisplay::new(2, 720, 1616, 226, None);
    let lower_buf = text::TextBuffer::default();
    let lower_style = text::TextBuffer::default();
    lower_view.set_buffer(lower_buf.clone());
    lower_view.set_highlight_data(lower_style.clone(), code_styles());
    style_display(&mut lower_view);

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

            // pseudocode
            let (mut txt, mut sty) = (String::new(), String::new());
            for (addr, line) in &u.code {
                let bp = addr.map_or(false, |a| u.breakpoints.contains(&a));
                let gutter = match addr {
                    Some(a) => format!("{} {:08x}  ", if bp { "*" } else { " " }, a),
                    None => " ".repeat(12),
                };
                sty.push_str(&(if bp { "H" } else { "G" }).repeat(gutter.len()));
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
                let head = format!("{} {:08x} ", if bp { "*" } else { " " }, a);
                let arrow = arrows.get(i).cloned().unwrap_or_else(|| "    ".into());
                asty.push_str(&(if bp { "H" } else { "G" }).repeat(head.len()));
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

            let pc = u.emu.as_ref().and_then(|e| e.pc);
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
        let redraw = redraw.clone();
        Rc::new(move || {
            let mut fn_list = fn_list.clone();
            {
                let mut u = ui.borrow_mut();
                u.rebuild();
                if u.emu.is_none() {
                    let e = u.prog.as_ref().map(|p| Emu::new(p, u.cur));
                    u.emu = e;
                }
            }
            fn_list.clear();
            {
                let u = ui.borrow();
                let pat = filter.value().to_lowercase();
                if let Some(p) = &u.prog {
                    for (i, f) in p.funcs.iter().enumerate() {
                        let name = u.fname(&f.name);
                        if !pat.is_empty() && !name.to_lowercase().contains(&pat) {
                            continue;
                        }
                        fn_list.add(&format!("{}\t{:x}", name, f.addr));
                        if i == u.cur {
                            fn_list.select(fn_list.size());
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
            match analysis::analyze_file(&path) {
                Ok(p) => {
                    let mut u = ui.borrow_mut();
                    u.cur = p.funcs.iter().position(|f| f.name == "main").unwrap_or(0);
                    u.prog = Some(p);
                    u.sel_addr = None;
                    u.sel_var = None;
                    u.emu = None;
                    u.node_pos.clear();
                }
                Err(e) => {
                    dialog::alert_default(&format!("{}\n\nOnly ELF objects are supported.", e));
                    return;
                }
            }
            reload();
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
                    } else if let Some((a, _)) = u.code.get(row) {
                        u.sel_addr = *a;
                        u.sel_var = u.var_on_line(row);
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
            _ => false,
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
                let name = name.split('\t').next().unwrap_or("").to_string();
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

    app.run().unwrap();
}

fn short(p: &str) -> String {
    p.rsplit('/').next().unwrap_or(p).to_string()
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
    let mut d = text::TextDisplay::new(0, 0, 760, 520, None);
    let mut b = text::TextBuffer::default();
    b.set_text(&body);
    d.set_buffer(b);
    style_display(&mut d);
    w.end();
    w.make_resizable(true);
    w.show();
    // FLTK keeps the window alive as long as it is shown; leaking the handle
    // here is what lets it outlive this call.
    std::mem::forget(w);
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
                let Some(e) = &self.emu else { return (String::new(), String::new()) };
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
                let Some(e) = &self.emu else { return (String::new(), String::new()) };
                let (sp, bp) = (e.reg("rsp"), e.reg("rbp"));
                // frame slots are relative to the frame pointer; naming them
                // is the whole point of having decompiled the function
                let slots = self.slot_names(f);
                for i in 0..20u64 {
                    let a = sp.wrapping_add(i * 8);
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
                    put(&line, if a == sp || a == bp { 'E' } else { 'A' }, &mut t, &mut s);
                }
            }

            Bottom::Memory => {
                let Some(e) = &self.emu else { return (String::new(), String::new()) };
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
                let Some(e) = &self.emu else { return (String::new(), String::new()) };
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
        spans.push((i.min(j), i.max(j)));
    }
    // give each span its own column so overlapping jumps stay readable
    let mut lanes: Vec<Vec<(usize, usize)>> = Vec::new();
    for sp in spans {
        match lanes.iter_mut().find(|l| l.iter().all(|o| sp.1 < o.0 || sp.0 > o.1)) {
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
        for (li, lane) in lanes.iter().enumerate() {
            for (a, b) in lane {
                if row == *a {
                    line[li] = '┌';
                } else if row == *b {
                    line[li] = '└';
                } else if row > *a && row < *b {
                    line[li] = '│';
                }
            }
        }
        let tail = if line.iter().any(|c| *c == '┌' || *c == '└') { "─▶ " } else { "   " };
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

fn style_display(d: &mut text::TextDisplay) {
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
fn row_at(v: &text::TextDisplay, y: i32) -> usize {
    let pos = v.xy_to_position(v.x() + 4, y, text::PositionType::Character);
    v.count_lines(0, pos, true).max(0) as usize
}

/// Bring the line for `addr` into view. TextDisplay has no per-row background,
/// so the current line is shown by moving the caret to it.
fn scroll_to(v: &mut text::TextDisplay, addrs: &[Option<u64>], addr: Option<u64>) {
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
