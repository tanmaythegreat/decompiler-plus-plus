//! `output-c`: translates the simplified, structured IR into human-readable
//! C-like pseudocode.
//!
//! Phases 3 and 4 feed into this module: type inference results are used to
//! emit proper C type declarations, and the `HighLevelAst` produced by the
//! `Structurer` pass (stored in [`Function::ast`]) drives structured
//! `if`/`while` output instead of goto-based fallbacks. If the `Structurer`
//! pass has not yet run, `generate` falls back to calling
//! [`Structurer::structure`] directly so unit tests that bypass the
//! `PassManager` still work.

use analysis_passes::Structurer;
use decompiler_core::{AddressSpace, BranchTarget, DataType, Function, HighLevelAst, Instruction, Varnode};
use std::collections::BTreeMap;

/// Translate `function` into a single C-like function definition, including
/// variable declarations for every register/temporary the function assigns.
pub fn generate(function: &Function) -> String {
    let mut out = String::new();

    out.push_str(&format!(
        "// Phase 4 pseudocode for `{}` -- structured (if/while)\n",
        function.name
    ));
    out.push_str(&format!("void {}(void) {{\n", sanitize_ident(&function.name)));

    for decl in declarations(function) {
        out.push_str(&format!("    {decl};\n"));
    }
    out.push('\n');

    // Use the AST cached by the Structurer pass if available; otherwise
    // call the structurer directly (supports unit tests that bypass PassManager).
    let ast = function.ast.clone()
        .unwrap_or_else(|| Structurer::structure(function));
    let ast_str = print_ast(&ast, 1, &function.types);
    out.push_str(&ast_str);

    out.push_str("}\n");
    out
}

fn print_ast(ast: &HighLevelAst, indent: usize, types: &BTreeMap<Varnode, DataType>) -> String {
    let mut out = String::new();
    let pad = "    ".repeat(indent);
    
    match ast {
        HighLevelAst::Block { start_address: _, instructions } => {
            for op in instructions {
                if let Some(stmt) = statement(op, types) {
                    out.push_str(&pad);
                    out.push_str(&stmt);
                    out.push('\n');
                }
            }
        }
        HighLevelAst::Seq(nodes) => {
            for node in nodes {
                out.push_str(&print_ast(node, indent, types));
            }
        }
        HighLevelAst::If { cond, then_body, else_body } => {
            out.push_str(&format!("{pad}if ({}) {{\n", operand(cond)));
            out.push_str(&print_ast(then_body, indent + 1, types));
            
            if let Some(els) = else_body {
                out.push_str(&format!("{pad}}} else {{\n"));
                out.push_str(&print_ast(els, indent + 1, types));
            }
            out.push_str(&format!("{pad}}}\n"));
        }
        HighLevelAst::While { cond, body } => {
            out.push_str(&format!("{pad}while ({}) {{\n", operand(cond)));
            out.push_str(&print_ast(body, indent + 1, types));
            out.push_str(&format!("{pad}}}\n"));
        }
        HighLevelAst::Goto(target) => {
            out.push_str(&format!("{pad}goto label_0x{target:x};\n"));
        }
        HighLevelAst::Label(addr) => {
            let lpad = if indent > 0 { "    ".repeat(indent - 1) } else { "".to_string() };
            out.push_str(&format!("{lpad}label_0x{addr:x}:\n"));
        }
        HighLevelAst::Return => {
            out.push_str(&format!("{pad}return;\n"));
        }
    }
    out
}

fn declarations(function: &Function) -> Vec<String> {
    let mut seen: BTreeMap<(AddressSpace, u64, u8), Varnode> = BTreeMap::new();
    for block in &function.blocks {
        for li in &block.instructions {
            for op in &li.ops {
                if let Some(dest) = op_dest(op) {
                    if dest.space == AddressSpace::Register || dest.space == AddressSpace::Unique
                    {
                        seen.entry((dest.space, dest.offset, dest.size)).or_insert_with(|| dest.clone());
                    }
                }
            }
        }
    }
    seen.into_values().map(|v| {
        let name = var_name(&v);
        if let Some(dt) = function.types.get(&v) {
            format!("{} {}", format_type(dt), name)
        } else {
            format!("{} {}", unsigned_ctype(v.size), name)
        }
    }).collect()
}

