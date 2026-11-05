// Syntax oracle for crates/js (S2): parse with the installed Acorn (ecma2022, script) and print the tree in the
// same S-expression form as `parse_check dump`: (kind@start-end attributes children...). Offsets are UTF-8 BYTE
// offsets. Parentheses are not shown (Acorn with preserveParens off). The input is only parsed, never run.
//
//   ACORN=file:///path/to/acorn.mjs node tools/js/parse_oracle.mjs dump FILE
//   ACORN=... node tools/js/parse_oracle.mjs cases FILE      (cases separated by lines `%%`)
//
// A case whose first line is `//! unsupported` is valid ES2022 that bare-js reports as unsupported (async
// generators, modules, static blocks). The oracle prints `unsupported` for it (Acorn rejects module syntax in a script, so the mark is the claim).
import { readFileSync } from 'node:fs';

const acorn = await (process.env.ACORN ? import(process.env.ACORN) : import('acorn'));

function q(s) {
  let o = '"';
  for (const ch of s) {
    const cp = ch.codePointAt(0);
    if (ch === '"') o += '\\"';
    else if (ch === '\\') o += '\\\\';
    else if (cp < 0x20 || cp > 0x7e) o += '\\u{' + cp.toString(16) + '}';
    else o += ch;
  }
  return o + '"';
}

