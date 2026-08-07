# decompiler — Phase 1

A trait-based, multi-ISA decompiler skeleton in Rust, following the
Ghidra-inspired roadmap: a p-code-style IR, a `Disassembler`/`Lifter` trait
pair that every architecture backend implements, and a Cargo workspace that
keeps `decompiler-core` free of any x86-specific (or ARM-, or RISC-V-
specific) code.

This is **Phase 1** of the 5-phase roadmap: *Foundational Crates & X86
Lifter*. It sets up the crate structure, defines the core IR types, and
lifts a small representative x86 instruction set (`MOV`, `ADD`, `CMP`,
`JMP`/`Jcc`, `CALL`, `RET`) into that IR.

## Workspace layout

```
decompiler-core/   Shared types (Function, BasicBlock, Variable) and the
                    Disassembler / Lifter traits every arch-* crate implements.
ir-pcode/           The IR itself: Varnode (generalized value location) and
                    Instruction (p-code-style opcode enum). Dependency-free.
arch-x86/           x86 backend: decodes bytes -> X86Instruction, lifts
                    X86Instruction -> Vec<ir_pcode::Instruction>.
cli/                decompiler-cli: the Phase 1 demo binary.
```

Dependency direction is one-way: `ir-pcode` → `decompiler-core` → `arch-x86`
→ `cli`. Nothing in `decompiler-core` knows x86 exists; `arch-x86` is "just"
an implementation of two traits.

## Running it

```bash
cd decompiler
cargo run                                             # built-in demo
cargo run -- --hex "B8 05 00 00 00 C3"                # lift your own bytes
cargo run -- --hex "B8 05 00 00 00 C3" --base 0x401000
cargo run -- --file some_raw_bytes.bin --base 0x1000
cargo test                                            # 19 unit tests
```

The built-in demo lifts a hand-assembled function that uses every
instruction in the Phase 1 subset — `mov`/`add`/`cmp`/`jne`/`jmp`/`ret` —
plus a separate `call` example, and prints the disassembly next to the IR
each instruction lifts to, e.g.:

```
0x100a:  3d 08 00 00 00            cmp EAX, 8
           └─ unique[0x0]:4 = INT_SUB EAX:4, 0x8:4
           └─ ZF:1 = INT_EQUAL unique[0x0]:4, 0x0:4
0x100f:  75 07                     jne +7
           └─ unique[0x1]:1 = INT_EQUAL ZF:1, 0x0:1
           └─ CBRANCH 0x1018, unique[0x1]:1
```

Notice `cmp` decomposes into two IR ops (an `INT_SUB` for flags plus an
`INT_EQUAL` that reads *that same SUB result* to produce a boolean `ZF`
Varnode) rather than being a single 1:1 IR instruction — this is deliberate,
matching the roadmap's guidance to decompose flag-setting machine
instructions into several p-code operations, each computed from the shared
subtraction result, rather than inventing a single "CMP" IR opcode.

## What's implemented vs. simplified in Phase 1

**Implemented:**
- Multi-crate workspace with `decompiler-core` as the shared contract.
- `Varnode` / `Instruction` IR types (`ir-pcode`), matching Ghidra's
  Varnode/p-code-opcode model.
- `Disassembler` and `Lifter` traits with associated `Instruction` types,
  so each architecture owns its own decoded-instruction representation.
- x86 decoding for: `mov` (reg↔reg, reg←imm32), `add` (reg↔reg, reg←imm32
  via the `eax`-short and general `81 /0` encodings, plus the sign-extended
  `83 /0 ib` imm8 form compilers actually prefer), `cmp` (the same three
  forms), `jmp`/`je`/`jne` (rel8/rel32), `call` (rel32), `ret`.
- Manual, per-instruction lifting functions (`lift_mov_reg_imm`,
  `lift_cmp`, etc.) as recommended over a SLEIGH-style DSL for a
  from-scratch Rust project.
- 17 unit tests covering decode edge cases (sign-extended negative rel8,
  truncated input, unsupported addressing modes) and lifter correctness
  (branch target resolution, temp-Varnode uniqueness).

**Deliberately simplified (left for later phases):**
- Only 32-bit register-direct operands are decoded — no memory operands
  (ModRM `mod != 11`), no SIB bytes, no REX prefixes / 64-bit registers.
  `ir-pcode::Instruction` already has `Load`/`Store` variants ready for
  when that lands.
- EFLAGS is modeled as a single simplified "ZF" Varnode rather than the
  real CF/PF/AF/ZF/SF/OF set — enough to demonstrate the CMP→Jcc lifting
  pattern without committing to a full flags model this early.
- No CFG construction yet (`BasicBlock` currently just accumulates
  instructions in program order) — that's Phase 2.
- No SSA form, no data-flow analysis, no type inference, no C output yet —
  Phases 2–4.

## Next: Phase 2

Per the roadmap: build a real CFG from branch targets (`BasicBlock`s become
graph nodes with actual edges), integrate a dataflow-analysis framework, and
write a first analysis pass (constant propagation) plus a bare-bones
SSA-to-C translator for straight-line code.
