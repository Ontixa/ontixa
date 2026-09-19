//! Runtime values for the MIR interpreter.
//!
//! Every storage location — locals, temporaries, and struct fields —
//! is a shared [`Cell`]. Call arguments whose inferred contract is
//! `borrow`/`borrow_mut` pass the *cell itself*, so a callee's writes
//! land in the caller's storage. Move-position values are copied into
//! fresh cells; ownership discipline was already enforced at compile
//! time.

use ontixa_source::DefId;
use std::cell::RefCell;
use std::rc::Rc;

/// A shared storage cell.
pub type Cell = Rc<RefCell<Value>>;

/// A runtime value.
#[derive(Debug, Clone)]
pub enum Value {
    /// Integer (machine width resolved at type-check time; `i128` is
    /// the evaluation superset).
    Int(i128),
    /// Float.
    Float(f64),
    /// String.
    Str(Rc<str>),
    /// Boolean.
    Bool(bool),
    /// Unit.
    Unit,
    /// A `data` value: fields as cells in declared order.
    Struct(DefId, Vec<Cell>),
    /// A read of a never-initialized local. Unreachable in accepted
    /// programs — the checker rejects uninitialized reads.
    Hole,
}

impl Value {
    /// Deep copy: shared cells are not preserved, so the result shares
    /// nothing with the input. Used where a genuine second owner is
    /// needed (none today — kept for completeness).
    pub fn deep_clone(&self) -> Value {
        match self {
            Value::Struct(d, fields) => Value::Struct(
                *d,
                fields
                    .iter()
                    .map(|c| Rc::new(RefCell::new(c.borrow().deep_clone())))
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    /// Renders a value for `run` output. `name_of` resolves the data
    /// definition name for struct display.
    pub fn show(&self, name_of: &dyn Fn(DefId) -> String) -> String {
        match self {
            Value::Int(v) => v.to_string(),
            Value::Float(v) => format!("{v}"),
            Value::Str(s) => format!("\"{s}\""),
            Value::Bool(b) => b.to_string(),
            Value::Unit => "unit".into(),
            Value::Hole => "<uninitialized>".into(),
            Value::Struct(d, fields) => {
                let inner: Vec<String> = fields.iter().map(|c| c.borrow().show(name_of)).collect();
                format!("{} {{ {} }}", name_of(*d), inner.join(", "))
            }
        }
    }
}
