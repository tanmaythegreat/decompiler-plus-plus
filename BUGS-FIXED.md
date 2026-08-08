# What was wrong with the original, and what was done about it

The original was four files, 1313 lines. Everything below was reproduced
against the checked-in test binaries before being fixed.

## Wrong output

| # | bug | consequence |
| --- | --- | --- |
| 1 | `lea` lifted through the same arm as `mov` | `lea rax,[rip+X]` became a *load*, so every `printf` format argument was wrong |
| 2 | RIP-relative displacement added to the base | `[rip+0x402004]`; iced's `memory_displacement64` already folds RIP in |
| 3 | `cmp` and `test` lifted identically | `test eax,eax; je` became `eax == eax`, always true — `main` took the wrong branch |
| 4 | condition smuggled as `Instr::Unknown{"__cond__Ne"}` | a text side channel between two passes |
| 5 | structuring matched three hardcoded shapes | `clamp`'s two sequential ifs matched none; it fell through to a goto and **the rest of the function was silently dropped** |
| 6 | `goto Lxxxx` emitted, labels never emitted | output could not compile even in principle |
| 7 | `Ret` hardcoded `return rax` | non-void returns invented from nothing |
| 8 | return type = "did anything write rax" | `xor eax,eax` counted, so almost nothing came out `void` |
| 9 | call arguments = contiguous-prefix guess | `printf(rax, eax, ...)` with four arguments |
| 10 | call return value discarded | `abs_val(0xfffffff9); *[rbp-4] = eax;` |
| 11 | immediates printed unsigned hex | `0xfffffff9` where the source said `-7` |
| 12 | `<u`, `>=u`, `*[rbp-0x4]` | not C |
| 13 | `imul` assumed two operands with `dst == lhs` | mangled the 1- and 3-operand forms |

## Hangs, panics, truncation

| # | bug | consequence |
| --- | --- | --- |
| 14 | `reverse_postorder` recursive | stack overflow on large functions |
| 15 | `dominators` indexed `rpo_index[&a]` unguarded | panic on any block reachable only through an unstructured edge |
| 16 | dominator `intersect` walked up from a root | a root is its own immediate dominator, so the inner loop never terminated — this **hung `-O2` and `-O3` outright** |
| 17 | `lift_region` stopped at the first invalid byte | padding or a jump table inside a symbol's extent truncated everything after it |
| 18 | stripped fallback treated all of `.text` as one function | the comment claimed "up to the first ret"; the code did not |
| 19 | `leads_back_to` ignored its `_headers` argument | unbounded despite the "bounded DFS" comment |

## Missing model

| # | bug | consequence |
| --- | --- | --- |
| 20 | `Value::Mem(String)` — operands stored as rendered text | no base, index, scale, displacement or size, so variables, arrays, structs and type inference were all impossible |
| 21 | no sub-register width tracking | `eax` and `rax` looked like unrelated variables to every pass |
| 22 | `reg_family` hand-written string match | missed the `r8b`–`r15b` spellings iced actually prints, and was wrong for `ah/bh/ch/dh`, which alias bits 8..16 rather than the low byte |
| 23 | `push`/`pop`/`leave` lifted to `Nop` | `pop rsi` dropped a real data movement |
| 24 | no lifting for `cdqe/cdq/cqo`, `setcc`, `cmovcc`, `sar`, `mul`, `div`, `idiv`, `xchg`, `adc`, `sbb`, or any SSE | pages of `/* unhandled */` |

## How each area was rebuilt

**Flags.** A comparison records *what set the flags* (`Cmp`, `Test`, or a
logical op) rather than a placeholder condition. The branch that consumes
them asks that source for the boolean it implies, so `test al,al; jne`
becomes `al != 0` and `test`-then-`jb` correctly becomes "never taken" —
`and` clears the carry flag.

**Structuring.** Immediate post-dominators give the if/else merge point, so
nesting depth is no longer a special case. Natural loops give `while`,
`do-while` and `for(;;)`, with `break` and `continue` instead of gotos. A
loop header that does real work becomes `for(;;) { work; if (!cond) break; ... }`,
because that work has to run on every iteration and cannot be hoisted above
the loop. Blocks the structured walk never reaches are still emitted, with a
label. Labels are emitted in a second pass, once the set of jump targets is
known.

**Registers.** `full_register()` and `size()` from iced replace the hand
table. Every register reference carries its access width, so narrowing and
widening are explicit casts rather than silent name changes.

**Simplification.** Forward copy propagation with width-aware reads (a
32-bit write zero-extends; a narrower read casts), algebraic and cast
folding, and backward liveness-based dead code elimination. A call result is
inlined into its consumer only when there is exactly one consumer before the
register is redefined — counting past a redefinition attributes a later
value's readers to an earlier one and deletes a live call.

**Recovery.** Described in the README.
