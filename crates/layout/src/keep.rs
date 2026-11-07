//! Layout results kept from one window layout to the next (`layout_kept`).
//!
//! Chrome (LayoutNG) keeps the last layout result of each block with the constraint space that it was laid out in, and uses it
//! again when the space is the same and the block is not marked for layout. We do the same for the "stable" blocks: the root, and
//! each block child of a stable block in block flow (`block::children`). A block inside a flex, grid or table container, an
//! inline-block, a float or a positioned box is not stable: its container lays it out with other inputs.
//!
//! For each stable block a layout keeps a `Rec`: the inputs that its layout reads from outside its subtree (the space, the call
//! depth, the list number), its `Done`, and what it changed outside its fragments (the list number after it, the counters of
//! `Cx`, and the scrollbars and offsets that it wrote by node). The layout also keeps its fragments as they are before
//! `absolute`, with places relative to the parent (`Kept::rel`). The next layout copies the fragments of a clean block with the
//! same inputs in one piece, and puts the rest back.
//!
//! A kept result is used only when all of these are true: the block and all that is inside it are not dirty (the caller marks
//! the changed nodes and their ancestors), no float of the formatting context is open before or after it, no list marker waits
//! for a line, no scrollbar pass of an ancestor runs again (`Cx::again`), and no static position of an absolute or fixed box is
//! inside it (`Cx::statics`). Each later layout either writes the `Rec` of a block it lays out, or forgets it, so the kept
//! results always match the fragments of the last layout.

use crate::block::{Done, Pos, Space};
use crate::fragment::{Fragment, Kind, Rect};
use crate::{Cx, Fast};
use bare_html::dom::NodeId;

const NONE: u32 = u32::MAX;

/// What a stable block read and wrote, from one layout.
#[derive(Clone, Copy)]
struct Rec {
    ok: bool,
    width: u32,
    height: Option<u32>,
    depth: u32,
    number: i32,
    done: Done,
    number_out: i32,
    work: u64,
    unknown: u32,
    scrolls: u32,
    items: u32,
    /// The offsets and scrollbars that the block and its subtree wrote by node (`Cx::set_rel`, `Cx::set_bars`).
    sets: u32,
}

/// The counters of `Cx` when a stable block starts.
#[derive(Clone, Copy)]
pub(crate) struct Entry {
    number: i32,
    work: u64,
    unknown: u32,
    scrolls: u32,
    items: u32,
    sets: u32,
    statics: u32,
    waiting: u32,
    clean: bool,
}

/// What a window layout keeps for the next one.
#[derive(Default)]
pub struct Kept {
    /// The fragments after `drop_statics` and before the relative offsets and `absolute`.
    rel: Vec<Fragment>,
    /// The `Rec` of each stable block: `slot` by `NodeId::index`, into `recs`.
    slot: Vec<u32>,
    recs: Vec<Rec>,
    /// The relative offsets and the scrollbars of the layout, by node.
    offsets: Fast<(f32, f32)>,
    bars: Fast<(bool, bool)>,
    /// The size of the window that the layout was made for.
    viewport: (f32, f32),
    /// How many fragments the layout copied from the last one.
    reused: usize,
    /// Memory for the fragments of the next layout (`recycle`).
    room: Vec<Fragment>,
}

/// How a layout uses the kept results.
pub(crate) struct Keep<'a> {
    pub(crate) kept: Kept,
    /// The fragments of the last layout (`Kept::rel` of it), and the dirty nodes. `None`: lay out all and keep the results.
    old: Option<(Vec<Fragment>, &'a [bool])>,
    /// The memory of `Kept::rel` when it is not read.
    spare: Vec<Fragment>,
}

/// What `block::children` passes to `block::sized` for the next block.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub(crate) enum Next {
    /// Not stable.
    #[default]
    No,
    /// Stable, with no fragment in the last layout.
    Fresh,
    /// Stable, and its fragment of the last layout is at this place of `Kept::rel`.
    Old(u32),
}

