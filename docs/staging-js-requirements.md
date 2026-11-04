# Staging login: JavaScript and browser API evidence

Date: 2026-11-04. Base: main `fd915d8`. Target: `https://staging.app.8vox.ai` (`/login`). No credentials were used.

Labels: **observed** = a command or source text showed it. **traced** = I followed the code path by reading; nothing executed it. **string match** = text only. **hypothesis** = not shown. Syntax-only parsing with acorn is not proof that `bare` can run a file. No target script was executed.

## Method

- `python3 -I tools/staging_js_fetch.py SCRATCH_DIR`: 15 GETs of public scripts named by the saved `login.html` (not the `noModule` polyfills). 469,525 decoded bytes, 2.3 s. Every SHA256 equals `manifest-2026-11-04.json`. `webpack-4af263207fca128f.js` is byte-identical to `tools/fixtures/staging/webpack-runtime.js`. Hashes and sizes are in the manifest; bundles are not committed.
- `python3 -I tools/staging_js_scan.py FILE...`: runs the installed `acorn --ecma2022` (script mode) and prints AST feature counts with first offset. Name references are **not scope-checked**.
- Offsets below are Acorn UTF-16 code-unit offsets in the decoded file. They differ from Python character indices after non-BMP text.

## Existing JS runtime in the repo

None. **Observed**: `crates/*/Cargo.toml` list only workspace crates (`bare-css/html/layout/net/url/...`). No dependency, no JS crate, no interpreter source. `data/css/color-name.js` is a data file for a generator.

## Parse result

All 14 login-route files parse with acorn `ecma2022` as a **script** (not a module). With `ecma2020` two fail on `#private` names (`4114`, `9919`). With `ecma2017` and `ecma2018` other files also fail (acorn: "Unexpected token"; I did not classify which token). No file uses `import`/`export`, dynamic `import()`, generators, `with`, BigInt literals, numeric separators, regex `u/y/s/d/v` flags, lookbehind or named groups (**observed** in the AST scan; string match `BigInt` in `2117@108021` is a name only).

## Initial script order (`login.html`, observed)

1. Head: 4 font preloads, 2 stylesheets, a preload of the webpack script, then 14 external `<script async>`: `fd9d1056` (react-dom, chunk 2971), `2117` (Next client), `main-app`, `8265`, `6792`, `1672`, `app/login/page`, `9919`, `4114`, `4438`, `app/layout`, `7648`, `not-found`, `global-error`.
2. Head: sync inline theme script: `matchMedia("(prefers-color-scheme: dark)")`, `localStorage.getItem("theme")` in `try`, `documentElement.classList.toggle("dark", bool)`, `m.addEventListener("change",a)`, `window.__applyTheme=a`.
3. Head: `polyfills-*.js` with `noModule` (must be skipped by a module-capable engine).
4. Body: `webpack-*.js` `async` (not first in document order), then 4 sync inline scripts: `(self.__next_f=self.__next_f||[]).push([0]);self.__next_f.push([2,null])`, then three `self.__next_f.push([1,"..."])` flight rows.

No script is `defer` or `type=module`. In a browser, async scripts run in completion order, so any order of chunk, runtime and inline scripts must work.

## Webpack bootstrap (runtime source, observed; behavior traced)

