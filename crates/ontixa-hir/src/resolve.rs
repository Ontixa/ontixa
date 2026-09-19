//! Module-level name resolution.
//!
//! Collects top-level definitions (`fn`, `data`), builds the module
//! symbol table, resolves the types used in signatures, and detects
//! duplicate definitions — everything needed before bodies are lowered.
//!
//! Resolution rules (milestone 1):
//!
//! - One module per file; no imports yet.
//! - `data` and `fn` share one top-level namespace: a name may only be
//!   defined once.
//! - Type positions resolve to a builtin primitive or a `data` def.
//! - Field names within one `data` def must be unique.

use crate::hir::{
    DataShape, Def, DefKind, FieldDef, FnSig, ModuleScope, ParamDef, Symbol, SymbolKind,
    SymbolTable, TypeRef,
};
use ontixa_ast::{AstModule, Item, TypeExpr};
use ontixa_diagnostics::{Code, Diagnostic, Diagnostics};
use ontixa_source::{DefId, InternId, Interner, ModuleId, Span, SymbolId};
use rustc_hash::FxHashMap;

/// Resolves the module's top-level structure.
pub fn resolve_module(
    ast: &AstModule,
    interner: &mut Interner,
    diags: &mut Diagnostics,
) -> ModuleScope {
    let mut r = Resolver {
        interner,
        diags,
        symbols: SymbolTable::default(),
        defs: Vec::new(),
        fns: FxHashMap::default(),
        datas: FxHashMap::default(),
    };
    r.run(ast);
    ModuleScope {
        module: ModuleId::new(0),
        defs: r.defs,
        symbols: r.symbols,
        fns: r.fns,
        datas: r.datas,
    }
}

struct Resolver<'a> {
    interner: &'a mut Interner,
    diags: &'a mut Diagnostics,
    symbols: SymbolTable,
    defs: Vec<Def>,
    fns: FxHashMap<InternId, DefId>,
    datas: FxHashMap<InternId, DefId>,
}

