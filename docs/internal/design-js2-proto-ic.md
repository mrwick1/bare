# Design: prototype-chain inline cache of js2

Status: built (PR of branch `t3/js2-prototype-chain-ic`). Date: 2026-11-21. Base: main `a04014eb`.

A read that finds its value on the prototype chain had no site cache in `crates/js2`. Examples: `p.then`, `obj.method()` with the method on the class prototype, `arr.push`, `str.slice`. Each read walked the chain (`get_prop_g`, `Shapes::find` at each link). The promise agent measured `p.then` at 473 instructions of a 1,849-instruction `then` step (PR #800). An own-property read already hit the shape cache.

Words used in this file:

- "receiver" is the value in `recv.name`. "holder" is the object on the chain that owns the property. "link" is an object between the receiver and the holder.
- "hypothesis" marks a number or a cause that no run proved.
- All instruction counts are `perf stat -e instructions:u`, minimum of 3 runs (`tools/js/instr.sh`). The machine was loaded, so only instruction counts are used.
- "Per turn" is `(I(N=100000) - I(N=0)) / 100000` of a loop in `script_run`. The loop body is in the `*.tpl` cases listed in "Results".

## 1. How it was (measured on `a04014eb`, 3 runs, spread 0 to 2 instructions)

- A shape fixes the prototype of its objects: the root of a shape tree holds the layout, the prototype and the inline capacity (`shapes.rs`). So the shape of the receiver says which object is the first link.
- `Feedback` holds one 16-byte record for each site: `Cache { shape, loc, to, state, poly }`. States: `FRESH`, `SEEN`, `MONO`, `POLY` (a pool of 4), `MEGA`. An own hit is `c.shape == shape`, then one load at `c.loc`.
- A read that missed went to `get_field_slow`, then `get_prop_g`: a `Shapes::find` at the receiver and at each link, until the key was found.

Where the time went, for `x.m()` with `m` on the class prototype: 888 instructions per turn against 199 for an own read. The extra 690 are the slow path: the chain walk (about 80 for each link: a shape lookup in the shared key table), the generic `Got` handling, and the call setup (hypothesis for the split: it was not profiled by line again after the rebase). The cost grows with the depth of the chain (888, 967 and 1,046 for depth 1, 2 and 3). A built-in method read (`arr.push`, `str.slice`) is the same walk, from `Array.prototype` or `String.prototype` (a string receiver has no shape, so it walked from `string_proto`).

## 2. What the engines do

Sources are from memory of the code and the public design notes. They were not fetched again in this session. To verify before quoting them elsewhere.

- **V8.** A `LoadIC` handler holds the holder (weakly) and the data to validate the chain. The receiver map is compared. For a holder on the prototype chain the handler also checks the "validity cell" of the receiver map's prototype info. A prototype object is put in "prototype mode" (a fast map that is not shared), and each change of a prototype invalidates the cells of the maps that depend on it. One compare of the map and one of the cell. This is design (b) below, with a cell for each prototype map instead of one epoch.
- **JavaScriptCore.** The DFG and the baseline IC use an "access case" with a structure of the receiver and a set of "conditions" (`ObjectPropertyCondition`) on the structures of the links and the holder. Each condition is a watchpoint: the compiler does not check it at run time. A change of a structure (a transition of a prototype) fires the watchpoint and the code is thrown away or patched. In the baseline the conditions are checked at run time when a watchpoint is not possible.
- **SpiderMonkey.** CacheIR `GuardShape` on the receiver, then `LoadProto` and `GuardShape` for each link (the "walk" form). Since 2021 the "teleporting" form guards only the shape of the receiver and of the holder. The links are covered by an invalidation: a change of a prototype of an object on a chain sets a flag and invalidates all stubs that teleported over it. This is design (b), per object.
- **QuickJS.** No inline cache for a prototype read. `JS_GetPropertyInternal` walks the chain and looks up the shape hash table at each link. This is the state that js2 had before this change.

## 3. Designs

The data that every design has to keep right: for a receiver shape `S` (it fixes the first prototype `P1`), the result of `find(name)` along `S, P1, P2, ... Pn (holder)`. It is wrong if a link gets the key (shadow), the holder loses the key or gets it reconfigured, a link gets another prototype, or the holder slot moves.

### (a) Guard the receiver shape and the shape of each link

The cache entry holds the receiver shape, the shape id of each link and of the holder, and the slot.

- Hit cost: one compare for the receiver, then a load and a compare for each link and for the holder. At depth 3 that is 4 compares and 3 pointer loads of objects that are cold in the cache.
- Memory per entry: 4 bytes for each link, plus the pointers to the objects (roots or weak).
- Invalidation: none to write. Every change is a shape change, so the compare fails.
- Dictionary-mode prototypes: they keep their shape id when they change, so the compare cannot see a change. They must not be cached, or they need another rule.
- Cost on the benchmark: it grows with the depth. For a class with a base class the hit is about 3 compares more than the own hit (hypothesis: about 20 instructions at depth 2).

### (b) A validity cell or an epoch

The entry holds the receiver shape, the holder, the slot, and a validity stamp. A hit compares the receiver shape and the stamp. Any change of an object that the entry depends on makes the stamp stale.

- Hit cost: two compares and one load of the holder slot, at any depth.
- Per-prototype cells (V8) need an allocated cell, a list of the cells that depend on an object, and a rule that invalidates the cells of the objects below. That is a new structure in `shapes.rs` and `object.rs`.
- One global epoch (chosen, section 4) needs no list. The price: a change of any marked prototype drops all entries of all sites.

### (c) Dictionary prototypes (not chosen)

Make each prototype a dictionary-mode object with a stable shape and a stamp in the shape, as V8 does with its "prototype mode". It removes the need to flag objects. It changes `shapes.rs` for every prototype, which is outside the IC code, and it makes the own reads of a prototype (static methods, `Math.floor`) slower (a dictionary shape is never cacheable here). Not chosen.

## 4. Chosen: (b) with one global epoch and a flag on the objects that an entry depends on

Data:

- `Shapes::proto_epoch: u64` (starts at 1). `Shapes::bump_proto_epoch()` adds 1.
- `F_PROTO`, a sticky object header flag (bit 23). It means: an entry depends on this object (it is the holder, or a link between the receiver and the holder).
- `Feedback::protos: Vec<ProtoHit>`. A `ProtoHit` is `{ holder: Obj, loc: u32, epoch: u64, refills: u32 }`, 24 bytes. `holder` is not a root.
- The record of the site (`Cache`) keeps its 16 bytes. A prototype entry in a record has `shape` 0 (an own hit never matches it, since no object has shape id 0), the receiver shape in `to`, and the index into `protos` in `loc`. A polymorphic pool stores the same entry with the bit `PROTO` in `to`. Memory per site: 16 bytes for the record, plus 24 bytes for each prototype entry (and 16 bytes for each extra pool entry).

Hit (in `get_field!` of `Vm::run`, after the own-hit check fails): `c.to == shape`, then `fb.protos.get(c.loc)`, then `h.epoch == realm.shapes.proto_epoch()`, then one load of the slot of the holder. Three compares and two loads, at any depth. The first entry of a polymorphic site is inline; the other entries go through the slow path (pool scan).

Fill (`fill_proto`, slow path): when the read found a data property on the chain and the site has run before (state not `FRESH`), `proto_target` walks from the first prototype, finds the holder, sets `F_PROTO` on the receiver's prototype, each link and the holder, and returns the holder and the place of its slot (`Obj::loc_now`, which also serves a dictionary holder). The entry is made again when its epoch is stale (`proto_probe`), at most 16 times (`PROTO_REFILLS`); then the site goes to `MEGA`.

Invalidation. Each of these is a shape change, or a flag change, of an object. `Obj::set_shape` bumps the epoch when the object has `F_PROTO`:

| Event                                                             | Why it is covered                                                                                                                                                                                       |
| ----------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Holder gets a new value                                           | Not a change: the hit reads the slot of the holder each time.                                                                                                                                           |
| A key is added to a link or to the holder                         | Shape change of a flagged object: bump.                                                                                                                                                                 |
| A key is deleted, reconfigured, or made an accessor on the holder | Shape change: bump.                                                                                                                                                                                     |
| `__proto__`, `Object.setPrototypeOf` on a link or on the holder   | The prototype is in the shape, so it is a shape change: bump.                                                                                                                                           |
| The same on the receiver                                          | The receiver shape changes, so the compare of `to` fails.                                                                                                                                               |
| An own property on the receiver that hides the key                | The receiver shape changes: the compare fails.                                                                                                                                                          |
| A shadow on an intermediate object                                | Shape change of a flagged link: bump.                                                                                                                                                                   |
| The slot record of the holder grows (overflow)                    | It comes with a shape change (`add`): bump. The place from `loc_now` is valid until the next bump.                                                                                                      |
| A cached add (`SetField` hit) on a flagged object                 | This write changes the shape id only (`set_shape_id`). It refuses a flagged object (`F_PROTO`) and goes the long way, which bumps. The test `an_add_by_a_cached_entry_cannot_skip_the_epoch` covers it. |
| Collection                                                        | `Realm::collect` bumps the epoch. A holder may be freed, and a slot place may change. So an entry never outlives a collection and holds no root.                                                        |

Not cached (read the slow path each time):

- A getter on the chain (a read that finds an accessor first). A call is needed each time.
- An absent property (a read of `undefined` for a missing key). It needs a guard on every link that the key is absent. Possible later with the same flag and epoch.
- `prototype` of a function, `length` of an array or a wrapper, and any key of a wrapper object (`F_WRAP`): they are not in the shape or not on the chain only.
- A dictionary-shape receiver (it keeps its id while it changes).
- A string, a number or a boolean receiver is cached by the slow path only (`get_prim_field`, keys `K_STR`, `K_NUM`, `K_BOOL` in `to`), because such a value has no shape in the interpreter loop. The slow path costs less than the old walk, but it is not the one-compare hit.

Miss and megamorphic story. A site that sees up to 4 receiver shapes keeps all entries (the first inline, the others in the pool). A fifth shape makes the site `MEGA`: no cache work, the plain read as before. A site whose entry is stale 16 times between two collections goes `MEGA` too. That bounds the cost of a program that changes a prototype in a loop: a site that is read in between pays one chain walk and one refill for each change, then it stops. (Per `CLAUDE.md`, a budget that guards against a slow algorithm is a design smell. Here the budget guards against a thrash of a global epoch. See "Limits".)

Why not (a). At depth 1 (a class instance and its prototype) it is one compare more than (b), but the cost grows with the depth, the links are cold objects, and it needs the dictionary rule. (b) costs the same at all depths (`m1`, `m2` and `m3` are equal after the change: 486 each).

## 5. Limits

- One global epoch. A change of any flagged prototype stales all prototype entries of all sites. Typical code changes prototypes at start-up (class definitions, polyfills) and not in a loop, so the entries are made again once. A program that adds a method to a prototype inside a hot loop gets the 16-refill budget and then plain reads. Hypothesis: a per-prototype stamp (V8's cell) would help such a program. It needs a new structure in `shapes.rs` and `object.rs`, and no measured case needs it yet.
- `F_PROTO` is sticky. An object that was once a link keeps the flag, so each later shape change of it (for example, a class instance that was used as a prototype with `Object.create`) costs one epoch bump. It is cheap and it is correct.
- Entries are dropped at each collection. A program that collects often refills each site once per collection (one chain walk).
- A string, a number or a boolean read is slow-path only.
- Accessors and absent keys are not cached.

## 6. Code

- `shapes.rs`: `proto_epoch`, `bump_proto_epoch`.
- `object.rs`: `F_PROTO`; `Obj::set_shape` bumps; `Obj::loc_now`; `Obj::set_shape_id` returns false for a flagged object (cold).
- `cell.rs`: `CellHeader::write_shape` (the cached add writes only the shape word, so it does not clear a flag).
- `interp_rt.rs`: `ProtoHit`, `Feedback::protos`, `fill_proto`, `proto_target`, `proto_probe`, `get_obj_field`, `get_prim_field`; `Realm::collect` bumps the epoch.
- `interp.rs`: the inline hit in `get_field!`; the `set_field!` hit checks the result of `set_shape_id`.
- Tests: `proto_ic_tests.rs`, 15 tests. Each runs normally and under constant collection. They cover: depth 1 to 3, a value that changes in the holder, an own shadow, a shadow on a link, a changed holder, a new prototype anywhere in the chain, a cached add on a flagged object, a polymorphic site, a stale site that goes `MEGA`, dead prototypes, a collection, primitives, getters, functions, arrays and wrappers, and classes with `super`. Before the rebase I broke the rules in the code one at a time (for example, no `F_PROTO` on a link) and a test failed each time.

## 7. Results

See `docs/progress.md`, entry "2026-11-21: js2 prototype-chain inline cache".