- Every chunk starts `(self.webpackChunk_N_E=self.webpackChunk_N_E||[]).push([[ids],{moduleId:function(e,t,n){...}},runtimeFn?])`. A chunk may run before the runtime.
- Runtime end (`webpack-runtime.js@4900-5100`): `(d=self.webpackChunk_N_E=self.webpackChunk_N_E||[]).forEach(l.bind(null,0)); d.push=l.bind(null,d.push.bind(d))`. It replays queued pushes, then replaces `push` on the array. Needs `self`, array `forEach`, `Function.prototype.bind`.
- Chunk handler `l`: for each id not yet `0` in `f`, copies modules into `b.m`, runs `runtimeFn(b)`, sets `f[id]=0`, then `b.O(result)`.
- Entry chunks use `e.O(0,[depIds],fn)` (`main-app`: deps 2971, 2117, entry modules 54278 then 46907; `login/page`: deps 8265, 6792, 1672, 2971, 2117, 1744, entry 37788). `b.O` defers `fn` until `b.O.j(id)` is true (all dep ids `0`). It uses `Object.keys(b.O).every(...)`, so `b.O.j` must be an enumerable own function property.
- Module cache `b(e)`: `try/finally` with `delete p[e]` on throw; `s[e].call(n.exports,n,n.exports,b)`.
- Helpers used: `Object.defineProperty` getters (`b.d`), `Object.prototype.hasOwnProperty.call`, `Symbol.toStringTag`, `Object.getPrototypeOf`, `Object.create(null)`, `Promise.all`.
- `b.g`: `typeof globalThis=="object"` first; the `Function("return this")()` fallback is traced as not taken if `globalThis` exists.
- `b.p="/_next/"`; `b.u(id)` has hash entries only for lazy ids 167, 3083, 3299, 6231, 9041; any other id gives `static/chunks/<id>.undefined.js` (**traced**, not executed).
- Dynamic script loader `b.l`: `document.getElementsByTagName("script")`, `getAttribute`, `createElement("script")`, `charset`, `timeout`, `setAttribute`, `src`, `onload/onerror`, `setTimeout(...,12e4)`, `clearTimeout`, `document.head.appendChild`, `parentNode.removeChild`. Also `trustedTypes` behind `typeof`. CSS loader `i/c`: `link` elements, `document.getElementsByTagName("link"|"style")`.

## Chunk graph (traced from flight rows `I[...]` in `login.html`)

- Route `/login` needs: 2971, 2117, 1744 (`main-app`), 8265, 6792, 1672, 2626 (`login/page`). Layout needs 9919, 4114, 4438, 1672, 3185. Also 7648 (not-found), 9160, 6470.
- Flight client (`2117@109922`) calls `n.e(id)` (webpack `b.e` -> `b.f.j`) per row chunk. If the id is already `0` nothing loads. **Hypothesis**: if `main-app` runs before an async route chunk (8265, ...) has executed, `b.f.j` requests `.../<id>.undefined.js` (traced from `b.u`; unexecuted). The parent must decide scheduling.
- Lazy ids 167, 3083, 3299, 6231, 9041 appear **only** in the runtime map. No fetched file contains `.e(<number>)` or those ids. Their reachability from login is **not shown** (negative result over these 14 files). Manifest: 3299 returned 404 HTML, 6231 is 249 bytes.

## Syntax observed (file@offset, excerpt)

- Arrow, `let`/`const`, template strings, spread/rest, object and array destructuring with defaults, `for..of`, `async`/`await` with `try/catch/finally`, `class ... extends Error` with `super`, optional chaining `?.`, `??`, `??=`, `**`, getters, computed keys, labels, `switch`, comma expressions (~2000 in `fd9d`, `2117`).
- `class extends a.F{#t;#e;#s;constructor(t){super(),this.#t=t.client` (`4114@191`): class fields and `#private` (120 refs in `4114`, 103 in `9919`).
- `Math.min(1e3*2**t,3e4)` (`9919@10345`), `(t??"` (`9919@10054`), `n??=e()` (`9919@15915`).
- Login async handler (`page@658`): `async function S(r){r.preventDefault(),N(!0),j("");try{let{access_token:r}=await l.I8.login(...)...}catch(e){...}finally{N(!1)}}`.
- Regex literals with lookahead `(?!new\/?$)` (`page@7556`, `^\/journeys\/(?!new\/?$)[^/]+\/?$`). Flags seen: `g`, `i` only.
- `"use strict"` directives; `class` static blocks and generators: none.

## Built-ins observed (call sites; name refs are AST-based)

Reached at login by trace: `Object.{keys,entries,assign,defineProperty,fromEntries,hasOwn,getPrototypeOf,create,is}`, `Array.{isArray,from}`, `Promise.{resolve,all,reject}` and `.then/.catch/.finally`, `Symbol.{for,iterator,toStringTag}`, `Reflect.{construct,get,set,has,ownKeys}`, `Proxy` (`2117@76758`), `WeakMap`, `Map`, `Set`, `JSON.{parse,stringify}` (login body, 65 refs in `1672`), `Math`, `Date.now`, `Error` subclassing, `URL`/`URL.canParse`/`URLSearchParams`, `TextEncoder` (`2117@4211`), `TextDecoder` (`@107011`), `Uint8Array.indexOf/length`, `AbortController`, `FormData`, `performance.now`, `Number.is*`, `String.fromCharCode`.
Not reached / server-only: `AsyncLocalStorage` (`2117@102428`, `globalThis` probe), `process`, `Buffer`, `require`: no refs. `window.clipboardData`, `document.documentMode` (`fd9d@125353`, `@133408`) are legacy feature checks (string match; reachability not traced).

