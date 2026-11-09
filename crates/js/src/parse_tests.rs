//! Parser tests. The big accept/reject corpus (`tools/fixtures/js/parse-cases.txt`) is checked here against
//! the status that Acorn 8.15.0 gave (`parse-cases.expected`); `tools/js/parse_check.sh` also compares the
//! whole trees with Acorn.

use super::*;

fn ok(src: &str) -> Program<'_> {
    match parse_script(src, Limits::default()) {
        Ok(p) => p,
        Err(e) => panic!("{src:?} failed: {e}"),
    }
}

fn err(src: &str) -> Error {
    match parse_script(src, Limits::default()) {
        Ok(_) => panic!("{src:?} was accepted"),
        Err(e) => e,
    }
}

fn bin(op: BinaryOp) -> &'static str {
    use BinaryOp::*;
    match op {
        Add => "+", Sub => "-", Mul => "*", Div => "/", Mod => "%", Exp => "**", Eq => "==", Ne => "!=",
        StrictEq => "===", StrictNe => "!==", Lt => "<", Gt => ">", Le => "<=", Ge => ">=", Shl => "<<",
        Shr => ">>", UShr => ">>>", BitAnd => "&", BitOr => "|", BitXor => "^", In => "in", Instanceof => "instanceof",
    }
}

/// A fully parenthesised rendering of an expression, to see the tree shape.
fn show_expr(p: &Program, id: ExprId) -> String {
    let e = p.expr(id);
    match e.kind {
        ExprKind::Ident | ExprKind::Number(_) | ExprKind::String | ExprKind::Null | ExprKind::Bool(_) => p.text(e.span).to_string(),
        ExprKind::This => "this".into(),
        ExprKind::Binary { op, left, right } => format!("({} {} {})", show_expr(p, left), bin(op), show_expr(p, right)),
        ExprKind::Logical { op, left, right } => {
            let o = match op { LogicalOp::And => "&&", LogicalOp::Or => "||", LogicalOp::Nullish => "??" };
            format!("({} {} {})", show_expr(p, left), o, show_expr(p, right))
        }
        ExprKind::Unary { arg, .. } => format!("(u {})", show_expr(p, arg)),
        ExprKind::Await(a) => format!("(await {})", show_expr(p, a)),
        ExprKind::Update { prefix, arg, .. } => format!("({}upd {})", if prefix { "pre-" } else { "post-" }, show_expr(p, arg)),
        ExprKind::Assign { target, value, .. } => format!("({} = {})", show_pat(p, target), show_expr(p, value)),
        ExprKind::Cond { test, cons, alt } => format!("({} ? {} : {})", show_expr(p, test), show_expr(p, cons), show_expr(p, alt)),
        ExprKind::Call { callee, args, optional } => format!("(call{} {}{})", if optional { "?" } else { "" }, show_expr(p, callee), show_list(p, args)),
        ExprKind::New { callee, args } => format!("(new {}{})", show_expr(p, callee), show_list(p, args)),
        ExprKind::Member { obj, name, optional } => format!("({}{}{})", show_expr(p, obj), if optional { "?." } else { "." }, p.text(name)),
        ExprKind::Index { obj, index, optional } => format!("({}{}[{}])", show_expr(p, obj), if optional { "?." } else { "" }, show_expr(p, index)),
        ExprKind::PrivateMember { obj, name, .. } => format!("({}.{})", show_expr(p, obj), p.text(name)),
        ExprKind::Seq(l) => format!("(seq{})", show_list(p, l)),
        ExprKind::Spread(a) => format!("(... {})", show_expr(p, a)),
        ExprKind::Paren(a) => show_expr(p, a),
        ExprKind::Chain(a) => format!("(chain {})", show_expr(p, a)),
        ExprKind::Array(l) => format!("[{}]", show_list(p, l).trim_start()),
        ExprKind::Hole => "<hole>".into(),
        ExprKind::Function(f) => {
            let f = p.func(f);
            let kind = if f.kind == FuncKind::Arrow { "arrow" } else { "fn" };
            let ps: Vec<String> = p.list(f.params).map(|x| show_pat(p, x)).collect();
            format!("({}{} ({}){})", if f.is_async() { "async " } else { "" }, kind, ps.join(" "),
                match f.body { Body::Expr(b) => format!(" => {}", show_expr(p, b)), Body::Block(_) => " {}".into() })
        }
        _ => format!("<{:?}>", std::mem::discriminant(&e.kind)),
    }
}

fn show_list(p: &Program, l: List<Expr>) -> String {
    p.list(l).map(|x| format!(" {}", show_expr(p, x))).collect()
}

fn show_pat(p: &Program, id: PatId) -> String {
    let n = p.pat(id);
    match n.kind {
        PatKind::Ident => p.text(n.span).to_string(),
        PatKind::Member(e) => show_expr(p, e),
        PatKind::Array(l) => format!("[{}]", p.list(l).map(|x| show_pat(p, x)).collect::<Vec<_>>().join(" ")),
        PatKind::Object(l) => format!("{{{}}}", p.list(l).map(|x| {
            show_pat(p, p.pat_prop(x).value)
        }).collect::<Vec<_>>().join(" ")),
        PatKind::Assign { target, default } => format!("({} = {})", show_pat(p, target), show_expr(p, default)),
        PatKind::Rest(t) => format!("...{}", show_pat(p, t)),
        PatKind::Hole => "<hole>".into(),
    }
}

/// The first statement must be an expression statement; render it.
fn shape(src: &str) -> String {
    let p = ok(src);
    let first = p.list(p.body).next().expect("a statement");
    match p.stmt(first).kind {
        StmtKind::Expr(e) => show_expr(&p, e),
        k => panic!("not an expression statement: {k:?}"),
    }
}

