#!/usr/bin/env python3
"""Syntax-only feature scan of JS files. Needs the installed `acorn` CLI. Never runs the JS.

  python3 -I tools/staging_js_scan.py FILE.js [FILE.js ...]

Prints per file: parse ok at ecmaVersion 2022 (script), then syntax features and
watched-name references as `name count first_offset`. Offsets from acorn are UTF-16 code units, not byte offsets or Python character indices.
A "name ref" is an AST Identifier that is not a property key or member property. It is
NOT scope-checked: a local variable named `window` would count. It is not proof of execution.
"""
import json, subprocess, sys
from collections import defaultdict

WATCH = set("""document window self globalThis navigator location history localStorage sessionStorage
fetch XMLHttpRequest setTimeout clearTimeout setInterval clearInterval queueMicrotask
requestAnimationFrame cancelAnimationFrame requestIdleCallback MessageChannel MutationObserver
ResizeObserver IntersectionObserver matchMedia getComputedStyle Promise Symbol Proxy Reflect Map Set
WeakMap WeakSet WeakRef FinalizationRegistry JSON Object Array String Number Boolean Math Date RegExp
Error TypeError URL URLSearchParams Headers Request Response AbortController AbortSignal FormData
Blob TextEncoder TextDecoder ReadableStream performance console crypto atob btoa Intl BigInt
Uint8Array ArrayBuffer DataView Event CustomEvent EventTarget Node Element HTMLElement DOMException
Function eval trustedTypes WebSocket BroadcastChannel structuredClone process Buffer require
Atomics SharedArrayBuffer Float32Array Int32Array Uint16Array Uint32Array
""".split())

def walk(n, f, parent=None, key=None):
    if isinstance(n, dict):
        if "type" in n: f(n, parent, key)
        for k, v in n.items():
            if isinstance(v, (dict, list)): walk(v, f, n, k)
    elif isinstance(n, list):
        for v in n: walk(v, f, parent, key)

def scan(path):
    src = open(path, encoding="utf-8").read()
    r = subprocess.run(["acorn", "--ecma2022", path], capture_output=True, text=True)
    print(f"== {path.rsplit('/',1)[-1]} chars={len(src)} acorn_ecma2022_script={'ok' if r.returncode == 0 else 'FAIL ' + r.stderr[:100]}")
    if r.returncode: return
    feats, names, members = {}, defaultdict(lambda: [0, None]), defaultdict(lambda: [0, None])
    def hit(d, k, off):
        e = d[k] if isinstance(d, defaultdict) else d.setdefault(k, [0, off])
        e[0] += 1
        if e[1] is None: e[1] = off
    def f(n, p, k):
        t, s = n["type"], n["start"]
        tags = {
            "ArrowFunctionExpression": "arrow", "TemplateLiteral": "template", "TaggedTemplateExpression": "tagged-template",
            "ClassDeclaration": "class", "ClassExpression": "class", "SpreadElement": "spread", "RestElement": "rest",
            "ObjectPattern": "obj-destructuring", "ArrayPattern": "arr-destructuring", "AwaitExpression": "await",
            "ForOfStatement": "for-of", "ForInStatement": "for-in", "ChainExpression": "optional-chaining",
            "PropertyDefinition": "class-field", "StaticBlock": "static-block", "PrivateIdentifier": "private-name",
            "LabeledStatement": "label", "WithStatement": "with", "DebuggerStatement": "debugger",
            "ImportExpression": "dynamic-import", "MetaProperty": "meta-property", "SwitchStatement": "switch",
            "TryStatement": "try", "SequenceExpression": "comma-expr", "YieldExpression": "yield",
            "AssignmentPattern": "default-param/destructure-default", "Super": "super", "ThisExpression": "this",
        }
        if t in tags: hit(feats, tags[t], s)
        if t in ("FunctionDeclaration", "FunctionExpression"):
            if n.get("async"): hit(feats, "async-fn", s)
            if n.get("generator"): hit(feats, "generator", s)
        if t == "ArrowFunctionExpression" and n.get("async"): hit(feats, "async-arrow", s)
        if t == "VariableDeclaration": hit(feats, n["kind"], s)
        if t == "LogicalExpression" and n["operator"] == "??": hit(feats, "nullish-??", s)
        if t == "AssignmentExpression" and n["operator"] in ("??=", "||=", "&&=", "**="): hit(feats, "assign-" + n["operator"], s)
        if t == "BinaryExpression" and n["operator"] in ("**", "in", "instanceof"): hit(feats, "op-" + n["operator"], s)
        if t == "Property" and n.get("kind") in ("get", "set"): hit(feats, "accessor-" + n["kind"], s)
        if t in ("Property", "MethodDefinition", "PropertyDefinition") and n.get("computed"): hit(feats, "computed-key", s)
        if t == "Property" and n.get("shorthand"): hit(feats, "shorthand-prop", s)
        if t == "Property" and n.get("method"): hit(feats, "method-shorthand", s)
        if t == "NewExpression" and n["callee"].get("name") == "Function": hit(feats, "new-Function", s)
        if t == "CallExpression" and n["callee"].get("name") in ("eval", "Function"): hit(feats, "call-" + n["callee"]["name"], s)
        if t == "Literal":
            if "regex" in n:
                hit(feats, "regex-literal", s)
                fl, pat = n["regex"]["flags"], n["regex"]["pattern"]
                for c in fl: hit(feats, "regex-flag-" + c, s)
                for pre, nm in (("(?<=", "lookbehind"), ("(?<!", "neg-lookbehind"), ("\\p{", "unicode-prop"), ("\\P{", "unicode-prop")):
                    if pre in pat: hit(feats, "regex-" + nm, s)
                if "(?<" in pat.replace("(?<=", "").replace("(?<!", ""): hit(feats, "regex-named-group", s)
            if "bigint" in n: hit(feats, "bigint", s)
            if "_" in n.get("raw", "") and isinstance(n.get("value"), (int, float)): hit(feats, "numeric-separator", s)
        if t == "ExpressionStatement" and "directive" in n: hit(feats, "directive:" + n["directive"], s)
        if t == "Identifier" and n["name"] in WATCH:
            if p and p["type"] == "MemberExpression" and k == "property" and not p.get("computed"): return
            if p and p["type"] in ("Property", "MethodDefinition", "PropertyDefinition") and k == "key" and not p.get("computed"): return
            hit(names, n["name"], s)
        if t == "MemberExpression" and not n.get("computed") and n["object"].get("type") == "Identifier" and n["object"]["name"] in WATCH:
            hit(members, n["object"]["name"] + "." + n["property"].get("name", "?"), s)
    walk(json.loads(r.stdout), f)
    print(" syntax:", "; ".join(f"{k} {v[0]}@{v[1]}" for k, v in sorted(feats.items())))
    print(" name refs:", "; ".join(f"{k} {v[0]}@{v[1]}" for k, v in sorted(names.items())))
    print(" watched members:", "; ".join(f"{k} {v[0]}@{v[1]}" for k, v in sorted(members.items())))

for p in sys.argv[1:]: scan(p)
