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
    /// An enum `data` value: discriminant index plus payload cells.
    Variant {
        /// The enum `data` definition.
        def: DefId,
        /// Discriminant (variant index in declaration order).
        tag: u32,
        /// Payload values, aligned with the variant's declaration.
        payload: Vec<Cell>,
    },
    /// A `[T]` value: elements as cells in order.
    Array(Vec<Cell>),
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
            Value::Variant { def, tag, payload } => Value::Variant {
                def: *def,
                tag: *tag,
                payload: payload
                    .iter()
                    .map(|c| Rc::new(RefCell::new(c.borrow().deep_clone())))
                    .collect(),
            },
            Value::Array(elems) => Value::Array(
                elems
                    .iter()
                    .map(|c| Rc::new(RefCell::new(c.borrow().deep_clone())))
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    /// Renders a value for `run` output. `name_of` resolves a data
    /// definition name; `variant_of` resolves `(def, discriminant)`
    /// to a variant name.
    pub fn show(
        &self,
        name_of: &dyn Fn(DefId) -> String,
        variant_of: &dyn Fn(DefId, u32) -> String,
    ) -> String {
        match self {
            Value::Int(v) => v.to_string(),
            Value::Float(v) => format!("{v}"),
            Value::Str(s) => format!("\"{s}\""),
            Value::Bool(b) => b.to_string(),
            Value::Unit => "unit".into(),
            Value::Hole => "<uninitialized>".into(),
            Value::Struct(d, fields) => {
                let inner: Vec<String> = fields
                    .iter()
                    .map(|c| c.borrow().show(name_of, variant_of))
                    .collect();
                format!("{} {{ {} }}", name_of(*d), inner.join(", "))
            }
            Value::Variant { def, tag, payload } => {
                let name = format!("{}::{}", name_of(*def), variant_of(*def, *tag));
                if payload.is_empty() {
                    name
                } else {
                    let inner: Vec<String> = payload
                        .iter()
                        .map(|c| c.borrow().show(name_of, variant_of))
                        .collect();
                    format!("{name}({})", inner.join(", "))
                }
            }
            Value::Array(elems) => {
                let inner: Vec<String> = elems
                    .iter()
                    .map(|c| c.borrow().show(name_of, variant_of))
                    .collect();
                format!("[{}]", inner.join(", "))
            }
        }
    }
}
