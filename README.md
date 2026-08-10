# decompiler++

A native x86-64 decompiler written in Rust that turns stripped binary executables back into readable C pseudocode. It lifts machine instructions to an IR, propagates types and values, recovers control flow structure, and prints the result as valid-looking C. It also ships a GUI with a disassembly view, an execution stepper, cross-references, a CFG graph, and a memory viewer.

---

## Quick start

```bash
# CLI — decompile to stdout
cargo run --release --bin mini_decompiler -- /path/to/binary

# GUI
cargo run --release --features gui --bin dpp-gui -- /path/to/binary

# Apply a custom FLIRT .sig or .pat file
cargo run --release --bin mini_decompiler -- --sig /path/to/libc.sig /path/to/binary
```

---

## How it works

### Pipeline

```
ELF/PE/Mach-O
   │
   ▼
[loader / analysis]   — find .text, resolve PLT stubs, detect functions
   │
   ▼
[FLIRT matching]      — rename libc / CRT / libgcc functions (see below)
   │
   ▼
[lifter]              — x86-64 instructions → IR expression trees
   │
   ▼
[frame]               — stack slots → typed locals, parameter detection
   │
   ▼
[simplify]            — copy propagation, dead-code elimination, folding
   │
   ▼
[idiom]               — undo compiler arithmetic rewrites (div, mod, shift)
   │
   ▼
[ptr]                 — pointer, struct, and array recovery
   │
   ▼
[cfg]                 — dominators, loops, structuring (while / for / if-else)
   │
   ▼
[cgen]                — precedence-aware C printer
```

| Module     | Responsibility |
|------------|----------------|
| `analysis` | ELF/PE parsing, PLT/GOT resolution, two-pass analysis |
| `lifter`   | One instruction → IR statements; structural, never textual |
| `frame`    | Stack slots to typed locals; arrays; parameters |
| `simplify` | Copy propagation, call argument inference, folding, DCE |
| `idiom`    | Undoes compiler arithmetic rewrites |
| `ptr`      | Pointer, struct, and array type recovery |
| `cfg`      | Blocks, dominators, post-dominators, loops, structuring |
| `cgen`     | Precedence-aware C printer |
| `emu`      | Interpreter over the lifted IR |
| `flirt`    | FLIRT signature matching (see below) |

---

## Function Identification (FLIRT)

One of the biggest challenges with stripped static binaries is that every function is named `sub_XXXXXX`. This decompiler uses **FLIRT** (Fast Library Identification and Recognition Technology) to automatically identify and rename thousands of standard library functions.

### How FLIRT matching works

FLIRT works by comparing the raw bytes of a function in the binary against a database of pre-computed patterns. Each pattern:
- Covers the first 32–256 bytes of a function
- Has **wildcard slots** where the compiler wrote a memory address (e.g., a `call` target or a `.rodata` pointer). These addresses differ between binaries, so they are masked out and match anything.

If a function's byte sequence matches a pattern, it is renamed to the corresponding library function name.

### Compile-time signature generation

Rather than downloading a signature database (which may be outdated or for the wrong libc version), this decompiler **generates signatures at compile time** directly from your system's toolchain. When you run `cargo build`, the `build.rs` script:

1. **Locates your system's static libraries** using `gcc -print-file-name=...`
2. **Parses every `.o` file** inside each archive using the `object` crate
3. **Extracts every function** — both public (`global`) and internal (`local/static`) ones
4. **Reads the ELF relocation table** to find which bytes are addresses, and **wildcards those bytes** so the pattern works on any binary
5. **Removes collisions** — if two functions have the same byte pattern, neither is used (it would be ambiguous)
6. **Serialises the result** to `$OUT_DIR/libc_sigs.bin` which is embedded into the decompiler binary with `include_bytes!`

**Sources scanned at compile time:**

