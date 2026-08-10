// rename.rs — Stage 3 AI-assisted naming: turn `sub_401660(a1, a2)` with
// locals `v1, v2, v3` into something a human would actually write, by
// showing the decompiled C to whichever AI provider is configured (OpenAI,
// Anthropic, Gemini, Qwen, or a custom OpenAI-compatible endpoint — see
// `crate::ai`) and asking for better names.
//
// This is deliberately the *last* stage of the pipeline (see the roadmap:
// deterministic sanitisation -> symbolic execution -> AI). It never changes
// behaviour, never reruns analysis, and never touches anything the earlier,
// deterministic stages already named (FLIRT-matched library functions, or a
// symbol table that already gave a function a real name). It only proposes
// names for the two patterns the rest of the pipeline gives up on:
// `sub_<addr>` functions and the default `a<N>` / `v<N>` variables.
//
// Renaming is applied to rendered text, the same way the GUI's manual F2
// rename works (see gui.rs) — never baked back into the `Analyzed` struct.
// That keeps this pass a pure post-process: if the model is wrong, or the
// API key isn't set, the ordinary deterministic output is untouched.

use crate::ai::{AiClient, AiError};
use crate::analysis::Analyzed;
use crate::ir::{Expr, Stmt};
use std::collections::{HashMap, HashSet};
use std::fmt;

// ---------------------------------------------------------------- errors

#[derive(Debug)]
pub enum RenameError {
    Ai(AiError),
    /// the model's reply wasn't a JSON object we could make sense of
    Parse(String),
}

impl fmt::Display for RenameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RenameError::Ai(e) => write!(f, "{e}"),
            RenameError::Parse(s) => write!(f, "couldn't parse the model's reply: {s}"),
        }
    }
}

impl std::error::Error for RenameError {}

impl From<AiError> for RenameError {
    fn from(e: AiError) -> Self {
        RenameError::Ai(e)
    }
}

// ------------------------------------------------------------- one reply

/// What the model proposed for a single function, before validation.
#[derive(Default, Debug)]
pub struct RenameSuggestion {
    /// `None` when the model had nothing better than `sub_XXXXXX`.
    pub function_name: Option<String>,
    /// old identifier (`a1`, `v3`, ...) -> proposed identifier
    pub variables: HashMap<String, String>,
}

// ------------------------------------------------------------ identifiers

const C_KEYWORDS: &[&str] = &[
    "auto", "break", "case", "char", "const", "continue", "default", "do", "double", "else",
    "enum", "extern", "float", "for", "goto", "if", "inline", "int", "long", "register",
    "restrict", "return", "short", "signed", "sizeof", "static", "struct", "switch", "typedef",
    "union", "unsigned", "void", "volatile", "while", "_Bool", "_Complex", "_Imaginary", "main",
];

/// A name we're willing to actually emit into C source: starts with a
/// letter or underscore, everything after is alphanumeric or underscore,
/// isn't empty, isn't absurdly long, and isn't a keyword.
fn is_usable_ident(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else { return false };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return false;
    }
    if s.len() > 48 {
        return false;
    }
    !C_KEYWORDS.contains(&s)
}

/// True for the exact default-name shapes `Frame::build` hands out: `a<N>`
/// (register parameter), `v<N>` (local), `sa<N>` (stack-passed parameter).
/// Anything else — including a name a previous pass already renamed —
/// is left alone.
fn is_default_var_name(s: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if let Some(rest) = s.strip_prefix("sa") {
        return digits(rest);
    }
    if let Some(rest) = s.strip_prefix('a').or_else(|| s.strip_prefix('v')) {
        return digits(rest);
    }
    false
}

