//! The `AnalysisPass` trait: the roadmap's "standard interface for all
//! analytical transformations applied to the IR". Every pass in this crate
//! -- CFG construction, constant propagation, and whatever Phase 3+ adds
//! (type inference, dead-code elimination, ...) -- implements this so a
//! driver can hold a `Vec<Box<dyn AnalysisPass>>` and run them in a
//! predictable, declared order without knowing anything about what each
//! pass actually does.

use decompiler_core::Function;

/// A single analytical transformation over a [`Function`]'s IR.
///
/// Passes run in-place (`&mut Function`) rather than returning a new
/// `Function`: most passes (constant folding, dead-code elimination, CFG
/// simplification) are naturally *edits* to the existing block/instruction
/// list rather than a full rebuild, and an in-place interface makes that the
/// cheap, obvious thing to do. A pass that needs an initial "empty" state to
/// build from (like CFG construction) still fits: it just splits/rewrites
/// the single flat block a `Function` starts with.
pub trait AnalysisPass {
    /// Short, human-readable identifier, used for logging/debugging (e.g.
    /// `"cfg-construction"`, `"constant-propagation"`).
    fn name(&self) -> &'static str;

    /// Apply this pass to `function`, mutating it in place.
    fn run(&self, function: &mut Function);
}

/// Runs a fixed, ordered list of passes over a `Function`. This is the
/// "driver program" the roadmap describes: it doesn't know or care what any
/// individual pass does, only that it implements [`AnalysisPass`].
pub struct PassManager {
    passes: Vec<Box<dyn AnalysisPass>>,
}

impl PassManager {
    pub fn new() -> Self {
        PassManager { passes: Vec::new() }
    }

    /// Add a pass to the end of the pipeline. Returns `self` so passes can
    /// be chained: `PassManager::new().with(CfgBuilder).with(ConstantPropagation)`.
    pub fn with(mut self, pass: impl AnalysisPass + 'static) -> Self {
        self.passes.push(Box::new(pass));
        self
    }

    /// Run every registered pass, in order, over `function`.
    pub fn run(&self, function: &mut Function) {
        for pass in &self.passes {
            pass.run(function);
        }
    }

    /// Same as [`PassManager::run`], but prints each pass's name as it
    /// starts -- handy for the CLI demo so the pipeline's stages are visible
    /// rather than a single opaque jump from raw IR to output.
    pub fn run_verbose(&self, function: &mut Function) {
        for pass in &self.passes {
            println!("  -- running pass: {}", pass.name());
            pass.run(function);
        }
    }
}

impl Default for PassManager {
    fn default() -> Self {
        Self::new()
    }
}