fn format_type(dt: &DataType) -> String {
    match dt {
        DataType::Primitive { size, signed } => {
            if *signed {
                signed_ctype(*size).to_string()
            } else {
                unsigned_ctype(*size).to_string()
            }
        }
        DataType::Pointer(inner) => {
            format!("{}*", format_type(inner))
        }
        DataType::Array(inner, len) => {
            // Simplified array notation for pseudocode declarations/casts
            format!("{}[{}]", format_type(inner), len)
        }
    }
}

fn op_dest(op: &Instruction) -> Option<&Varnode> {
    match op {
        Instruction::Copy { dest, .. }
        | Instruction::Load { dest, .. }
        | Instruction::IntAdd { dest, .. }
        | Instruction::IntSub { dest, .. }
        | Instruction::IntEqual { dest, .. }
        | Instruction::IntNotEqual { dest, .. }
        | Instruction::IntSless { dest, .. }
        | Instruction::IntSlessEqual { dest, .. } => Some(dest),
        Instruction::Store { .. }
        | Instruction::Branch { .. }
        | Instruction::CBranch { .. }
        | Instruction::Call { .. }
        | Instruction::Return
        | Instruction::Nop => None,
        // Phi defines its dest; emit it so declarations are generated.
        Instruction::Phi { dest, .. } => Some(dest),
    }
}

fn unsigned_ctype(size: u8) -> &'static str {
    match size {
        1 => "uint8_t",
        2 => "uint16_t",
        4 => "uint32_t",
        8 => "uint64_t",
        _ => "uint64_t", // odd/unmodeled size: widest fallback, still compiles as pseudocode
    }
}

fn signed_ctype(size: u8) -> &'static str {
    match size {
        1 => "int8_t",
        2 => "int16_t",
        4 => "int32_t",
        8 => "int64_t",
        _ => "int64_t",
    }
}

fn var_name(v: &Varnode) -> String {
    match v.space {
        AddressSpace::Register => v.name.clone().unwrap_or_else(|| format!("reg_0x{:x}", v.offset)),
        AddressSpace::Unique => format!("t{}", v.offset),
        AddressSpace::Memory => format!("mem_0x{:x}", v.offset),
        AddressSpace::Constant => unreachable!("constants are never declared"),
    }
}

/// A Varnode as it appears on the *use* side of an expression (an operand,
/// not a declaration).
fn operand(v: &Varnode) -> String {
    match v.space {
        AddressSpace::Constant => format!("{}", v.offset),
        AddressSpace::Memory => format!("MEM[0x{:x}]", v.offset),
        AddressSpace::Register | AddressSpace::Unique => var_name(v),
    }
}

fn branch_target(target: &BranchTarget) -> String {
    match target {
        BranchTarget::Absolute(addr) => format!("label_0x{addr:x}"),
        BranchTarget::Indirect => "/* unresolved indirect target */".to_string(),
    }
}

