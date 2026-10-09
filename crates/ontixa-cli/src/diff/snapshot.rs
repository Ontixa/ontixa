//! Stable comparison records derived from resolved compiler artifacts.
//!
//! Session IDs and source coordinates never enter these records. Names are
//! qualified by their declaring module, including types and escape callees.

use ontixa_db::Artifacts;
use ontixa_hir::DefKind;
use ontixa_memory::EscapeExit;
use ontixa_source::DefId;
use ontixa_types::Ty;
use serde::Serialize;
use serde_json::{Value as Json, json};
use std::collections::{BTreeMap, BTreeSet};

pub(super) type Snapshot = BTreeMap<String, Definition>;

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind")]
pub(super) enum Definition {
    #[serde(rename = "fn")]
    Function { params: Vec<Param>, returns: String },
    #[serde(rename = "data")]
    Data {
        shape: String,
        fields: Vec<Field>,
        variants: Vec<Variant>,
    },
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub(super) struct Param {
    name: String,
    #[serde(rename = "type")]
    ty: String,
    mutable: bool,
    behavior: String,
    escapes: Vec<Escape>,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Escape {
    Return,
    Call { callee: String },
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub(super) struct Field {
    name: String,
    #[serde(rename = "type")]
    ty: String,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub(super) struct Variant {
    name: String,
    discriminant: u32,
    payload: Vec<String>,
}

/// Module names must be representable without ambiguity in `module::name`.
pub(super) fn valid_module(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Unknown is an explicit inference result, not a missing field. Preserve it
/// in the record and separately disclose it, including on an unchanged report.
pub(super) fn uncertainties(snapshot: &Snapshot) -> Vec<Json> {
    let mut out = Vec::new();
    for (definition, record) in snapshot {
        if let Definition::Function { params, .. } = record {
            for (position, param) in params.iter().enumerate() {
                if param.behavior == "unknown" {
                    out.push(json!({
                        "definition": definition, "parameter": position,
                        "name": param.name, "behavior": "unknown",
                    }));
                }
            }
        }
    }
    out
}

fn identity(a: &Artifacts, id: DefId) -> Result<String, String> {
    let scope = &a.module.scope;
    let def = scope
        .defs
        .get(id.index())
        .ok_or_else(|| "definition identity is unavailable".to_string())?;
    let module = scope
        .file_name(def.file)
        .map(|name| a.interner.resolve(name))
        .filter(|name| valid_module(name))
        .ok_or_else(|| "logical module identity is unavailable or invalid".to_string())?;
    let name = a.interner.resolve(scope.symbols.get(def.name).name);
    Ok(format!("{module}::{name}"))
}

fn type_name(a: &Artifacts, ty: Ty) -> Result<String, String> {
    match ty {
        Ty::Struct(id) => identity(a, id),
        Ty::Array(elem) => Ok(format!("[{}]", type_name(a, elem.ty())?)),
        Ty::Poison => Err("a compared type is unresolved (poison)".into()),
        other => Ok(other.as_str().to_string()),
    }
}

/// Refuses incomplete semantic information instead of treating it as equality.
pub(super) fn normalize(a: &Artifacts) -> Result<Snapshot, String> {
    if !a.is_valid() {
        return Err("source diagnostics prevent comparison".into());
    }
    let scope = &a.module.scope;
    let mut modules = BTreeSet::new();
    for file in &scope.files {
        let name = scope
            .file_name(*file)
            .map(|n| a.interner.resolve(n))
            .filter(|name| valid_module(name))
            .ok_or_else(|| "logical module identity is unavailable or invalid".to_string())?;
        if !modules.insert(name) {
            return Err(format!("ambiguous logical module `{name}`"));
        }
    }
    let mut out = Snapshot::new();
    for def in &scope.defs {
        let key = identity(a, def.id)?;
        let record = match &def.kind {
            DefKind::Function(sig) => {
                let behaviors = a.ownership.param_behaviors.get(&def.id);
                let summaries = a.ownership.summaries.get(&def.id);
                let (behaviors, summaries) = match (behaviors, summaries) {
                    (Some(bs), Some(ss))
                        if bs.len() == sig.params.len() && ss.len() == sig.params.len() =>
                    {
                        (bs, ss)
                    }
                    _ => return Err(format!("incomplete parameter contracts for `{key}`")),
                };
                let params = sig
                    .params
                    .iter()
                    .zip(behaviors)
                    .zip(summaries)
                    .map(|((p, behavior), summary)| {
                        let escapes: Result<BTreeSet<Escape>, String> = summary
                            .escapes
                            .iter()
                            .map(|exit| match exit {
                                EscapeExit::Return => Ok(Escape::Return),
                                EscapeExit::ViaCall(id) => Ok(Escape::Call {
                                    callee: identity(a, *id)?,
                                }),
                            })
                            .collect();
                        Ok(Param {
                            name: a.interner.resolve(p.name).to_string(),
                            ty: type_name(a, Ty::from_ref(p.ty))?,
                            mutable: p.mutable,
                            behavior: behavior.as_str().to_string(),
                            escapes: escapes?.into_iter().collect(),
                        })
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                Definition::Function {
                    params,
                    returns: type_name(a, Ty::from_ref(sig.ret))?,
                }
            }
            DefKind::Data(shape) => {
                let fields = shape
                    .fields
                    .iter()
                    .map(|field| {
                        Ok(Field {
                            name: a
                                .interner
                                .resolve(scope.symbols.get(field.symbol).name)
                                .to_string(),
                            ty: type_name(a, Ty::from_ref(field.ty))?,
                        })
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                let variants = shape
                    .variants
                    .iter()
                    .map(|variant| {
                        Ok(Variant {
                            name: a
                                .interner
                                .resolve(scope.symbols.get(variant.symbol).name)
                                .to_string(),
                            discriminant: variant.index,
                            payload: variant
                                .payload
                                .iter()
                                .map(|ty| type_name(a, Ty::from_ref(*ty)))
                                .collect::<Result<Vec<_>, _>>()?,
                        })
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                Definition::Data {
                    shape: shape.shape_name().to_string(),
                    fields,
                    variants,
                }
            }
        };
        if out.insert(key.clone(), record).is_some() {
            return Err(format!("ambiguous definition identity `{key}`"));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ontixa_db::Db;
    use ontixa_hir::TypeRef;
    use ontixa_memory::ParamBehavior;

    fn artifact() -> Artifacts {
        let mut db = Db::new();
        let f = db.add_source_named("main", "fn f(x: i32) -> i32 { x }");
        db.compile_owned(f)
    }

    #[test]
    fn unknown_is_disclosed_and_missing_contracts_are_unavailable() {
        let mut a = artifact();
        assert!(normalize(&a).is_ok());
        a.ownership.param_behaviors.get_mut(&DefId::new(0)).unwrap()[0] = ParamBehavior::Unknown;
        let snapshot = normalize(&a).unwrap();
        assert_eq!(
            json!(snapshot["main::f"])["params"][0]["behavior"],
            "unknown"
        );
        assert_eq!(
            uncertainties(&snapshot),
            vec![json!({
                "definition": "main::f", "parameter": 0, "name": "x", "behavior": "unknown",
            })]
        );
        a.ownership.param_behaviors.clear();
        assert!(normalize(&a).unwrap_err().contains("incomplete"));
    }

    #[test]
    fn poison_and_ambiguous_identity_are_unavailable() {
        let mut a = artifact();
        let DefKind::Function(sig) = &mut a.module.scope.defs[0].kind else {
            panic!("expected function")
        };
        sig.ret = TypeRef::Poison;
        assert!(normalize(&a).unwrap_err().contains("poison"));

        let mut a = artifact();
        a.module.scope.defs.push(a.module.scope.defs[0].clone());
        assert!(normalize(&a).unwrap_err().contains("ambiguous"));
    }
}