#[test]
fn precedence_and_associativity() {
    assert_eq!(shape("a + b * c"), "(a + (b * c))");
    assert_eq!(shape("a * b + c"), "((a * b) + c)");
    assert_eq!(shape("a - b - c"), "((a - b) - c)");
    assert_eq!(shape("a ** b ** c"), "(a ** (b ** c))");
    assert_eq!(shape("(a ** b) ** c"), "((a ** b) ** c)");
    assert_eq!(shape("a ** -b"), "(a ** (u b))");
    assert_eq!(shape("a = b = c"), "(a = (b = c))");
    assert_eq!(shape("a ? b : c ? d : e"), "(a ? b : (c ? d : e))");
    assert_eq!(shape("a || b && c"), "(a || (b && c))");
    assert_eq!(shape("a | b ^ c & d"), "(a | (b ^ (c & d)))");
    assert_eq!(shape("a == b < c"), "(a == (b < c))");
    assert_eq!(shape("a << b + c"), "(a << (b + c))");
    assert_eq!(shape("a ?? (b || c)"), "(a ?? (b || c))");
    assert_eq!(shape("a, b, c"), "(seq a b c)");
    assert_eq!(shape("a = (b, c)"), "(a = (seq b c))");
    assert_eq!(shape("a++ + ++b"), "((post-upd a) + (pre-upd b))");
    assert_eq!(shape("typeof a === 'x'"), "((u a) === 'x')");
    assert_eq!(shape("!a in b"), "((u a) in b)");
    assert_eq!(shape("a in b in c"), "((a in b) in c)");
    assert_eq!(shape("a = b ? c : d"), "(a = (b ? c : d))");
    assert_eq!(shape("a ? b : c = d"), "(a ? b : (c = d))");
    assert_eq!(shape("a += b -= c"), "(a = (b = c))");
}

#[test]
fn exponent_and_nullish_rules() {
    for s in ["-a ** b", "typeof a ** b", "!a ** b", "+a ** 2", "~a ** 2", "a ?? b || c", "a || b ?? c", "a && b ?? c", "a ?? b && c"] {
        assert_eq!(err(s).kind, ErrorKind::Syntax, "{s}");
    }
    for s in ["(-a) ** b", "-(a ** b)", "(a ?? b) || c", "a ?? (b && c)", "++a ** b", "a++ ** b"] {
        ok(s);
    }
    assert_eq!(err("async function f(){ await a ** 2 }").kind, ErrorKind::Syntax);
    ok("async function f(){ (await a) ** 2 }");
}

#[test]
fn calls_new_member_and_chains() {
    assert_eq!(shape("new a.b.c"), "(new ((a.b).c))");
    assert_eq!(shape("new a().b"), "((new a).b)");
    assert_eq!(shape("new new a()()"), "(new (new a))");
    assert_eq!(shape("new (a())()"), "(new (call a))");
    assert_eq!(shape("a(b)(c)"), "(call (call a b) c)");
    assert_eq!(shape("new a(b)(c)"), "(call (new a b) c)");
    assert_eq!(shape("a?.b.c"), "(chain ((a?.b).c))");
    assert_eq!(shape("(a?.b).c"), "((chain (a?.b)).c)");
    assert_eq!(shape("a?.[0]?.(1)"), "(chain (call? (a?.[0]) 1))");
    assert_eq!(shape("a.b?.c.d"), "(chain (((a.b)?.c).d))");
    assert_eq!(shape("a[b][c]"), "((a[b])[c])");
    assert_eq!(shape("f(...a, b)"), "(call f (... a) b)");
    assert_eq!(shape("f(a, b,)"), "(call f a b)");
    for s in ["a?.b = 1", "a?.b++", "++a?.b", "a?.`x`", "a?.b`x`", "new a?.b", "new a?.()", "for (a?.b of c);"] {
        err(s);
    }
    ok("a?.5:1");
}

#[test]
fn arrows_versus_grouping() {
    assert_eq!(shape("(a, b)"), "(seq a b)");
    assert_eq!(shape("(a)"), "a");
    assert_eq!(shape("(a, b) => a"), "(arrow (a b) => a)");
    assert_eq!(shape("x => y => z"), "(arrow (x) => (arrow (y) => z))");
    assert_eq!(shape("a ? b => c : d => e"), "(a ? (arrow (b) => c) : (arrow (d) => e))");
    assert_eq!(shape("async x => x"), "(async arrow (x) => x)");
    assert_eq!(shape("async (x) => x"), "(async arrow (x) => x)");
    assert_eq!(shape("async(x)"), "(call async x)");
    assert_eq!(shape("async"), "async");
    assert_eq!(shape("(a = 1, [b], {c}, ...d) => a"), "(arrow ((a = 1) [b] {c} ...d) => a)");
    assert!(shape("() => ({})").starts_with("(arrow () => "));
    for s in ["(a,) ", "(,) => 1", "(...a, b) => 1", "(...a,) => 1", "((a)) => 1", "(a, (b)) => 1", "a + () => 1", "x\n=> 1", "(a)\n=> 1",
        "async (x)\n=> 1", "() => {}()", "() => {} + 1", "(a, a) => 1", "async (a, ...b,) => a", "(a = 1) => { 'use strict' }"] {
        err(s);
    }
    ok("(a,) => 1");
    ok("(() => {})()");
    ok("(a => a)(1)");
}

