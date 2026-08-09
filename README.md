# decompiler++

A static x86-64 ELF decompiler that emits readable C, with a native viewer.

```
cargo build --release                     # command line only
cargo build --release --features gui      # command line + native viewer
```

Per-platform instructions, including making a macOS `.app`, are in
[BUILDING.md](BUILDING.md).

## Command line

```
./target/release/mini_decompiler ./bin/prog          # named functions
./target/release/mini_decompiler ./bin/prog -f main  # one function
./target/release/mini_decompiler ./bin/prog -A       # everything
./target/release/mini_decompiler ./bin/prog -a       # with disassembly
./target/release/mini_decompiler ./bin/prog -j out.json
```

## Native viewer

```
./target/release/dpp-gui ./bin/prog [function]
```

FLTK, not a browser: one native window, six dependency crates, no runtime.

| pane | contents |
| --- | --- |
| left | every recovered function |
| centre | pseudocode, or the control-flow graph |
| right | disassembly |
| bottom | registers, stack, memory, program output |

Everything is keyed on address. Clicking a line of C selects the instruction
that produced it, and the block that contains it; clicking an instruction or a
graph node selects back the other way. The stepper drives all three at once.

Actions live in the menu bar and in the right-click menu; the keys are the
same in both.

| key | action |
| --- | --- |
| F2 | rename the selected variable, or the function if none is selected |
| F3 | change the type of the selected variable |
| F4 | toggle a breakpoint on the selected line |
| F5 | start, or restart, the program |
| F7 | execute one instruction |
| F8 | step over a call |
| F9 | continue to the next breakpoint |
| Ctrl+F | search the strings |
| Shift+F12 | find references to what the caret is on |
| Ctrl+T | show or hide the width-conversion casts |

Renames and retypes are applied when the text is drawn, so they take effect
immediately and never re-run the analysis. `Structs…` defines a structure
(`offset type name`, semicolon separated) that then appears in the type list.
`Casts:` switches the width-conversion casts off — both renderings come from
the decompiler, so what you see is exact rather than a regex strip.

## The stepper

It interprets the lifted IR, not the raw bytes: the lifter has already turned
each instruction into statements over registers and memory, so executing those
is executing the program. Calls into functions in the binary are stepped into.
Imported functions are not in the image, so `printf`, `puts`, `malloc`, `free`,
`strcpy`, `memcpy`, `memset`, `strlen`, `exit` and the stack canary are
modelled directly — without that, every `printf` argument is a dangling
pointer.

It agrees with the real binaries: `testing.c` and `testing2.c` both produce
byte-identical output under the interpreter and under Linux.

## Pipeline

```
ELF ─▶ lifter ─▶ frame ─▶ simplify ─▶ idiom ─▶ ptr ─▶ cfg ─▶ cgen ─▶ C
       (IR)      (vars)   (propagate) (undo)   (types) (structure)
                                                              └▶ emu ─▶ stepper
```

| module | job |
| --- | --- |
| `ir` | expression tree, C type model, struct table, condition codes |
| `lifter` | one instruction to statements; structural, never text |
| `frame` | stack slots to typed locals; arrays; parameters |
| `simplify` | copy propagation, call arguments, folding, dead-code elimination |
| `idiom` | undoes the compiler's arithmetic rewrites |
| `ptr` | pointer, struct and array recovery from finished expressions |
| `cfg` | blocks, dominators, post-dominators, loops, structuring |
| `cgen` | precedence-aware C printer |
| `emu` | interpreter over the lifted IR |
| `analysis` | ELF parsing, PLT resolution, two-pass analysis |

## What it recovers

**Variables.** Stack slots become typed locals sized by the gap to the next
slot. Registers that survive structuring are declared too, so the output has no
undeclared identifiers.

**Parameters.** From prologue spills at `-O0`, and from argument registers read
before they are written at `-O1` and above.

**Structs and arrays.** Two mechanisms. Addressing-mode analysis in `frame`
handles aggregates that live *in* the frame. Expression analysis in `ptr`
handles aggregates reached *through* a pointer — necessary because gcc at `-O0`
computes `p + i*4` with a detached `add` that destroys the base register, so no
addressing-mode analysis can attribute the load. After copy propagation the
shape is visible again in one expression. A pointer that is advanced (`p++`,
`&p[1]`) is indexing a sequence, not walking one object's fields, which is how
`char *` is told apart from a struct pointer.

**Compiler idioms.** A compiler never emits `idiv` for a constant divisor.
`idiom` checks the algebraic identity rather than matching an instruction
sequence, so it survives scheduling and differs between compilers:

