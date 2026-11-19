# Host-filled properties through the host interface

Status: proposal, 2026-11-19 (plan step S0.4, lazy and virtual properties). It waits for the owner's review.

## Problem

Some host objects have properties that the browser fills when a script reads them: `NodeList`, `HTMLCollection`, `DOMTokenList`, `CSSStyleDeclaration`, `NamedNodeMap`, `TextTrackList`, `CSSRuleList`, `DOMStringMap` (`dataset`), `Storage` and the named properties of the window and the document. Today the old engine gives them to the browser through two engine traits, `IndexedSource` and `NamedSource`, plus `Host::fill_indexed` and `Host::named_write`. Each trait call gets the `ObjId` of the object that was asked.

S0.2 decided that these stay an engine feature, and that `bare-js-api` gets them after the DOM wrappers (`docs/design-host-interface.md`, question 3). The wrapper PR (#700) is merged, so they come next.

## Current design

- The heap marks the object as exotic (`Exotic::Indexed` or `Exotic::Named`). It holds no data.
- A read asks the one installed source with the `ObjId`. The browser finds the owner by probing the map of each subsystem in a fixed order. `IndexedSource::indexed_item` tries node lists, then token lists, then style, then attribute maps, then captions, then CSS rule lists. `Host::fill_indexed` tries six subsystems. `NamedSource` tries storage, then dataset, then frames.
- An `Slot::Missing` answer makes the heap return `NeedsHost`. The evaluator calls `Host::fill_indexed`, the host makes the object, and the read runs again.
- A write, define or delete of a named object returns `NeedsNamed`. The evaluator calls `Host::named_write`.
- `fill_indexed` also carries the lazy `on*` members (`LAZY_ON`, `LAZY_ALL`, `LAZY_TAG`) as reserved index values.
- Each of these subsystems also has a `trace` function in the old per-object hook (`Host::trace(owner)`), and a `prune` that walks its map with a `live` query.

Ten creation sites in nine files make these objects: `browser_attr.rs`, `browser_cssom.rs` (2), `browser_dataset.rs`, `browser_frames.rs` (2 objects at one site), `browser_media.rs`, `browser_navigation_storage.rs`, `browser_nodelist.rs`, `browser_platform_class.rs`, `browser_style.rs`.

## Measurements

archMachine, origin/main `46b95a42`, callgrind on a build with symbols, one run per row.

| Row                        |   Total | Cost found                                                                                                                                                                                            |      Share |
| -------------------------- | ------: | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------: |
| `attr_load react 1`        | 225.1 M | Missed probes before the attribute map: 16,000 `indexed_item` calls try node lists (0.40 M), token lists (0.50 M) and style (0.50 M); `fill_indexed` tries token lists (0.24 M) and captions (0.23 M) | about 0.8% |
| `browser_load mutation 1`  |  72.6 M | `trace` of the subsystems of this page in the old per-object hook: attributes 0.23 M, token lists (`browser_platform`) 0.22 M, style 0.22 M, frames 0.20 M, dataset 0.15 M, node lists 0.10 M                                 | about 1.5% |
| `dataset_load read 1000 1` |  17.4 M | `named_get` and `named_has`, inclusive                                                                                                                                                                |      0.7 M |

Hypothesis: the probe chains cost more on rows that read a source late in the chain (CSS rule lists, captions). Not measured.

## How other engines do it

From memory of each engine's source. To verify before the build.

| Engine                      | Mechanism                                                                                                                                                                                                                                     |
| --------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| V8 (Blink)                  | Interceptors on the object template: named and indexed handlers (getter, setter, query, deleter, enumerator, definer). The handler gets the receiver; the embedder reads its C++ object from an internal field. No lookup by object identity. |
| SpiderMonkey (Servo, Gecko) | Proxy-based DOM objects (`DOMProxyHandler`); the C++ object is in a reserved slot of the reflector.                                                                                                                                           |
| WebKit                      | `getOwnPropertySlot` overrides in the generated `JS*` classes; the wrapper holds a pointer to its C++ object.                                                                                                                                 |
| Ladybird                    | `LegacyPlatformObject` with virtual `item_value` and `named_item_value` on the object itself.                                                                                                                                                 |

In all four the engine gives the host its own object at once: an internal field, a reserved slot or the object itself. None searches by identity.

## Options

### A. The host key in the exotic object; one dispatch by key (recommended)

- `Exotic::Indexed(key)` and `Exotic::Named(key)` carry a host key (`u32`). The size of `Exotic` does not change.
- The engine asks the host with the key, not the `ObjId`: `indexed_len(key)`, `indexed_item(key, index)`, `named_has(key, name)` and the others. `fill` and `write` get the key too. On `bare-js-api` these become methods of the `Host` trait (or one sub-trait), with the same semantics as today.
- The browser's owner table (`Owners`, from #697 and #700) gets one variant per kind: `NodeList`, `TokenList`, `Style`, `AttrMap`, `Captions`, `RuleList`, `Dataset`, `Storage`, `FrameNames`. A read is one `Vec` index and one `match`.
- The object is also an owner for tracing (`set_host_owner`). Its `trace(key)` and `prune(dead)` replace the subsystem's `trace` and `prune` in the old hook. The six subsystems above leave the per-object hook.
- Cost: one key and one owner entry per object (as for a wrapper). Gain: no probe chain on a read, and no per-object probe at collection.

### B. Move the traits as they are

- `IndexedSource` and `NamedSource` move to `bare-js-api` with a `Local` handle in place of `ObjId`.
- Smallest change. The probe chains stay, and so do the six probes per marked object in the old hook.

### C. One interceptor interface (V8 style)

- One handler set per key: `get(key, property)`, `query`, `set`, `define`, `delete`, `keys`. Indexed and named merge.
- The heap code of `heap_named.rs` and the indexed path of `heap_binary.rs` merge too. The new engine (S1) can implement one path.
- Larger change to the old engine, for no measured gain over A. A can move to C later, because both pass a key.

### D. Real properties that the host updates

- The host defines ordinary properties and updates them after each DOM change. No virtual reads.
- A live list then costs O(n) per mutation, also when no script reads it. The spec's liveness rules become the host's job. Rejected.

### Comparison

|     | Read                       | Collection                                  | New memory                 | Size |
| --- | -------------------------- | ------------------------------------------- | -------------------------- | ---- |
| A   | one index and one `match`  | one call per object, only for these objects | one owner entry per object | M    |
| B   | probe chain (up to 6 maps) | per-object probe in 6 subsystems            | none                       | S    |
| C   | as A                       | as A                                        | as A                       | L    |
| D   | a normal property read     | none from the host                          | all properties stored      | L    |

Expected gain of A (hypothesis, from the table above): about 0.8% on `attr_load react 1`, and up to 1.5% on `browser_load mutation 1` when the six subsystems leave the old hook. Rows that make many short-lived lists pay the registration, as `select_load churn 1000` did for wrappers (+0.49%).

## Lazy `on*` members

`LAZY_ON`, `LAZY_ALL` and `LAZY_TAG` reuse `fill_indexed` with reserved index values. They are prototype members, not host-filled objects. Proposal: a separate method on the interface (`Host::lazy_member(object, name)`), in the same PR as A or after it. They keep their engine semantics.

## Plan for option A

1. Engine: `new_indexed(proto, key)`, `new_named(proto, key)` and `new_named_open(proto, key)`. The heap passes the key to the source. `bare-js-api` gets the methods.
2. Browser: the owner variants and one dispatch by key. The probe chains go.
3. The objects become owners for tracing. The six subsystem `trace` and `prune` functions leave the old hook.
4. Tests: each kind keeps its current tests. New: a list collected while its element lives frees its key; a key that is reused after a collection answers for the new object only.
5. Gates: Linux and the Windows VM; the cost matrix within +0.5% under both allocators; the rows above measured again.

Steps 1 and 2 can be one PR. Step 3 can be a second PR.

## Open questions for the owner

1. Option A (proposed), B, C or D?
2. Lazy `on*` members: in the same PR as A, or after it (proposed: after)?
3. Tracing of these objects (step 3): in the same PR, or in a second PR (proposed: second)?