#[test]
fn await_in_arrow_parameters() {
    // ECMA-262 15.3.1 and 15.8.1: no AwaitExpression in parameters.
    for s in ["async function f(){ (a = await 1) => 1 }", "async function f(){ async (a = await 1) => 1 }", "async function f(a = await 1){}",
        "async function f({a = await 1}){}"] {
        assert_eq!(err(s).kind, ErrorKind::Early, "{s}");
    }
    // An await inside a nested function body does not count.
    err("async (a = await 1) => 1");
    ok("async function f(){ (a = async function(){ await 1 }) => 1 }");
    ok("async function f(){ x = (async () => await 1, 2) }");
    ok("async function f(){ (a = async () => await 1) => 1 }");
}

#[test]
fn destructuring() {
    assert_eq!(shape("[a, b = 1, ...c] = d"), "([a (b = 1) ...c] = d)");
    assert_eq!(shape("({a, b: [c], d = 1} = e)"), "({a [c] (d = 1)} = e)");
    assert_eq!(shape("[a.b, c[d]] = e"), "([(a.b) (c[d])] = e)");
    ok("for ([a, b] of c);");
    ok("for ({a, b} of c);");
    for s in ["[(a = 1)] = b", "({a = 1})", "({a = 1}, b)", "f({a = 1})", "x = {a = 1}", "[{a = 1}]", "[...a,] = b", "({...{a}} = b)", "var {...{a}} = b",
        "var [...a, b] = c", "[...a = 1] = b", "({__proto__: a, __proto__: b})", "[1] = c", "[a + b] = c", "({a: 1} = c)", "[this] = b", "[f()] = b",
        "1 = 2", "a + b = c", "++a++", "a++ = 1", "(a, b) = 1", "([a]) = 1", "({a}) = 1", "for ({a = 1};;);"] {
        err(s);
    }
    ok("({a = 1} = b)");
    ok("[{a = 1}] = b");
    ok("({__proto__: a, __proto__: b} = c)");
    ok("({__proto__: a, ['__proto__']: b})");
    ok("({__proto__, __proto__})");
    ok("[(a)] = b");
    ok("[(a.b)] = c");
    ok("({a: (b)} = c)");
}

#[test]
fn directives_and_strict_mode() {
    assert!(ok("'use strict'; a").strict);
    assert!(!ok("a; 'use strict'").strict);
    assert!(!ok("('use strict'); a").strict);
    let p = ok("'a'; 'use strict'; b");
    assert_eq!(p.list(p.body).filter(|s| matches!(p.stmt(*s).kind, StmtKind::Directive(_))).count(), 2);
    let p = ok("'a' + 1; 'b'");
    assert_eq!(p.list(p.body).filter(|s| matches!(p.stmt(*s).kind, StmtKind::Directive(_))).count(), 0);
    for s in ["'use strict'; 010", "'use strict'; with (a) {}", "'use strict'; var eval", "'use strict'; arguments = 1", "'use strict'; delete a",
        "'use strict'; delete (a)", "function f(a, a){ 'use strict' }", "function f(a = 1){ 'use strict' }", "function eval(){ 'use strict' }",
        "function f(eval){ 'use strict' }", "'use strict'; var public", "'use strict'; \"\\07\"", "\"\\07\"; \"use strict\"", "class A { m(){ 010 } }",
        "class let {}", "'use strict'; if (a) function f(){}", "'use strict'; yield: 1", "'use strict'; ({ eval } = a)", "'use strict'; [eval] = a"] {
        err(s);
    }
    ok("010");
    ok("with (a) {}");
    ok("function f(a, a){}");
    ok("'use strict'; delete a.b");
    ok("function f(){ 'use strict' } 010");
}

#[test]
fn asi_and_restricted_productions() {
    assert_eq!(ok("a\nb").list(ok("a\nb").body).len(), 2);
    assert_eq!(shape("a\n++b"), "a");
    ok("a\n(b)");
    assert_eq!(shape("a\n(b)"), "(call a b)");
    assert_eq!(shape("a\n[b]"), "(a[b])");
    for s in ["throw\na", "a b", "a ++ b", "if (a) b else c", "for (;;)\n"] {
        err(s);
    }
    let p = ok("function f(){ return\n1 }");
    let f = p.list(p.body).next().unwrap();
    let StmtKind::Function(f) = p.stmt(f).kind else { panic!() };
    let Body::Block(b) = p.func(f).body else { panic!() };
    assert_eq!(p.list(b).len(), 2);
    ok("do ; while(0) a");
    ok("do ; while(0)\na");
    ok("x\n++\ny");
    assert_eq!(p_count("x\n++\ny"), 2);
    ok("async\nfunction f(){}");
    assert_eq!(p_count("async\nfunction f(){}"), 2);
    ok("let\nx");
    assert_eq!(p_count("let\nx"), 1);
}

fn p_count(src: &str) -> usize {
    ok(src).list(ok(src).body).len()
}

#[test]
fn contexts() {
    for s in ["return", "break", "continue", "a: { continue a }", "while(1) continue b", "a: a: ;", "function f(){ new.targe }", "new.target", "super.x",
        "function f(){ super.x }", "({ m: function(){ super.x } })", "class A { constructor(){ super() } }", "class A extends B { m(){ super() } }",
        "class A extends B { constructor(){ function f(){ super() } } }", "x: while(1) { function f(){ break x } }", "function f(){ await 1 }",
        "switch (a) { default: default: }", "switch (a) { case 1: continue }", "class A { m(){ this.#a } }", "class A { #a; #a }",
        "class A { constructor(){} constructor(){} }", "class A { get constructor(){} }", "class A { static prototype(){} }", "class A { #constructor }",
        "class A { #a; m(){ delete this.#a } }", "class A { x = arguments }", "class A { m(){ super() } }", "async function f(await){}", "function* g(){ yield\n* 2 }", "function* g(){ var yield }"] {
        err(s);
    }
    ok("function f(){ new.target }");
    ok("function g(){ () => new.target }");
    ok("class A extends B { constructor(){ () => super() } }");
    ok("class A extends B { m(){ super.x; super[y]; super.z() } }");
    ok("({ m(){ super.x } })");
    ok("class A { #a; m(){ this.#a; this?.#a; #a in this } }");
    ok("class A { x = () => 1; static y = this }");
    ok("class A { m(){ class B { n(o){ o.#a } } } #a }");
    ok("a: while(1) continue a");
    ok("a: { break a }");
    ok("a: ; a: ;");
    ok("class A { get #a(){} set #a(v){} }");
    ok("async function f(){ await 1; for await (a of b); }");
    ok("async function* g(){ yield* x; for await (a of b); } x = async function*(){}; ({ async *g(){} }); class A { async *g(){} static async *[k](){} }");
    ok("var await; var yield; var let; var async; var of; var get; var set; var static");
}