| source | before | now |
| --- | --- | --- |
| `x / 3` | `(int)((long)a1 * 0x55555556 >> 32) - (a1 >> 31)` | `a1 / 3` |
| `x % 7` | `a1 - ((...* -0x6db6db6d >> 32) + a1 >> 2) * 8 - (...)` | `a1 % 7` |
| `x / 8` | `(a1 >= 0 ? a1 : a1 + 7) >> 3` | `a1 / 8` |

**Calls.** PLT entries, `.o` relocations and GOT-indirect targets are resolved
to names. A function passed as a value reads as its name: `_start` calls
`__libc_start_main(main, ...)`, not `__libc_start_main(4682, ...)`. Argument
counts come from a libc prototype table, from counting `%` specifiers in the
recovered format string, or from signatures learned in a first pass over the
binary's own functions.

**Control flow.** Natural loops as `while`, `do`/`while` and `for(;;)`, with
`break` and `continue` rather than gotos, and if/else merge points from the
immediate post-dominator. Blocks the structured walk cannot reach are still
emitted, with a label.

**Vector code.** The packed SSE instructions gcc's auto-vectoriser emits lift to
named intrinsics (`_mm_add_epi32`, `_mm_cmpgt_epi32`, `_mm_unpacklo_epi32` …)
rather than opaque assembly, so they take part in propagation and dead-code
elimination.

## Sample

`testing2.c` at `-O0`:

```c
int sum_array(int *a1, int a2)
{
    int v2;
    int v1;

    v1 = 0;
    v2 = 0;
    while (v2 < a2) {
        v1 += a1[v2];
        v2++;
    }
    return v1;
}

struct point_dist2_s0 { int field_0; int field_4; long field_8; };

long point_dist2(struct point_dist2_s0 *a1)
{
    long v2, v1;
    v1 = (long)a1->field_0;
    v2 = (long)a1->field_4;
    return a1->field_8 + (v1 * v1 + v2 * v2);
}

int hash_str(char *a1)
{
    int v1 = -0x7ee3623b;
    while ((char)*a1 != 0) {
        a1++;
        v1 ^= (unsigned int)*a1;
        v1 *= 0x1000193;
    }
    return v1;
}
```

`main` in both test programs decompiles to the source it was built from. More
in `samples/`.

## Known limits

* Full devectorisation is not attempted. gcc's four-wide reduction plus its
  unrolled scalar tail reads as intrinsics and an unrolled loop, not as the
  original `for`. Recovering that needs induction-variable analysis across the
  vector and scalar pair.
* Optimised loops that gcc has rotated leave residual registers in the body and
  a few unreachable blocks, emitted as labelled `goto`s.
* 2D stack arrays whose address is built with `shl` + `add rbp` lose the frame
  base and print as a raw computed address.
* A stack array's extent comes from the gap to the next slot, so trailing
  alignment padding can make it read one or two elements too long.
* Floating point lifts but is barely typed: SSE registers print as `double`
  whether the code used the `ss` or `sd` forms.
* No switch or jump-table recovery; an indirect jump ends the block.
* The viewer has no breakpoints, no memory search, and does not save renames.

## Viewer detail

**Registers and stack are annotated**, in the way pwndbg annotates them: every
value is looked up in the memory map and reported as what it actually is — a
pointer into `.text`, `.rodata`, the stack or the heap, a known symbol, a
string (quoted, printable prefix), a small integer, or nothing recognisable.
The stack pane additionally names each slot with the *variable the
decompilation gave it*, so `rbp-0x18` reads as `a1` rather than as an offset.

**The memory map** (`Map`) lists every mapped range with permissions, the way
`vmmap` does, and highlights the one the program counter is in.

**Strings** lists every string recovered from the image and filters as you
type; `Ctrl+F` searches them.

**Cross references** (`Shift+F12`) lists every place in the binary that calls,
branches to, or mentions the thing under the caret.

**The disassembly** is syntax-coloured and carries jump rails down the left,
so a loop is visible as a shape rather than as a pair of addresses to compare.

**The graph** shows either the instructions of each block or the pseudocode
statements they produced — the `Graph:` button switches. Blocks can be dragged
into whatever arrangement makes the function readable.

**Breakpoints** are set with F4 or the right-click menu, marked in both the
pseudocode and the disassembly gutters, and honoured by `Continue`.

**The output pane accepts input**: type in the field at the bottom and press
Enter to feed the emulated program's `stdin`, which `scanf`, `gets`, `fgets`
and `getchar` read.

Any bottom panel can be `Detach`ed into its own window, which can then be moved
and resized independently.

## File formats

ELF, PE/COFF and Mach-O are all read through the same loader — executables,
shared libraries and `.o` files. The code section is found by kind rather than
by name, so `.text`, `__text` and anything a linker script invented all work,
and every loadable section is handed to the stepper rather than a hardcoded
list. Only x86-64 is lifted; anything else is refused by name rather than
misdecompiled. A file that is not an object file at all gets an error saying
so instead of a parser message.