| Source | What it contains |
|--------|-----------------|
| `libc.a` | Full C standard library: `printf`, `malloc`, `strlen`, `__vfprintf_internal`, `_itoa_word`, and ~4,000 more |
| `libgcc.a` | Compiler runtime: `__divdi3`, `__moddi3`, `__muldi3`, `__floatdidf`, etc. |
| `crt1.o` | `_start` — the real ELF entry point |
| `crti.o` / `crtn.o` | `.init` / `.fini` section frame functions |
| `crtbeginT.o` / `crtend.o` | C++ constructor and destructor registration |

This means:
- **End users don't need `gcc` or any toolchain installed** — the patterns are already baked into the binary
- **Signatures always match your exact libc version** — no version mismatch problems
- **Rebuilding picks up updates automatically** — `cargo:rerun-if-changed` tracks `libc.a`

### Adding a new architecture

The signature generation is architecture-agnostic. To add ARM64 support, for example:

1. **`build.rs`** — call `gcc_find()` with the cross-compiler prefix (`aarch64-linux-gnu-gcc`) and write a separate `libc_sigs_arm64.bin`
2. **`default_sigs.rs`** — add a new `include_bytes!` for that blob and a `load_libc_sigs_arm64()` function
3. **`analysis.rs`** — match on `obj.architecture()` and select the right sig set

Each addition is ~20 lines of code. The hard work (archive parsing, pattern generation, deduplication, embedding) is fully reusable.

---

## What it recovers

**Variables.** Stack slots become typed locals sized by the gap to the next slot.

**Parameters.** From prologue spills at `-O0`, and from argument registers read before they are written at `-O1` and above.

**Structs and arrays.** Two mechanisms:
- Addressing-mode analysis in `frame` handles aggregates that live *in* the frame
- Expression analysis in `ptr` handles aggregates reached *through* a pointer

**Compiler idioms.** Compilers never emit `idiv` for a constant divisor. `idiom` checks the algebraic identity rather than matching a specific instruction sequence:

| Source | Before idiom pass | After |
|--------|-------------------|-------|
| `x / 3` | `(int)((long)a1 * 0x55555556 >> 32) - (a1 >> 31)` | `a1 / 3` |
| `x % 7` | `a1 - ((...* -0x6db6db6d >> 32) + a1 >> 2) * 8 - (...)` | `a1 % 7` |
| `x / 8` | `(a1 >= 0 ? a1 : a1 + 7) >> 3` | `a1 / 8` |

**Calls.** PLT entries, `.o` relocations, and GOT-indirect targets are resolved to names.

**Control flow.** Natural loops as `while`, `do`/`while`, and `for(;;)`, with `break` and `continue` rather than gotos. Merge points from the immediate post-dominator produce `if`/`else`.

**Vector code.** Packed SSE instructions lift to named intrinsics (`_mm_add_epi32`, etc.) rather than opaque assembly, so they participate in propagation and DCE.

---

## GUI features

- **Pseudocode view** with inline renaming and retyping
- **Disassembly** with syntax colouring and jump rails
- **CFG graph** showing either instructions or pseudocode per block; draggable
- **Execution stepper** that interprets the lifted IR, not raw bytes
- **Cross-references** (`Shift+F12`) for calls, branches, and data references
- **Strings panel** with live filtering
- **Memory map** showing every mapped range with permissions
- **Register/stack annotation** — values are looked up in the memory map and reported as what they actually are
- **Breakpoints** set with F4 or right-click, shown in both pseudocode and disassembly gutters
- **Stdin input** — type in the bottom field to feed the emulated program

Any bottom panel can be detached into its own window.

---

## File formats

ELF, PE/COFF, and Mach-O are all read through the same loader — executables, shared libraries, and `.o` files. Only x86-64 is lifted; other architectures are refused by name rather than misdecompiled.

---

## Known limits

- Full devectorisation is not attempted. GCC's vectorised loops read as intrinsics and unrolled loops, not the original source.
- Optimised loops that GCC has rotated leave residual registers and a few unreachable blocks (emitted as labelled `goto`s).
- No switch / jump-table recovery; an indirect jump ends the block.
- Floating-point lifts but is barely typed — SSE registers print as `double` regardless of `ss`/`sd` form.
- The viewer does not save renames or renamings to disk.