impl Kept {
    /// The size of the window of the layout that made this.
    pub fn viewport(&self) -> (f32, f32) {
        self.viewport
    }

    /// How many fragments the layout that made this copied from the one before it.
    pub fn reused(&self) -> usize {
        self.reused
    }

    /// Give the fragments of the layout that made this, when they are not needed now: the next layout writes in their memory.
    pub fn recycle(&mut self, fragments: Vec<Fragment>) {
        self.room = fragments;
    }

    /// The memory for the fragments of a layout of `nodes` nodes: the recycled one, or a new one.
    pub(crate) fn room(&mut self, nodes: usize) -> Vec<Fragment> {
        let mut out = std::mem::take(&mut self.room);
        out.clear();
        out.reserve(nodes);
        out
    }
}

impl<'a> Keep<'a> {
    /// Use the results of `kept` again where the nodes are not `dirty` (by `NodeId::index`, ancestors included).
    /// `dirty: None`: lay out all and keep the new results.
    pub(crate) fn new(mut kept: Kept, dirty: Option<&'a [bool]>, viewport: (f32, f32)) -> Keep<'a> {
        let rel = std::mem::take(&mut kept.rel);
        let (old, spare) = match dirty.filter(|_| kept.viewport == viewport && !rel.is_empty()) {
            Some(d) => (Some((rel, d)), Vec::new()),
            None => {
                // No result of the last layout is used: start again, and keep the memory.
                kept.recs.clear();
                kept.slot.fill(NONE);
                (None, rel)
            }
        };
        kept.viewport = viewport;
        kept.reused = 0;
        Keep { kept, old, spare }
    }

    /// The end of the layout: keep the fragments before `absolute`, and the offsets and scrollbars.
    pub(crate) fn finish(mut self, out: &[Fragment], rel: &Fast<(f32, f32)>, bars: &Fast<(bool, bool)>) -> Kept {
        let mut buf = self.old.take().map_or(std::mem::take(&mut self.spare), |o| o.0);
        buf.clear();
        buf.extend_from_slice(out);
        self.kept.rel = buf;
        self.kept.offsets.clone_from(rel);
        self.kept.bars.clone_from(bars);
        self.kept
    }

    /// The place of the root in the last layout.
    pub(crate) fn root(&self, root: NodeId) -> Next {
        match &self.old {
            Some((old, _)) if old.first().is_some_and(|f| f.node == root && f.kind == Kind::Box) => Next::Old(0),
            _ => Next::Fresh,
        }
    }

    fn rec(&self, node: NodeId) -> Option<&Rec> {
        match self.kept.slot.get(node.index()) {
            Some(&s) if s != NONE => Some(&self.kept.recs[s as usize]).filter(|r| r.ok),
            _ => None,
        }
    }

    /// Forget the result of `node`: it is laid out now in a way that is not kept.
    #[inline]
    pub(crate) fn forget(&mut self, node: NodeId) {
        if let Some(&s) = self.kept.slot.get(node.index())
            && s != NONE
        {
            self.kept.recs[s as usize].ok = false;
        }
    }

    fn write(&mut self, node: NodeId, rec: Rec) {
        let i = node.index();
        if self.kept.slot.len() <= i {
            self.kept.slot.resize(i + 1, NONE);
        }
        match self.kept.slot[i] {
            NONE => {
                self.kept.slot[i] = self.kept.recs.len() as u32;
                self.kept.recs.push(rec);
            }
            s => self.kept.recs[s as usize] = rec,
        }
    }
}

/// The cursor over the old children of a stable parent: where to look for the next one in `Kept::rel`.
#[derive(Clone, Copy)]
pub(crate) struct Cursor {
    at: u32,
    end: u32,
}

/// How many old fragments the cursor skips at most to find a child: removed children and lines.
const LOOK: u32 = 256;

impl Cx<'_> {
    /// The cursor over the old children of the stable block with the old fragment `next`.
    pub(crate) fn cursor(&self, next: Next) -> Option<Cursor> {
        let Next::Old(at) = next else { return None };
        let (old, _) = self.keep.as_ref()?.old.as_ref()?;
        Some(Cursor { at: at + 1, end: old[at as usize].end })
    }