/// Picks `base`, or `base_2`, `base_3`, ... — whichever isn't already in
/// `used` — and records the winner in `used` before returning it.
fn dedupe(base: &str, used: &mut HashSet<String>) -> String {
    if used.insert(base.to_string()) {
        return base.to_string();
    }
    let mut n = 2;
    loop {
        let candidate = format!("{base}_{n}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
        n += 1;
    }
}

// -------------------------------------------------------------- prompting

/// Functions this one calls, in source order, deduped. Used as extra
/// grounding for the model — "calls malloc, memcpy, free" says a lot more
/// than the mangled name of a stack slot ever will.
fn callees_of(a: &Analyzed) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let mut visit = |e: &Expr| {
        if let Expr::Call { name, .. } = e {
            if seen.insert(name.clone()) {
                out.push(name.clone());
            }
        }
    };
    for insn in &a.insns {
        for st in &insn.stmts {
            match st {
                Stmt::Assign { dst, src } => {
                    dst.walk(&mut visit);
                    src.walk(&mut visit);
                }
                Stmt::Do(e) | Stmt::If { cond: e, .. } => e.walk(&mut visit),
                Stmt::Return(Some(e)) => e.walk(&mut visit),
                _ => {}
            }
        }
    }
    out
}

const SYSTEM_PROMPT: &str = r#"You are the AI-assisted naming stage of a reverse-engineering decompiler. You are shown decompiled C pseudocode for one function recovered from a stripped binary. Its real name and its variables' real names are gone; they were auto-generated as sub_<address>, a1/a2/... (parameters) and v1/v2/... (locals).

Infer what the function and its variables are actually for, from the code alone, and reply with ONLY a single JSON object — no prose, no markdown fences, nothing before or after it. Shape:

{"function_name": "lower_snake_case_name_or_null", "variables": {"a1": "new_name", "v3": "new_name"}}

Rules:
- function_name is a short lower_snake_case C identifier describing what the function does, or JSON null if the code gives you nothing to go on beyond what "sub_..." already says.
- variables maps OLD identifiers (must be exactly one of the a<N>/v<N> names that appear in the code you were shown) to NEW lower_snake_case identifiers. Omit any variable you aren't reasonably confident about — an omitted variable simply keeps its current name. Do not invent entries for names that don't appear in the code.
- Every identifier you propose must be a valid, ordinary C identifier and must not restate the parameter/local number (don't propose "a1" -> "a1_buffer", that's not an improvement).
- Base every guess only on what the code does: control flow, called functions, constants, and any string literals visible in the text. Do not assume a CTF flag format or invent a purpose that isn't supported by the code.
- If this genuinely looks like generic/utility code with no recoverable purpose, return {"function_name": null, "variables": {}} rather than guessing."#;

fn build_user_prompt(name: &str, callees: &[String], code: &str) -> String {
    let callee_line =
        if callees.is_empty() { "(calls nothing)".to_string() } else { callees.join(", ") };
    format!("Function: {}\nCalls: {}\n\n```c\n{}\n```", name, callee_line, code)
}

