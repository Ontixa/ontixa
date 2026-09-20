//! Workspace-level name resolution.
//!
//! Collects top-level definitions (`fn`, `data`) across every
//! reachable file, builds per-file name environments ([`FileEnv`]),
//! resolves `use` declarations and the types used in signatures, and
//! detects duplicate definitions — everything needed before bodies
//! are lowered.
//!
//! Resolution rules:
//!
//! - One module per file; the file's stem is its module name.
//! - `use m;` binds `m` to the file that provides module `m`.
//! - `use m::x [as y];` binds `x` (or `y`) to a member of `m`.
//! - `data` and `fn` share one top-level namespace *per file*: the
//!   same name in two different files is not a conflict.
//! - Type positions resolve to a builtin primitive, a `data` def in
//!   the file's env (own defs and imports), or a qualified `m::T`.
//! - Field names within one `data` def must be unique.

use crate::hir::{
    DataShape, Def, DefKind, FieldDef, FileEnv, FnSig, ModuleScope, ParamDef, Symbol, SymbolKind,
    SymbolTable, TypeRef,
};
use ontixa_ast::{AstModule, Item, TypeExpr, UseDecl};
use ontixa_diagnostics::{Code, Diagnostic, Diagnostics};
use ontixa_source::{DefId, FileId, InternId, Interner, ModuleId, Span, SymbolId};
use rustc_hash::FxHashMap;

/// One file participating in workspace resolution.
pub struct WorkspaceFile<'a> {
    /// The file's id in the session.
    pub file: FileId,
    /// The module name this file provides (its stem).
    pub module: &'a str,
    /// The file's parsed AST.
    pub ast: &'a AstModule,
}

/// Resolves a single file with no imports — the degenerate workspace.
/// Kept for convenience callers (tests, `parse_hir`); the database
/// always calls [`resolve_workspace`].
pub fn resolve_module(
    ast: &AstModule,
    interner: &mut Interner,
    diags: &mut Diagnostics,
) -> ModuleScope {
    let files = [WorkspaceFile {
        file: FileId::new(0),
        module: "main",
        ast,
    }];
    resolve_workspace(FileId::new(0), &files, interner, diags)
}

/// Resolves the workspace reachable from `root`. `files` lists every
/// reachable file in discovery order (root first); each file's
/// `module` is the name `use` declarations resolve against. Module
/// resolution itself — turning `use m` into a file — happened when
/// the workspace was loaded; here `use` bindings are checked and
/// wired into per-file environments.
pub fn resolve_workspace(
    root: FileId,
    files: &[WorkspaceFile],
    interner: &mut Interner,
    diags: &mut Diagnostics,
) -> ModuleScope {
    let mut r = Resolver {
        interner,
        diags,
        symbols: SymbolTable::default(),
        defs: Vec::new(),
        envs: FxHashMap::default(),
        file_names: FxHashMap::default(),
        module_of: FxHashMap::default(),
    };
    r.run(root, files);
    ModuleScope {
        module: ModuleId::new(0),
        root,
        defs: r.defs,
        symbols: r.symbols,
        envs: r.envs,
        files: files.iter().map(|f| f.file).collect(),
        file_names: r.file_names,
    }
}

struct Resolver<'a> {
    interner: &'a mut Interner,
    diags: &'a mut Diagnostics,
    symbols: SymbolTable,
    defs: Vec<Def>,
    envs: FxHashMap<FileId, FileEnv>,
    /// File → its module name (for `use` resolution and diagnostics).
    file_names: FxHashMap<FileId, InternId>,
    /// Module name → providing file.
    module_of: FxHashMap<InternId, FileId>,
}