function dumpTree(src) {
  // UTF-16 offset -> byte offset.
  const byteAt = new Uint32Array(src.length + 1);
  for (let i = 0, b = 0; i < src.length; ) {
    const cp = src.codePointAt(i), n = cp > 0xffff ? 2 : 1, len = cp < 0x80 ? 1 : cp < 0x800 ? 2 : cp < 0x10000 ? 3 : 4;
    for (let k = 0; k < n; k++) byteAt[i + k] = b;
    i += n; b += len;
    byteAt[i] = b;
  }
  const ast = acorn.parse(src, { ecmaVersion: 2022, sourceType: 'script', allowHashBang: true });
  const S = (n) => `@${byteAt[n.start]}-${byteAt[n.end]}`;
  const L = (list) => list.map((x) => ' ' + D(x)).join('');
  const key = (p) => {
    if (p.computed) return `(computed ${D(p.key)})`;
    return D(p.key);
  };
  const fn = (tag, n) =>
    `(${tag}${S(n)} ${n.id ? n.id.name : '-'} ${n.async ? 'async' : 'sync'}${n.generator ? ' gen' : ''} ${n.type === 'ArrowFunctionExpression' ? 'arrow' : 'func'} (params${L(n.params)}) ` +
    (n.body.type === 'BlockStatement' ? `(body${L(n.body.body)})` : `(expr ${D(n.body)})`) + ')';
  const cls = (tag, n) => {
    let o = `(${tag}${S(n)} ${n.id ? n.id.name : '-'} ${n.superClass ? D(n.superClass) : '-'}`;
    for (const el of n.body.body) {
      const st = el.static ? 'static' : 'inst';
      if (el.type === 'MethodDefinition') o += ` (method${S(el)} ${el.kind} ${st} ${key(el)} ${D(el.value)})`;
      else if (el.type === 'PropertyDefinition') o += ` (field${S(el)} ${st} ${key(el)} ${el.value ? D(el.value) : '-'})`;
      else o += ` (? ${el.type})`;
    }
    return o + ')';
  };
  const opt = (x) => (x ? D(x) : '-');
  function D(n) {
    if (n === null) return '(hole)';
    switch (n.type) {
      case 'Identifier': return `(id${S(n)} ${n.name})`;
      case 'PrivateIdentifier': return `(priv${S(n)} ${n.name})`;
      case 'ThisExpression': return `(this${S(n)})`;
      case 'Literal': return `(lit${S(n)} ${q(n.raw)})`;
      case 'TemplateLiteral': {
        let o = `(tpl${S(n)}`;
        n.quasis.forEach((qq, i) => { o += ' ' + q(qq.value.raw); if (i < n.expressions.length) o += ' ' + D(n.expressions[i]); });
        return o + ')';
      }
      case 'TaggedTemplateExpression': return `(tagged${S(n)} ${D(n.tag)} ${D(n.quasi)})`;
      case 'ArrayExpression': return `(array${S(n)}${L(n.elements)})`;
      case 'ArrayPattern': return `(parr${S(n)}${L(n.elements)})`;
      case 'ObjectExpression':
        return `(object${S(n)}` + n.properties.map((p) => {
          if (p.type === 'SpreadElement') return ' ' + D(p);
          const kind = p.kind === 'get' ? 'get' : p.kind === 'set' ? 'set' : p.method ? 'method' : p.shorthand ? 'shorthand' : 'init';
          return ` (prop${S(p)} ${kind} ${key(p)} ${D(p.value)})`;
        }).join('') + ')';
      case 'ObjectPattern':
        return `(pobj${S(n)}` + n.properties.map((p) => (p.type === 'RestElement' ? ' ' + D(p) : ` (pp${S(p)} ${key(p)} ${D(p.value)})`)).join('') + ')';
      case 'AssignmentPattern': return `(passign${S(n)} ${D(n.left)} ${D(n.right)})`;
      case 'RestElement': return `(rest${S(n)} ${D(n.argument)})`;
      case 'SpreadElement': return `(spread${S(n)} ${D(n.argument)})`;
      case 'FunctionExpression':
      case 'ArrowFunctionExpression': return fn('fn', n);
      case 'FunctionDeclaration': return fn('fdecl', n);
      case 'ClassExpression': return cls('class', n);
      case 'ClassDeclaration': return cls('cdecl', n);
      case 'UnaryExpression': return `(unary${S(n)} ${n.operator} ${D(n.argument)})`;
      case 'UpdateExpression': return `(update${S(n)} ${n.operator} ${n.prefix ? 'prefix' : 'postfix'} ${D(n.argument)})`;
      case 'BinaryExpression': return `(bin${S(n)} ${n.operator} ${D(n.left)} ${D(n.right)})`;
      case 'LogicalExpression': return `(logical${S(n)} ${n.operator} ${D(n.left)} ${D(n.right)})`;
      case 'AssignmentExpression': return `(assign${S(n)} ${n.operator} ${D(n.left)} ${D(n.right)})`;
      case 'ConditionalExpression': return `(cond${S(n)} ${D(n.test)} ${D(n.consequent)} ${D(n.alternate)})`;
      case 'CallExpression': return `(call${S(n)} ${n.optional ? 'opt' : '-'} ${D(n.callee)} (args${L(n.arguments)}))`;
      case 'NewExpression': return `(new${S(n)} ${D(n.callee)} (args${L(n.arguments)}))`;
      case 'MemberExpression':
        return n.computed
          ? `(index${S(n)} ${n.optional ? 'opt' : '-'} ${D(n.object)} ${D(n.property)})`
          : `(member${S(n)} ${n.optional ? 'opt' : '-'} ${D(n.object)} ${D(n.property)})`;
      case 'ChainExpression': return `(chain${S(n)} ${D(n.expression)})`;
      case 'SequenceExpression': return `(seq${S(n)}${L(n.expressions)})`;
      case 'AwaitExpression': return `(await${S(n)} ${D(n.argument)})`;
      case 'YieldExpression': return `(yield${S(n)} ${n.delegate ? 'delegate' : '-'} ${n.argument ? D(n.argument) : '-'})`;
      case 'Super': return `(super${S(n)})`;
      case 'MetaProperty': return `(newtarget${S(n)})`;
      // statements
      case 'ExpressionStatement': return `(${n.directive !== undefined ? 'dir' : 'expr'}${S(n)} ${D(n.expression)})`;
      case 'BlockStatement': return `(block${S(n)}${L(n.body)})`;
      case 'EmptyStatement': return `(empty${S(n)})`;
      case 'DebuggerStatement': return `(debugger${S(n)})`;
      case 'WithStatement': return `(with${S(n)} ${D(n.object)} ${D(n.body)})`;
      case 'ReturnStatement': return `(return${S(n)} ${opt(n.argument)})`;
      case 'LabeledStatement': return `(labeled${S(n)} ${n.label.name} ${D(n.body)})`;
      case 'BreakStatement': return `(break${S(n)} ${n.label ? n.label.name : '-'})`;
      case 'ContinueStatement': return `(continue${S(n)} ${n.label ? n.label.name : '-'})`;
      case 'IfStatement': return `(if${S(n)} ${D(n.test)} ${D(n.consequent)} ${opt(n.alternate)})`;
      case 'SwitchStatement':
        return `(switch${S(n)} ${D(n.discriminant)}` + n.cases.map((c) => ` (case${S(c)} ${opt(c.test)}${L(c.consequent)})`).join('') + ')';
      case 'ThrowStatement': return `(throw${S(n)} ${D(n.argument)})`;
      case 'TryStatement':
        return `(try${S(n)} ${D(n.block)} ${n.handler ? `(catch${S(n.handler)} ${opt(n.handler.param)} ${D(n.handler.body)})` : '-'} ${opt(n.finalizer)})`;
      case 'WhileStatement': return `(while${S(n)} ${D(n.test)} ${D(n.body)})`;
      case 'DoWhileStatement': return `(do${S(n)} ${D(n.body)} ${D(n.test)})`;
      case 'ForStatement': return `(for${S(n)} ${opt(n.init)} ${opt(n.test)} ${opt(n.update)} ${D(n.body)})`;
      case 'ForInStatement': return `(forin${S(n)} ${D(n.left)} ${D(n.right)} ${D(n.body)})`;
      case 'ForOfStatement': return `(forof${S(n)} ${n.await ? 'await' : '-'} ${D(n.left)} ${D(n.right)} ${D(n.body)})`;
      case 'VariableDeclaration':
        return `(var${S(n)} ${n.kind}` + n.declarations.map((d) => ` (decl${S(d)} ${D(d.id)} ${opt(d.init)})`).join('') + ')';
      default: return `(? ${n.type})`;
    }
  }
  return `(program${L(ast.body)})`;
}

const [mode, file] = process.argv.slice(2);
const text = readFileSync(file, 'utf8');
function one(src) {
  try {
    return 'ok ' + dumpTree(src);
  } catch (e) {
    return 'err ' + (e && e.message);
  }
}
if (mode === 'dump') {
  console.log(one(text));
} else if (mode === 'cases') {
  text.split('\n%%\n').forEach((c, i) => {
    let r = one(c);
    if (c.startsWith('//! unsupported')) r = 'unsupported';
    console.log(`${i} ${r}`);
  });
} else {
  console.error('usage: parse_oracle.mjs dump|cases FILE');
  process.exit(2);
}