/// Pulls the first top-level `{ ... }` JSON object out of `raw`, tolerating
/// the markdown code fences models add despite being told not to.
fn extract_json_object(raw: &str) -> Option<&str> {
    let start = raw.find('{')?;
    let bytes = raw.as_bytes();
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&raw[start..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

fn parse_reply(raw: &str) -> Result<RenameSuggestion, RenameError> {
    let obj_text = extract_json_object(raw)
        .ok_or_else(|| RenameError::Parse(format!("no JSON object found in: {raw}")))?;
    let value: serde_json::Value = serde_json::from_str(obj_text)
        .map_err(|e| RenameError::Parse(format!("{e}: {obj_text}")))?;

    let function_name = value
        .get("function_name")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let mut variables = HashMap::new();
    if let Some(map) = value.get("variables").and_then(|v| v.as_object()) {
        for (old, new) in map {
            if let Some(new) = new.as_str() {
                variables.insert(old.clone(), new.trim().to_string());
            }
        }
    }

    Ok(RenameSuggestion { function_name, variables })
}

/// Asks the configured AI provider to name one function's return value and locals from its
/// rendered C text. `code` should be the function's full rendered body
/// (signature, declarations, and all) — e.g. from `analysis::render`.
pub fn suggest_names(
    client: &AiClient,
    name: &str,
    callees: &[String],
    code: &str,
) -> Result<RenameSuggestion, RenameError> {
    let user = build_user_prompt(name, callees, code);
    // A naming call has a whole function's worth of control flow to weigh,
    // not a one-line lookup, so it earns a step above the client's default
    // "low" effort.
    let reply = client.ask_with_effort(SYSTEM_PROMPT, &user, "medium")?;
    parse_reply(&reply.text)
}

// ------------------------------------------------------------------ plan

/// The accumulated, validated result of running `suggest_names` over some
/// subset of a program's functions: a global rename for every accepted
/// function name (function names are visible from every call site, so they
/// must be unique across the whole program), plus, per function, a local
/// rename for that function's own variables.
#[derive(Default)]
pub struct RenamePlan {
    pub fn_renames: HashMap<String, String>,
    pub var_renames: HashMap<String, HashMap<String, String>>,
}

impl RenamePlan {
    /// Validates one function's raw suggestion against the function it was
    /// generated from and folds it into the plan, deduping against every
    /// name already accepted. Silently drops anything unusable — a
    /// suggestion that fails validation just means that identifier keeps
    /// its deterministic `sub_...`/`a<N>`/`v<N>` name.
    ///
    /// Returns exactly what got applied for this one function (its accepted
    /// function rename, if any, and its accepted variable renames) so a
    /// caller driving this incrementally — e.g. the GUI, one function at a
    /// time off a background thread — can apply just that slice immediately
    /// instead of waiting for the whole plan.
    fn accept(
        &mut self,
        fn_name: &str,
        existing_vars: &[String],
        suggestion: RenameSuggestion,
        used_fn_names: &mut HashSet<String>,
    ) -> (Option<String>, HashMap<String, String>) {
        let mut fn_rename = None;
        if let Some(new_name) = suggestion.function_name {
            if is_usable_ident(&new_name) && new_name != fn_name {
                let final_name = dedupe(&new_name, used_fn_names);
                self.fn_renames.insert(fn_name.to_string(), final_name.clone());
                fn_rename = Some(final_name);
            }
        }

        let existing: HashSet<&str> = existing_vars.iter().map(String::as_str).collect();
        let mut used_var_names: HashSet<String> = existing_vars.iter().cloned().collect();
        let mut accepted = HashMap::new();
        for (old, new) in suggestion.variables {
            // only ever rename a variable that's still at its default name
            // and actually belongs to this function
            if !is_default_var_name(&old) || !existing.contains(old.as_str()) {
                continue;
            }
            if !is_usable_ident(&new) || new == old {
                continue;
            }
            let final_name = dedupe(&new, &mut used_var_names);
            accepted.insert(old, final_name);
        }
        if !accepted.is_empty() {
            self.var_renames.insert(fn_name.to_string(), accepted.clone());
        }
        (fn_rename, accepted)
    }
}

/// True for the exact case this stage exists to help with: a function the
/// earlier, deterministic stages (symbol table, FLIRT) never managed to
/// name. Already-named functions — library matches, debug symbols, `main`
/// — are left alone.
pub fn needs_naming(a: &Analyzed) -> bool {
    !a.is_lib && a.name.starts_with("sub_")
}

/// Everything the naming pass needs for one function, pulled out of
/// `Analyzed` up front. This is what actually gets handed to a background
/// thread: it's plain owned data (`String`/`u64`/`Vec`), so it's `Send`
/// without requiring `Analyzed` itself — which holds borrowed-once analysis
/// state that lives behind the GUI's `RefCell` — to cross a thread boundary.
pub struct AiTarget {
    pub name: String,
    pub addr: u64,
    callees: Vec<String>,
    code: String,
    existing_vars: Vec<String>,
}

/// Builds an [`AiTarget`] for one function. `code` is that function's
/// rendered C text (e.g. from `analysis::render`) — rendering isn't free,
/// so the caller only does it for functions that `needs_naming`.
pub fn make_target(a: &Analyzed, code: String) -> AiTarget {
    AiTarget {
        name: a.name.clone(),
        addr: a.addr,
        callees: callees_of(a),
        code,
        existing_vars: a.frame.vars.iter().map(|v| v.name.clone()).collect(),
    }
}

/// Runs the AI naming pass over every target in `targets` (every one of
/// which should already have passed `needs_naming` — see [`make_target`]).
/// `on_start` fires right before each request goes out (1-based `index` of
/// `total`), so a caller can show which function is currently in flight;
/// `on_result` fires after, with the `(function_rename, variable_renames)`
/// that were actually accepted for that one function on success, so a
/// caller can apply names incrementally instead of waiting for the whole
/// pass to finish.
///
/// One request per target; a failure on any single function (network
/// error, unparseable reply) is skipped rather than aborting the whole
/// pass, so a flaky call doesn't cost every other function's names.
///
/// Takes only owned data (`&[AiTarget]`, not `&[&Analyzed]`), so this is
/// safe to call from a background thread — the GUI does exactly that, to
/// keep the slow network round-trips off the UI thread.
pub fn build_plan(
    client: &AiClient,
    targets: &[AiTarget],
    mut on_start: impl FnMut(&AiTarget, usize, usize),
    mut on_result: impl FnMut(
        &AiTarget,
        usize,
        usize,
        Result<(Option<String>, HashMap<String, String>), &RenameError>,
    ),
) -> RenamePlan {
    let mut plan = RenamePlan::default();
    let total = targets.len();
    let mut used_fn_names: HashSet<String> =
        targets.iter().map(|t| t.name.clone()).filter(|n| !n.starts_with("sub_")).collect();

    for (i, t) in targets.iter().enumerate() {
        let index = i + 1;
        on_start(t, index, total);
        match suggest_names(client, &t.name, &t.callees, &t.code) {
            Ok(suggestion) => {
                let delta = plan.accept(&t.name, &t.existing_vars, suggestion, &mut used_fn_names);
                on_result(t, index, total, Ok(delta));
            }
            Err(e) => on_result(t, index, total, Err(&e)),
        }
    }
    plan
}

// --------------------------------------------------------- text rewriting

/// Whole-word identifier substitution: replaces every standalone
/// occurrence of an old identifier with its new one, without touching
/// identifiers that merely contain it (`v1` inside `v10`, `sub_1000` inside
/// `sub_10000`, etc.). Mirrors the GUI's manual-rename text pass so both
/// paths behave the same way.
///
/// Byte-indexed rather than char-indexed for speed, but every slice it
/// takes lands on a UTF-8 boundary: `is_ident` only ever matches ASCII
/// bytes, and an ASCII byte can never be a continuation byte of a
/// multi-byte sequence, so a non-identifier run (which may contain
/// multi-byte text, e.g. inside a recovered string literal) is always
/// copied out as one slice rather than reassembled byte by byte.
pub fn replace_identifiers(text: &str, pairs: &HashMap<String, String>) -> String {
    if pairs.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let is_ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut i = 0;
    while i < bytes.len() {
        if is_ident(bytes[i]) && (i == 0 || !is_ident(bytes[i - 1])) {
            let mut j = i;
            while j < bytes.len() && is_ident(bytes[j]) {
                j += 1;
            }
            let word = &text[i..j];
            out.push_str(pairs.get(word).map(String::as_str).unwrap_or(word));
            i = j;
        } else {
            let mut j = i + 1;
            while j < bytes.len() && !is_ident(bytes[j]) {
                j += 1;
            }
            out.push_str(&text[i..j]);
            i = j;
        }
    }
    out
}

/// Applies a plan to one function's already-rendered text: that function's
/// own variable renames (local to its text) plus every accepted function
/// rename in the whole plan (function names are visible from any caller's
/// text, including this one, since they may show up in call expressions).
pub fn apply_plan(fn_name: &str, text: &str, plan: &RenamePlan) -> String {
    let mut pairs = plan.var_renames.get(fn_name).cloned().unwrap_or_default();
    for (old, new) in &plan.fn_renames {
        pairs.insert(old.clone(), new.clone());
    }
    replace_identifiers(text, &pairs)
}