#[test]
fn redeclaration() {
    for s in ["let a; let a", "let a; var a", "var a; let a", "{ var a } let a", "function f(a){ let a }", "try {} catch (e) { let e }",
        "try {} catch ([e]) { var e }", "{ function f(){} var f }", "switch (a) { case 1: let x; case 2: let x }", "'use strict'; { function f(){} function f(){} }",
        "for (let a;;) { var a }", "for (let a of b) { var a }", "for (let [a, a] of b);", "let a, a", "function f(){ let f; var f }"] {
        assert_eq!(err(s).kind, ErrorKind::Early, "{s}");
    }
    assert_eq!(err("const a").kind, ErrorKind::Syntax);
    ok("var a; var a");
    ok("function f(){} function f(){}");
    ok("{ function f(){} function f(){} }");
    ok("try {} catch (e) { var e }");
    ok("try {} catch (e) { { let e } }");
    ok("let a; { let a }");
    ok("function f(a){ var a }");
    ok("switch (a) { case 1: { let x } case 2: { let x } }");
}

#[test]
fn class_elements() {
    let p = ok("class A extends B { static x = 1; #p; get g(){} set g(v){} static async m(){} [k](){} 'a' = 2; constructor(){ super() } }");
    let c = p.list(p.body).next().unwrap();
    let StmtKind::Class(c) = p.stmt(c).kind else { panic!() };
    let c = p.class(c);
    assert!(c.superclass.is_some());
    assert_eq!(p.list(c.elems).len(), 8);
    assert_eq!(p_count("class A { get; set; static; async; }"), 1);
    assert_eq!(p_count("class A { a = 1\nb = 2 }"), 1);
    for s in ["class A { a = 1 b = 2 }", "class A { a b }", "class A { get a(b){} }", "class A { set a(){} }", "class A { set a(...v){} }"] {
        err(s);
    }
}

#[test]
fn templates() {
    let p = ok("`a${b}c${d}e`");
    let s = p.list(p.body).next().unwrap();
    let StmtKind::Expr(e) = p.stmt(s).kind else { panic!() };
    let ExprKind::Template(t) = p.expr(e).kind else { panic!() };
    let t = *p.template(t);
    assert_eq!(p.template_exprs(&t).len(), 2);
    let mut out = Vec::new();
    let mut texts = Vec::new();
    for q in p.template_quasis(&t) {
        out.clear();
        p.quasi(p.quasi_node(q), false, &mut out).unwrap();
        texts.push(String::from_utf16(&out).unwrap());
    }
    assert_eq!(texts, ["a", "c", "e"]);
    ok("`${`${a}`}`");
    ok("`${{a:1}.a}`");
    ok("`a${b}` / 2");
    ok("`${/}/.source}`");
    ok("tag`\\unicode`");
    err("`\\unicode`");
    err("`${a");
    err("`${}`");
    ok("`${a}}`");
}

#[test]
fn literal_values_decode_on_demand() {
    let p = ok("'a\\u{1F600}b'; 0x1f; 1_000.5; /ab+c/gi; x");
    let mut it = p.list(p.body);
    let mut out = Vec::new();
    let s = it.next().unwrap();
    // A string statement at the top is a directive.
    let StmtKind::Directive(e) = p.stmt(s).kind else { panic!() };
    p.string(p.expr(e).span, &mut out).unwrap();
    assert_eq!(String::from_utf16(&out).unwrap(), "a\u{1F600}b");
    let StmtKind::Expr(e) = p.stmt(it.next().unwrap()).kind else { panic!() };
    let ExprKind::Number(b) = p.expr(e).kind else { panic!() };
    assert_eq!(p.number(p.expr(e).span, b), Some(31.0));
    let StmtKind::Expr(e) = p.stmt(it.next().unwrap()).kind else { panic!() };
    let ExprKind::Number(b) = p.expr(e).kind else { panic!() };
    assert_eq!(p.number(p.expr(e).span, b), Some(1000.5));
    let StmtKind::Expr(e) = p.stmt(it.next().unwrap()).kind else { panic!() };
    assert!(matches!(p.expr(e).kind, ExprKind::RegExp));
    assert_eq!(p.regexp(p.expr(e).span), Some(("ab+c", "gi")));
}

#[test]
fn regexp_goal_is_chosen_by_the_parser() {
    assert_eq!(shape("a / b / c"), "((a / b) / c)");
    assert!(shape("a = /x/ / /y/").starts_with("(a = ("));
    ok("if (a) /b/.test(c)");
    ok("x = a\n/b/g");
    assert_eq!(p_count("x = a\n/b/g"), 1);
    ok("() => /x/");
    ok("`${a}` / 2");
    ok("({}) / 2");
    ok("typeof /x/");
    ok("a++ / 2");
    err("/a/gg");
    err("/a");
    err("/[/");
}

