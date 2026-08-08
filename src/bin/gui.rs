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
use fltk::{app, browser, button, dialog, draw, frame, group, text, window};
use mini_decompiler::analysis::{self, Analyzed, Program};
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
    ]
}

fn reg_styles() -> Vec<text::StyleTableEntry> {
    let mk = |c: Color| text::StyleTableEntry { color: c, font: Font::Courier, size: 13 };
    vec![mk(FG), mk(DIM), mk(WARN)]
}

fn main() {
    let app = app::App::default();
    app::background(0x16, 0x18, 0x1d);
    app::foreground(0xd7, 0xda, 0xe0);
    app::background2(0x22, 0x26, 0x2f);
    app::set_font(Font::Courier);
    app::set_font_size(13);

    let ui = Rc::new(RefCell::new(Ui::new()));

    let mut win = window::Window::default().with_size(1500, 950).with_label("decompiler++");
    win.set_color(BG);

    // toolbar
    let mut b_open = tool_button(6, 4, 130, "Open binary…");
    let mut b_cast = tool_button(142, 4, 110, "Casts: on");
    let mut b_struct = tool_button(258, 4, 100, "Structs…");
    let mut b_rename = tool_button(364, 4, 120, "Rename F2");
    let mut b_type = tool_button(490, 4, 110, "Type F3");
    let mut b_reset = tool_button(606, 4, 90, "Reset F5");
    let mut b_step = tool_button(702, 4, 100, "Step F7");
    let mut b_run = tool_button(808, 4, 100, "Run F9");
    // Button shortcuts fire regardless of which widget holds focus, which
    // the window-level key handler alone does not guarantee.
    b_rename.set_shortcut(Shortcut::from_key(Key::F2));
    b_type.set_shortcut(Shortcut::from_key(Key::F3));
    b_reset.set_shortcut(Shortcut::from_key(Key::F5));
    b_step.set_shortcut(Shortcut::from_key(Key::F7));
    b_run.set_shortcut(Shortcut::from_key(Key::F9));
    let mut lbl_file = frame::Frame::new(920, 4, 570, 26, None);
    lbl_file.set_label_color(DIM);
    lbl_file.set_label_size(11);
    lbl_file.set_align(Align::Inside | Align::Left);

    // panes
    let left = pane(0, 34, 230, 656, "Functions");
    let mut fn_list = browser::HoldBrowser::new(2, 56, 226, 632, None);
    fn_list.set_color(PANEL);
    fn_list.set_selection_color(HOT);
    fn_list.set_text_size(12);
    fn_list.set_frame(FrameType::FlatBox);
    left.end();

    let centre = pane(230, 34, 760, 656, "");
    let mut t_code = tool_button(234, 36, 110, "Pseudocode");
    let mut t_graph = tool_button(348, 36, 80, "Graph");
    t_code.set_color(HOT);
    let mut code_view = text::TextDisplay::new(232, 66, 756, 622, None);
    let code_buf = text::TextBuffer::default();
    let code_style = text::TextBuffer::default();
    code_view.set_buffer(code_buf.clone());
    code_view.set_highlight_data(code_style.clone(), code_styles());
    style_display(&mut code_view);

    let mut graph_scroll = group::Scroll::new(232, 66, 756, 622, None);
    graph_scroll.set_color(PANEL);
    graph_scroll.set_frame(FrameType::FlatBox);
    let mut graph = frame::Frame::new(232, 66, 3000, 3000, None);
    graph_scroll.end();
    graph_scroll.hide();
    centre.end();

    let right = pane(990, 34, 510, 656, "Disassembly");
    let mut asm_view = text::TextDisplay::new(992, 56, 506, 632, None);
    let asm_buf = text::TextBuffer::default();
    let asm_style = text::TextBuffer::default();
    asm_view.set_buffer(asm_buf.clone());
    asm_view.set_highlight_data(asm_style.clone(), code_styles());
    style_display(&mut asm_view);
    right.end();

    let p_regs = pane(0, 690, 380, 260, "Registers");
    let mut regs_view = text::TextDisplay::new(2, 712, 376, 236, None);
    let regs_buf = text::TextBuffer::default();
    let regs_style = text::TextBuffer::default();
    regs_view.set_buffer(regs_buf.clone());
    regs_view.set_highlight_data(regs_style.clone(), reg_styles());
    style_display(&mut regs_view);
    p_regs.end();

    let p_stack = pane(380, 690, 340, 260, "Stack");
    let mut stack_view = text::TextDisplay::new(382, 712, 336, 236, None);
    let stack_buf = text::TextBuffer::default();
    stack_view.set_buffer(stack_buf.clone());
    style_display(&mut stack_view);
    p_stack.end();

    let p_mem = pane(720, 690, 470, 260, "Memory");
    let mut mem_view = text::TextDisplay::new(722, 712, 466, 236, None);
    let mem_buf = text::TextBuffer::default();
    mem_view.set_buffer(mem_buf.clone());
    style_display(&mut mem_view);
    p_mem.end();

    let p_out = pane(1190, 690, 310, 260, "Output");
    let mut out_view = text::TextDisplay::new(1192, 712, 306, 236, None);
    let out_buf = text::TextBuffer::default();
    out_view.set_buffer(out_buf.clone());
    style_display(&mut out_view);
    out_view.set_text_color(STRC);
    p_out.end();

    win.end();
    win.make_resizable(true);
    win.show();

    // ---------------------------------------------------------------- paint --
    let redraw: Rc<dyn Fn()> = {
        let ui = ui.clone();
        let (code_buf, code_style) = (code_buf.clone(), code_style.clone());
        let (asm_buf, asm_style) = (asm_buf.clone(), asm_style.clone());
        let (regs_buf, regs_style) = (regs_buf.clone(), regs_style.clone());
        let stack_buf = stack_buf.clone();
        let mem_buf = mem_buf.clone();
        let out_buf = out_buf.clone();
        let code_view = code_view.clone();
        let asm_view = asm_view.clone();
        let graph = graph.clone();
        let lbl_file = lbl_file.clone();
        Rc::new(move || {
            // FLTK handles are cheap references to the same widget; cloning
            // here is what lets this be an `Fn` closure that can be shared
            // by every callback.
            let (mut code_buf, mut code_style) = (code_buf.clone(), code_style.clone());
            let (mut asm_buf, mut asm_style) = (asm_buf.clone(), asm_style.clone());
            let (mut regs_buf, mut regs_style) = (regs_buf.clone(), regs_style.clone());
            let mut stack_buf = stack_buf.clone();
            let mut mem_buf = mem_buf.clone();
            let mut out_buf = out_buf.clone();
            let mut code_view = code_view.clone();
            let mut asm_view = asm_view.clone();
            let mut graph = graph.clone();
            let mut lbl_file = lbl_file.clone();

            let u = ui.borrow();
            let Some(p) = &u.prog else { return };
            lbl_file.set_label(&format!(
                "{}   {} functions   .text {:#x}..{:#x}",
                p.path,
                p.funcs.len(),
                p.text_range.0,
                p.text_range.1
            ));
            let Some(f) = u.func() else { return };

            let vars: Vec<String> = (0..f.frame.vars.len()).map(|i| u.var_label(f, i)).collect();
            let fns: Vec<String> = p.funcs.iter().map(|g| u.fname(&g.name)).collect();

            let mut txt = String::new();
            let mut sty = String::new();
            for (addr, line) in &u.code {
                let prefix = match addr {
                    Some(a) => format!("{:08x}  ", a),
                    None => " ".repeat(10),
                };
                sty.push_str(&"G".repeat(prefix.len()));
                sty.push_str(&style_line(line, &vars, &fns));
                sty.push('\n');
                txt.push_str(&prefix);
                txt.push_str(line);
                txt.push('\n');
            }
            code_buf.set_text(&txt);
            code_style.set_text(&sty);

            let mut atxt = String::new();
            let mut asty = String::new();
            for (a, t) in &u.asm {
                let head = format!("{:08x}  ", a);
                asty.push_str(&"G".repeat(head.len()));
                asty.push_str(&"A".repeat(t.len()));
                asty.push('\n');
                atxt.push_str(&head);
                atxt.push_str(t);
                atxt.push('\n');
            }
            asm_buf.set_text(&atxt);
            asm_style.set_text(&asty);

            let pc = u.emu.as_ref().and_then(|e| e.pc);
            let focus = pc.or(u.sel_addr);
            scroll_to(&mut code_view, &u.code.iter().map(|(a, _)| *a).collect::<Vec<_>>(), focus);
            scroll_to(
                &mut asm_view,
                &u.asm.iter().map(|(a, _)| Some(*a)).collect::<Vec<_>>(),
                focus,
            );

            if let Some(e) = &u.emu {
                let mut rt = String::new();
                let mut rs = String::new();
                for pair in SHOWN_REGS.chunks(2) {
                    for r in pair {
                        let name = format!("{:<4} ", r);
                        let val = format!("{:016x}   ", e.reg(r));
                        rs.push_str(&"B".repeat(name.len()));
                        rs.push_str(&(if e.changed(r) { "C" } else { "A" }).repeat(val.len()));
                        rt.push_str(&name);
                        rt.push_str(&val);
                    }
                    rt.push('\n');
                    rs.push('\n');
                }
                let tail = format!(
                    "\nrip  {}\nsteps {}   {}",
                    e.pc.map(|v| format!("{:016x}", v)).unwrap_or_else(|| "--".into()),
                    e.steps,
                    e.reason
                );
                rs.push_str(&"A".repeat(tail.len()));
                rt.push_str(&tail);
                regs_buf.set_text(&rt);
                regs_style.set_text(&rs);

                let (sp, bp) = (e.reg("rsp"), e.reg("rbp"));
                let mut st = String::new();
                for i in 0..14u64 {
                    let a = sp.wrapping_add(i * 8);
                    let mark = if i == 0 {
                        "rsp>"
                    } else if a == bp {
                        "rbp>"
                    } else {
                        "    "
                    };
                    st.push_str(&format!("{} {:012x}  {:016x}\n", mark, a, e.read(a, 8)));
                }
                stack_buf.set_text(&st);

                let base = sp & !0xf;
                let mut mt = String::new();
                for r in 0..13u64 {
                    let a = base.wrapping_add(r * 16);
                    let mut h = String::new();
                    let mut c = String::new();
                    for col in 0..16u64 {
                        let b = e.read(a + col, 1) as u8;
                        h.push_str(&format!("{:02x} ", b));
                        c.push(if (0x20..0x7f).contains(&b) { b as char } else { '.' });
                    }
                    mt.push_str(&format!("{:012x}  {}{}\n", a, h, c));
                }
                mem_buf.set_text(&mt);
                out_buf.set_text(if e.out.is_empty() { "(no output yet)" } else { &e.out });
            }
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
            let boxes = graph_layout(f, w.x() + 20, w.y() + 20);
            let pc = u.emu.as_ref().and_then(|e| e.pc);

            for b in &boxes {
                for (i, s) in b.succs.iter().enumerate() {
                    let Some(t) = boxes.iter().find(|x| x.addr == *s) else { continue };
                    draw::set_draw_color(if b.succs.len() == 2 {
                        if i == 0 {
                            Color::from_rgb(0x7f, 0xbf, 0x7f)
                        } else {
                            Color::from_rgb(0xbf, 0x7f, 0x7f)
                        }
                    } else {
                        DIM
                    });
                    draw::draw_line(b.x + b.w / 2, b.y + b.h, t.x + t.w / 2, t.y - 2);
                }
            }
            draw::set_font(Font::Courier, 11);
            for b in &boxes {
                let hot = u.sel_addr.map_or(false, |a| b.insns.contains(&a));
                let is_pc = pc.map_or(false, |a| b.insns.contains(&a));
                draw::set_draw_color(if is_pc { PCBG } else { PANEL2 });
                draw::draw_rectf(b.x, b.y, b.w, b.h);
                draw::set_draw_color(if is_pc {
                    STRC
                } else if hot {
                    ACCENT
                } else {
                    LINE
                });
                draw::draw_rect(b.x, b.y, b.w, b.h);
                draw::set_draw_color(ACCENT);
                draw::draw_text(&format!("{:#x}", b.addr), b.x + 8, b.y + 15);
                draw::set_draw_color(FG);
                for (i, t) in b.text.iter().enumerate() {
                    draw::draw_text(t, b.x + 8, b.y + 15 + 13 * (i as i32 + 1));
                }
            }
        });
    }
    {
        let ui = ui.clone();
        let redraw = redraw.clone();
        graph.handle(move |w, ev| {
            if ev == Event::Push {
                let (mx, my) = app::event_coords();
                let hit = {
                    let u = ui.borrow();
                    u.func().and_then(|f| {
                        graph_layout(f, w.x() + 20, w.y() + 20)
                            .into_iter()
                            .find(|b| mx >= b.x && mx <= b.x + b.w && my >= b.y && my <= b.y + b.h)
                            .and_then(|b| b.insns.first().copied())
                    })
                };
                if let Some(a) = hit {
                    ui.borrow_mut().sel_addr = Some(a);
                    redraw();
                    return true;
                }
            }
            false
        });
    }

    // ------------------------------------------------------------ callbacks --
    let reload: Rc<dyn Fn()> = {
        let ui = ui.clone();
        let fn_list = fn_list.clone();
        let redraw = redraw.clone();
        Rc::new(move || {
            let mut fn_list = fn_list.clone();
            {
                let mut u = ui.borrow_mut();
                u.rebuild();
                let e = u.prog.as_ref().map(|p| Emu::new(p, u.cur));
                u.emu = e;
            }
            fn_list.clear();
            {
                let u = ui.borrow();
                if let Some(p) = &u.prog {
                    for f in &p.funcs {
                        fn_list.add(&format!("{}   @{:x}", u.fname(&f.name), f.addr));
                    }
                    fn_list.select(u.cur as i32 + 1);
                }
            }
            redraw();
        })
    };

    {
        let ui = ui.clone();
        let reload = reload.clone();
        b_open.set_callback(move |_| {
            let mut c = dialog::NativeFileChooser::new(dialog::NativeFileChooserType::BrowseFile);
            c.show();
            let path = c.filename();
            if path.as_os_str().is_empty() {
                return;
            }
            match analysis::analyze_file(&path.to_string_lossy()) {
                Ok(p) => {
                    let mut u = ui.borrow_mut();
                    u.prog = Some(p);
                    u.cur = 0;
                    u.sel_addr = None;
                    u.sel_var = None;
                }
                Err(e) => {
                    dialog::alert_default(&e);
                    return;
                }
            }
            reload();
        });
    }

    {
        let ui = ui.clone();
        let reload = reload.clone();
        fn_list.set_callback(move |b| {
            let i = b.value();
            if i > 0 {
                let mut u = ui.borrow_mut();
                u.cur = i as usize - 1;
                u.sel_addr = None;
                u.sel_var = None;
                drop(u);
                reload();
            }
        });
    }

    {
        let ui = ui.clone();
        let redraw = redraw.clone();
        b_cast.set_callback(move |b| {
            {
                let mut u = ui.borrow_mut();
                u.no_cast = !u.no_cast;
                b.set_label(if u.no_cast { "Casts: off" } else { "Casts: on" });
                u.rebuild();
            }
            redraw();
        });
    }

    {
        let ui = ui.clone();
        let redraw = redraw.clone();
        code_view.handle(move |v, ev| {
            if ev == Event::Released {
                let row = row_at(v, app::event_y());
                {
                    let mut u = ui.borrow_mut();
                    if let Some((addr, _)) = u.code.get(row) {
                        u.sel_addr = *addr;
                        u.sel_var = u.var_on_line(row);
                    }
                }
                redraw();
            }
            false
        });
    }
    {
        let ui = ui.clone();
        let redraw = redraw.clone();
        asm_view.handle(move |v, ev| {
            if ev == Event::Released {
                let row = row_at(v, app::event_y());
                {
                    let mut u = ui.borrow_mut();
                    if let Some((addr, _)) = u.asm.get(row) {
                        u.sel_addr = Some(*addr);
                    }
                }
                redraw();
            }
            false
        });
    }

    let do_rename: Rc<dyn Fn()> = {
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
    {
        let d = do_rename.clone();
        b_rename.set_callback(move |_| d());
    }

    let do_type: Rc<dyn Fn()> = {
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
    {
        let d = do_type.clone();
        b_type.set_callback(move |_| d());
    }

    {
        let ui = ui.clone();
        let reload = reload.clone();
        b_struct.set_callback(move |_| {
            let existing: Vec<String> =
                ui.borrow().structs().defs.iter().map(|d| d.name.clone()).collect();
            let prompt = format!(
                "Existing: {}\nNew struct name:",
                if existing.is_empty() { "none".to_string() } else { existing.join(", ") }
            );
            let Some(name) = dialog::input_default(&prompt, "my_struct") else { return };
            let name = name.trim().to_string();
            if !valid_identifier(&name) {
                dialog::alert_default("not a valid identifier");
                return;
            }
            let Some(body) =
                dialog::input_default("Fields, semicolon separated: offset type name", "0 int x; 4 int y; 8 long tag")
            else {
                return;
            };
            match parse_struct(&name, &body) {
                Some(d) => {
                    let mut u = ui.borrow_mut();
                    u.user_structs.retain(|x| x.name != d.name);
                    u.user_structs.push(d);
                }
                None => {
                    dialog::alert_default("could not parse the field list");
                    return;
                }
            }
            reload();
        });
    }

    let do_reset: Rc<dyn Fn()> = {
        let ui = ui.clone();
        let redraw = redraw.clone();
        Rc::new(move || {
            {
                let mut u = ui.borrow_mut();
                let e = u.prog.as_ref().map(|p| Emu::new(p, u.cur));
                u.emu = e;
                u.sel_addr = u.emu.as_ref().and_then(|e| e.pc);
            }
            redraw();
        })
    };
    {
        let d = do_reset.clone();
        b_reset.set_callback(move |_| d());
    }

    let do_step: Rc<dyn Fn()> = {
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
    {
        let d = do_step.clone();
        b_step.set_callback(move |_| d());
    }

    let do_run: Rc<dyn Fn()> = {
        let ui = ui.clone();
        let redraw = redraw.clone();
        Rc::new(move || {
            {
                let mut u = ui.borrow_mut();
                let Ui { prog: Some(p), emu: Some(e), .. } = &mut *u else { return };
                e.run(p, 5_000_000);
                let pc = e.pc;
                u.sel_addr = pc;
            }
            redraw();
        })
    };
    {
        let d = do_run.clone();
        b_run.set_callback(move |_| d());
    }

    {
        let mut cv = code_view.clone();
        let mut gs = graph_scroll.clone();
        let mut a = t_code.clone();
        let mut b = t_graph.clone();
        t_code.set_callback(move |_| {
            cv.show();
            gs.hide();
            a.set_color(HOT);
            b.set_color(PANEL2);
            app::redraw();
        });
    }
    {
        let mut cv = code_view.clone();
        let mut gs = graph_scroll.clone();
        let mut a = t_code.clone();
        let mut b = t_graph.clone();
        t_graph.set_callback(move |_| {
            cv.hide();
            gs.show();
            b.set_color(HOT);
            a.set_color(PANEL2);
            app::redraw();
        });
    }

    {
        let (r, t, s, run, reset) =
            (do_rename.clone(), do_type.clone(), do_step.clone(), do_run.clone(), do_reset.clone());
        win.handle(move |_, ev| {
            if ev == Event::KeyDown {
                match app::event_key() {
                    Key::F2 => {
                        r();
                        true
                    }
                    Key::F3 => {
                        t();
                        true
                    }
                    Key::F5 => {
                        reset();
                        true
                    }
                    Key::F7 | Key::F8 => {
                        s();
                        true
                    }
                    Key::F9 => {
                        run();
                        true
                    }
                    _ => false,
                }
            } else {
                false
            }
        });
    }

    // dpp-gui <binary> [function] -- naming a function opens straight to it
    if let Some(path) = std::env::args().nth(1) {
        match analysis::analyze_file(&path) {
            Ok(p) => {
                let want = std::env::args().nth(2);
                let mut u = ui.borrow_mut();
                if let Some(n) = want {
                    u.cur = p.funcs.iter().position(|f| f.name == n).unwrap_or(0);
                }
                u.prog = Some(p);
                drop(u);
                reload();
            }
            Err(e) => eprintln!("{}", e),
        }
    }

    app.run().unwrap();
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

fn tool_button(x: i32, y: i32, w: i32, label: &str) -> button::Button {
    let mut b = button::Button::new(x, y, w, 26, None).with_label(label);
    b.set_color(PANEL2);
    b.set_label_color(FG);
    b.set_frame(FrameType::FlatBox);
    b.set_label_size(12);
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
fn graph_layout(f: &Analyzed, ox: i32, oy: i32) -> Vec<Box2> {
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
            let text: Vec<String> =
                b.instrs.iter().map(|&i| f.insns[i].asm_text.clone()).collect();
            let widest = text.iter().map(|t| t.len()).max().unwrap_or(10).max(12);
            let w = (widest as i32 * 7 + 24).min(500);
            let h = 20 + 13 * (text.len() as i32 + 1);
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
