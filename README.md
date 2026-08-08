# decompiler++

A static x86-64 ELF decompiler that emits readable C. Disassembles with
`iced-x86`, lifts to a typed expression IR, recovers variables, structs and
arrays, structures the control flow, and prints C.

```
cargo build --release
./target/release/mini_decompiler ./bin/testing_O0          # named functions
./target/release/mini_decompiler ./bin/prog -f main        # one function
./target/release/mini_decompiler ./bin/prog -A             # everything
./target/release/mini_decompiler ./bin/prog -a -f main     # with disassembly
./target/release/mini_decompiler ./bin/prog -n 20          # first 20 functions
```

## Pipeline

```
ELF ──▶ lifter ──▶ frame ──▶ simplify ──▶ ptr ──▶ cfg ──▶ cgen ──▶ C
        (IR)       (vars)    (propagate)  (types) (structure)
```

| module | job |
| --- | --- |
| `ir.rs` | expression tree, C type model, struct table, condition codes |
| `lifter.rs` | one instruction → statements; no text, all structural |
| `frame.rs` | stack slots → named locals; arrays; spilled parameters |
| `simplify.rs` | copy propagation, call arguments, folding, dead-code elimination |
| `ptr.rs` | pointer / struct / array recovery from finished expressions |
| `cfg.rs` | basic blocks, dominators, post-dominators, loops, structuring |
| `cgen.rs` | precedence-aware C printer |
| `main.rs` | ELF parsing, PLT/relocation resolution, two-pass analysis, output |

## What it recovers

**Variables.** Stack slots become typed locals sized by the gap to the next
slot and by how they are accessed. Registers that survive structuring (a
value produced in both arms of an `if`) are declared as locals too, so the
output is not full of undeclared identifiers.

**Parameters.** From prologue spills at `-O0`, and from argument registers
that are read before they are written at `-O1` and above.

**Structs and arrays.** Two independent mechanisms:

* addressing-mode analysis in `frame.rs`, for aggregates that live *in* the
  frame — this is what turns `[rbp+rax*4-0x30]` into `buf[i]`;
* expression analysis in `ptr.rs`, for aggregates reached *through* a
  pointer. This second pass is necessary because gcc at `-O0` computes
  `p + i*4` with a separate `add` that destroys the base register, so no
  addressing-mode-level analysis can attribute the load. Once copy
  propagation has run, the shape is visible again in one expression and can
  be read back out.

`ptr.rs` distinguishes a struct pointer from an element pointer by looking
for pointer advance (`p = p + 1`, `p = &p[1]`): a pointer that walks is
indexing a sequence, so `*p` and `*(p+1)` are the same field of two
elements, not two fields of one object.

**Types.** Widths come from access size, signedness from the instruction
(`movsx` vs `movzx`, `sar` vs `shr`, `jl` vs `jb`). Pointer returns are
propagated into the signature. `malloc` and friends type their destination.

**Calls.** PLT entries, `.o` relocations and GOT-indirect targets are all
resolved to names. Argument counts come from a libc prototype table, from
counting `%` specifiers in the recovered format string, or from the
signatures learned in the first analysis pass over local functions.

**Control flow.** Natural loops with `while` / `do-while` / `for(;;)`,
`break` and `continue` rather than gotos, and if/else merge points taken
from the immediate post-dominator. Anything the structured walk cannot reach
is still emitted, with a label, rather than dropped.

## Sample

`testing2.c` compiled at `-O0`, decompiled:

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

struct point_dist2_s0 {
    int field_0;
    int field_4;
    long field_8;
};

long point_dist2(struct point_dist2_s0 *a1)
{
    long v2;
    long v1;

    v1 = (long)a1->field_0;
    v2 = (long)a1->field_4;
    return a1->field_8 + (v1 * v1 + v2 * v2);
}

int hash_str(char *a1)
{
    int v1;

    v1 = -0x7ee3623b;
    while ((char)*a1 != 0) {
        a1++;
        v1 ^= (unsigned int)*a1;
        v1 *= 0x1000193;
    }
    return v1;
}
```

The same program at `-O2`, where gcc has turned the branches into `cmov`:

```c
int clamp(int a1, int a2, int a3)
{
    return a1 >= a2 ? a1 <= a3 ? a1 : a3 : a2;
}
```

More in `samples/`.

## Known limits

* Optimised loops that gcc has vectorised or rotated leave residual
  registers in the body and a few unreachable blocks at the end of the
  function, emitted as labelled `goto`s rather than folded away.
* 2D stack arrays whose address gcc builds with `shl` + `add rbp` lose the
  frame base and print as a raw computed address.
* A stack array's extent is inferred from the gap to the next slot, so
  trailing alignment padding can make it read one or two elements too long.
* Floating point is lifted but barely typed: SSE registers print as `double`
  regardless of whether the code used `ss` or `sd` forms.
* No switch/jump-table recovery; an indirect jump ends the block.