#[test]
fn regexp_pattern_gate() {
    // The pattern grammar is owned by another worker. Until it lands the parser only splits body and flags.
    const { assert!(!REGEXP_PATTERN_VALIDATED) };
    ok("/(/");
    ok("/a**/");
    ok("/a{2,1}/");
}

#[test]
fn class_static_blocks() {
    ok("class A { static { } }");
    ok("class A { static { var a; let b; this.x = super.y; new.target; (() => this); function f() { return arguments; } } }");
    ok("class A { static static() {} static async() {} static get() {} static x = 1; static\n{ } }");
    for s in [
        "class A { static { return; } }",
        "class A { static { arguments; } }",
        "class A { static { (() => arguments); } }",
        "class A { static { let a; var a; } }",
        "class A { static { await 1; } }",
        "class A { static { yield; } }",
        "class A { static { super(); } }",
        "class A { static { break x; } }",
        "x: { class A { static { break x; } } }",
        "class A { static { var await; } }",
        "class A { static async { } }",
        "class A { static { } static { }",
    ] {
        assert!(parse_script(s, Limits::default()).is_err(), "{s}");
    }
}

#[test]
fn known_acorn_divergences() {
    // Acorn 8.15.0 accepts these. ECMA-262 rejects them, and so does this parser.
    // `() => {} ? 1 : 2`: an ArrowFunction is not a ConditionalExpression operand (15.3).
    err("() => {} ? 1 : 2");
    // `[{a = 1}.x] = b`: CoverInitializedName only exists in a pattern (13.2.5.1).
    err("[{a = 1}.x] = b");
    // `new super()`: `super` is only a SuperProperty or SuperCall (13.3.5).
    err("class A extends B { constructor(){ new super() } }");
    // An octal escape in a directive before "use strict" (11.2.2).
    err("\"\\07\"; \"use strict\"");
}

#[test]
fn error_spans_and_locations() {
    let e = err("let a;\n  let a");
    assert_eq!(e.kind, ErrorKind::Early);
    assert_eq!((e.start, e.end), (13, 14));
    let l = e.location("let a;\n  let a");
    assert_eq!((l.line, l.column), (2, 6));
    // Columns count UTF-16 units: the emoji is 2 units and 4 bytes.
    let src = "'😀'; x y";
    let e = err(src);
    assert_eq!(e.kind, ErrorKind::Syntax);
    let l = e.location(src);
    assert_eq!((l.line, l.column), (1, 8));
    assert_eq!(&src[e.start as usize..e.end as usize], "y");
    // The first error wins.
    let e = err("a b c d");
    assert_eq!(e.start, 2);
    assert_eq!(p_count(""), 0);
}

#[test]
fn truncated_input_never_panics() {
    // Every prefix of a rich source either parses or returns an error.
    let src = "async function f(a = 1, {b, c: [d]}, ...e){ for await (const [x] of y) { label: if (x?.z ?? w) break label; else throw new Error(`m${x}`) } \
        return class A extends B { #p = 1; static s(){ super.s() } get g(){ return this.#p } }, (q) => ({ q }), /re[/]x/g.test(\"s\\u{41}\") }";
    ok(src);
    for i in 0..src.len() {
        if src.is_char_boundary(i) {
            let _ = parse_script(&src[..i], Limits::default());
        }
    }
}

fn deep_in_thread(f: impl FnOnce() + Send + 'static) {
    // Release: 2 MiB (the default of a spawned thread). The worst case at depth 256 measured about 0.5 MiB.
    // Debug frames are about 6 times larger (about 3.5 MiB for nested classes), so debug uses 8 MiB.
    let stack = if cfg!(debug_assertions) { 8 << 20 } else { 2 << 20 };
    std::thread::Builder::new().stack_size(stack).spawn(f).unwrap().join().unwrap();
}

#[test]
fn deep_nesting_is_a_bounded_error_on_a_2mib_stack() {
    deep_in_thread(|| {
        let n = 100_000;
        let cases: Vec<(&str, String)> = vec![
            ("parens", format!("{}1{}", "(".repeat(n), ")".repeat(n))),
            ("arrays", format!("{}{}", "[".repeat(n), "]".repeat(n))),
            ("objects", format!("{}{}", "({a:".repeat(n), "})".repeat(n))),
            ("blocks", format!("{}{}", "{".repeat(n), "}".repeat(n))),
            ("unary", format!("{}a", "!".repeat(n))),
            ("minus", format!("{}a", "- ".repeat(n))),
            ("typeof", format!("{}a", "typeof ".repeat(n))),
            ("update", format!("{}a", "++".repeat(n).replace("++++", "++ ++"))),
            ("exp", format!("{}1", "1 ** ".repeat(n))),
            ("ternary", format!("{}1", "a ? 1 : ".repeat(n))),
            ("assign", format!("{}1", "a = ".repeat(n))),
            ("arrows", format!("{}1", "a => ".repeat(n))),
            ("templates", format!("{}{}", "`${".repeat(n), "}`".repeat(n))),
            ("new", format!("{}a", "new ".repeat(n))),
            ("calls_in_args", format!("{}{}", "f(".repeat(n), ")".repeat(n))),
            ("if", format!("{}1", "if (1) ".repeat(n))),
            ("for", format!("{}1", "for (;;) ".repeat(n))),
            ("fn", format!("{}{}", "function f(){".repeat(n), "}".repeat(n))),
            ("class", format!("{}{}", "class A { m(){".repeat(n), "}}".repeat(n))),
            ("patterns", format!("var {}a{}", "[".repeat(n), "]".repeat(n))),
            ("pattern_assign", format!("{}a{} = 1", "[".repeat(n), "]".repeat(n))),
            ("param_patterns", format!("function f({}a{}){{}}", "[".repeat(n), "]".repeat(n))),
            ("await", format!("async function f(){{ {}1 }}", "await ".repeat(n))),
            ("spread", format!("{}a{}", "[...".repeat(n), "]".repeat(n))),
        ];
        for (name, src) in &cases {
            let e = match parse_script(src, Limits::default()) {
                Ok(_) => panic!("{name} was accepted"),
                Err(e) => e,
            };
            assert!(matches!(e.kind, ErrorKind::Limit(LimitKind::Depth)), "{name}: {e}");
        }
        // Just inside the limit still parses.
        let ok_src = format!("{}1{}", "(".repeat(100), ")".repeat(100));
        assert!(parse_script(&ok_src, Limits::default()).is_ok());
    });
}