    /// `block::sized` for the child block `b` of a stable parent: its kept result when it can be used, else a new layout that is kept.
    #[inline(never)]
    pub(crate) fn stable_child(&mut self, cursor: &mut Option<Cursor>, b: NodeId, space: Space, forced: Option<(f32, f32, f32)>, pos: Pos) -> Done {
        match self.reuse(cursor, b, space) {
            Ok(done) => done,
            Err(old) => {
                self.next = old;
                let e = self.entry();
                let done = crate::block::sized(self, b, space, forced, None, pos);
                self.keep_result(b, space, e, &done);
                done
            }
        }
    }

    /// The child block `b` of a stable parent, before its layout: copy its kept result when it can be used, else give what
    /// `block::sized` needs to keep a new one.
    pub(crate) fn reuse(&mut self, cursor: &mut Option<Cursor>, b: NodeId, space: Space) -> Result<Done, Next> {
        let Some(c) = cursor.as_mut() else { return Err(Next::Fresh) };
        let Some((old, dirty)) = self.keep.as_ref().and_then(|k| k.old.as_ref()) else { return Err(Next::Fresh) };
        // Find the old fragment of `b` among the old children that are left.
        let (mut i, mut n) = (c.at, 0);
        while i < c.end && old[i as usize].node != b {
            if n == LOOK {
                return Err(Next::Fresh);
            }
            i = old[i as usize].end;
            n += 1;
        }
        if i >= c.end || old[i as usize].kind != Kind::Box {
            return Err(Next::Fresh);
        }
        let end = old[i as usize].end;
        c.at = end;
        let keep = self.keep.as_ref().expect("checked");
        let usable = !dirty.get(b.index()).copied().unwrap_or(true) && self.floats.is_empty() && self.marker.is_none() && self.again == 0;
        let Some(&r) = keep.rec(b).filter(|r| usable && r.width == space.width.to_bits() && r.height == space.height.map(f32::to_bits) && r.depth == self.depth as u32 && r.number == self.number) else {
            return Err(Next::Old(i));
        };
        // Copy the subtree: its `end` places move with it, and the parent places its top fragment.
        let from = self.out.len() as u32;
        let (lo, hi) = (i as usize, end as usize);
        let delta = from.wrapping_sub(i);
        self.out.extend(old[lo..hi].iter().map(|f| Fragment { end: f.end.wrapping_add(delta), ..*f }));
        self.keep.as_mut().expect("checked").kept.reused += hi - lo;
        let top = &mut self.out[from as usize];
        top.rect = Rect { x: r.done.margin_left, y: 0.0, ..top.rect };
        self.number = r.number_out;
        self.work = self.work.wrapping_add(r.work);
        self.unknown.set(self.unknown.get().wrapping_add(r.unknown));
        self.scrolls = self.scrolls.wrapping_add(r.scrolls);
        self.items = self.items.wrapping_add(r.items);
        if r.sets > 0 {
            self.put_back(from as usize);
        }
        Ok(Done { fragment: from as usize, ..r.done })
    }

    /// Put back the offsets and scrollbars that the kept subtree at `from` wrote by node.
    #[cold]
    fn put_back(&mut self, from: usize) {
        let keep = self.keep.take().expect("checked");
        for j in from..self.out.len() {
            let f = self.out[j];
            if f.kind != Kind::Box {
                continue;
            }
            let n = f.node.index() as u32;
            if let Some(&o) = keep.kept.offsets.get(&n) {
                self.set_rel(f.node, o);
            }
            if let Some(&b) = keep.kept.bars.get(&n) {
                self.set_bars(f.node, b);
            }
        }
        self.keep = Some(keep);
    }

