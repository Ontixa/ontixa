//! Builds the [`SemanticGraph`] from the typed, analyzed module.

use crate::graph::{EdgeKind, NodeId, NodeKind, SemanticGraph};
use ontixa_hir::{DefKind, HirBody, HirExprKind, HirModule, HirPat, HirStmt, SymbolKind};
use ontixa_memory::{OwnershipTables, ParamBehavior};
use ontixa_source::{DefId, ExprId, FileId, Interner, Span, SymbolId};
use ontixa_types::{ModuleTypes, Ty, TypeTables};
use rustc_hash::FxHashMap;
use serde_json::json;

/// Builds the semantic program graph for a fully analyzed module.
///
/// `bases[def]` is the absolute start offset of `def`'s source item —
/// scope and body spans are item-relative, so node spans are
/// translated back to file-absolute for consumers.
pub fn build_graph(
    module: &HirModule,
    types: &ModuleTypes,
    ownership: &OwnershipTables,
    interner: &Interner,
    bases: &[u32],
) -> SemanticGraph {
    let mut b = Builder {
        module,
        types,
        ownership,
        interner,
        bases,
        g: SemanticGraph::new(),
        symbol_nodes: FxHashMap::default(),
        expr_nodes: FxHashMap::default(),
        type_nodes: FxHashMap::default(),
    };
    b.build()
}

/// `symbol_nodes` and `expr_nodes` are keyed by `(DefId, id)`:
/// `SymbolId`s and `ExprId`s are body-local, so a param or expr in
/// `f` must not collide with one in `g`.
struct Builder<'a> {
    module: &'a HirModule,
    types: &'a ModuleTypes,
    ownership: &'a OwnershipTables,
    interner: &'a Interner,
    /// `bases[def]` — absolute start of `def`'s item, for rebasing.
    bases: &'a [u32],
    g: SemanticGraph,
    symbol_nodes: FxHashMap<(DefId, SymbolId), NodeId>,
    expr_nodes: FxHashMap<(DefId, ExprId), NodeId>,
    type_nodes: FxHashMap<Ty, NodeId>,
}

impl Builder<'_> {
    /// Rebase an item-relative span belonging to `def` to
    /// file-absolute.
    fn abs(&self, def: DefId, span: Span) -> Span {
        span.abs(self.bases.get(def.index()).copied().unwrap_or(0))
    }
}