---

## AI-assisted naming (Stage 3, opt-in)

For functions that survive every deterministic stage — no symbol, no FLIRT
match, still sitting there as `sub_401660(a1, a2)` — `--ai-rename` sends the
decompiled C to an AI provider and asks for better names.

Five providers are supported, all through one client (`src/ai.rs`):

| Provider | `AI_PROVIDER` value | API key env var | Default model |
|---|---|---|---|
| OpenAI (ChatGPT) | `openai` | `OPENAI_API_KEY` | `gpt-5-mini` |
| Anthropic (Claude) | `anthropic` | `ANTHROPIC_API_KEY` | `claude-sonnet-4-6` |
| Google (Gemini) | `gemini` | `GEMINI_API_KEY` | `gemini-3.6-flash` |
| Alibaba (Qwen / DashScope) | `qwen` | `DASHSCOPE_API_KEY` | `qwen3.8-max` |
| Custom (any OpenAI-compatible endpoint) | `custom` | `CUSTOM_API_KEY` | *(must be set — see below)* |

Provider lineups move fast — if a default model above gets deprecated or
retired, set `OPENAI_MODEL`/`ANTHROPIC_MODEL`/`GEMINI_MODEL`/`QWEN_MODEL`
(or the model field in **File > AI Settings…**) to whatever's current;
there's no need to rebuild.

Pick a provider with `AI_PROVIDER`, set its key, and run:

```bash
export AI_PROVIDER=openai
export OPENAI_API_KEY=sk-...
cargo run --release --bin mini_decompiler -- --ai-rename /path/to/binary
```

If `AI_PROVIDER` isn't set, the client falls back to whatever provider was
last chosen in the GUI's **File > AI Settings…** dialog, then to `qwen` (the
original default, kept for backwards compatibility with existing setups).

Every provider also has a `*_MODEL` and `*_BASE_URL` override
(`OPENAI_MODEL`, `OPENAI_BASE_URL`, `ANTHROPIC_MODEL`, ..., `CUSTOM_MODEL`,
`CUSTOM_BASE_URL`) for pointing at a newer model or a self-hosted /
alternative endpoint without a rebuild. `custom` has no built-in base URL or
model — those two must come from the env vars or the Settings dialog, since
"custom" only means anything with a user-supplied OpenAI-compatible
endpoint (e.g. a local Ollama/vLLM server, Groq, DeepSeek, etc).

In the GUI, **File > AI Settings…** lets you pick a provider, paste in its
key (masked, saved to `~/.config/dpp-gui/config.toml`, chmod 600), and
optionally override its model/base-URL — all without touching environment
variables. A key saved this way is only ever used as a fallback for when
the matching env var isn't already set, so scripted/CI usage is unaffected.
Sanity-check any provider's connection with:

```bash
cargo run --bin ai_test -- "what CPU architecture is a Harvard architecture typically paired with?"
```

What `--ai-rename` does, precisely:

- Only targets functions matching `sub_XXXXXX` that weren't already named by
  FLIRT or the symbol table (`rename::needs_naming`); everything else is
  left exactly as the deterministic pipeline produced it.
- One request per eligible function, with the function's callee list
  included as extra grounding. A failed or unparseable reply is skipped
  (with a note on stderr) rather than aborting the run — one flaky call
  doesn't cost every other function's names.
- Every proposed identifier is validated (real C identifier, not a
  keyword, not a no-op) and deduped against everything else already
  accepted before it's used — function names program-wide (they're visible
  from every call site), variable names within their own function.
- Renaming is applied to the *rendered* C text, never baked back into the
  analysis — the same approach the GUI's manual F2 rename already uses —
  so a bad suggestion never corrupts anything the deterministic stages
  produced.

This is deliberately the tool's last resort, per the taxonomy this project
is built from: deterministic sanitisation first, symbolic execution second,
AI only for what's left. See `src/rename.rs` for the implementation and
`src/ai.rs` for the underlying multi-provider client.
