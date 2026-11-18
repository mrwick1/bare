# DOM wrappers through the host interface

Status: proposal for owner review (plan step S0.4, DOM wrappers). Nothing is built.

## Problem

The browser keeps node wrappers in two side tables and gives their edges to the old engine through `Host::trace(owner, out)`. The engine calls that hook for each object that it marks. The hook asks about 20 subsystems if they hold something for the object. Nearly all objects are not theirs.

`docs/design-host-tracing.md` (option A) replaces this with owners: an object with a host key gets one `Host::trace(key)` call; other objects cost nothing. The events PR (#697) moved listeners and event state. Wrappers are next. The plan also puts here the questions that S0.2 moved: wrapper identity, the edges that the host holds for a wrapper, and lazy and virtual properties.

## Measurements

archMachine, origin/main `32e6d597`, callgrind on a build with symbols, one run per row.

| Row                       |   Total | `Interp::collect` (inclusive) | Old `Host::trace` (inclusive) | `Dom::wrap` (inclusive) |
| ------------------------- | ------: | ----------------------------: | ----------------------------: | ----------------------: |
| `browser_load mutation 1` |  72.6 M |                         35.4% |                          8.9% |                    1.3% |
| `browser_load lookup 1`   |  93.5 M |                         12.6% |                          2.7% |                    1.0% |
| `fragment_load parse 1`   |  55.2 M |                          3.3% |                          0.5% |                    1.9% |
| `attr_load react 1`       | 225.2 M |                          2.4% |                          0.3% |                    0.2% |

Self cost on `browser_load mutation 1`: `Host::trace` 2.31 M; the `trace` functions of 11 subsystems over 0.2 M each, 0.22 M to 0.40 M, about 3.4 M together. The near-equal counts show one probe for each marked object in each subsystem, not real work.

Hypothesis: on DOM-heavy rows the per-object hook is up to 9% of all instructions. It goes only when no subsystem needs it. Moving the wrappers alone removes the `Dom` part of the hook (the wrapper edges and the `by_obj` probe), not the other probes.

## Current design

- `by_node: IdTab<NodeId, ObjId>` gives the wrapper of a node. `by_obj: IdTab<ObjId, Target>` gives the target of a wrapper. `IdTab` is a paged table keyed by slot: a lookup is two loads and a compare.
- `wrap(node)` makes the wrappers of all ancestors that have none, so the wrappers of one tree are connected.
- The document wrapper is a root. `Host::trace(o)` for a node wrapper adds the wrappers of its parent, its children, its shadow host or root and its template content. So attached wrappers live, and a detached tree lives while a script holds one of its wrappers.
- `Host::prune(live)` drops the `by_node` and `by_obj` entries of dead wrappers, then the listeners of dead targets. `browser_reclaim` later frees native trees that lost all wrappers.
- Cost of the edges: the children step walks all children of a wrapped node, wrapped or not. A wrapped `body` with 10 000 children costs 10 000 probes at each collection.
- Lazy properties: `Heap::set_indexed_source` and `set_named_source` give the engine two host traits (`IndexedSource`, `NamedSource`). The engine asks them for a key that an object does not have yet.

## How other engines do it

From memory of each engine's source. To verify before the build.

| Engine        | Node and wrapper                                                                                           | Why an attached wrapper lives                                                                                       |
| ------------- | ---------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------- |
| Blink with V8 | C++ node in the Oilpan heap; the wrapper is a V8 object; `ScriptWrappable` holds a `TracedReference` to it | One unified heap: the document reaches the node through traced C++ members; the node visits its wrapper.            |
| WebKit        | C++ node with reference counts; `JSNode` wrapper                                                           | Opaque roots: the root node of a tree is the opaque root. A wrapper is live when its tree's opaque root is reached. |
| Servo         | DOM structs are owned by the SpiderMonkey heap through their reflector                                     | The tree pointers (`Dom<T>`) are traced by the JS collector.                                                        |
| Ladybird      | DOM nodes are GC cells; the node is the JS object                                                          | Each cell visits its own edges (`visit_edges`).                                                                     |

In all four the tree edges are traced by the collector itself. Bare keeps nodes in a native arena outside the JS heap, so the host must give the edges.

## Options

### A. A wrapper is an owner; its trace visits its neighbours (recommended)

- A target has one host key (design rule: "a wrapper gets the key of its target"). The node owner of #697 (`Owner::Target`) grows to cover the wrapper: the key is set when the wrapper is made, not when a listener is added.
- `by_node` holds `Traced` values. The map is not visited as a whole, so its entries stay weak. `Host::trace(key)` for a node visits the `Traced` wrapper of its parent, children, shadow host or root and template content, then the listeners.
- `Host::prune(dead)` drops the `by_node` and `by_obj` entries and the listeners of the dead keys. No `live` query is needed.
- `visit_raw` goes. The `Dom` branch of the old hook goes.
- Cost: the same edges as today, but only for wrappers, through one call per wrapper. Registration: one `Vec` push per new wrapper (as for events).
- Risk: the children walk stays O(children) for each wrapped node.

### B. Opaque roots: one owner for each tree

- Each tree (attached document, or a detached root) has a key. A wrapper's trace visits only the owner object of its tree. The tree owner visits all wrappers of its tree from a list that the host keeps for each tree.
- No children walk: O(wrappers) per collection, not O(children of wrapped nodes).
- A DOM mutation that moves a subtree to another tree must move its wrappers between lists. `appendChild` of a detached subtree costs O(wrappers in the subtree).
- One more object for each detached tree.

### C. Edges as hidden engine data

- Each wrapper keeps hidden slots with its parent and child wrappers. The engine traces them as normal data, and the host holds no engine values for wrappers.
- Every DOM mutation writes these slots. A move costs O(children). This is the design that the DOM crate and the engine crate kept apart (`IdTab` header): the DOM would have to know the engine.

### D. Nodes in the JS heap (Ladybird, Servo)

- Removes the side tables and the host edges. It means a rewrite of `bare-dom`. Out of scope for S0. A candidate for S2 or later.

### Comparison

|     | Work per collection                                  | Work per DOM mutation               | New memory                                               | Size |
| --- | ---------------------------------------------------- | ----------------------------------- | -------------------------------------------------------- | ---- |
| A   | one call per wrapper, plus children of wrapped nodes | none                                | none (`Traced` is 8 bytes, as `ObjId`)                   | M    |
| B   | one call per wrapper                                 | O(wrappers) on a move between trees | one list entry per wrapper, one object per detached tree | L    |
| C   | none from the host                                   | slot writes, O(children) on a move  | slots in each wrapper                                    | L    |
| D   | none from the host                                   | none                                | none                                                     | XL   |

Expected gain of A on `browser_load mutation 1`: the `Dom` part of the old hook, at most 2.3 M of 72.6 M (3%). Hypothesis: less, because the self cost of the hook also holds the probes of subsystems that the compiler inlined into it. The other 3.4 M (and the smaller subsystems) goes only when the other subsystems move (later S0.4 PRs). Then the hook itself goes.

## Lazy and virtual properties

They stay an engine feature (S0.2 decision). Proposal: a separate PR after A, because they do not depend on wrapper tracing:

- `Host::get_indexed(key, index)` and `Host::get_named(key, name)` on `bare-js-api`, called for an object with a host key, in place of the two engine traits.
- The host key replaces the `ObjId` lookup that the traits do today.

## Plan for the wrapper PR (option A)

1. `wrap` registers each new wrapper as the owner of its node target (one key per target; #697 keys move to wrapper creation).
2. `by_node` values become `Traced`. The node branch of the old hook moves to `Host::trace(key)`. `visit_raw` is removed.
3. `Host::prune(dead)` drops the wrapper entries and listeners of dead keys. The `by_node`, `by_obj` and listener `retain` calls of the old prune go.
4. Tests: the existing collect tests (`collect_keeps_*`, `removed_attached_subtrees_*`, shadow and template pairs); a detached tree held by one leaf keeps its root and siblings; a moved subtree keeps its expandos.
5. Gates: Linux and the Windows VM; the cost matrix within +0.5% under both allocators; the four rows above measured again.

## Open questions for the owner

1. Option A (proposed), B, C or D?
2. Lazy properties: a separate PR after the wrappers (proposed), or in the same PR?
3. The children walk of option A: accept O(children of wrapped nodes) now, and move to B only if a row shows it? (proposed: yes)