impl Builder<'_> {
    fn build(&mut self) -> SemanticGraph {
        let module_node = self.g.add_node(NodeKind::Module, "module", None);
        self.g.set_attr(
            module_node,
            "module",
            json!(self.module.scope.module.index()),
        );

        // One module node per reachable file — its stem is the name
        // `use` declarations resolve against. Defs hang off *their*
        // file's module node, so cross-file ownership stays visible.
        let mut file_nodes: FxHashMap<FileId, NodeId> = FxHashMap::default();
        for &file in &self.module.scope.files {
            let label = self
                .module
                .scope
                .file_name(file)
                .map(|n| self.interner.resolve(n).to_string())
                .unwrap_or_else(|| format!("file {}", file.index()));
            let n = self.g.add_node(NodeKind::Module, label, None);
            self.g.set_attr(n, "file", json!(file.index()));
            self.g.add_edge(module_node, n, EdgeKind::Declares);
            file_nodes.insert(file, n);
        }

        // Pass 1: def + module-level symbol nodes, so cross-references
        // resolve. Body-local symbols (params, locals) are created in
        // pass 2 when their owning body is walked.
        for def in &self.module.scope.defs {
            let (kind, label) = match def.kind {
                DefKind::Function(_) => (NodeKind::Function, self.sym_name(None, def.name)),
                DefKind::Data(_) => (NodeKind::Data, self.sym_name(None, def.name)),
            };
            let n = self
                .g
                .add_node(kind, label, Some(self.abs(def.id, def.span)));
            self.symbol_nodes.insert((def.id, def.name), n);
            let owner = file_nodes.get(&def.file).copied().unwrap_or(module_node);
            self.g.add_edge(owner, n, EdgeKind::Declares);
            self.g.set_attr(n, "def", json!(def.id.index()));
            self.g.set_attr(n, "file", json!(def.file.index()));
        }
        for sym in self.module.scope.symbols.iter() {
            // Module-level symbols: defs (already added), fields, and
            // enum variants.
            debug_assert!(!sym.id.is_local());
            if matches!(sym.kind, SymbolKind::Function | SymbolKind::Data) {
                continue; // already added as def nodes
            }
            let kind = match sym.kind {
                SymbolKind::Variant => NodeKind::Variant,
                _ => NodeKind::Field,
            };
            let owner = sym.owner.unwrap_or(DefId::new(0));
            let n = self.g.add_node(
                kind,
                self.sym_name(None, sym.id),
                Some(self.abs(owner, sym.span)),
            );
            self.symbol_nodes.insert((owner, sym.id), n);
            self.g.set_attr(n, "symbol", json!(sym.id.index()));
        }

        // Pass 2: per-def structure.
        for def in &self.module.scope.defs.clone() {
            match &def.kind {
                DefKind::Function(sig) => self.function_body_graph(def.id, sig.clone()),
                DefKind::Data(shape) => {
                    let dnode = self.symbol_nodes[&(def.id, def.name)];
                    self.g.set_attr(dnode, "shape", json!(shape.shape_name()));
                    for f in &shape.fields {
                        let fnode = self.symbol_nodes[&(def.id, f.symbol)];
                        self.g.add_edge_attr(
                            dnode,
                            fnode,
                            EdgeKind::HasField,
                            "position",
                            json!(f.index),
                        );
                        let ty = Ty::from_ref(f.ty);
                        let tnode = self.type_node(ty);
                        self.g.add_edge(fnode, tnode, EdgeKind::TypedAs);
                    }
                    for v in &shape.variants {
                        let vnode = self.symbol_nodes[&(def.id, v.symbol)];
                        self.g.add_edge_attr(
                            dnode,
                            vnode,
                            EdgeKind::HasVariant,
                            "discriminant",
                            json!(v.index),
                        );
                        // Payload element types, in order.
                        for (i, t) in v.payload.iter().enumerate() {
                            let tnode = self.type_node(Ty::from_ref(*t));
                            self.g.add_edge_attr(
                                vnode,
                                tnode,
                                EdgeKind::TypedAs,
                                "position",
                                json!(i),
                            );
                        }
                    }
                }
            }
        }
        std::mem::take(&mut self.g)
    }

    fn function_body_graph(&mut self, def: DefId, sig: ontixa_hir::FnSig) {
        let fnode = self.symbol_nodes[&(def, self.module.scope.def(def).name)];
        let ret_ty = Ty::from_ref(sig.ret);
        let ret_node = self.type_node(ret_ty);
        self.g.add_edge(fnode, ret_node, EdgeKind::Returns);

        let Some(body) = self.module.body(def) else {
            // Body missing (error path) — still expose params.
            self.param_edges(def, fnode, &sig, None);
            return;
        };
        let tables = self.types[def.index()].as_ref();

        // Body-local symbol nodes: params then `let` bindings.
        for sym in &body.local_symbols {
            let kind = match sym.kind {
                SymbolKind::Param => NodeKind::Param,
                SymbolKind::Local => NodeKind::Local,
                _ => continue,
            };
            // A `Passes` edge from an earlier-def call may already
            // have materialized this param node — don't duplicate it.
            if self.symbol_nodes.contains_key(&(def, sym.id)) {
                continue;
            }
            let n = self.g.add_node(
                kind,
                self.sym_name(Some(body), sym.id),
                Some(self.abs(def, sym.span)),
            );
            self.symbol_nodes.insert((def, sym.id), n);
            self.g.set_attr(n, "symbol", json!(sym.id.index()));
        }

        self.param_edges(def, fnode, &sig, Some(body));

        for sym in &body.local_symbols {
            if sym.kind != SymbolKind::Local {
                continue;
            }
            let lnode = self.symbol_nodes[&(def, sym.id)];
            self.g.add_edge(fnode, lnode, EdgeKind::HasLocal);
            if let Some(ty) = tables.and_then(|t| t.local_types.get(&sym.id)) {
                let tnode = self.type_node(*ty);
                self.g.add_edge(lnode, tnode, EdgeKind::TypedAs);
            }
        }
        let root = self.expr_node(body, tables, body.root);
        self.g.add_edge(fnode, root, EdgeKind::Contains);
    }

    /// Parameter contract edges — the inferred ownership behaviors.
    fn param_edges(
        &mut self,
        def: DefId,
        fnode: NodeId,
        sig: &ontixa_hir::FnSig,
        body: Option<&HirBody>,
    ) {
        let contract = self.ownership.contract(def);
        for (i, p) in sig.params.iter().enumerate() {
            let pnode = match self.symbol_nodes.get(&(def, p.symbol)) {
                Some(n) => *n,
                None => {
                    // No body (error path) — synthesize the node from
                    // the signature's ParamDef data.
                    let n = self.g.add_node(
                        NodeKind::Param,
                        self.interner.resolve(p.name).to_string(),
                        Some(self.abs(def, p.span)),
                    );
                    self.symbol_nodes.insert((def, p.symbol), n);
                    n
                }
            };
            let behavior = contract.get(i).copied().unwrap_or(ParamBehavior::Unknown);
            self.g.add_edge_attr(
                fnode,
                pnode,
                EdgeKind::HasParam,
                "behavior",
                json!(behavior.as_str()),
            );
            self.g.set_attr(pnode, "behavior", json!(behavior.as_str()));
            self.g.set_attr(pnode, "position", json!(i));
            if let Some(sum) = self.ownership.summary(def).get(i) {
                if !sum.escapes.is_empty() {
                    self.g.set_attr(
                        pnode,
                        "escapes",
                        json!(
                            sum.escapes
                                .iter()
                                .map(|e| e.as_str(&self.module.scope, self.interner))
                                .collect::<Vec<_>>()
                        ),
                    );
                }
                if !sum.evidence.is_empty() {
                    self.g.set_attr(
                        pnode,
                        "evidence",
                        json!(sum
                            .evidence
                            .iter()
                            .map(|e| {
                                let at = self.abs(def, e.at);
                                json!({"kind": e.kind.as_str(), "start": at.start, "end": at.end})
                            })
                            .collect::<Vec<_>>()),
                    );
                }
            }
            let _ = body;
            let ty = Ty::from_ref(p.ty);
            let tnode = self.type_node(ty);
            self.g.add_edge(pnode, tnode, EdgeKind::TypedAs);
        }
    }

    /// The callee's i-th param's node — get-or-create, since a call
    /// may target a def whose body hasn't been walked yet.
    fn param_node(&mut self, def: DefId, sym: SymbolId) -> NodeId {
        if let Some(n) = self.symbol_nodes.get(&(def, sym)) {
            return *n;
        }
        let sig = self.module.scope.fn_sig(def).expect("param of non-fn");
        let p = sig
            .params
            .iter()
            .find(|p| p.symbol == sym)
            .expect("param sym in sig");
        let n = self.g.add_node(
            NodeKind::Param,
            self.interner.resolve(p.name).to_string(),
            Some(self.abs(def, p.span)),
        );
        self.symbol_nodes.insert((def, sym), n);
        self.g.set_attr(n, "symbol", json!(sym.index()));
        n
    }

    // ---------- node helpers ----------

    /// Resolves a symbol's name. Body-local ids need the owning body;
    /// module-level ids pass `None`.
    fn sym_name(&self, body: Option<&HirBody>, sym: SymbolId) -> String {
        let s = if sym.is_local() {
            &body.expect("local symbol").local_symbols[sym.local_index()]
        } else {
            self.module.scope.symbols.get(sym)
        };
        self.interner.resolve(s.name).to_string()
    }

    fn type_node(&mut self, ty: Ty) -> NodeId {
        if let Some(n) = self.type_nodes.get(&ty) {
            return *n;
        }
        let label = match ty {
            Ty::Bool => "bool".into(),
            Ty::I32 => "i32".into(),
            Ty::I64 => "i64".into(),
            Ty::U32 => "u32".into(),
            Ty::U64 => "u64".into(),
            Ty::F32 => "f32".into(),
            Ty::F64 => "f64".into(),
            Ty::Str => "str".into(),
            Ty::Char => "char".into(),
            Ty::Unit => "unit".into(),
            Ty::Struct(d) => self.sym_name(None, self.module.scope.def(d).name),
            Ty::Array(e) => {
                // Element labels are plain — `[i32]`, `[P]`, ...
                let elem = match e.ty() {
                    Ty::Struct(d) => self.sym_name(None, self.module.scope.def(d).name),
                    t => t.as_str().to_string(),
                };
                format!("[{elem}]")
            }
            Ty::Poison => "<error>".into(),
        };
        let n = self.g.add_node(NodeKind::Type, label, None);
        self.g.set_attr(n, "ty", json!(format!("{ty:?}")));
        self.type_nodes.insert(ty, n);
        n
    }

    // ---------- expression & statement walk ----------

    fn expr_node(&mut self, body: &HirBody, tables: Option<&TypeTables>, id: ExprId) -> NodeId {
        if let Some(n) = self.expr_nodes.get(&(body.def, id)) {
            return *n;
        }
        let e = body.expr(id);
        let kind_name = match &e.kind {
            HirExprKind::Literal(_) => "literal",
            HirExprKind::Var(_) => "var",
            HirExprKind::Call { .. } => "call",
            HirExprKind::Field { .. } => "field",
            HirExprKind::Len { .. } => "len",
            HirExprKind::Index { .. } => "index",
            HirExprKind::Slice { .. } => "slice",
            HirExprKind::ArrayLit { .. } => "array_lit",
            HirExprKind::Range { .. } => "range",
            HirExprKind::For { .. } => "for",
            HirExprKind::Binary { .. } => "binary",
            HirExprKind::Unary { .. } => "unary",
            HirExprKind::If { .. } => "if",
            HirExprKind::Block { .. } => "block",
            HirExprKind::StructLit { .. } => "struct_lit",
            HirExprKind::VariantLit { .. } => "variant_lit",
            HirExprKind::Match { .. } => "match",
            HirExprKind::Poison => "poison",
        };
        let n = self.g.add_node(
            NodeKind::Expr,
            kind_name.to_string(),
            Some(self.abs(body.def, e.span)),
        );
        self.expr_nodes.insert((body.def, id), n);
        self.g.set_attr(n, "expr", json!(id.index()));
        self.g.set_attr(n, "expr_kind", json!(kind_name));
        if let Some(t) = tables {
            let ty = t.ty_of(id);
            let tnode = self.type_node(ty);
            self.g.add_edge(n, tnode, EdgeKind::TypedAs);
        }

        match e.kind.clone() {
            HirExprKind::Var(sym) => {
                if let Some(s) = self.symbol_nodes.get(&(body.def, sym)) {
                    let s = *s;
                    self.g.add_edge(n, s, EdgeKind::Reads);
                }
            }
            HirExprKind::Call { def, args } => {
                let callee = self.symbol_nodes[&(def, self.module.scope.def(def).name)];
                self.g.add_edge(n, callee, EdgeKind::Calls);
                let contract = self.ownership.contract(def).to_vec();
                for (i, a) in args.iter().enumerate() {
                    let an = self.expr_node(body, tables, *a);
                    self.g
                        .add_edge_attr(n, an, EdgeKind::Contains, "position", json!(i));
                    // The memory edge: arg expr → callee param, under
                    // the inferred contract. Requires the callee's
                    // param node — created lazily when the callee
                    // hasn't been walked yet.
                    if let Some(psym) = callee_param_sym(&self.module.scope, def, i) {
                        let pnode = self.param_node(def, psym);
                        let behavior = contract.get(i).copied().unwrap_or(ParamBehavior::Unknown);
                        self.g.edges.push(crate::graph::SpgEdge {
                            from: an,
                            to: pnode,
                            kind: EdgeKind::Passes,
                            attrs: serde_json::Map::from_iter([
                                ("position".into(), json!(i)),
                                ("behavior".into(), json!(behavior.as_str())),
                            ]),
                        });
                    }
                }
            }
            HirExprKind::Field { base, name, .. } => {
                let bn = self.expr_node(body, tables, base);
                self.g.add_edge(n, bn, EdgeKind::Contains);
                // Accessed field symbol, if resolvable.
                if let Some(Ty::Struct(def)) = tables.map(|t| t.ty_of(base)) {
                    if let Some(shape) = self.module.scope.data_shape(def) {
                        if let Some(idx) = shape.field_index.get(&name.id) {
                            let fsym = shape.fields[*idx as usize].symbol;
                            if let Some(fnid) = self.symbol_nodes.get(&(def, fsym)) {
                                let fnid = *fnid;
                                self.g.add_edge(n, fnid, EdgeKind::AccessesField);
                            }
                        }
                    }
                }
            }
            HirExprKind::Len { base } => {
                let bn = self.expr_node(body, tables, base);
                self.g.add_edge(n, bn, EdgeKind::Contains);
            }
            HirExprKind::ArrayLit { elems } => {
                for (i, e) in elems.iter().enumerate() {
                    let en = self.expr_node(body, tables, *e);
                    self.g
                        .add_edge_attr(n, en, EdgeKind::Contains, "position", json!(i));
                }
            }
            HirExprKind::Range { lo, hi } => {
                if let Some(l) = lo {
                    let ln = self.expr_node(body, tables, l);
                    self.g
                        .add_edge_attr(n, ln, EdgeKind::Contains, "role", json!("lo"));
                }
                if let Some(h) = hi {
                    let hn = self.expr_node(body, tables, h);
                    self.g
                        .add_edge_attr(n, hn, EdgeKind::Contains, "role", json!("hi"));
                }
            }
            HirExprKind::For { var, iter, body: b } => {
                if let Some(s) = self.symbol_nodes.get(&(body.def, var)) {
                    let s = *s;
                    self.g.add_edge(n, s, EdgeKind::Binds);
                }
                let it = self.expr_node(body, tables, iter);
                self.g
                    .add_edge_attr(n, it, EdgeKind::Contains, "role", json!("iter"));
                let bn = self.expr_node(body, tables, b);
                self.g
                    .add_edge_attr(n, bn, EdgeKind::Contains, "role", json!("body"));
            }
            HirExprKind::Index { base, index } => {
                let bn = self.expr_node(body, tables, base);
                self.g
                    .add_edge_attr(n, bn, EdgeKind::Contains, "position", json!(0));
                let ix = self.expr_node(body, tables, index);
                self.g
                    .add_edge_attr(n, ix, EdgeKind::Contains, "position", json!(1));
            }
            HirExprKind::Slice { base, lo, hi } => {
                let bn = self.expr_node(body, tables, base);
                self.g
                    .add_edge_attr(n, bn, EdgeKind::Contains, "position", json!(0));
                if let Some(l) = lo {
                    let ln = self.expr_node(body, tables, l);
                    self.g
                        .add_edge_attr(n, ln, EdgeKind::Contains, "role", json!("lo"));
                }
                if let Some(h) = hi {
                    let hn = self.expr_node(body, tables, h);
                    self.g
                        .add_edge_attr(n, hn, EdgeKind::Contains, "role", json!("hi"));
                }
            }
            HirExprKind::Binary { op, lhs, rhs } => {
                self.g.set_attr(n, "op", json!(format!("{op:?}")));
                let l = self.expr_node(body, tables, lhs);
                let r = self.expr_node(body, tables, rhs);
                self.g
                    .add_edge_attr(n, l, EdgeKind::Contains, "position", json!(0));
                self.g
                    .add_edge_attr(n, r, EdgeKind::Contains, "position", json!(1));
            }
            HirExprKind::Unary { op, expr } => {
                self.g.set_attr(n, "op", json!(format!("{op:?}")));
                let c = self.expr_node(body, tables, expr);
                self.g.add_edge(n, c, EdgeKind::Contains);
            }
            HirExprKind::If { cond, then, else_ } => {
                let c = self.expr_node(body, tables, cond);
                self.g
                    .add_edge_attr(n, c, EdgeKind::Contains, "role", json!("cond"));
                let t = self.expr_node(body, tables, then);
                self.g
                    .add_edge_attr(n, t, EdgeKind::Contains, "role", json!("then"));
                if let Some(e) = else_ {
                    let en = self.expr_node(body, tables, e);
                    self.g
                        .add_edge_attr(n, en, EdgeKind::Contains, "role", json!("else"));
                }
            }
            HirExprKind::Block { stmts, tail } => {
                for (i, s) in stmts.iter().enumerate() {
                    let sn = self.stmt_node(body, tables, s);
                    self.g
                        .add_edge_attr(n, sn, EdgeKind::Contains, "position", json!(i));
                }
                if let Some(t) = tail {
                    let tn = self.expr_node(body, tables, t);
                    self.g
                        .add_edge_attr(n, tn, EdgeKind::Contains, "role", json!("tail"));
                }
            }
            HirExprKind::StructLit { def, fields } => {
                let dnode = self.symbol_nodes[&(def, self.module.scope.def(def).name)];
                self.g.add_edge(n, dnode, EdgeKind::Constructs);
                for (name, v) in &fields {
                    let vn = self.expr_node(body, tables, *v);
                    self.g.add_edge_attr(
                        n,
                        vn,
                        EdgeKind::Contains,
                        "field",
                        json!(self.interner.resolve(name.id)),
                    );
                }
            }
            HirExprKind::VariantLit { def, variant, args } => {
                // Construction targets the variant symbol (its parent
                // data def is one `has_variant` edge away).
                if let Some(vdef) = self
                    .module
                    .scope
                    .data_shape(def)
                    .and_then(|s| s.variants.get(variant as usize))
                {
                    let vsym = vdef.symbol;
                    if let Some(vn) = self.symbol_nodes.get(&(def, vsym)) {
                        let vn = *vn;
                        self.g.add_edge(n, vn, EdgeKind::Constructs);
                    }
                    self.g.set_attr(n, "variant", json!(variant));
                }
                for (i, a) in args.iter().enumerate() {
                    let an = self.expr_node(body, tables, *a);
                    self.g
                        .add_edge_attr(n, an, EdgeKind::Contains, "position", json!(i));
                }
            }
            HirExprKind::Match { scrutinee, arms } => {
                let sn = self.expr_node(body, tables, scrutinee);
                self.g
                    .add_edge_attr(n, sn, EdgeKind::Contains, "role", json!("scrutinee"));
                for (i, arm) in arms.iter().enumerate() {
                    let an = self.g.add_node(
                        NodeKind::Arm,
                        "arm".to_string(),
                        Some(self.abs(body.def, arm.span)),
                    );
                    self.g
                        .add_edge_attr(n, an, EdgeKind::Contains, "position", json!(i));
                    match &arm.pat {
                        HirPat::Variant {
                            def,
                            variant,
                            binds,
                            ..
                        } => {
                            self.g.set_attr(an, "pattern", json!("variant"));
                            self.g.set_attr(an, "variant", json!(variant));
                            if let Some(vdef) = self
                                .module
                                .scope
                                .data_shape(*def)
                                .and_then(|s| s.variants.get(*variant as usize))
                            {
                                let vsym = vdef.symbol;
                                if let Some(vn) = self.symbol_nodes.get(&(*def, vsym)) {
                                    let vn = *vn;
                                    self.g.add_edge(an, vn, EdgeKind::Matches);
                                }
                            }
                            for (j, b) in binds.iter().enumerate() {
                                if let Some(sym) = b {
                                    if let Some(bn) = self.symbol_nodes.get(&(body.def, *sym)) {
                                        let bn = *bn;
                                        self.g.add_edge_attr(
                                            an,
                                            bn,
                                            EdgeKind::Binds,
                                            "position",
                                            json!(j),
                                        );
                                    }
                                }
                            }
                        }
                        HirPat::Bind { sym, .. } => {
                            self.g.set_attr(
                                an,
                                "pattern",
                                json!(if sym.is_some() { "bind" } else { "wildcard" }),
                            );
                            if let Some(sym) = sym {
                                if let Some(bn) = self.symbol_nodes.get(&(body.def, *sym)) {
                                    let bn = *bn;
                                    self.g.add_edge(an, bn, EdgeKind::Binds);
                                }
                            }
                        }
                        HirPat::Poison => {
                            self.g.set_attr(an, "pattern", json!("poison"));
                        }
                    }
                    let bn = self.expr_node(body, tables, arm.body);
                    self.g
                        .add_edge_attr(an, bn, EdgeKind::Contains, "role", json!("body"));
                }
            }
            HirExprKind::Literal(_) | HirExprKind::Poison => {}
        }
        n
    }

    fn stmt_node(&mut self, body: &HirBody, tables: Option<&TypeTables>, stmt: &HirStmt) -> NodeId {
        let (kind_name, span) = match stmt {
            HirStmt::Let { span, .. } => ("let", *span),
            HirStmt::Assign { span, .. } => ("assign", *span),
            HirStmt::Expr { expr, .. } => ("expr_stmt", body.expr(*expr).span),
            HirStmt::Return { span, .. } => ("return", *span),
        };
        let n = self.g.add_node(
            NodeKind::Stmt,
            kind_name.to_string(),
            Some(self.abs(body.def, span)),
        );
        self.g.set_attr(n, "stmt_kind", json!(kind_name));
        match stmt {
            HirStmt::Let { symbol, init, .. } => {
                if let Some(s) = self.symbol_nodes.get(&(body.def, *symbol)) {
                    let s = *s;
                    self.g.add_edge(n, s, EdgeKind::Binds);
                }
                if let Some(i) = init {
                    let in_ = self.expr_node(body, tables, *i);
                    self.g
                        .add_edge_attr(n, in_, EdgeKind::Contains, "role", json!("init"));
                }
            }
            HirStmt::Assign { target, value, .. } => {
                if let Some(s) = self.symbol_nodes.get(&(body.def, target.base)) {
                    let s = *s;
                    self.g.add_edge(n, s, EdgeKind::Writes);
                }
                let v = self.expr_node(body, tables, *value);
                self.g
                    .add_edge_attr(n, v, EdgeKind::Contains, "role", json!("value"));
            }
            HirStmt::Expr { expr, .. } => {
                let e = self.expr_node(body, tables, *expr);
                self.g.add_edge(n, e, EdgeKind::Contains);
            }
            HirStmt::Return { value, .. } => {
                if let Some(v) = value {
                    let vn = self.expr_node(body, tables, *v);
                    self.g
                        .add_edge_attr(n, vn, EdgeKind::Contains, "role", json!("value"));
                }
            }
        }
        n
    }
}

/// The i-th param's symbol in `def`'s signature, when it has one.
fn callee_param_sym(scope: &ontixa_hir::ModuleScope, def: DefId, i: usize) -> Option<SymbolId> {
    scope.fn_sig(def)?.params.get(i).map(|p| p.symbol)
}
