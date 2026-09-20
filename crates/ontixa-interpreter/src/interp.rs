//! MIR execution.
//!
//! A straight-line CFG walker: each block's statements run in order,
//! then the terminator selects the next block or returns. Calls are
//! contract-aware — `borrow`/`borrow_mut` arguments pass the caller's
//! storage cell, so callee writes land in place; all other positions
//! receive a fresh cell.
//!
//! Deliberately small: values are [`Value`], locals are [`Cell`]s, and
//! the call stack is the Rust stack (deep recursion traps by overflow —
//! acceptable for milestone 1).

use crate::value::{Cell, Value};
use ontixa_hir::{BinOp, HirModule, UnOp};
use ontixa_memory::ParamBehavior;
use ontixa_mir::{Const, MirModule, Operand, Place, Rvalue, Terminator};
use ontixa_source::{DefId, Interner};
use std::cell::RefCell;
use std::rc::Rc;

/// A runtime failure — always a compiler bug or a poisoned input,
/// since accepted programs type-check and pass ownership analysis.
#[derive(Debug, Clone, PartialEq)]
pub enum RuntimeError {
    /// The requested entry function does not exist.
    MissingEntry(String),
    /// An impossible state was reached (malformed MIR, hole read, ...).
    Trap(String),
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RuntimeError::MissingEntry(n) => write!(f, "no entry function `{n}`"),
            RuntimeError::Trap(m) => write!(f, "runtime trap: {m}"),
        }
    }
}

impl std::error::Error for RuntimeError {}

/// Executes a MIR module.
pub struct Interp<'a> {
    mir: &'a MirModule,
    module: &'a HirModule,
    interner: &'a Interner,
}

impl<'a> Interp<'a> {
    /// Binds an interpreter to a module.
    pub fn new(mir: &'a MirModule, module: &'a HirModule, interner: &'a Interner) -> Self {
        Interp {
            mir,
            module,
            interner,
        }
    }

    /// Runs `entry` (usually `main`) and returns its value. A
    /// `module::name` entry selects the function from that module's
    /// file; a bare name resolves in the workspace root file.
    pub fn run(&self, entry: &str) -> Result<Value, RuntimeError> {
        if let Some((m, f)) = entry.split_once("::") {
            return self.run_in(m, f);
        }
        let sym = self
            .interner
            .get(entry)
            .ok_or_else(|| RuntimeError::MissingEntry(entry.to_string()))?;
        let def = self
            .module
            .scope
            .root_env()
            .fns
            .get(&sym)
            .copied()
            .ok_or_else(|| RuntimeError::MissingEntry(entry.to_string()))?;
        self.call(def, Vec::new())
    }

    /// Runs `entry` from the file providing module `module` — the
    /// form cross-module entry points take (`dep::helper`).
    pub fn run_in(&self, module: &str, entry: &str) -> Result<Value, RuntimeError> {
        let missing = || RuntimeError::MissingEntry(format!("{module}::{entry}"));
        let m = self.interner.get(module).ok_or_else(missing)?;
        let file = self
            .module
            .scope
            .files
            .iter()
            .find(|f| self.module.scope.file_name(**f) == Some(m))
            .copied()
            .ok_or_else(missing)?;
        let sym = self.interner.get(entry).ok_or_else(missing)?;
        let def = self
            .module
            .scope
            .env(file)
            .and_then(|e| e.fns.get(&sym))
            .copied()
            .ok_or_else(missing)?;
        self.call(def, Vec::new())
    }

    /// Pretty-prints a result value using source names.
    pub fn show(&self, v: &Value) -> String {
        v.show(&|d| self.def_name(d))
    }

    fn def_name(&self, def: DefId) -> String {
        self.module
            .scope
            .defs
            .get(def.index())
            .map(|d| {
                self.interner
                    .resolve(self.module.scope.symbols.get(d.name).name)
                    .to_string()
            })
            .unwrap_or_else(|| format!("<def {}>", def.index()))
    }

    /// Calls a function with prepared argument cells.
    fn call(&self, def: DefId, args: Vec<Cell>) -> Result<Value, RuntimeError> {
        let body = self
            .mir
            .body(def)
            .ok_or_else(|| RuntimeError::Trap(format!("{} has no body", self.def_name(def))))?;
        if args.len() != body.params.len() {
            return Err(RuntimeError::Trap(format!(
                "{}: {} args for {} params",
                self.def_name(def),
                args.len(),
                body.params.len()
            )));
        }
        let mut locals: Vec<Cell> = args;
        locals.resize_with(body.locals.len(), || Rc::new(RefCell::new(Value::Hole)));
        let mut pc = 0usize;
        loop {
            let block = body
                .blocks
                .get(pc)
                .ok_or_else(|| RuntimeError::Trap(format!("bad block id {pc}")))?;
            for s in &block.stmts {
                match s {
                    ontixa_mir::MirStmt::Assign { dst, val } => {
                        let v = self.rvalue(&locals, val)?;
                        *self.cell(&locals, dst)?.borrow_mut() = v;
                    }
                    ontixa_mir::MirStmt::Eval { val } => {
                        self.rvalue(&locals, val)?;
                    }
                }
            }
            match &block.term {
                Terminator::Return(op) => return self.operand(&locals, op),
                Terminator::Goto(next) => pc = next.0 as usize,
                Terminator::Branch { cond, then, else_ } => {
                    pc = match self.operand(&locals, cond)? {
                        Value::Bool(true) => then.0 as usize,
                        Value::Bool(false) => else_.0 as usize,
                        v => {
                            return Err(RuntimeError::Trap(format!(
                                "non-bool branch condition {v:?}"
                            )));
                        }
                    };
                }
            }
        }
    }

