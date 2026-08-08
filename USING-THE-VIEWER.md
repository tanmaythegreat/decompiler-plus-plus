# The native viewer

```
cargo build --release --features gui
./target/release/dpp-gui ./bin/testing_O0 main
```

FLTK. One native window, six dependency crates, no browser and no runtime.
Naming a function on the command line opens straight to it; otherwise use
`Open binary…` and the function list on the left.

## Layout

```
┌── Functions ──┬── Pseudocode │ Graph ─────────────┬── Disassembly ──┐
│ every         │ C, with the address each line     │ the instruction │
│ recovered     │ came from in the gutter           │ stream          │
│ function      │                                   │                 │
├───────────────┴──────────┬──────────┬─────────────┴─────────────────┤
│ Registers                │ Stack    │ Memory      │ Output          │
└──────────────────────────┴──────────┴─────────────┴─────────────────┘
```

Everything is keyed on address. Click a line of C and the instruction that
produced it is selected, along with the block containing it in the graph. Click
an instruction or a graph node and the selection travels the other way.

## Editing what you see

| key | action |
| --- | --- |
| F2 | rename the selected variable; with no variable selected, renames the function |
| F3 | change the type of the selected variable |

Selecting means clicking a line that mentions the variable. Both changes are
applied when the text is drawn rather than fed back into the analysis, so they
appear immediately and cost nothing.

`Structs…` defines a structure. Fields are `offset type name`, separated by
semicolons or newlines:

```
0 int x; 4 int y; 8 long tag
```

It then appears in the type list, so a recovered `long v1` can be corrected to
`struct my_struct *v1`.

`Casts:` switches the width-conversion casts off. Both renderings come from the
decompiler itself, so the result is exact rather than a regular expression
applied to the output.

## Stepping

| key | action |
| --- | --- |
| F5 | reset to the entry of the selected function |
| F7 / F8 | execute one instruction |
| F9 | run to completion |

The current instruction is highlighted in all three views at once. Registers
that changed on the last step are shown in red. The stack pane marks `rsp` and
`rbp`; the memory pane is a hex dump around the stack pointer; `Output` is what
the program printed.

The stepper interprets the lifted IR rather than the raw bytes. The lifter has
already turned each instruction into statements over registers and memory, so
executing those is executing the program. Calls into the binary's own functions
are stepped into. Imported functions are not present in the image, so `printf`,
`puts`, `malloc`, `free`, `strcpy`, `memcpy`, `memset`, `strlen`, `exit` and the
stack canary are modelled directly.

Two things follow from that. Anything the lifter left as `__asm__` does nothing
when stepped over, and an unmodelled library call returns zero and says so in
the output pane.

## Verified

Under the interpreter, `testing.c` and `testing2.c` produce output identical to
the real binaries:

```
abs=7 clamp=10 sum=15        dist2=115
hello, decompiler            local=140
odd                          hash=2880064923
                             steps=9
                             trace=12
```

## Not there yet

No breakpoints, no memory search, no undo, and renames are not saved between
sessions.