    /// The counters at the start of a stable block.
    pub(crate) fn entry(&self) -> Entry {
        Entry {
            number: self.number,
            work: self.work,
            unknown: self.unknown.get(),
            scrolls: self.scrolls,
            items: self.items,
            sets: self.sets,
            statics: self.statics,
            waiting: self.waiting,
            clean: self.floats.is_empty() && self.marker.is_none() && self.again == 0,
        }
    }

    /// The end of the layout of a stable block: keep its result, or forget it when it read state that the `Rec` does not hold.
    pub(crate) fn keep_result(&mut self, node: NodeId, space: Space, e: Entry, done: &Done) {
        let ok = e.clean && self.floats.is_empty() && self.marker.is_none() && self.statics == e.statics && self.waiting == e.waiting;
        let rec = Rec {
            ok,
            width: space.width.to_bits(),
            height: space.height.map(f32::to_bits),
            depth: self.depth as u32,
            number: e.number,
            done: *done,
            number_out: self.number,
            work: self.work.wrapping_sub(e.work),
            unknown: self.unknown.get().wrapping_sub(e.unknown),
            scrolls: self.scrolls.wrapping_sub(e.scrolls),
            items: self.items.wrapping_sub(e.items),
            sets: self.sets.wrapping_sub(e.sets),
        };
        let keep = self.keep.as_mut().expect("a stable block has a keep");
        if ok { keep.write(node, rec) } else { keep.forget(node) }
    }
}

#[cfg(test)]
mod tests {
    use bare_css::cascade::{Origin, RuleSet, Styles, UA_CSS};
    use bare_html::dom::{Document, NodeData, NodeId};
    use bare_html::tree::parse;

    const CSS: &str = ".a{margin:5px 0}.b{padding:3px;border:1px solid}.c{height:40px}.d{display:none}.e{float:left;width:50px;height:20px}\
        .f{position:relative;top:3px}.g{position:absolute;top:2px;left:4px}.h{overflow:auto;height:30px}.i{display:inline-block;width:30px}\
        .j{display:flex}.k{font-size:24px}.l{margin-top:-4px}.m{height:50%}.n{display:list-item;list-style:decimal inside}.o{position:relative}\
        .p{clear:both}.q{width:200px;margin:0 auto}.r{display:table}.s{display:flow-root}.t{overflow:hidden;height:10px}";
    const CLASSES: [&str; 20] = ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r", "s", "t"];
    const TAGS: [&str; 6] = ["div", "p", "span", "li", "ol", "div"];

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    fn styles(doc: &mut Document) -> Styles {
        let mut rules = RuleSet::new((800.0, 600.0));
        rules.add(doc, UA_CSS, Origin::UserAgent);
        rules.add(doc, CSS, Origin::Author);
        rules.compute(doc)
    }

    /// The elements and texts under `body`.
    fn nodes(doc: &Document, body: NodeId) -> (Vec<NodeId>, Vec<NodeId>) {
        let (mut els, mut texts, mut stack) = (Vec::new(), Vec::new(), vec![body]);
        while let Some(n) = stack.pop() {
            match doc.data(n) {
                NodeData::Element { .. } => els.push(n),
                NodeData::Text(_) => texts.push(n),
                _ => {}
            }
            stack.extend(doc.children(n));
        }
        (els, texts)
    }

    fn words(r: &mut Rng) -> String {
        (0..1 + r.below(12)).map(|i| ["fox", "a", "jumps", "over", "the", "lazy", "dog"][(i + r.below(7)) % 7]).collect::<Vec<_>>().join(" ")
    }

    fn class(r: &mut Rng) -> String {
        (0..r.below(3)).map(|_| CLASSES[r.below(CLASSES.len())]).collect::<Vec<_>>().join(" ")
    }