    /// The cell a place refers to — the local, or a field cell inside
    /// a struct value. Shared cells are what make `borrow_mut` real.
    fn cell(&self, locals: &[Cell], place: &Place) -> Result<Cell, RuntimeError> {
        let mut c = locals
            .get(place.local.0 as usize)
            .cloned()
            .ok_or_else(|| RuntimeError::Trap(format!("bad local {}", place.local.0)))?;
        for f in &place.proj {
            let next = match &*c.borrow() {
                Value::Hole => {
                    return Err(RuntimeError::Trap(
                        "projection into a moved-out or uninitialized local".into(),
                    ));
                }
                Value::Struct(_, fields) => fields.get(*f as usize).cloned(),
                v => {
                    return Err(RuntimeError::Trap(format!(
                        "field projection on non-struct {v:?}"
                    )));
                }
            };
            c = next.ok_or_else(|| RuntimeError::Trap(format!("bad field index {f}")))?;
        }
        Ok(c)
    }

    /// Reads an operand's value (place contents are copied out).
    fn operand(&self, locals: &[Cell], op: &Operand) -> Result<Value, RuntimeError> {
        match op {
            Operand::Const(c) => Ok(match c {
                Const::Int(v) => Value::Int(*v),
                Const::Float(v) => Value::Float(*v),
                Const::Str(s) => Value::Str(Rc::from(s.as_str())),
                Const::Bool(b) => Value::Bool(*b),
                Const::Unit => Value::Unit,
            }),
            Operand::Place(p) => {
                let c = self.cell(locals, p)?;
                let v = c.borrow().clone();
                match v {
                    Value::Hole => Err(RuntimeError::Trap(
                        "read of moved-out or uninitialized local".into(),
                    )),
                    v => Ok(v),
                }
            }
        }
    }

    fn rvalue(&self, locals: &[Cell], rv: &Rvalue) -> Result<Value, RuntimeError> {
        match rv {
            Rvalue::Use(op) => self.operand(locals, op),
            Rvalue::Unary { op, operand } => {
                let v = self.operand(locals, operand)?;
                match (op, v) {
                    (UnOp::Neg, Value::Int(i)) => Ok(Value::Int(-i)),
                    (UnOp::Neg, Value::Float(f)) => Ok(Value::Float(-f)),
                    (UnOp::Not, Value::Bool(b)) => Ok(Value::Bool(!b)),
                    (op, v) => Err(RuntimeError::Trap(format!("bad unary {op:?} on {v:?}"))),
                }
            }
            Rvalue::Binary { op, lhs, rhs } => {
                let l = self.operand(locals, lhs)?;
                let r = self.operand(locals, rhs)?;
                binary(*op, l, r)
            }
            Rvalue::StructLit { def, fields } => {
                let n = self
                    .module
                    .scope
                    .data_shape(*def)
                    .map(|s| s.fields.len())
                    .unwrap_or(fields.len());
                let mut cells: Vec<Cell> =
                    (0..n).map(|_| Rc::new(RefCell::new(Value::Hole))).collect();
                for (idx, op) in fields {
                    let v = self.operand(locals, op)?;
                    let slot = cells.get_mut(*idx as usize).ok_or_else(|| {
                        RuntimeError::Trap(format!("struct lit field index {idx}"))
                    })?;
                    *slot = Rc::new(RefCell::new(v));
                }
                Ok(Value::Struct(*def, cells))
            }
            Rvalue::Call {
                def,
                args,
                contract,
            } => {
                // Contract-aware argument passing — and the oracle:
                // execution *validates* the inferred contract, it does
                // not merely trust it.
                //
                // - `borrow` args share the caller cell; the callee
                //   must not write through it. The cell's value is
                //   snapshotted before the call and compared after —
                //   a write under a shared contract traps.
                // - `borrow_mut` args share the cell, no check (writes
                //   are the point).
                // - `move`/`escape`/`unknown` args get a fresh cell —
                //   and the *caller's* cell is poisoned with `Hole`
                //   after the call, so a later read traps as a
                //   use-after-move the static pass should have caught.
                let mut cells = Vec::with_capacity(args.len());
                let mut shared: Vec<(Cell, Value)> = Vec::new();
                let mut consumed: Vec<Cell> = Vec::new();
                for (i, arg) in args.iter().enumerate() {
                    let b = contract.get(i).copied().unwrap_or(ParamBehavior::Unknown);
                    match (b, arg) {
                        (ParamBehavior::Borrow, Operand::Place(p)) => {
                            let c = self.cell(locals, p)?;
                            shared.push((c.clone(), c.borrow().deep_clone()));
                            cells.push(c);
                        }
                        (ParamBehavior::BorrowMut, Operand::Place(p)) => {
                            cells.push(self.cell(locals, p)?);
                        }
                        (
                            ParamBehavior::Move | ParamBehavior::Escape | ParamBehavior::Unknown,
                            Operand::Place(p),
                        ) => {
                            let c = self.cell(locals, p)?;
                            if matches!(*c.borrow(), Value::Hole) {
                                return Err(RuntimeError::Trap(
                                    "argument moved from an already-consumed local".into(),
                                ));
                            }
                            let v = c.borrow().clone();
                            cells.push(Rc::new(RefCell::new(v)));
                            consumed.push(c);
                        }
                        _ => cells.push(Rc::new(RefCell::new(self.operand(locals, arg)?))),
                    }
                }
                let v = self.call(*def, cells)?;
                for c in consumed {
                    *c.borrow_mut() = Value::Hole;
                }
                for (c, before) in shared {
                    if !deep_eq(&c.borrow(), &before) {
                        return Err(RuntimeError::Trap(
                            "borrow contract violated: callee wrote through a shared borrow".into(),
                        ));
                    }
                }
                Ok(v)
            }
        }
    }
}