#[test]
fn long_flat_input_is_not_deep() {
    deep_in_thread(|| {
        let n = 200_000;
        assert!(parse_script(&"a;".repeat(n), Limits::default()).is_ok());
        let chain = format!("a{}", " + b".repeat(n));
        let l = Limits { max_list: u32::MAX, ..Limits::default() };
        assert!(parse_script(&chain, l).is_ok());
        assert!(parse_script(&format!("a{}", ".b".repeat(n)), l).is_ok());
        assert!(parse_script(&format!("a{}", "(1)".repeat(n)), l).is_ok());
        assert!(parse_script(&format!("a{}", "[0]".repeat(n)), l).is_ok());
        assert!(parse_script(&format!("a{}", "`x`".repeat(n)), l).is_ok());
        assert!(parse_script(&format!("a{}", ", b".repeat(n)), l).is_ok());
    });
}

#[test]
fn limits() {
    let l = Limits { max_source: 4, ..Limits::default() };
    assert_eq!(parse_script("a;b;c", l).unwrap_err().kind, ErrorKind::Limit(LimitKind::Source));
    assert!(parse_script("a;b;", l).is_ok());
    let l = Limits { max_list: 3, ..Limits::default() };
    assert_eq!(parse_script("f(1,2,3,4)", l).unwrap_err().kind, ErrorKind::Limit(LimitKind::List));
    assert!(parse_script("f(1,2,3)", l).is_ok());
    assert_eq!(parse_script("a;b;c;d", l).unwrap_err().kind, ErrorKind::Limit(LimitKind::List));
    let l = Limits { max_nodes: 10, ..Limits::default() };
    assert_eq!(parse_script("a;a;a;a;a;a;a;a;a;a;a;a", l).unwrap_err().kind, ErrorKind::Limit(LimitKind::Nodes));
    let l = Limits { max_depth: 3, ..Limits::default() };
    assert_eq!(parse_script("((((a))))", l).unwrap_err().kind, ErrorKind::Limit(LimitKind::Depth));
    assert!(parse_script("a", l).is_ok());
}

#[test]
fn stats_count_the_nodes() {
    let p = ok("var a = 1 + 2;");
    let s = p.stats();
    assert_eq!(s.stmts, 1);
    assert_eq!(s.decls, 1);
    assert_eq!(s.exprs, 3);
    assert!(p.stats().nodes() >= 5);
}

#[test]
fn corpus_status_matches_acorn() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tools/fixtures/js/");
    let cases = std::fs::read_to_string(format!("{dir}parse-cases.txt")).unwrap();
    let expected = std::fs::read_to_string(format!("{dir}parse-cases.expected")).unwrap();
    let cases: Vec<&str> = cases.split("\n%%\n").collect();
    let expected: Vec<&str> = expected.lines().collect();
    assert_eq!(cases.len(), expected.len());
    let mut bad = Vec::new();
    for (i, (c, e)) in cases.iter().zip(&expected).enumerate() {
        let got = match parse_script(c, Limits::default()) {
            Ok(_) => "ok",
            Err(e) if e.kind == ErrorKind::Unsupported => "unsupported",
            Err(_) => "err",
        };
        if *e != format!("{i} {got}") {
            bad.push(format!("case {i} {c:?}: expected `{e}`, got `{got}`"));
        }
    }
    assert!(bad.is_empty(), "{} differ:\n{}", bad.len(), bad.join("\n"));
}

#[test]
fn cached_bundles_parse() {
    // The gate: real bundles. The directory is outside the repository, so skip when it is absent.
    let Ok(dir) = std::env::var("BARE_JS_BUNDLES") else { return };
    let mut n = 0;
    for f in std::fs::read_dir(dir).unwrap() {
        let path = f.unwrap().path();
        if path.extension().is_some_and(|e| e == "js") {
            let src = std::fs::read_to_string(&path).unwrap();
            if let Err(e) = parse_script(&src, Limits::default()) {
                panic!("{}: {} at {:?}", path.display(), e, e.location(&src));
            }
            n += 1;
        }
    }
    assert!(n > 0);
}

#[test]
fn caller_cannot_remove_the_recursion_cap() {
    deep_in_thread(|| {
        let source = format!("{}1{}", "(".repeat(500), ")".repeat(500));
        let limits = Limits { max_depth: u32::MAX, ..Limits::default() };
        assert!(matches!(parse_script(&source, limits), Err(Error { kind: ErrorKind::Limit(LimitKind::Depth), .. })));
    });
}

// ---------------------------------------------------------------- modules

fn modok(src: &str) -> Program<'_> {
    match parse_module(src, Limits::default()) {
        Ok(p) => p,
        Err(e) => panic!("{src:?} failed: {e}"),
    }
}

fn moderr(src: &str) -> Error {
    match parse_module(src, Limits::default()) {
        Ok(_) => panic!("{src:?} was accepted as a module"),
        Err(e) => e,
    }
}