/// Translate one IR instruction into a C statement (including its trailing
/// `;` / braces as appropriate). Returns `None` for instructions that emit
/// no visible statement (`Nop`).
fn statement(op: &Instruction, types: &BTreeMap<Varnode, DataType>) -> Option<String> {
    let s = match op {
        Instruction::Copy { dest, src } => format!("{} = {};", var_name(dest), operand(src)),
        Instruction::Load { dest, addr } => {
            let ct = if let Some(DataType::Pointer(inner)) = types.get(addr) {
                format_type(inner)
            } else {
                unsigned_ctype(dest.size).to_string()
            };
            format!("{} = *({ct}*)(uintptr_t){};", var_name(dest), operand(addr))
        }
        Instruction::Store { addr, src } => {
            let ct = if let Some(DataType::Pointer(inner)) = types.get(addr) {
                format_type(inner)
            } else {
                unsigned_ctype(src.size).to_string()
            };
            format!("*({ct}*)(uintptr_t){} = {};", operand(addr), operand(src))
        }
        Instruction::IntAdd { dest, lhs, rhs } => {
            format!("{} = {} + {};", var_name(dest), operand(lhs), operand(rhs))
        }
        Instruction::IntSub { dest, lhs, rhs } => {
            format!("{} = {} - {};", var_name(dest), operand(lhs), operand(rhs))
        }
        Instruction::IntEqual { dest, lhs, rhs } => {
            format!("{} = ({} == {});", var_name(dest), operand(lhs), operand(rhs))
        }
        Instruction::IntNotEqual { dest, lhs, rhs } => {
            format!("{} = ({} != {});", var_name(dest), operand(lhs), operand(rhs))
        }
        Instruction::IntSless { dest, lhs, rhs } => {
            let ct = signed_ctype(lhs.size);
            format!(
                "{} = (({ct}){} < ({ct}){});",
                var_name(dest),
                operand(lhs),
                operand(rhs)
            )
        }
        Instruction::IntSlessEqual { dest, lhs, rhs } => {
            let ct = signed_ctype(lhs.size);
            format!(
                "{} = (({ct}){} <= ({ct}){});",
                var_name(dest),
                operand(lhs),
                operand(rhs)
            )
        }
        Instruction::Branch { target } => format!("goto {};", branch_target(target)),
        Instruction::CBranch { condition, target } => {
            format!("if ({}) goto {};", operand(condition), branch_target(target))
        }
        Instruction::Call { target } => match target {
            BranchTarget::Absolute(addr) => format!("sub_0x{addr:x}();"),
            BranchTarget::Indirect => "/* indirect call */".to_string(),
        },
        Instruction::Return => "return;".to_string(),
        Instruction::Nop => return None,
        // Phi-nodes are SSA artefacts; render as a structured comment so the
        // pseudocode remains readable while making their presence explicit.
        Instruction::Phi { dest, srcs } => {
            let src_list: Vec<String> = srcs.iter().map(|s| operand(s)).collect();
            format!("/* {} = φ({}) */", var_name(dest), src_list.join(", "))
        }
    };
    Some(s)
}

/// Turn an arbitrary function name (which may contain spaces, parens, etc.
/// -- see the Phase 1 CLI's demo names) into a valid C identifier.
fn sanitize_ident(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect();
    if out.is_empty() || out.chars().next().unwrap().is_ascii_digit() {
        out.insert(0, '_');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use decompiler_core::{BasicBlock, LiftedInstruction};

    fn reg(id: u64, size: u8, name: &str) -> Varnode {
        Varnode::named(AddressSpace::Register, id, size, name)
    }

    #[test]
    fn generates_declarations_and_statements_for_linear_code() {
        let eax = reg(0, 4, "EAX");
        let mut block = BasicBlock::new(0x1000);
        block.instructions = vec![
            LiftedInstruction {
                address: 0x1000,
                ops: vec![Instruction::Copy { dest: eax.clone(), src: Varnode::constant(5, 4) }],
            },
            LiftedInstruction {
                address: 0x1005,
                ops: vec![Instruction::IntAdd {
                    dest: eax.clone(),
                    lhs: eax.clone(),
                    rhs: Varnode::constant(3, 4),
                }],
            },
            LiftedInstruction { address: 0x100a, ops: vec![Instruction::Return] },
        ];
        let mut function = Function::new("demo", 0x1000);
        function.blocks.push(block);

        let c = generate(&function);

        assert!(c.contains("uint32_t EAX;"), "{c}");
        assert!(c.contains("EAX = 5;"), "{c}");
        assert!(c.contains("EAX = EAX + 3;"), "{c}");
        assert!(c.contains("return;"), "{c}");
        assert!(c.contains("label_0x1000:"), "{c}");
    }

    #[test]
    fn branches_become_structured_if_blocks() {
        let cond = reg(1000, 1, "ZF");
        let mut b0 = BasicBlock::new(0x0);
        b0.instructions.push(LiftedInstruction {
            address: 0x0,
            ops: vec![Instruction::CBranch {
                condition: cond.clone(),
                target: BranchTarget::Absolute(0x10),
            }],
        });
        let mut b1 = BasicBlock::new(0x10);
        b1.instructions.push(LiftedInstruction { address: 0x10, ops: vec![Instruction::Return] });

        let mut function = Function::new("branchy", 0x0);
        function.blocks = vec![b0, b1];

        let c = generate(&function);
        assert!(c.contains("if (ZF) {"), "{c}");
        assert!(!c.contains("goto"), "{c}");
    }

    #[test]
    fn sanitizes_function_names_with_odd_characters() {
        assert_eq!(sanitize_ident("demo_function (mov/add/cmp/jne/jmp/ret)"),
            "demo_function__mov_add_cmp_jne_jmp_ret_");
        assert_eq!(sanitize_ident("123abc"), "_123abc");
    }
}
