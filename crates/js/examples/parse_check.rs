//! Check the parser (S2). Parsing only: nothing is run.
//!
//!   parse_check dump FILE          print `ok <tree>` or `err ...` for one script
//!   parse_check cases FILE         the same for every case in FILE (cases are separated by lines `%%`);
//!                                  one line each: `<n> ok <tree>`, `<n> err ...` or `<n> unsupported ...`
//!   parse_check stats FILE...      parse each file with its own goal choices; print size, time, nodes.
//!                                  Exit 1 if any file fails. This is the bundle gate.
//!
//! The tree text is the S-expression that `tools/js/parse_oracle.mjs` writes from Acorn, so the two
//! can be compared with `diff`: `(kind@start-end attributes children...)`, byte offsets, parentheses
//! not shown. See tools/js/parse_check.sh.

use bare_js::ast::*;
use bare_js::parse::*;
use std::fmt::Write;
use std::process::exit;
use std::time::Instant;

fn fail(msg: String) -> ! {
    eprintln!("FAIL: {msg}");
    exit(1)
}

struct Dump<'a> {
    p: &'a Program<'a>,
    out: String,
}

fn quote(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                let _ = write!(out, "\\u{{{:x}}}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

impl<'a> Dump<'a> {
    fn open(&mut self, tag: &str, span: Span) {
        let _ = write!(self.out, "({tag}@{}-{}", span.start, span.end);
    }
    fn close(&mut self) {
        self.out.push(')');
    }
    fn sp(&mut self) {
        self.out.push(' ');
    }
    fn text(&mut self, span: Span) {
        let t = self.p.text(span);
        quote(t, &mut self.out);
    }
    fn name(&mut self, span: Span) {
        let n = self.p.name(span);
        self.out.push_str(&n);
    }
    fn dash(&mut self) {
        self.out.push('-');
    }

    fn opt_expr(&mut self, e: Option<ExprId>) {
        self.sp();
        match e {
            Some(e) => self.expr(e),
            None => self.dash(),
        }
    }

    fn exprs(&mut self, l: List<Expr>) {
        for e in self.p.list(l) {
            self.sp();
            self.expr(e);
        }
    }

    fn key(&mut self, k: Key) {
        self.sp();
        match k {
            Key::None => self.dash(),
            Key::Name(s) => {
                self.open("id", s);
                self.sp();
                self.name(s);
                self.close();
            }
            Key::Str(s) | Key::Num(s) | Key::BigInt(s) => {
                self.open("lit", s);
                self.sp();
                self.text(s);
                self.close();
            }
            Key::Computed(e) => {
                self.out.push_str("(computed ");
                self.expr(e);
                self.close();
            }
            Key::Private(s) => {
                self.open("priv", s);
                self.sp();
                self.name(s);
                self.close();
            }
        }
    }

    fn func(&mut self, tag: &str, f: FuncId) {
        let f = *self.p.func(f);
        self.open(tag, f.span);
        self.sp();
        match f.name {
            Some(n) => self.name(n),
            None => self.dash(),
        }
        self.out.push_str(if f.is_async() { " async" } else { " sync" });
        if f.is_generator() {
            self.out.push_str(" gen");
        }
        self.out.push_str(if f.kind == FuncKind::Arrow { " arrow" } else { " func" });
        self.out.push_str(" (params");
        for p in self.p.list(f.params) {
            self.sp();
            self.pat(p);
        }
        self.out.push_str(") ");
        match f.body {
            Body::Block(l) => {
                self.out.push_str("(body");
                self.stmts(l);
                self.close();
            }
            Body::Expr(e) => {
                self.out.push_str("(expr ");
                self.expr(e);
                self.close();
            }            // `parse_script` keeps every body.
            Body::Lazy(_) => self.out.push_str("(lazy)"),
        }
        self.close();
    }

    fn class(&mut self, tag: &str, c: ClassId) {
        let c = *self.p.class(c);
        self.open(tag, c.span);
        self.sp();
        match c.name {
            Some(n) => self.name(n),
            None => self.dash(),
        }
        self.opt_expr(c.superclass);
        for el in self.p.list(c.elems) {
            let el = *self.p.elem(el);
            self.sp();
            let st = if el.is_static { "static" } else { "inst" };
            match el.kind {
                ElemKind::Field(init) => {
                    self.open("field", el.span);
                    self.sp();
                    self.out.push_str(st);
                    self.key(el.key);
                    self.opt_expr(init);
                }
                ElemKind::StaticBlock(f) => {
                    self.open("staticblock", el.span);
                    if let Body::Block(l) = self.p.func(f).body {
                        self.stmts(l);
                    }
                }
                ElemKind::Method(f) | ElemKind::Getter(f) | ElemKind::Setter(f) => {
                    let k = match el.kind {
                        ElemKind::Getter(_) => "get",
                        ElemKind::Setter(_) => "set",
                        _ if matches!(self.p.func(f).kind, FuncKind::Constructor | FuncKind::DerivedConstructor) => "constructor",
                        _ => "method",
                    };
                    self.open("method", el.span);
                    self.sp();
                    self.out.push_str(k);
                    self.sp();
                    self.out.push_str(st);
                    self.key(el.key);
                    self.sp();
                    self.func("fn", f);
                }
            }
            self.close();
        }
        self.close();
    }

    fn expr(&mut self, e: ExprId) {
        let ex = *self.p.expr(e);
        let sp = ex.span;
        match ex.kind {
            ExprKind::Paren(inner) => self.expr(inner),
            ExprKind::Ident => {
                self.open("id", sp);
                self.sp();
                self.name(sp);
                self.close();
            }
            ExprKind::This => {
                self.open("this", sp);
                self.close();
            }
            ExprKind::Null | ExprKind::Bool(_) | ExprKind::Number(_) | ExprKind::BigInt | ExprKind::String | ExprKind::RegExp => {
                self.open("lit", sp);
                self.sp();
                self.text(sp);
                self.close();
            }
            ExprKind::Template(t) => self.template("tpl", t),
            ExprKind::Tagged { tag, quasi } => {
                self.open("tagged", sp);
                self.sp();
                self.expr(tag);
                self.sp();
                self.template("tpl", quasi);
                self.close();
            }
            ExprKind::Array(l) => {
                self.open("array", sp);
                for el in self.p.list(l) {
                    self.sp();
                    if matches!(self.p.expr(el).kind, ExprKind::Hole) {
                        self.out.push_str("(hole)");
                    } else {
                        self.expr(el);
                    }
                }
                self.close();
            }
            ExprKind::Object(l) => {
                self.open("object", sp);
                for pr in self.p.list(l) {
                    let pr = *self.p.prop(pr);
                    self.sp();
                    if pr.kind == PropKind::Spread {
                        self.open("spread", pr.span);
                        self.sp();
                        self.expr(pr.value);
                        self.close();
                        continue;
                    }
                    self.open("prop", pr.span);
                    self.sp();
                    self.out.push_str(match pr.kind {
                        PropKind::Init => "init",
                        PropKind::Shorthand => "shorthand",
                        PropKind::Method => "method",
                        PropKind::Get => "get",
                        _ => "set",
                    });
                    self.key(pr.key);
                    self.sp();
                    self.expr(pr.value);
                    self.close();
                }
                self.close();
            }
            ExprKind::Function(f) => self.func("fn", f),
            ExprKind::Class(c) => self.class("class", c),
            ExprKind::Unary { op, arg } => {
                self.open("unary", sp);
                self.sp();
                self.out.push_str(match op {
                    UnaryOp::Neg => "-",
                    UnaryOp::Plus => "+",
                    UnaryOp::Not => "!",
                    UnaryOp::BitNot => "~",
                    UnaryOp::Typeof => "typeof",
                    UnaryOp::Void => "void",
                    UnaryOp::Delete => "delete",
                });
                self.sp();
                self.expr(arg);
                self.close();
            }
            ExprKind::Update { op, prefix, arg } => {
                self.open("update", sp);
                self.sp();
                self.out.push_str(if op == UpdateOp::Inc { "++" } else { "--" });
                self.out.push_str(if prefix { " prefix " } else { " postfix " });
                self.expr(arg);
                self.close();
            }
            ExprKind::Binary { op, left, right } => {
                self.open("bin", sp);
                self.sp();
                self.out.push_str(bin_str(op));
                self.sp();
                self.expr(left);
                self.sp();
                self.expr(right);
                self.close();
            }
            ExprKind::Logical { op, left, right } => {
                self.open("logical", sp);
                self.sp();
                self.out.push_str(logical_str(op));
                self.sp();
                self.expr(left);
                self.sp();
                self.expr(right);
                self.close();
            }
            ExprKind::Assign { op, target, value } => {
                self.open("assign", sp);
                self.sp();
                match op {
                    AssignOp::Assign => self.out.push('='),
                    AssignOp::Compound(b) => {
                        self.out.push_str(bin_str(b));
                        self.out.push('=');
                    }
                    AssignOp::Logical(l) => {
                        self.out.push_str(logical_str(l));
                        self.out.push('=');
                    }
                }
                self.sp();
                self.pat(target);
                self.sp();
                self.expr(value);
                self.close();
            }
            ExprKind::Cond { test, cons, alt } => {
                self.open("cond", sp);
                for x in [test, cons, alt] {
                    self.sp();
                    self.expr(x);
                }
                self.close();
            }
            ExprKind::Call { callee, args, optional } => {
                self.open("call", sp);
                self.out.push_str(if optional { " opt " } else { " - " });
                self.expr(callee);
                self.out.push_str(" (args");
                self.exprs(args);
                self.out.push_str("))");
            }
            ExprKind::New { callee, args } => {
                self.open("new", sp);
                self.sp();
                self.expr(callee);
                self.out.push_str(" (args");
                self.exprs(args);
                self.out.push_str("))");
            }
            ExprKind::Member { obj, name, optional } => {
                self.open("member", sp);
                self.out.push_str(if optional { " opt " } else { " - " });
                self.expr(obj);
                self.sp();
                self.open("id", name);
                self.sp();
                self.name(name);
                self.close();
                self.close();
            }
            ExprKind::PrivateMember { obj, name, optional } => {
                self.open("member", sp);
                self.out.push_str(if optional { " opt " } else { " - " });
                self.expr(obj);
                self.sp();
                self.open("priv", name);
                self.sp();
                self.name(name);
                self.close();
                self.close();
            }
            ExprKind::Index { obj, index, optional } => {
                self.open("index", sp);
                self.out.push_str(if optional { " opt " } else { " - " });
                self.expr(obj);
                self.sp();
                self.expr(index);
                self.close();
            }
            ExprKind::PrivateIn { name, right } => {
                self.open("bin", sp);
                self.out.push_str(" in ");
                self.open("priv", name);
                self.sp();
                self.name(name);
                self.close();
                self.sp();
                self.expr(right);
                self.close();
            }
            ExprKind::Super => {
                self.open("super", sp);
                self.close();
            }
            ExprKind::NewTarget => {
                self.open("newtarget", sp);
                self.close();
            }
            // The script goal never produces these: module code is not dumped.
            ExprKind::ImportCall { .. } | ExprKind::ImportMeta => {
                self.open("import", sp);
                self.close();
            }
            ExprKind::Await(a) => {
                self.open("await", sp);
                self.sp();
                self.expr(a);
                self.close();
            }
            ExprKind::Yield { arg, delegate } => {
                self.open("yield", sp);
                self.out.push_str(if delegate { " delegate " } else { " - " });
                match arg {
                    Some(a) => self.expr(a),
                    None => self.dash(),
                }
                self.close();
            }
            ExprKind::Seq(l) => {
                self.open("seq", sp);
                self.exprs(l);
                self.close();
            }
            ExprKind::Spread(a) => {
                self.open("spread", sp);
                self.sp();
                self.expr(a);
                self.close();
            }
            ExprKind::Hole => self.out.push_str("(hole)"),
            // Only a parse with lazy bodies makes one; this tool parses every body.
            ExprKind::Data => self.out.push_str("(data)"),
            ExprKind::Chain(a) => {
                self.open("chain", sp);
                self.sp();
                self.expr(a);
                self.close();
            }
        }
    }

    fn template(&mut self, tag: &str, t: Id<Template>) {
        let t = *self.p.template(t);
        let (first, last) = (self.p.template_quasis(&t).next().unwrap(), self.p.template_quasis(&t).last().unwrap());
        let sp = Span { start: self.p.quasi_node(first).span.start, end: self.p.quasi_node(last).span.end };
        self.open(tag, sp);
        let mut exprs = self.p.template_exprs(&t);
        let mut buf = Vec::new();
        for q in self.p.template_quasis(&t) {
            let q = *self.p.quasi_node(q);
            buf.clear();
            let _ = self.p.quasi(&q, true, &mut buf);
            self.sp();
            quote(&String::from_utf16_lossy(&buf), &mut self.out);
            if let Some(e) = exprs.next() {
                self.sp();
                self.expr(e);
            }
        }
        self.close();
    }

    fn pat(&mut self, p: PatId) {
        let pt = *self.p.pat(p);
        let sp = pt.span;
        match pt.kind {
            PatKind::Ident => {
                self.open("id", sp);
                self.sp();
                self.name(sp);
                self.close();
            }
            PatKind::Member(e) => self.expr(e),
            PatKind::Hole => self.out.push_str("(hole)"),
            PatKind::Array(l) => {
                self.open("parr", sp);
                for el in self.p.list(l) {
                    self.sp();
                    self.pat(el);
                }
                self.close();
            }
            PatKind::Object(l) => {
                self.open("pobj", sp);
                for pp in self.p.list(l) {
                    let pp = *self.p.pat_prop(pp);
                    self.sp();
                    if matches!(pp.key, Key::None) {
                        self.pat(pp.value);
                        continue;
                    }
                    self.open("pp", pp.span);
                    self.key(pp.key);
                    self.sp();
                    self.pat(pp.value);
                    self.close();
                }
                self.close();
            }
            PatKind::Assign { target, default } => {
                self.open("passign", sp);
                self.sp();
                self.pat(target);
                self.sp();
                self.expr(default);
                self.close();
            }
            PatKind::Rest(i) => {
                self.open("rest", sp);
                self.sp();
                self.pat(i);
                self.close();
            }
        }
    }

    fn stmts(&mut self, l: List<Stmt>) {
        for s in self.p.list(l) {
            self.sp();
            self.stmt(s);
        }
    }

    fn opt_stmt(&mut self, s: Option<StmtId>) {
        self.sp();
        match s {
            Some(s) => self.stmt(s),
            None => self.dash(),
        }
    }

    fn stmt(&mut self, s: StmtId) {
        let st = *self.p.stmt(s);
        let sp = st.span;
        match st.kind {
            StmtKind::Expr(e) => {
                self.open("expr", sp);
                self.sp();
                self.expr(e);
                self.close();
            }
            StmtKind::Directive(e) => {
                self.open("dir", sp);
                self.sp();
                self.expr(e);
                self.close();
            }
            StmtKind::Var { kind, decls } => {
                self.open("var", sp);
                self.sp();
                self.out.push_str(match kind {
                    VarKind::Var => "var",
                    VarKind::Let => "let",
                    VarKind::Const => "const",
                });
                for d in self.p.list(decls) {
                    let d = *self.p.decl(d);
                    self.sp();
                    self.open("decl", d.span);
                    self.sp();
                    self.pat(d.target);
                    self.opt_expr(d.init);
                    self.close();
                }
                self.close();
            }
            StmtKind::Function(f) => self.func("fdecl", f),
            StmtKind::Class(c) => self.class("cdecl", c),
            StmtKind::Return(a) => {
                self.open("return", sp);
                self.opt_expr(a);
                self.close();
            }
            StmtKind::If { test, cons, alt } => {
                self.open("if", sp);
                self.sp();
                self.expr(test);
                self.sp();
                self.stmt(cons);
                self.opt_stmt(alt);
                self.close();
            }
            StmtKind::For { init, test, update, body } => {
                self.open("for", sp);
                self.sp();
                match init {
                    ForInit::None => self.dash(),
                    ForInit::Var(v) => self.stmt(v),
                    ForInit::Expr(e) => self.expr(e),
                }
                self.opt_expr(test);
                self.opt_expr(update);
                self.sp();
                self.stmt(body);
                self.close();
            }
            StmtKind::ForIn { left, right, body } => {
                self.open("forin", sp);
                self.sp();
                self.for_left(left);
                self.sp();
                self.expr(right);
                self.sp();
                self.stmt(body);
                self.close();
            }
            StmtKind::ForOf { left, right, body, is_await } => {
                self.open("forof", sp);
                self.out.push_str(if is_await { " await " } else { " - " });
                self.for_left(left);
                self.sp();
                self.expr(right);
                self.sp();
                self.stmt(body);
                self.close();
            }
            StmtKind::While { test, body } => {
                self.open("while", sp);
                self.sp();
                self.expr(test);
                self.sp();
                self.stmt(body);
                self.close();
            }
            StmtKind::DoWhile { body, test } => {
                self.open("do", sp);
                self.sp();
                self.stmt(body);
                self.sp();
                self.expr(test);
                self.close();
            }
            StmtKind::Break(l) | StmtKind::Continue(l) => {
                self.open(if matches!(st.kind, StmtKind::Break(_)) { "break" } else { "continue" }, sp);
                self.sp();
                match l {
                    Some(l) => self.name(l),
                    None => self.dash(),
                }
                self.close();
            }
            StmtKind::Throw(e) => {
                self.open("throw", sp);
                self.sp();
                self.expr(e);
                self.close();
            }
            StmtKind::Try { block, handler, finalizer } => {
                self.open("try", sp);
                self.sp();
                self.stmt(block);
                self.sp();
                match handler {
                    None => self.dash(),
                    Some(h) => {
                        let h = *self.p.catch(h);
                        self.open("catch", h.span);
                        self.sp();
                        match h.param {
                            Some(p) => self.pat(p),
                            None => self.dash(),
                        }
                        self.sp();
                        self.stmt(h.body);
                        self.close();
                    }
                }
                self.opt_stmt(finalizer);
                self.close();
            }
            StmtKind::Switch { disc, cases } => {
                self.open("switch", sp);
                self.sp();
                self.expr(disc);
                for c in self.p.list(cases) {
                    let c = *self.p.case(c);
                    self.sp();
                    self.open("case", c.span);
                    self.opt_expr(c.test);
                    self.stmts(c.body);
                    self.close();
                }
                self.close();
            }
            StmtKind::Labeled { label, body } => {
                self.open("labeled", sp);
                self.sp();
                self.name(label);
                self.sp();
                self.stmt(body);
                self.close();
            }
            StmtKind::Block(l) => {
                self.open("block", sp);
                self.stmts(l);
                self.close();
            }
            StmtKind::Empty => {
                self.open("empty", sp);
                self.close();
            }
            StmtKind::Debugger => {
                self.open("debugger", sp);
                self.close();
            }
            StmtKind::ExportDefault(e) => {
                self.open("exportdefault", sp);
                self.sp();
                self.expr(e);
                self.close();
            }
            StmtKind::With { obj, body } => {
                self.open("with", sp);
                self.sp();
                self.expr(obj);
                self.sp();
                self.stmt(body);
                self.close();
            }
        }
    }

    fn for_left(&mut self, l: ForLeft) {
        match l {
            ForLeft::Var(s) => self.stmt(s),
            ForLeft::Pat(p) => self.pat(p),
        }
    }
}

fn bin_str(op: BinaryOp) -> &'static str {
    use BinaryOp::*;
    match op {
        Add => "+",
        Sub => "-",
        Mul => "*",
        Div => "/",
        Mod => "%",
        Exp => "**",
        Eq => "==",
        Ne => "!=",
        StrictEq => "===",
        StrictNe => "!==",
        Lt => "<",
        Gt => ">",
        Le => "<=",
        Ge => ">=",
        Shl => "<<",
        Shr => ">>",
        UShr => ">>>",
        BitAnd => "&",
        BitOr => "|",
        BitXor => "^",
        In => "in",
        Instanceof => "instanceof",
    }
}

fn logical_str(op: LogicalOp) -> &'static str {
    match op {
        LogicalOp::And => "&&",
        LogicalOp::Or => "||",
        LogicalOp::Nullish => "??",
    }
}

