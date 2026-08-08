# Using this Makefile

Matches your actual layout — Makefile sits at the crate root, next to
`Cargo.toml`/`src`/`target`:

```
decompiler-plus-plus/     <- crate root (this is where Makefile goes)
├── Cargo.toml
├── Cargo.lock
├── src/
├── target/
├── Makefile               <- this file
└── testing.c               <- add your C file here
```

`DECOMPILER_DIR` is set to `.` to match this. If you ever move the
Makefile somewhere else relative to the crate, just update that one
variable at the top.

## Commands

```
make            # build all 7 variants + decompile all of them
make build      # just compile testing.c all the ways, no decompiling
make decompile  # decompile whatever's already built
make show-O2    # build+decompile+print just one variant to your terminal
make clean      # remove bin/ and decompiled/
```

## Variants built

| target      | flags                          | what it shows                          |
|-------------|---------------------------------|-----------------------------------------|
| O0          | `-O0`                           | baseline, unoptimized, easiest to read  |
| O1          | `-O1`                           | light optimization                      |
| O2          | `-O2`                           | inlining/peephole opts kick in          |
| O3          | `-O3`                           | aggressive optimization/vectorization   |
| static      | `-O0 -static`                   | statically linked (decompiler will find 1000+ libc functions, capped at MAX_FUNCS) |
| stripped    | `-O0` + `strip --strip-all`     | no symbol table — decompiler falls back to entry-point-only disassembly |
| pie         | `-O0 -pie -fpie`                | position-independent executable         |

Output per variant lands in `decompiled/testing_<variant>.txt` (asm +
pseudocode). `MAX_FUNCS` (default 30) is overridable:

```
make MAX_FUNCS=100
```

## Fixed since last version

`DECOMPILER_DIR` no longer defaults to a `mini_decompiler/` subfolder
(you don't have one), and the old `make -C $(DECOMPILER_DIR)` fallback
was removed — with `DECOMPILER_DIR = .` that would have recursively
re-invoked this same Makefile. Both confirmed fixed by an actual test
run in this layout (7/7 variants built + decompiled, no recursion).

## Known decompiler limitation (unrelated to this Makefile)

At `-O2`/`-O3`, gcc often turns arithmetic into `lea`
(e.g. `lea eax,[rdi+1]`). The lifter currently prints its memory operand
as a dereference (`*[rdi+0x1]`) even though `lea` never touches memory.
Cosmetic, not a crash — can fix on request.