impl Resolver<'_> {
    fn run(&mut self, _root: FileId, files: &[WorkspaceFile]) {
        // Pass 0: module-name table. Two files providing the same
        // module name is an error — `use m` would be ambiguous.
        for f in files {
            let name = self.interner.intern(f.module);
            self.file_names.insert(f.file, name);
            if self.module_of.insert(name, f.file).is_some() {
                self.diags.set_file(Some(f.file));
                self.diags.push(
                    Diagnostic::error(
                        Code::DuplicateDef,
                        format!(
                            "module `{name}` is provided by more than one file",
                            name = f.module
                        ),
                    )
                    .subject(f.module.to_string()),
                );
            }
            self.envs.insert(f.file, FileEnv::default());
        }

        // Pass 1: allocate def symbols + check duplicates per file, so
        // signatures in any order can reference each other — including
        // across files.
        for wf in files {
            self.diags.set_file(Some(wf.file));
            for (item_index, item) in wf.ast.items.iter().enumerate() {
                // Spans stored in the scope are item-relative: an edit
                // that only shifts an item's absolute offset leaves
                // this `ModuleScope` value unchanged, so dependent
                // queries cut off instead of re-running. Diagnostics
                // emitted here use the AST's absolute spans directly.
                let base = item.span().start;
                let name_ident = match item {
                    Item::Data(d) => &d.name,
                    Item::Fn(f) => &f.name,
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
                    span: name_ident.span.rel(base),
                });
                let def_id = DefId::new(self.defs.len() as u32);
                let conflict = {
                    let env = &self.envs[&wf.file];
                    env.fns.contains_key(&interned) || env.datas.contains_key(&interned)
                };
                if conflict {
                    self.duplicate(&name_ident.name, name_ident.span);
                } else {
                    let env = self.envs.get_mut(&wf.file).expect("env allocated");
                    match item {
                        Item::Data(_) => {
                            env.datas.insert(interned, def_id);
                        }
                        Item::Fn(_) => {
                            env.fns.insert(interned, def_id);
                        }
                    }
                }
                // The def is allocated even on duplicates so later
                // references still resolve somewhere deterministic
                // (the first def wins).
                self.defs.push(Def {
                    id: def_id,
                    file: wf.file,
                    item: item_index as u32,
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
                    // The *name* span, not the whole item: a body-only
                    // edit must leave the scope value equal so
                    // dependent queries cut off. Item-relative, like
                    // every span stored in the scope.
                    span: name_ident.span.rel(base),
                });
            }
        }

        // Pass 2: `use` declarations — module bindings and member
        // imports, resolved against the module-name table.
        for wf in files {
            self.diags.set_file(Some(wf.file));
            for u in &wf.ast.uses {
                self.use_decl(wf.file, u);
            }
        }

        // Pass 3: fill signatures and shapes, resolving type
        // references through each file's own environment.
        let mut def_cursor = 0usize;
        for wf in files {
            self.diags.set_file(Some(wf.file));
            for item in &wf.ast.items {
                let def_id = DefId::new(def_cursor as u32);
                def_cursor += 1;
                let base = item.span().start;
                match item {
                    Item::Data(d) => {
                        let mut fields: Vec<FieldDef> = Vec::new();
                        let mut field_index: FxHashMap<InternId, u32> = FxHashMap::default();
                        // Absolute spans of already-seen field names —
                        // the duplicate-field label must render
                        // file-absolute.
                        let mut field_spans: FxHashMap<InternId, Span> = FxHashMap::default();
                        for f in &d.fields {
                            let fname = self.interner.intern(&f.name.name);
                            if field_index.contains_key(&fname) {
                                self.diags.push(
                                    Diagnostic::error(
                                        Code::DuplicateField,
                                        format!(
                                            "field `{}` is defined more than once",
                                            f.name.name
                                        ),
                                    )
                                    .primary(f.name.span)
                                    .label(field_spans[&fname], "previous definition here")
                                    .subject(f.name.name.clone()),
                                );
                                continue;
                            }
                            let ty = self.resolve_type(&f.ty, wf.file);
                            let symbol = self.symbols.push(Symbol {
                                id: SymbolId::new(0),
                                name: fname,
                                kind: SymbolKind::Field,
                                mutable: false,
                                owner: Some(def_id),
                                span: f.name.span.rel(base),
                            });
                            field_index.insert(fname, fields.len() as u32);
                            field_spans.insert(fname, f.name.span);
                            fields.push(FieldDef {
                                symbol,
                                ty,
                                index: fields.len() as u32,
                            });
                        }
                        self.defs[def_id.index()].kind = DefKind::Data(DataShape {
                            fields,
                            field_index,
                        });
                    }
                    Item::Fn(f) => {
                        let mut params = Vec::new();
                        // Absolute spans of already-seen param names —
                        // the duplicate-param label renders absolute.
                        let mut seen: FxHashMap<InternId, Span> = FxHashMap::default();
                        for (i, p) in f.params.iter().enumerate() {
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
                            let ty = self.resolve_type(&p.ty, wf.file);
                            // Params are body-local: their `Symbol`
                            // record is allocated by body lowering
                            // into `local_symbols[i]`. Resolution only
                            // fixes the identity (`local(i)`) and type.
                            params.push(ParamDef {
                                symbol: SymbolId::local(i as u32),
                                name: pname,
                                mutable: p.mutable,
                                ty,
                                span: p.span.rel(base),
                            });
                        }
                        let ret = f
                            .ret
                            .as_ref()
                            .map(|t| self.resolve_type(t, wf.file))
                            .unwrap_or(TypeRef::Unit);
                        self.defs[def_id.index()].kind = DefKind::Function(FnSig { params, ret });
                    }
                }
            }
        }
        self.diags.set_file(None);
    }

    /// `use m;` binds a module name; `use m as n;` binds it under an
    /// alias; `use m::x [as y];` binds one member of `m` locally.
    fn use_decl(&mut self, file: FileId, u: &UseDecl) {
        let segs = &u.path.segs;
        if segs.is_empty() {
            return;
        }
        let module_name = self.interner.intern(&segs[0].name);
        let module_file = match self.module_of.get(&module_name) {
            Some(&f) if f == file => {
                self.diags.push(
                    Diagnostic::error(
                        Code::UnknownModule,
                        format!("module `{}` is this file itself", segs[0].name),
                    )
                    .primary(segs[0].span)
                    .subject(segs[0].name.clone()),
                );
                return;
            }
            Some(&f) => f,
            None => {
                self.diags.push(
                    Diagnostic::error(
                        Code::UnknownModule,
                        format!("unknown module `{}`", segs[0].name),
                    )
                    .primary(segs[0].span)
                    .subject(segs[0].name.clone()),
                );
                return;
            }
        };
        if segs.len() > 2 {
            self.diags.push(
                Diagnostic::error(
                    Code::UnknownSymbol,
                    format!(
                        "unsupported path `{}` — only `m` or `m::x`",
                        u.path.display()
                    ),
                )
                .primary(u.path.span)
                .subject(u.path.display()),
            );
            return;
        }
        if segs.len() == 1 {
            // Module import: bind `m` (or the `as` alias).
            let bind = u.alias.as_ref().unwrap_or(&segs[0]);
            let interned = self.interner.intern(&bind.name);
            self.envs
                .get_mut(&file)
                .expect("env allocated")
                .modules
                .insert(interned, module_file);
            return;
        }
        // Member import: `use m::x [as y];`
        let member_name = self.interner.intern(&segs[1].name);
        let member = self.envs[&module_file]
            .fns
            .get(&member_name)
            .or_else(|| self.envs[&module_file].datas.get(&member_name))
            .copied();
        let Some(def) = member else {
            let module = self.interner.resolve(module_name).to_string();
            self.diags.push(
                Diagnostic::error(
                    Code::UnknownSymbol,
                    format!("module `{module}` has no member `{}`", segs[1].name),
                )
                .primary(segs[1].span)
                .subject(segs[1].name.clone()),
            );
            return;
        };
        let bind = u.alias.as_ref().unwrap_or(&segs[1]);
        let bind_interned = self.interner.intern(&bind.name);
        let env = self.envs.get_mut(&file).expect("env allocated");
        let conflicts = env.fns.contains_key(&bind_interned)
            || env.datas.contains_key(&bind_interned)
            || env.imports.contains_key(&bind_interned);
        if conflicts {
            self.diags.push(
                Diagnostic::error(
                    Code::DuplicateDef,
                    format!(
                        "name `{}` is already defined or imported in this file",
                        bind.name
                    ),
                )
                .primary(bind.span)
                .subject(bind.name.clone()),
            );
            return;
        }
        env.imports.insert(bind_interned, def);
    }

    fn duplicate(&mut self, name: &str, span: Span) {
        self.diags.push(
            Diagnostic::error(
                Code::DuplicateDef,
                format!("`{name}` is defined more than once"),
            )
            .primary(span)
            .subject(name.to_string()),
        );
    }

    /// Resolves a syntactic type path to a [`TypeRef`] through
    /// `file`'s environment: builtins, own `data` defs, imports, or
    /// a qualified `m::T` path.
    fn resolve_type(&mut self, ty: &TypeExpr, file: FileId) -> TypeRef {
        let segs = &ty.path.segs;
        if segs.len() == 1 {
            let name = segs[0].name.as_str();
            match name {
                "bool" => return TypeRef::Bool,
                "i32" => return TypeRef::I32,
                "i64" => return TypeRef::I64,
                "u32" => return TypeRef::U32,
                "u64" => return TypeRef::U64,
                "f32" => return TypeRef::F32,
                "f64" => return TypeRef::F64,
                "str" => return TypeRef::Str,
                "unit" => return TypeRef::Unit,
                _ => {}
            }
            let interned = self.interner.intern(name);
            let env = &self.envs[&file];
            let def = env.datas.get(&interned).copied().or_else(|| {
                env.imports
                    .get(&interned)
                    .copied()
                    .filter(|d| matches!(self.defs[d.index()].kind, DefKind::Data(_)))
            });
            return match def {
                Some(d) => TypeRef::Struct(d),
                None => {
                    self.diags.push(
                        Diagnostic::error(Code::UnknownType, format!("unknown type `{name}`"))
                            .primary(segs[0].span)
                            .subject(name.to_string()),
                    );
                    TypeRef::Poison
                }
            };
        }
        if segs.len() == 2 {
            let module_name = self.interner.intern(&segs[0].name);
            let Some(&module_file) = self.envs[&file].modules.get(&module_name) else {
                self.diags.push(
                    Diagnostic::error(
                        Code::UnknownModule,
                        format!("unknown module `{}`", segs[0].name),
                    )
                    .primary(segs[0].span)
                    .subject(segs[0].name.clone()),
                );
                return TypeRef::Poison;
            };
            let member = self.interner.intern(&segs[1].name);
            return match self.envs[&module_file].datas.get(&member) {
                Some(d) => TypeRef::Struct(*d),
                None => {
                    let module = self.interner.resolve(module_name).to_string();
                    self.diags.push(
                        Diagnostic::error(
                            Code::UnknownType,
                            format!("module `{module}` has no type `{}`", segs[1].name),
                        )
                        .primary(segs[1].span)
                        .subject(segs[1].name.clone()),
                    );
                    TypeRef::Poison
                }
            };
        }
        self.diags.push(
            Diagnostic::error(
                Code::UnknownType,
                format!("unsupported type path `{}`", ty.path.display()),
            )
            .primary(ty.path.span)
            .subject(ty.path.display()),
        );
        TypeRef::Poison
    }
}