fn exports(src: &str) -> Vec<String> {
    let p = modok(src);
    let m = p.module.as_ref().unwrap();
    m.exports
        .iter()
        .map(|e| match &e.source {
            ExportSource::Local(sp) => format!("{}={}", e.name, p.name(*sp)),
            ExportSource::Default => format!("{}=*default*", e.name),
            ExportSource::Indirect { request, name } => match name {
                ImportName::Name(n) => format!("{}={}.{}", e.name, m.requests[*request as usize], n),
                ImportName::Namespace => format!("{}={}.*", e.name, m.requests[*request as usize]),
            },
        })
        .collect()
}

#[test]
fn a_script_does_not_take_module_syntax() {
    for s in ["import a from 'b'", "import 'a'", "export const a = 1", "export default 1", "export {}", "import.meta"] {
        let e = err(s);
        assert_ne!(e.kind, ErrorKind::Unsupported, "{s}");
    }
    // Dynamic import is script syntax.
    ok("import('x').then(m => m)");
    ok("import('x', { with: {} })");
    ok("a = import('x')\nimport('y')");
    err("new import('x')");
    err("import()");
    err("import(...a)");
    err("import('a', 'b', 'c')");
}

#[test]
fn a_module_is_strict_and_has_no_return() {
    assert!(modok("var a;").strict);
    moderr("with (a) {}");
    moderr("return 1");
    moderr("var public;");
    moderr("var await;");
    moderr("function f() { var await; }");
    moderr("010");
    modok("this");
    modok("import.meta.url; import.meta");
    modok("function f() { return import.meta; }");
}

#[test]
fn import_forms() {
    let p = modok("import 'a'; import d from 'b'; import * as n from 'c'; import {x, y as z, \"s t\" as w} from 'd'; import e, {f} from 'b'; import g, * as h from 'e';");
    let m = p.module.as_ref().unwrap();
    assert_eq!(m.requests.iter().map(|r| &**r).collect::<Vec<_>>(), ["a", "b", "c", "d", "e"]);
    let got: Vec<String> = m
        .imports
        .iter()
        .map(|i| {
            let n = match &i.name { ImportName::Name(n) => n.to_string(), ImportName::Namespace => "*".into() };
            format!("{}:{}->{}", i.request, n, p.name(i.local))
        })
        .collect();
    assert_eq!(got, ["1:default->d", "2:*->n", "3:x->x", "3:y->z", "3:s t->w", "1:default->e", "1:f->f", "4:default->g", "4:*->h"]);
    // The statements of the module body hold no import.
    assert_eq!(p.list(p.body).len(), 0);
    // `from`, `as`, `of`, `get` and other context words are names.
    modok("import from from 'a'; import { as as as, from as from2, default as dd } from 'a';");
    modok("import { if as iff } from 'a';");
    modok("import json from './x.json' with { type: 'json' };");
    modok("import json2 from './x.json' assert { type: 'json' };");
    modok("import 'a' with { };");
    moderr("import { if } from 'a';");
    moderr("import {a} from;");
    moderr("import a, b from 'x';");
    moderr("import * from 'x';");
    moderr("import * as a, b from 'x';");
    moderr("import {a} 'x';");
    moderr("import a from x;");
    moderr("import {\"s\"} from 'x';");
    moderr("import a from 'x' with { type: 1 };");
    moderr("import a from 'x' with { type: 'json', type: 'css' };");
}

#[test]
fn import_early_errors() {
    moderr("import a from 'x'; var a;");
    moderr("import a from 'x'; let a;");
    moderr("import a from 'x'; function a() {}");
    moderr("import a from 'x'; import a from 'y';");
    moderr("import {a, b as a} from 'x';");
    moderr("import eval from 'x';");
    moderr("import arguments from 'x';");
    moderr("import {await} from 'x';");
    moderr("import {a as await} from 'x';");
    modok("import {await as a} from 'x';");
    modok("import a from 'x'; { let a; }");
    modok("import a from 'x'; function f(a) { var a; }");
    // Imports and exports only at the top level of a module.
    moderr("{ import a from 'x'; }");
    moderr("function f() { import a from 'x'; }");
    moderr("if (1) import a from 'x';");
    moderr("{ export var a; }");
    moderr("function f() { export default 1; }");
}