## Host APIs on the bootstrap/login path

- **Observed + traced to hydration**: `document.readyState`, `DOMContentLoaded`, `window.addEventListener("error")`, `self.__next_f` array hook, `new ReadableStream({start(e){e.enqueue/close}})`, `stream.getReader().read().then(...)` (`2117@4640`, `@108760`), `hydrateRoot(document, tree, opts)` (`2117@5387`), `window.__next_root_layout_missing_tags`, `document.documentElement.id`.
- Scheduler (`2117@84295-85417`): `setImmediate` if present, else `MessageChannel`/`port.onmessage`, else `setTimeout(fn,0)`. `navigator.scheduling` is checked with `typeof`. `queueMicrotask` falls back to `Promise.resolve().then` (`fd9d@157047`).
- React DOM (`fd9d`): `createElement`, `innerHTML` for scratch elements, `textContent`, `addEventListener("load"/"error"/"test")` feature probe, `window.event`, `matchMedia`, `focus`.
- History/location (`2117@10231`, `@14440`): `history.pushState/replaceState` (also **reassigned** on `window.history`), `history.back/forward`, `history.state`, `popstate`, `pageshow`, `location.href/origin/search/pathname`.
- Login code (`1672@2970-4900`): `localStorage`/`sessionStorage` `getItem/setItem/removeItem` key `token`; `fetch("/api/auth/login",{method:"POST",headers,body})`, then `/api/auth/me` with `Authorization: Bearer`; `res.ok`, `res.status`, `res.json()`, `res.blob()`, `window.location.href=` on 401. Also `XMLHttpRequest` (`1672@9258`) and `URL.createObjectURL` (download path; not login).
- Layout components: `visibilitychange`, `online`/`offline`, `setInterval`/`clearInterval` (`9919`), `keydown`, `getSelection`, `requestAnimationFrame`, `matchMedia`, `getComputedStyle` (`4438`), `createTextNode`, `document.activeElement`, `IntersectionObserver`/`requestIdleCallback` (`7648`), `ResizeObserver`, `animationstart/end`, `dispatchEvent` (`6792`). Reachable once those components hydrate (traced from flight rows); whether each call executes at load is **not** traced.
- Router RSC `fetch(url,{credentials:"same-origin",headers})` with `n.headers.get`, `n.redirected`, `n.url` (`2117@45576`) is reachable from `router.push` after login (call site observed; full path not traced).

## Smallest snippets for future tests

1. Webpack queue replay: the `webpack-*.js` runtime plus `main-app` (471 bytes) in either order.
2. Login handler `page@658` (async, destructuring, `finally`).
3. `2**t`, `??`, `??=`, `#private` (`4114@191`, `9919@10320`).
4. `new ReadableStream`/`getReader().read().then` loop (`2117@108760`).
5. `b.l` script injection (`webpack@2033`) with `onload`/`onerror` and 120 s timeout.
6. Theme inline script (login.html).

## Gaps (not tested)

- No bundle was run. Not tested: `bare` parsing speed, Chrome behavior comparison, `polyfills-*.js` (not fetched), the `/api` responses (no login attempted), CSS chunk 3299.
- Runtime-generated chunks: none proven reachable from login. Ordering hypothesis for `.undefined.js` is untested.
- Offsets use UTF-16 code units; the AST scan ignores scoping, so shadowed names (`window`, `Function`) could be over-counted.

Parent review: the fetch tool now requests identity encoding, rejects redirects and compressed replies, and checks per-file and remaining aggregate bytes before retaining them. Offline exact/over-cap, encoding, length, deadline and redirect regressions pass. The elapsed limit is checked between blocking operations, so one DNS/socket wait can exceed it. Directive scanning now reads the ESTree expression-statement field. No target script was executed during review.
