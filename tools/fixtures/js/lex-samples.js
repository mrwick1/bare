#!/usr/bin/env node
// Selected syntax for the lexer oracle check (tools/js_lex_check.sh). Parsed by Acorn, never run.
class C extends Base { #p = 1; static s = 2; get #g() { return this.#p ** 2 } m() { return (#p in this && this?.#p) ?? 0 } }
let a = b / c / d, re = /[/\]]+\/x/gi.test(s), t = x++ / 2, u = (1) / 2 / 3;
if (a) /re/.exec(b); else a /= 2;
a ??= b; a ||= c; a &&= d; a **= 2; a >>>= 1; x = a ? .5 : 1; y = a?.b?.[0]?.(1);
const n = [0, 17, 0o17, 0b1_01, 0xFF_ff, 1_000.5e-3, .5, 5., 5.e3, 1e+3, 10n, 0xFn, 08, 089.5];
const s = ['a\nb\x41\u{1F600}\u{D83D}', "q\'\"\\", 'line \
continued', '\0\1\08\101', 'é日本😀', '\u{10FFFF}'.length && 'ok'];
const tpl = `a${ `b${ c }d` }e${ {x: {y: 1}}.x.y }f\n\${no} $ {no} ${ /re}/.source }g`;
const raw = String.raw`\unicode and \xerxes ${1}\u{`;
tag`x${1}y${2}z`;
lbl: for (const [k, { v = 1, ...rest }] of Object.entries(o)) { if (k) continue lbl; else break lbl }
async function f() { try { await g() } catch { } finally { h() } }
x = a
/b/g
/* block
comment */ y = 1 // line
z = function () { return /=/ }
var abc = 1, \u{62}cd = 2;