/// Dynamic-dispatch binary op. Accepted programs are type-correct, so
/// `(Int, Int)`/`(Float, Float)`/`(Bool, Bool)` pairs are the only
/// reachable combinations.
fn binary(op: BinOp, l: Value, r: Value) -> Result<Value, RuntimeError> {
    use Value::{Bool, Float, Int, Str};
    Ok(match (op, l, r) {
        (BinOp::Add, Int(a), Int(b)) => Int(a + b),
        (BinOp::Add, Float(a), Float(b)) => Float(a + b),
        (BinOp::Sub, Int(a), Int(b)) => Int(a - b),
        (BinOp::Sub, Float(a), Float(b)) => Float(a - b),
        (BinOp::Mul, Int(a), Int(b)) => Int(a * b),
        (BinOp::Mul, Float(a), Float(b)) => Float(a * b),
        (BinOp::Div, Int(a), Int(b)) => {
            if b == 0 {
                return Err(RuntimeError::Trap("division by zero".into()));
            }
            Int(a / b)
        }
        (BinOp::Div, Float(a), Float(b)) => Float(a / b),
        (BinOp::Rem, Int(a), Int(b)) => {
            if b == 0 {
                return Err(RuntimeError::Trap("remainder by zero".into()));
            }
            Int(a % b)
        }
        (BinOp::Eq, a, b) => Bool(values_eq(&a, &b)),
        (BinOp::Ne, a, b) => Bool(!values_eq(&a, &b)),
        (BinOp::Lt, Int(a), Int(b)) => Bool(a < b),
        (BinOp::Lt, Float(a), Float(b)) => Bool(a < b),
        (BinOp::Le, Int(a), Int(b)) => Bool(a <= b),
        (BinOp::Le, Float(a), Float(b)) => Bool(a <= b),
        (BinOp::Gt, Int(a), Int(b)) => Bool(a > b),
        (BinOp::Gt, Float(a), Float(b)) => Bool(a > b),
        (BinOp::Ge, Int(a), Int(b)) => Bool(a >= b),
        (BinOp::Ge, Float(a), Float(b)) => Bool(a >= b),
        (BinOp::And, Bool(a), Bool(b)) => Bool(a && b),
        (BinOp::Or, Bool(a), Bool(b)) => Bool(a || b),
        (BinOp::Add, Str(a), Str(b)) => Str(Rc::from(format!("{a}{b}").as_str())),
        (op, a, b) => {
            return Err(RuntimeError::Trap(format!(
                "bad binary {op:?} on {a:?}, {b:?}"
            )));
        }
    })
}

/// Structural equality for the borrow oracle — follows field cells.
fn deep_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Struct(da, fa), Value::Struct(db, fb)) => {
            da == db
                && fa.len() == fb.len()
                && fa
                    .iter()
                    .zip(fb.iter())
                    .all(|(x, y)| deep_eq(&x.borrow(), &y.borrow()))
        }
        _ => values_eq(a, b),
    }
}

fn values_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Unit, Value::Unit) => true,
        _ => false,
    }
}