fn dump(src: &str) -> Result<String, Error> {
    let prog = parse_script(src, Limits::default())?;
    let mut d = Dump { p: &prog, out: String::new() };
    d.out.push_str("(program");
    d.stmts(prog.body);
    d.close();
    Ok(d.out)
}

fn describe(src: &str, e: &Error) -> String {
    let l = e.location(src);
    format!("{:?} {}:{} {}", e.kind, l.line, l.column, e.msg)
}

fn status_kib(key: &str) -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines().find_map(|l| l.strip_prefix(key)).and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok()).unwrap_or(0)
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let mode = a.get(1).map(String::as_str).unwrap_or("");
    match mode {
        "dump" => {
            let f = a.get(2).unwrap_or_else(|| fail("usage: parse_check dump FILE".into()));
            let src = std::fs::read_to_string(f).unwrap_or_else(|e| fail(format!("{f}: {e}")));
            match dump(&src) {
                Ok(t) => println!("ok {t}"),
                Err(e) => println!("err {}", describe(&src, &e)),
            }
        }
        "cases" => {
            let f = a.get(2).unwrap_or_else(|| fail("usage: parse_check cases FILE".into()));
            let text = std::fs::read_to_string(f).unwrap_or_else(|e| fail(format!("{f}: {e}")));
            for (n, case) in text.split("\n%%\n").enumerate() {
                match dump(case) {
                    Ok(t) => println!("{n} ok {t}"),
                    Err(e) if e.kind == ErrorKind::Unsupported => println!("{n} unsupported"),
                    Err(e) => println!("{n} err {}", describe(case, &e)),
                }
            }
        }
        "stats" => {
            if a.len() < 3 {
                fail("usage: parse_check stats FILE...".into());
            }
            let mut bad = 0;
            let (mut total_bytes, mut total_nodes, mut total_us) = (0usize, 0usize, 0u128);
            for f in &a[2..] {
                let src = std::fs::read_to_string(f).unwrap_or_else(|e| fail(format!("{f}: {e}")));
                let t = Instant::now();
                let r = parse_script(&src, Limits::default());
                let us = t.elapsed().as_micros();
                match r {
                    Ok(p) => {
                        let s = p.stats();
                        println!("ok   {f}: {} bytes, {} nodes, {} list items, tree {} bytes, {us} us", src.len(), s.nodes(), s.list_items, s.bytes);
                        total_bytes += src.len();
                        total_nodes += s.nodes();
                        total_us += us;
                    }
                    Err(e) => {
                        bad += 1;
                        println!("FAIL {f}: {}", describe(&src, &e));
                    }
                }
            }
            println!("files={} failed={bad} bytes={total_bytes} nodes={total_nodes} parse_us={total_us} rss_kib={}", a.len() - 2, status_kib("VmHWM:"));
            if bad > 0 {
                exit(1);
            }
        }
        _ => fail("usage: parse_check dump|cases|stats ...".into()),
    }
}
