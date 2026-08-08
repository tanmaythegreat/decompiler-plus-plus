use crate::pass::AnalysisPass;
use decompiler_core::{AddressSpace, DataType, Function, Instruction, Varnode};
use std::collections::BTreeMap;

/// A simple constraint-based type inference engine.
/// It observes operations like `Load` and `Store` to infer pointers,
/// and propagates these types across assignments (`Copy`) and pointer arithmetic (`IntAdd`).
pub struct TypeInference;

impl AnalysisPass for TypeInference {
    fn name(&self) -> &'static str {
        "type-inference"
    }

    fn run(&self, function: &mut Function) {
        let mut changed = true;
        
        // Loop until a fixed point is reached (no new types inferred).
        while changed {
            changed = false;
            
            // We need to borrow function.types separately, but we can't iterate over blocks and mutate types if they are in the same struct.
            let mut types = std::mem::take(&mut function.types);

            for block in &function.blocks {
                for li in &block.instructions {
                    for op in &li.ops {
                        changed |= Self::infer_op(op, &mut types);
                    }
                }
            }
            
            function.types = types;
        }
    }
}

impl TypeInference {
    fn infer_op(op: &Instruction, types: &mut BTreeMap<Varnode, DataType>) -> bool {
        let mut changed = false;
        match op {
            Instruction::Load { dest, addr } => {
                // addr must be a pointer to dest's type
                if !types.contains_key(addr) && addr.space != AddressSpace::Constant {
                    let dest_type = types.get(dest).cloned().unwrap_or(DataType::Primitive { size: dest.size, signed: false });
                    types.insert(addr.clone(), DataType::Pointer(Box::new(dest_type)));
                    changed = true;
                }
            }
            Instruction::Store { addr, src } => {
                if !types.contains_key(addr) && addr.space != AddressSpace::Constant {
                    let src_type = types.get(src).cloned().unwrap_or(DataType::Primitive { size: src.size, signed: false });
                    types.insert(addr.clone(), DataType::Pointer(Box::new(src_type)));
                    changed = true;
                }
            }
            Instruction::IntAdd { dest, lhs, rhs } | Instruction::IntSub { dest, lhs, rhs } => {
                // If lhs is pointer, dest is pointer
                if let Some(DataType::Pointer(inner)) = types.get(lhs).cloned() {
                    if !types.contains_key(dest) {
                        types.insert(dest.clone(), DataType::Pointer(inner));
                        changed = true;
                    }
                } else if let Some(DataType::Pointer(inner)) = types.get(rhs).cloned() {
                    if !types.contains_key(dest) {
                        types.insert(dest.clone(), DataType::Pointer(inner));
                        changed = true;
                    }
                }
            }
            Instruction::Copy { dest, src } => {
                // Forward-only: if src has a type, propagate it to dest.
                //
                // Backward propagation (typing src based on dest's type) is
                // deliberately omitted here. In a single forward scan, dest's
                // type may not be fully resolved when we visit this Copy, so
                // propagating backward would produce false constraints. A
                // proper backward pass (using the backward dataflow engine)
                // is needed to propagate types in the reverse direction.
                if let Some(t) = types.get(src).cloned() {
                    if !types.contains_key(dest) {
                        types.insert(dest.clone(), t);
                        changed = true;
                    }
                }
            }
            // A phi-node defines dest as the join of all incoming srcs.
            // Propagate type if all srcs agree (first typed src wins; a
            // proper constraint solver would unify all of them).
            Instruction::Phi { dest, srcs } => {
                let t = srcs.iter().find_map(|s| types.get(s).cloned());
                if let Some(t) = t {
                    if !types.contains_key(dest) {
                        types.insert(dest.clone(), t);
                        changed = true;
                    }
                }
            }
            _ => {}
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use decompiler_core::{AddressSpace, BasicBlock, LiftedInstruction, Function, Varnode};

    fn reg(id: u64, size: u8) -> Varnode {
        Varnode::new(AddressSpace::Register, id, size)
    }

    #[test]
    fn test_infers_pointer_from_load() {
        let r1 = reg(1, 8); // address
        let r2 = reg(2, 4); // dest
        
        let mut function = Function::new("test", 0);
        let mut block = BasicBlock::new(0);
        block.instructions.push(LiftedInstruction {
            address: 0,
            ops: vec![Instruction::Load { dest: r2.clone(), addr: r1.clone() }]
        });
        function.blocks.push(block);

        let pass = TypeInference;
        pass.run(&mut function);

        assert_eq!(
            function.types.get(&r1),
            Some(&DataType::Pointer(Box::new(DataType::Primitive { size: 4, signed: false })))
        );
    }
}