#[test]
fn export_forms() {
    assert_eq!(exports("export var a = 1, b; export let c; export const d = 2;"), ["a=a", "b=b", "c=c", "d=d"]);
    assert_eq!(exports("export function f() {} export function* g() {} export async function h() {} export async function* i() {} export class C {}"), ["f=f", "g=g", "h=h", "i=i", "C=C"]);
    assert_eq!(exports("export const {a, b: [c, ...d], ...e} = o;"), ["a=a", "c=c", "d=d", "e=e"]);
    assert_eq!(exports("var a, b; export {a, b as c, a as d, a as default, b as \"x y\"};"), ["a=a", "c=b", "d=a", "default=a", "x y=b"]);
    assert_eq!(exports("export default 1;"), ["default=*default*"]);
    assert_eq!(exports("export default function () {}"), ["default=*default*"]);
    assert_eq!(exports("export default function f() {}"), ["default=f"]);
    assert_eq!(exports("export default async function () {}"), ["default=*default*"]);
    assert_eq!(exports("export default function* () {}"), ["default=*default*"]);
    assert_eq!(exports("export default class {}"), ["default=*default*"]);
    assert_eq!(exports("export default class K {}"), ["default=K"]);
    assert_eq!(exports("export default (function () {});"), ["default=*default*"]);
    assert_eq!(exports("export default async () => 1;"), ["default=*default*"]);
    assert_eq!(exports("export default async\nfunction f() {}"), ["default=*default*"]);
    assert_eq!(exports("export { a as b, c } from 'm'; export {default} from 'n'; export {default as z, \"q\" as r} from 'n';"), ["b=m.a", "c=m.c", "default=n.default", "z=n.default", "r=n.q"]);
    assert_eq!(exports("export * as ns from 'm'; export * as \"a b\" from 'm';"), ["ns=m.*", "a b=m.*"]);
    let p = modok("export * from 'a'; export * from 'b'; export * from 'a';");
    assert_eq!(p.module.as_ref().unwrap().stars, [0, 1, 0]);
    // An exported import binding is a re-export, also for a namespace import.
    assert_eq!(exports("import {a} from 'm'; export {a};"), ["a=m.a"]);
    assert_eq!(exports("import d from 'm'; export {d as e};"), ["e=m.default"]);
    assert_eq!(exports("import * as ns from 'm'; export {ns};"), ["ns=m.*"]);
    // A string export name must be well-formed Unicode.
    assert!(parse_module("export { a as \"\\uD83D\" }; var a;", Limits::default()).is_err());
    assert!(parse_module("export { a as \"\\uD83D\\uDE00\" }; var a;", Limits::default()).is_ok());
    // The export declarations keep their declaration as a statement.
    assert_eq!(modok("export var a = 1; export function f() {} export class C {}").list(modok("export var a = 1; export function f() {} export class C {}").body).len(), 3);
}

#[test]
fn export_early_errors() {
    moderr("export var a; export var a;");
    moderr("export var a; export {a};");
    moderr("var a, b; export {a, b as a};");
    moderr("export default 1; export default 2;");
    moderr("export default 1; var d; export {d as default};");
    moderr("export {a};");
    moderr("export {a as b}; ");
    moderr("var a; export {a as default}; export default 1;");
    moderr("export {if};");
    moderr("export {\"s\"};");
    moderr("export {a} from 'x'; export {b as a} from 'y';");
    moderr("export * as a from 'x'; export * as a from 'y';");
    moderr("export * from;");
    moderr("export * as from 'x';");
    moderr("export default;");
    moderr("export;");
    moderr("export let;");
    moderr("export var a = 1, a;");
    moderr("export function () {}");
    moderr("export class {}");
    moderr("export default var a;");
    moderr("export default let a = 1;");
    moderr("export {a,,};");
    moderr("let a; export {a} from");
    moderr("export {\"a\" as b};");
    modok("let a; export {a,};");
    modok("export {} from 'x'; export {};");
    modok("export { a as default } from 'x'; export default 1 ;".replace(" export default 1 ;", "").as_str());
    modok("export function f() {} export {f as g};");
}

#[test]
fn top_level_await() {
    let p = modok("await 1;");
    assert!(p.module.as_ref().unwrap().has_tla);
    assert_eq!(p.func(p.module.as_ref().unwrap().func).mode, FuncMode::Async);
    assert!(modok("for await (const x of y) {}").module.as_ref().unwrap().has_tla);
    assert!(modok("let x = await f(); export {x};").module.as_ref().unwrap().has_tla);
    assert!(modok("export default await 1;").module.as_ref().unwrap().has_tla);
    assert!(modok("{ await 1 }").module.as_ref().unwrap().has_tla);
    assert!(modok("if (a) await b;").module.as_ref().unwrap().has_tla);
    // An await inside a function is not top-level.
    let p = modok("async function f() { await 1; } export {f};");
    assert!(!p.module.as_ref().unwrap().has_tla);
    assert_eq!(p.func(p.module.as_ref().unwrap().func).mode, FuncMode::Plain);
    assert!(!modok("var f = async () => await 1;").module.as_ref().unwrap().has_tla);
    assert!(!modok("var f = () => 1; var g = function () {};").module.as_ref().unwrap().has_tla);
    // Not in a plain function, not as a name.
    moderr("function f() { await 1; }");
    moderr("var f = () => await 1;");
    moderr("await;");
    moderr("var f = (await) => 1;");
    moderr("class A { m() { await 1; } }");
    // Allowed: await as an operand of an operator.
    modok("var x = (await a) + (await b); var y = await await c;");
    // A class static field or computed key at the top level: await is an operator in a computed key.
    modok("class A { [await 1]() {} }");
}

#[test]
fn module_scoping_rules() {
    // Top-level functions are lexical in a module.
    moderr("function f() {} var f;");
    moderr("var f; function f() {}");
    moderr("function f() {} function f() {}");
    moderr("let a; function a() {}");
    modok("function f() { var a; function a() {} }");
    // The HTML-like comment forms are not comments in a module (they are operators and a syntax error here).
    moderr("x = 1; <!-- not a comment");
    // A module starts with a directive prologue too.
    modok("'use strict'; var a;");
    // `new.target` and `super` are not allowed at the top level.
    moderr("new.target");
    moderr("super.x");
    // A label named await is not allowed, nor a name `yield` (strict).
    moderr("await: 1;");
    moderr("yield: 1;");
}

#[test]
fn a_module_counts_nodes_and_limits() {
    let big = format!("{}", "export var a = 1;\n".repeat(1).as_str());
    let lim = Limits { max_nodes: 3, ..Limits::default() };
    assert!(matches!(parse_module(&big, lim), Err(Error { kind: ErrorKind::Limit(LimitKind::Nodes), .. })));
    let (r, used) = parse_module_used(&big, Limits::default());
    assert!(r.is_ok());
    assert!(used > 3);
    // The same count is enough to parse the same source.
    assert!(parse_module(&big, Limits { max_nodes: used, ..Limits::default() }).is_ok());
}