    /// Random changes, each followed by a full cascade, a layout that reuses the last one, and a full layout: the fragments must be the same.
    #[test]
    fn a_kept_layout_equals_a_full_layout_after_random_changes() {
        let mut copied = 0;
        for seed in 1..=12u64 {
            let mut r = Rng(seed * 0x9E37_79B9_7F4A_7C15);
            let mut doc = parse("<!doctype html><body></body>");
            let body = doc.children(doc.children(doc.root()).last().unwrap()).last().unwrap();
            // A page of nested blocks with text.
            for _ in 0..60 {
                let (els, _) = nodes(&doc, body);
                let parent = els[r.below(els.len())];
                let c = class(&mut r);
                let el = doc.insert_element(parent, None, TAGS[r.below(TAGS.len())], &[("class", c.as_str())]);
                let t = doc.create_text(&words(&mut r));
                doc.move_to(t, el, None);
            }
            let mut st = styles(&mut doc);
            let mut layout = crate::layout_kept(&doc, &st, (800.0, 600.0), u64::MAX, None, None);
            for step in 0..150 {
                let (els, texts) = nodes(&doc, body);
                let mut touched = Vec::new();
                match r.below(5) {
                    0 if !texts.is_empty() => {
                        let t = texts[r.below(texts.len())];
                        doc.set_text(t, &words(&mut r));
                        touched.push(t);
                    }
                    1 => {
                        let el = els[r.below(els.len())];
                        let c = class(&mut r);
                        doc.set_attr(el, "class", &c);
                        touched.push(el);
                    }
                    2 => {
                        let parent = els[r.below(els.len())];
                        let kids: Vec<_> = doc.children(parent).collect();
                        let before = if kids.is_empty() { None } else { Some(kids[r.below(kids.len())]) };
                        let c = class(&mut r);
                        let el = doc.insert_element(parent, before, TAGS[r.below(TAGS.len())], &[("class", c.as_str())]);
                        let t = doc.create_text(&words(&mut r));
                        doc.move_to(t, el, None);
                        touched.push(el);
                    }
                    3 if els.len() > 1 => {
                        let el = els[1 + r.below(els.len() - 1)];
                        touched.push(doc.parent(el).unwrap());
                        doc.detach(el);
                    }
                    _ => {
                        let el = els[r.below(els.len())];
                        let t = doc.create_text(&words(&mut r));
                        doc.move_to(t, el, None);
                        touched.push(el);
                    }
                }
                let new = styles(&mut doc);
                // Dirty: the touched nodes, the new nodes and the elements whose values changed, and all their ancestors.
                let mut dirty = vec![false; doc.len()];
                let mut all = vec![doc.root()];
                let mut k = 0;
                while k < all.len() {
                    let kids: Vec<_> = doc.children(all[k]).collect();
                    all.extend(kids);
                    k += 1;
                }
                for n in all {
                    let changed = n.index() >= st.nodes() || st.style(n) != new.style(n);
                    if changed || touched.contains(&n) {
                        let mut up = Some(n);
                        while let Some(u) = up.filter(|u| !dirty[u.index()]) {
                            dirty[u.index()] = true;
                            up = doc.flat_parent(u);
                        }
                    }
                }
                st = new;
                let kept = layout.kept.take();
                layout = crate::layout_kept(&doc, &st, (800.0, 600.0), u64::MAX, kept, Some(&dirty));
                let full = crate::layout(&doc, &st, (800.0, 600.0));
                copied += layout.kept.as_ref().unwrap().reused();
                if layout.fragments != full.fragments || layout.height != full.height || layout.bars != full.bars {
                    panic!("seed {seed} step {step}:\nkept:\n{}\nfull:\n{}", crate::dump(&doc, &layout), crate::dump(&doc, &full));
                }
            }
        }
        assert!(copied > 10_000, "the layouts copied only {copied} fragments");
    }
}
