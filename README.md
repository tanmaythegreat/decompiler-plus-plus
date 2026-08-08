# mini_decompiler

Toy x86-64 → C-pseudocode decompiler, built as a small proof-of-concept
of the architecture described in the accompanying roadmap doc.

Pipeline: ELF/PE file (`object` crate) → per-function byte range →
disassembly (`iced-x86`) → lift to a small p-code-style IR (`src/ir.rs`,
`src/lifter.rs`) → basic blocks + dominators + back-edge/if-diamond
detection → structured `while`/`if`/`else` pseudocode with `goto`
fallback for anything unrecognized (`src/cfg.rs`).

## Build & run

```
cargo build --release
./target/release/mini_decompiler /path/to/binary [max_functions]
```

Works on ELF binaries/objects (`.o`, statically or dynamically linked
executables). Tested against gcc -O0 output: correctly recovers
straight-line functions, if/else, and counting `while` loops.

## Scope / limitations (matches doc's "Important Limitations" section)

- x86-64 only, ~20 mnemonics lifted (mov/lea, add/sub/and/or/xor/imul/
  shl/shr, cmp/test, jcc, jmp, call, ret, push/pop/nop as no-ops).
- No SSA, no real type inference, no struct/array recovery — everything
  is untyped registers/memory expressions.
- Structuring handles simple diamonds and single back-edge loops;
  anything more irregular (switch/jump tables, nested/interleaved
  control flow, obfuscated CFGs) falls back to `if (cond) goto ...`.
- No inlining beyond raw call targets. PLT-based calls in dynamically
  linked binaries *are* resolved to real names (`printf`, `malloc`, ...)
  by decoding the `.plt`/`.plt.sec`/`.plt.got` stubs and matching them
  against the binary's own `.rela.plt`/`.rela.dyn` relocations + dynamic
  symbol table — no external `libc.so`/`ld.so` needed, since imported
  symbol *names* are already embedded in the binary itself. Same-object
  calls in unlinked `.o` files are also resolved by name (read straight
  from `.rela.text`), instead of the bogus `sub_<addr>` you'd get from
  decoding an unpatched call displacement.

This is Phase 1–2 (+ a slice of Phase 4) of the 5-phase roadmap in the
design doc — a working foundation, not the full system.