impl Resolver<'_> {
    fn run(&mut self, ast: &AstModule) {
        // Pass 1: allocate def symbols + check duplicates, so signatures
        // in any order can reference each other.
        for item in &ast.items {
            let (name_ident, span) = match item {
                Item::Data(d) => (&d.name, d.span),
                Item::Fn(f) => (&f.name, f.span),
            };
            let interned = self.interner.intern(&name_ident.name);
            let kind = match item {
                Item::Data(_) => SymbolKind::Data,
                Item::Fn(_) => SymbolKind::Function,
            };
            let symbol = self.symbols.push(Symbol {
                id: SymbolId::new(0),
                name: interned,
                kind,
                mutable: false,
                owner: None,
                span: name_ident.span,
            });
            let def_id = DefId::new(self.defs.len() as u32);
            match item {
                Item::Data(_) => {
                    if self.fns.contains_key(&interned) || self.datas.contains_key(&interned) {
                        self.duplicate(&name_ident.name, name_ident.span, span);
                    } else {
                        self.datas.insert(interned, def_id);
                    }
                }
                Item::Fn(_) => {
                    if self.fns.contains_key(&interned) || self.datas.contains_key(&interned) {
                        self.duplicate(&name_ident.name, name_ident.span, span);
                    } else {
                        self.fns.insert(interned, def_id);
                    }
                }
            }
            // The def is allocated even on duplicates so later references
            // still resolve somewhere deterministic (the first def wins).
            self.defs.push(Def {
                id: def_id,
                name: symbol,
                kind: match item {
                    Item::Data(_) => DefKind::Data(DataShape {
                        fields: Vec::new(),
                        field_index: FxHashMap::default(),
                    }),
                    Item::Fn(_) => DefKind::Function(FnSig {
                        params: Vec::new(),
                        ret: TypeRef::Unit,
                    }),
                },
                span,
            });
        }

        // Pass 2: fill signatures and shapes, resolving type references.
        for (idx, item) in ast.items.iter().enumerate() {
            let def_id = DefId::new(idx as u32);
            match item {
                Item::Data(d) => {
                    let mut fields: Vec<FieldDef> = Vec::new();
                    let mut field_index: FxHashMap<InternId, u32> = FxHashMap::default();
                    for f in &d.fields {
                        let fname = self.interner.intern(&f.name.name);
                        if let Some(prev) = field_index.get(&fname) {
                            let prev_span = fields[*prev as usize].symbol;
                            self.diags.push(
                                Diagnostic::error(
                                    Code::DuplicateField,
                                    format!("field `{}` is defined more than once", f.name.name),
                                )
                                .primary(f.name.span)
                                .label(self.symbols.get(prev_span).span, "previous definition here")
                                .subject(f.name.name.clone()),
                            );
                            continue;
                        }
                        let ty = self.resolve_type(&f.ty);
                        let symbol = self.symbols.push(Symbol {
                            id: SymbolId::new(0),
                            name: fname,
                            kind: SymbolKind::Field,
                            mutable: false,
                            owner: Some(def_id),
                            span: f.name.span,
                        });
                        field_index.insert(fname, fields.len() as u32);
                        fields.push(FieldDef {
                            symbol,
                            ty,
                            index: fields.len() as u32,
                        });
                    }
                    self.defs[idx].kind = DefKind::Data(DataShape {
                        fields,
                        field_index,
                    });
                }
                Item::Fn(f) => {
                    let mut params = Vec::new();
                    let mut seen: FxHashMap<InternId, Span> = FxHashMap::default();
                    for p in &f.params {
                        let pname = self.interner.intern(&p.name.name);
                        if let Some(prev) = seen.get(&pname) {
                            self.diags.push(
                                Diagnostic::error(
                                    Code::DuplicateDef,
                                    format!(
                                        "parameter `{}` is defined more than once",
                                        p.name.name
                                    ),
                                )
                                .primary(p.name.span)
                                .label(*prev, "previous parameter here")
                                .subject(p.name.name.clone()),
                            );
                        } else {
                            seen.insert(pname, p.name.span);
                        }
                        let ty = self.resolve_type(&p.ty);
                        let symbol = self.symbols.push(Symbol {
                            id: SymbolId::new(0),
                            name: pname,
                            kind: SymbolKind::Param,
                            mutable: p.mutable,
                            owner: Some(def_id),
                            span: p.name.span,
                        });
                        params.push(ParamDef {
                            symbol,
                            ty,
                            span: p.span,
                        });
                    }
                    let ret = f
                        .ret
                        .as_ref()
                        .map(|t| self.resolve_type(t))
                        .unwrap_or(TypeRef::Unit);
                    self.defs[idx].kind = DefKind::Function(FnSig { params, ret });
                }
            }
        }
    }

    fn duplicate(&mut self, name: &str, span: Span, item_span: Span) {
        let _ = item_span;
        self.diags.push(
            Diagnostic::error(
                Code::DuplicateDef,
                format!("`{name}` is defined more than once"),
            )
            .primary(span)
            .subject(name.to_string()),
        );
    }

    /// Resolves a syntactic type name to a [`TypeRef`].
    fn resolve_type(&mut self, ty: &TypeExpr) -> TypeRef {
        let name = ty.name.name.as_str();
        match name {
            "bool" => TypeRef::Bool,
            "i32" => TypeRef::I32,
            "i64" => TypeRef::I64,
            "u32" => TypeRef::U32,
            "u64" => TypeRef::U64,
            "f32" => TypeRef::F32,
            "f64" => TypeRef::F64,
            "str" => TypeRef::Str,
            "unit" => TypeRef::Unit,
            _ => {
                let interned = self.interner.intern(name);
                match self.datas.get(&interned) {
                    Some(def) => TypeRef::Struct(*def),
                    None => {
                        self.diags.push(
                            Diagnostic::error(Code::UnknownType, format!("unknown type `{name}`"))
                                .primary(ty.name.span)
                                .subject(name.to_string()),
                        );
                        TypeRef::Poison
                    }
                }
            }
        }
    }
}
