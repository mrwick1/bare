//! The DOM: the tree of nodes that a page becomes.
//!
//! `<p>Hi <b>you</b></p>` is a tree. `p` has two children: the text "Hi " and the element `b`.
//! `b` has one child: the text "you". CSS, layout and JavaScript all work on this tree.
//!
//! How do we store a tree in Rust? There are three options:
//! 1. Each node owns its children (`Vec<Node>` inside `Node`). Simple, but a child cannot point
//!    to its parent. The DOM needs that: `element.parentNode` in JS, and CSS rules like `div > p`.
//! 2. `Rc<RefCell<Node>>`, with a parent pointer in each child. It works, but the borrow rules are
//!    checked while the program runs, so a mistake is a crash. A parent and a child that point at
//!    each other are never freed (a leak), unless we use weak pointers too.
//! 3. An arena: one `Vec` holds all the nodes. A node refers to another by its place in the `Vec`,
//!    a plain number. This is our choice. Numbers are cheap to copy. There is no leak, and no
//!    run-time borrow checks. To delete a whole tree, drop the `Vec`.
//!    The cost: a `NodeId` has no meaning without its `Document`. Use it with another document and
//!    you get the wrong node, or a crash. Servo uses a different design (garbage collected nodes),
//!    because JavaScript can hold on to nodes.
//!
//! One more benefit: a tab owns one `Document`, so the memory of a tab's DOM is the size of
//! a few `Vec`s. Module 10 counts the memory for each tab.
//!
//! The nodes are small, because every byte counts when a page has 100,000 elements:
//! - A `NodeId` is 4 bytes (a `u32`), not 8. It is never zero, so `Option<NodeId>` is 4 bytes too.
//! - A node does not own a `Vec` of children. It holds its first and last child, and each child
//!   holds the next and the previous child. This is a linked list in the arena. It saves a heap
//!   allocation for each node. Adding at the end, or in front of a known node, is O(1). The
//!   cost: to reach child number 5, you walk the list. CSS and layout walk all the children
//!   in order, so for us this is no cost. Chrome stores children the same way.
//! - Names of elements and attributes (`div`, `class`) are stored once, in a table of the thread
//!   (`Names`). A node holds an `Atom`: the number of its name in the table. A page has
//!   100,000 `div` and the word is in memory once. Comparing two names is comparing two numbers.
//!   The table is shared by all documents of the thread, so an atom means the same in each of them
//!   and a parsed selector does not depend on one document. A `Document` keeps the list of the names that it
//!   uses (`name_count`, `atom_at`, `find_atom`).
//! - Texts, attribute values, comments and doctypes are not `String`s. They all live in one big
//!   `String` of the `Document`, and a node holds a `Span`: where its text starts, and how long it is.
//!   A `String` for each small text costs 24 bytes for the header, and a heap allocation of at least
//!   32 bytes for a text of one letter. A `Span` is 8 bytes. All the attributes of all the elements
//!   also live in one `Vec`, and an element holds the range of its own. So a node needs no
//!   allocation of its own at all. The cost: a text cannot be changed in the middle. When we add
//!   to a text node that is not the last text in the buffer, we copy it to the end, and the old
//!   bytes are wasted. For a page that is read once, this almost never happens. JavaScript will
//!   change the DOM later, and then we may need to clean up the buffer.
//!
//! Comments compare Rust with JavaScript, because you know JS.

mod slots;

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};
use std::num::NonZeroU32;
use std::rc::Rc;

/// The number of a node. It is the place of the node in the `Vec` of its `Document`, plus one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
// `Copy` means that passing a `NodeId` around copies it, like a number.
pub struct NodeId(NonZeroU32);
// JS: like a number, but the type stops us from mixing it up with other numbers.
// `NonZeroU32` is a number that is never 0. Rust uses the free "0" to mean `None`, so an
// `Option<NodeId>` needs no extra space. The inner number has no `pub`, so other crates cannot
// make a `NodeId` of their own.

impl NodeId {
    /// The place in the `Vec` of the document, from 0. A `Vec` with one item for each node, like the style table, uses it.
    pub fn index(self) -> usize {
        self.0.get() as usize - 1
    }

    fn from_index(index: usize) -> NodeId {
        NodeId(NonZeroU32::new(u32::try_from(index + 1).expect("more than 4 billion nodes")).unwrap())
    }
}

/// The number of a name in the name table of the thread. Two atoms are equal when the names are equal, also when they
/// come from different documents of the thread. An atom means nothing in another thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Atom(u32);

impl Atom {
    /// The place of the name in the name table. Style code uses it to make hashes.
    pub fn index(self) -> u32 {
        self.0
    }

    /// The atom of `id`. The table has it from the start, so the style code can find an `id`
    /// attribute with one number compare and no table lookup.
    pub const ID: Atom = Atom(0);
    /// The atom of `class`. Same reason as `ID`.
    pub const CLASS: Atom = Atom(1);
    /// The atom of `name`. The table has it from the start, but a document counts it as used only when an element has it.
    pub const NAME: Atom = Atom(2);
}

/// A piece of text in the text buffer of a `Document`: where it starts, and how long it is, in bytes.
/// Use `Document::text` to read it. A span of one document means nothing in another document.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Span {
    start: u32,
    len: u32,
    // 32 bits are enough for 4 GB of text in one page.
}

impl Span {
    fn end(self) -> usize {
        self.start as usize + self.len as usize
    }
}

/// One attribute of an element: `class="a b"`. Use `Document::text` to read the value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Attr {
    pub name: Atom,
    pub value: Span,
}

/// The attributes of one element: a range in the attribute list of the `Document`.
/// Use `Document::attrs` to read them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AttrList {
    start: u32,
    len: u32,
}

/// A namespace that the document knows. An element is in `Html`, `Svg` or `MathMl`. An attribute is in no namespace,
/// or in `Xml`, `Xmlns` or `Xlink`. These are the only ones that the parser makes and that scripts may ask for. A
/// namespace outside this list is refused by the host with a clear error; it is never stored as a lie.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ns {
    Html,
    Svg,
    MathMl,
    Xml,
    Xmlns,
    Xlink,
}

impl Ns {
    /// The namespace URI, as `namespaceURI` gives it.
    pub fn uri(self) -> &'static str {
        match self {
            Ns::Html => "http://www.w3.org/1999/xhtml",
            Ns::Svg => "http://www.w3.org/2000/svg",
            Ns::MathMl => "http://www.w3.org/1998/Math/MathML",
            Ns::Xml => "http://www.w3.org/XML/1998/namespace",
            Ns::Xmlns => "http://www.w3.org/2000/xmlns/",
            Ns::Xlink => "http://www.w3.org/1999/xlink",
        }
    }

    /// The namespace of a URI, if it is one of ours.
    pub fn from_uri(uri: &str) -> Option<Ns> {
        [Ns::Html, Ns::Svg, Ns::MathMl, Ns::Xml, Ns::Xmlns, Ns::Xlink].into_iter().find(|n| n.uri() == uri)
    }
}

/// The namespace identity of an element that is not in the HTML namespace. See `Document::element_ns`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Foreign {
    ns: Ns,
    prefix: Option<Atom>,
}

/// What a node is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NodeData {
    // `Copy`: every variant is small, and holds no `String`. Copying a `NodeData` copies 16 bytes.
    /// The top of the tree. It has no parent. It holds the doctype and the `html` element.
    Document,
    /// `<!DOCTYPE html>`. The name is in lowercase. Use `Document::text` to read it.
    Doctype(Span),
    /// An element like `<p class="a">`. The name is in lowercase for HTML. For an SVG or MathML element it is the local
    /// name with its own case (`foreignObject`). Use `Document::atom_str` to read the name and `Document::attrs` to read
    /// the attributes. The namespace is not here: see `Document::element_ns`.
    Element { name: Atom, attrs: AttrList },
    /// A piece of text. Use `Document::text` to read it.
    Text(Span),
    /// `<!-- text -->`. Use `Document::text` to read it.
    Comment(Span),
    /// The top of a shadow tree (DOM Standard, ShadowRoot), or a plain `DocumentFragment` (the contents of a `template`, `Document::create_fragment`: no host).
    /// It has no parent link and is never a child: the nodes of its tree have it as their top. A shadow root has its host and mode in sparse tables
    /// (`Document::shadow_host`), so a page without shadow roots pays nothing. The tree builder makes one for each `template` that has children.
    ShadowRoot,
}

struct Node {
    // No `pub`: only this file can change a node. This keeps the links in step.
    parent: Option<NodeId>,
    // `None` for the document at the top. JS: `parentNode` is `null` there.
    first_child: Option<NodeId>,
    last_child: Option<NodeId>,
    prev: Option<NodeId>,
    next: Option<NodeId>,
    // The sibling before and after this node. JS: `previousSibling` and `nextSibling`.
    data: NodeData,
}

fn is_on_name(name: &str) -> bool {
    name.len() > 2 && name.as_bytes()[..2].eq_ignore_ascii_case(b"on")
}

/// A whole tree. It owns all of its nodes.
pub struct Document {
    nodes: Vec<Node>,
    // The arena. The first node is the `Document` node, and every other node comes after it.
    names: &'static Table,
    // The name table of the thread.
    used: Vec<u32>,
    // The atoms that this document uses (a tag or an attribute name of a node, or a name that a selector or a script added),
    // in the order of their first use. See `name_count`.
    used_bits: Vec<u64>,
    // The same set, by atom number: one bit for each atom.
    on_name: bool,
    // Has the name table ever had a name that starts with `on`? See `has_on_name`.
    attrs: Vec<Attr>,
    // The attributes of all elements. Each element has a run of them, one after the other.
    text: String,
    // All the text of the page: texts, attribute values, comments and doctypes. See `Span`.
    class_sets: RefCell<HashMap<usize, HashSet<Box<str>, BuildHasherDefault<Fnv>>, BuildHasherDefault<Fnv>>>,
    // The classes of the elements that have a long `class` attribute, as a set. See `has_class`.
    // The attributes of an element never change, so this needs no clean-up.
    places: [OnceCell<Vec<(u32, u32)>>; 2],
    // The place of each element among its sibling elements, from the start and from the end. Made the
    // first time that `sibling_place` asks, and dropped when the tree changes. `[0]` counts all
    // elements, `[1]` counts the elements with the same name. See `sibling_place`.
    foreign: HashMap<u32, Foreign, BuildHasherDefault<Fnv>>,
    // The SVG and MathML elements, by `NodeId::index`. An element that is not here is in the HTML namespace, so a page
    // without SVG pays nothing. The name atom of a foreign element is its local name, shared with the HTML atoms, so a
    // type selector `rect` matches an SVG `rect`. The namespace stays with the node when it moves, and `clone_element` copies it.
    any_ns: bool,
    // Has this document used an attribute in a namespace (`xlink:href`)? See `intern_ns`.
    watched: HashSet<(u32, u32), BuildHasherDefault<Fnv>>,
    // The attributes (element index, atom) that a host holds an `Attr` object for. See `watch_attr`.
    removed: Vec<RemovedAttr>,
    // The watched attributes that were removed since the host last took the list. See `take_removed_attrs`.
    revision: u64,
    // Moves at every change that can alter a tree query: a node joins or leaves the tree, an attribute is set or removed, an
    // element gets a namespace. See `revision`.
    text_revision: u64,
    // Moves at every edit of the text of an existing node. See `text_revision`.
    name_revision: u64,
    // Moves at every change that can alter the `id` or `name` of an element in the tree. See `name_revision`.
    seen_names: HashSet<u64, BuildHasherDefault<Fnv>>,
    // The hash of every non-empty value that an `id` or `name` attribute of this document ever had. It only grows. See `may_have_name`.
    shadow_of: HashMap<u32, NodeId, BuildHasherDefault<Fnv>>,
    // The shadow root of each shadow host, by `NodeId::index` of the host. The root is not a child of the host: the light links of the host never show it.
    shadow_host: HashMap<u32, (NodeId, bool), BuildHasherDefault<Fnv>>,
    /// The `delegatesFocus` (1), `clonable` (2) and `serializable` (4) bits of a shadow root that has one of them.
    shadow_flags: HashMap<u32, u8, BuildHasherDefault<Fnv>>,
    // The way back: the host of each shadow root, and whether its mode is open. A host that is not in `shadow_of` has no root.
    slots: RefCell<Option<slots::Slots>>,
    // The slot assignment, made when it is read (`Document::assigned_nodes`) and dropped when `revision` moves.
    template_content: HashMap<u32, NodeId, BuildHasherDefault<Fnv>>,
    // The contents of each HTML `template` that has any, by `NodeId::index` of the template: a fragment node that is not a child of the template.
    watcher: Option<Box<Hooks>>,
    // The host function that is told of each change (see `set_watcher`), and the log of the changes (see `track_damage`). A page
    // with neither pays one `Option` check at a change.
    custom: HashMap<u32, u32, BuildHasherDefault<Fnv>>,
    // The custom element state of the elements that have one, by `NodeId::index`: the number of the definition, or `PRECUSTOMIZED`, or `FAILED`. See `is_defined`.
    states: HashMap<u32, Vec<Box<str>>, BuildHasherDefault<Fnv>>,
    // The custom states (`ElementInternals.states`) of the elements that have any, in the order they were added. See `has_state`.
    ui: HashMap<u32, u8, BuildHasherDefault<Fnv>>,
    // The form state of the controls that a host keeps outside the tree, by `NodeId::index`: `UI_*` bits. See `set_ui`.
    ui_revision: u64,
    // The changes of `ui`, also counted in `revision`. See `ui_revision`.
    images: HashMap<u32, Bitmap, BuildHasherDefault<Fnv>>,
    // The decoded pictures of the `<img>` elements that have one, by `NodeId::index`. See `set_image`.
    css_images: HashMap<u32, Bitmap, BuildHasherDefault<Fnv>>,
    // The decoded pictures of `background-image` and `mask-image`, by the number of their address (`bare_css::urls`). See `set_css_image`.
    user: UserState,
    // What the user points at, presses and focuses: the user action pseudo-classes. See `user`.
    sheet_names: [u32; 3],
    // The atoms of `link`, `style` and `base`, when the name table has them. See `sheet_nodes`.
    sheet_made: RefCell<Vec<NodeId>>,
    sheet_scanned: Cell<usize>,
    // Every `link`, `style` and `base` element among the first `sheet_scanned` nodes. The name of an element never changes, so the list is exact. See `sheet_nodes`.
    sheet_list: RefCell<Option<Rc<[SheetNode]>>>,
    // Those of them that are in the tree, in order. Made when it is read, and dropped when a change can alter it. See `sheet_nodes`.
}

/// A `link`, `style` or `base` element that is in the tree (see `Document::sheet_nodes`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SheetNode {
    pub node: NodeId,
    /// It is in the light tree: no shadow root is on its way up to the top node.
    pub light: bool,
    /// It is in the light tree, it is in the HTML namespace, and no HTML `template` is above it. A scan of the loader (`<link>`, `<base>`) takes only these.
    pub html: bool,
}

/// The elements that the user action pseudo-classes start from. The host sets them (`Document::set_hover` and the others). A set
/// is O(1): a selector that reads the state walks up from the element (`Document::in_chain`). It costs the same as a scan of a
/// stored chain, and it follows a node that moved.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct UserState {
    /// `:hover`: the element under the pointer. It and its ancestors match.
    pub hover: Option<NodeId>,
    /// `:active`: the element that the primary button pressed, while the button is down. It and its ancestors match.
    pub active: Option<NodeId>,
    /// `:focus` matches the focused element, `:focus-within` it and its ancestors.
    pub focus: Option<NodeId>,
    /// `:focus-visible`: the focused element shows a focus ring.
    pub focus_visible: bool,
    /// Moves at each change of `hover`, `active` and the focus pair, in that order. A host that keeps styles checks them.
    pub revision: [u64; 3],
}

/// A change of the tree that `Document::set_watcher` reports, before the document answers a later read. `prev` and `next` are the
/// siblings of `node` at that moment (for a node that left, the siblings it had).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Change {
    /// `node` was put in the children of `parent`.
    Link { parent: NodeId, node: NodeId, prev: Option<NodeId>, next: Option<NodeId> },
    /// `node` is about to leave the children of `parent`.
    Unlink { parent: NodeId, node: NodeId, prev: Option<NodeId>, next: Option<NodeId> },
    /// The attribute `name` of `el` is set or removed. `old` is the value before, `None` when there was none. `new` is the value
    /// after, `None` for a removal. The watcher runs while the change is in progress: it must not read the attribute, only use `old` and `new`.
    Attr { el: NodeId, name: Atom, old: Option<Span>, new: Option<Span> },
    /// The text of `node` changes. `old` is the value it had.
    Text { node: NodeId, old: Span },
}

/// A change of the tree in the log of `Document::track_damage`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mark {
    /// `node` was put in the children of `parent`.
    Link { parent: NodeId, node: NodeId },
    /// `node` left the children of `parent`.
    Unlink { parent: NodeId, node: NodeId },
    /// An attribute of `el` was set or removed. `old` is the value before, `None` when there was none. The text buffer only
    /// grows, so `Document::text(old)` stays readable.
    Attr { el: NodeId, name: Atom, old: Option<Span> },
    /// The text of `node` changed.
    Text { node: NodeId },
    /// The form state of `el` changed (`set_ui`).
    Ui { el: NodeId },
    /// The picture of the `<img>` `el` came or went (`set_image`).
    Image { el: NodeId },
}

/// A decoded picture: its size and its pixels, `0xAARRGGBB` not premultiplied. Shared, so that one download serves many elements.
pub type Bitmap = (u32, u32, Pixels);

/// The pixels of a picture. A vector makes them at once. `lazy` makes them at the first read (`Deref`, `shared`): an SVG file gives its size
/// without a raster, and a page that nobody paints or reads never pays for one. Clones share the pixels.
#[derive(Clone)]
pub struct Pixels(std::sync::Arc<LazyPixels>);

struct LazyPixels {
    px: std::sync::OnceLock<std::sync::Arc<[u32]>>,
    make: Option<Box<dyn Fn() -> Vec<u32> + Send + Sync>>,
}

impl Pixels {
    /// Pixels that `make` gives, called once, at the first read.
    pub fn lazy(make: impl Fn() -> Vec<u32> + Send + Sync + 'static) -> Pixels {
        Pixels(std::sync::Arc::new(LazyPixels { px: std::sync::OnceLock::new(), make: Some(Box::new(make)) }))
    }

    /// The pixels as a shared slice (made now if they were not).
    pub fn shared(&self) -> std::sync::Arc<[u32]> {
        self.0.px.get_or_init(|| self.0.make.as_ref().map_or_else(Default::default, |m| m().into())).clone()
    }

    /// Were the pixels made already?
    pub fn is_ready(&self) -> bool {
        self.0.px.get().is_some()
    }

    /// The identity of the pixels: equal for clones, and not for another picture.
    pub fn addr(&self) -> usize {
        std::sync::Arc::as_ptr(&self.0) as usize
    }
}

impl From<std::sync::Arc<[u32]>> for Pixels {
    fn from(px: std::sync::Arc<[u32]>) -> Pixels {
        Pixels(std::sync::Arc::new(LazyPixels { px: std::sync::OnceLock::from(px), make: None }))
    }
}

impl From<Vec<u32>> for Pixels {
    fn from(v: Vec<u32>) -> Pixels {
        std::sync::Arc::<[u32]>::from(v).into()
    }
}

impl std::ops::Deref for Pixels {
    type Target = [u32];
    fn deref(&self) -> &[u32] {
        self.0.px.get_or_init(|| self.0.make.as_ref().map_or_else(Default::default, |m| m().into()))
    }
}

impl std::fmt::Debug for Pixels {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Pixels({})", self.0.px.get().map_or(0, |p| p.len()))
    }
}

/// The readers of the log of `Document::track_damage`: the window frame, and the styles of `getComputedStyle` and the geometry reads.
pub const DAMAGE_FRAME: usize = 0;
pub const DAMAGE_STYLE: usize = 1;

/// The log of `Document::track_damage`. Each reader reads from its own place, so two readers see every mark.
#[derive(Debug, Default, Clone)]
struct Damage {
    marks: Vec<Mark>,
    /// The number of the first mark of `marks`, counted from the first mark of the document.
    start: u64,
    /// For each reader: the number of the first mark that it did not read. `None`: the reader is off, or it lost marks.
    at: [Option<u64>; 2],
}

impl Damage {
    fn add(&mut self, m: Mark) {
        if self.at == [None, None] {
            return;
        }
        if self.marks.len() >= MAX_MARKS {
            // The reader that is the most behind loses its place. The other one keeps its marks.
            let low = self.at.iter().flatten().min().copied();
            for a in &mut self.at {
                if *a == low {
                    *a = None;
                }
            }
            self.trim();
            if self.at == [None, None] {
                return;
            }
        }
        self.marks.push(m);
    }

    /// A change that the log does not describe, or too many changes: every reader must update all of its view.
    fn lose(&mut self) {
        self.start += self.marks.len() as u64;
        self.marks.clear();
        self.at = [None, None];
    }

    /// Drop the marks that every reader has read.
    fn trim(&mut self) {
        let low = self.at.iter().flatten().min().map_or(self.marks.len(), |&a| (a - self.start) as usize);
        self.marks.drain(..low);
        self.start += low as u64;
    }
}

/// A copy of the log, from `Document::peek_damage`.
pub struct SavedDamage(Option<Damage>);

/// What a document tells of its changes: the host function of `set_watcher` and the log of `track_damage`. They share one box,
/// so a change checks one `Option`.
#[derive(Default)]
struct Hooks {
    f: Option<Watcher>,
    damage: Option<Damage>,
}

/// The most marks that the log holds. Past it, the host updates all of its view of the page.
const MAX_MARKS: usize = 4096;

/// What `Document::parse_cut` took out of the tree. Give it to `parse_close`.
pub struct Cut {
    el: NodeId,
    parent: Option<NodeId>,
    attrs: Option<AttrList>,
    kids: Option<(NodeId, NodeId)>,
    /// For each ancestor: the first and last of the children that follow.
    tails: Vec<(NodeId, NodeId, NodeId)>,
}

/// A host function for `Document::set_watcher`. It gets the document as it is at the call and must not run script.
pub type Watcher = Box<dyn FnMut(&Document, Change)>;

/// A watched attribute that was removed: the element, the name and the value it had. The value stays readable
/// with `Document::text`, because the text buffer never frees.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RemovedAttr {
    pub el: NodeId,
    pub name: Atom,
    pub value: Span,
}

/// The names of the thread: the text of each atom, and the way back. Names are never removed, so the text is kept for the life of
/// the thread (`&'static str`) and `Document::atom_str` gives it without a borrow of the document.
struct Names {
    text: Vec<&'static str>,
    atoms: HashMap<&'static str, u32, BuildHasherDefault<NameHash>>,
    // The attributes that are in a namespace (`xlink:href`). Their atom is for the pair (namespace, qualified name), and `text` holds the
    // qualified name. The same qualified name with no namespace is another atom.
    ns_atoms: HashMap<(Ns, Box<str>), u32, BuildHasherDefault<Fnv>>,
    // The way back: from the atom of a namespaced attribute to its namespace.
    atom_ns: HashMap<u32, Ns, BuildHasherDefault<Fnv>>,
    // The last atoms of the names of 8 bytes or less (the tags and the attributes of a page), one slot for each hash of the name.
    // A hit costs a few instructions: no hash of the text, no probe, no compare of the text.
    recent: Box<[Recent; RECENT]>,
}

const RECENT: usize = 512;

#[derive(Clone, Copy)]
struct Recent {
    /// The bytes of the name (`pack`), and its length. A slot that was never used has a length that no name has.
    key: u64,
    len: u8,
    atom: u32,
}

/// The bytes of a name of 8 bytes or less in one number. Two names of the same length have the same number only if they are the same.
/// 1 to 3 bytes: the first, the middle, the last. 4 to 8 bytes: the first 4 and the last 4, which cover all of them.
#[inline(always)]
fn pack(b: &[u8]) -> u64 {
    let n = b.len();
    if n >= 4 {
        u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as u64 | (u32::from_le_bytes([b[n - 4], b[n - 3], b[n - 2], b[n - 1]]) as u64) << 32
    } else if n > 0 {
        b[0] as u64 | (b[n / 2] as u64) << 8 | (b[n - 1] as u64) << 16
    } else {
        0
    }
}

impl Names {
    fn add(&mut self, name: &str) -> u32 {
        let atom = u32::try_from(self.text.len()).expect("more than 4 billion names");
        let text: &'static str = Box::leak(Box::from(name));
        self.text.push(text);
        atom
    }

    #[inline]
    fn intern(&mut self, name: &str) -> u32 {
        let b = name.as_bytes();
        if b.len() > 8 {
            return self.intern_slow(name);
        }
        let key = pack(b);
        let at = ((key ^ b.len() as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 55) as usize;
        let slot = self.recent[at];
        if slot.key == key && slot.len as usize == b.len() {
            return slot.atom;
        }
        let atom = self.intern_slow(name);
        self.recent[at] = Recent { key, len: b.len() as u8, atom };
        atom
    }

    fn intern_slow(&mut self, name: &str) -> u32 {
        if let Some(&atom) = self.atoms.get(name) {
            return atom;
        }
        let atom = self.add(name);
        self.atoms.insert(self.text[atom as usize], atom);
        atom
    }
}

/// The name table of one thread. Reads of the text of a name are the hottest calls of the tree builder and of the style code
/// (`Document::atom_str`), so they do not pay for a `RefCell` flag.
struct Table(std::cell::UnsafeCell<Names>);

impl Table {
    /// The text of an atom.
    #[inline]
    fn text(&self, atom: u32) -> &'static str {
        // SAFETY: `Table` is not `Sync`, so only the thread that made it uses it. `with` is the only place that makes a `&mut Names`, and
        // its closures never call a `Table` method again. So no `&mut Names` is alive here. The `&'static str` is copied out of the table.
        unsafe { (&(*self.0.get()).text)[atom as usize] }
    }

    /// Use the table. `f` must not use a `Table` method.
    #[inline]
    fn with<R>(&self, f: impl FnOnce(&mut Names) -> R) -> R {
        // SAFETY: as in `text`: one thread, and no other reference to the table is alive while `f` runs.
        f(unsafe { &mut *self.0.get() })
    }
}

thread_local! {
    // The table lives as long as the thread, and a `Document` keeps this reference (`Document::names`), so a look-up is not a
    // thread-local access. A `Document` is not `Send`, so it never sees the table of another thread.
    static NAMES: &'static Table = Box::leak(Box::new(Table(std::cell::UnsafeCell::new({
        let mut n = Names { text: Vec::new(), atoms: HashMap::default(), ns_atoms: HashMap::default(), atom_ns: HashMap::default(), recent: Box::new([Recent { key: 0, len: u8::MAX, atom: 0 }; RECENT]) };
        // The order matters: it must match `Atom::ID`, `Atom::CLASS` and `Atom::NAME`.
        n.intern("id");
        n.intern("class");
        n.intern("name");
        n
    }))));
}

/// A fast hash for short names (FNV-1a). The standard one (SipHash) is slower, and it protects against
/// a server that picks keys to make the table slow. Here the "server" is the page, and it can only
/// slow down its own tab.
pub struct Fnv(u64);

impl Default for Fnv {
    fn default() -> Fnv {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
}

impl Hasher for Fnv {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        }
    }
}

/// The hash of an `id` or `name` value, by its UTF-8 bytes (see `Document::may_have_name`).
fn name_value_hash(chars: impl Iterator<Item = char>) -> u64 {
    let mut h = Fnv::default();
    for c in chars {
        h.write(c.encode_utf8(&mut [0; 4]).as_bytes());
    }
    h.finish()
}

/// A hash for the name table: 8 bytes at a time. A name is short, and `Fnv` costs 5 instructions for each byte of it. It is as open to a
/// page that picks colliding names as `Fnv` is.
#[derive(Default)]
struct NameHash(u64);

impl NameHash {
    const K: u64 = 0x517c_c1b7_2722_0a95;

    #[inline]
    fn add(&mut self, word: u64) {
        self.0 = (self.0.rotate_left(5) ^ word).wrapping_mul(Self::K);
    }
}

impl Hasher for NameHash {
    #[inline]
    fn finish(&self) -> u64 {
        // The low bits of a product depend on the low bits of the words only. The table uses them for the place of the name.
        self.0.rotate_left(26)
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut chunks = bytes.chunks_exact(8);
        for w in &mut chunks {
            self.add(u64::from_le_bytes(w.try_into().unwrap()));
        }
        let rest = chunks.remainder();
        if !rest.is_empty() {
            let mut last = [0u8; 8];
            last[..rest.len()].copy_from_slice(rest);
            self.add(u64::from_le_bytes(last));
        }
    }

    #[inline]
    fn write_u8(&mut self, byte: u8) {
        self.add(u64::from(byte));
    }
}

/// The children of a node, in order. JS: like walking `firstChild`, then `nextSibling`.
pub struct Children<'a> {
    doc: &'a Document,
    next: Option<NodeId>,
}

impl Iterator for Children<'_> {
    type Item = NodeId;

    fn next(&mut self) -> Option<NodeId> {
        let id = self.next?;
        self.next = self.doc.nodes[id.index()].next;
        Some(id)
    }
}

/// The children of a node in the flat tree, in order (`Document::flat_children`).
pub struct FlatChildren<'a> {
    doc: &'a Document,
    next: Option<NodeId>,
    shadow: bool,
}

impl Iterator for FlatChildren<'_> {
    type Item = NodeId;

    #[inline]
    fn next(&mut self) -> Option<NodeId> {
        let id = self.next?;
        self.next = if self.shadow { self.doc.shadowed_next(id) } else { self.doc.nodes[id.index()].next };
        Some(id)
    }
}

impl Document {
    /// A new tree with only the top node.
    pub fn new() -> Document {
        let mut doc = Document {
            nodes: vec![Node { parent: None, first_child: None, last_child: None, prev: None, next: None, data: NodeData::Document }],
            names: NAMES.with(|n| *n),
            used: Vec::new(),
            used_bits: Vec::new(),
            attrs: Vec::new(),
            text: String::new(),
            places: Default::default(),
            class_sets: Default::default(),
            foreign: HashMap::default(),
            any_ns: false,
            watched: HashSet::default(),
            removed: Vec::new(),
            revision: 0,
            text_revision: 0,
            name_revision: 0,
            seen_names: HashSet::default(),
            shadow_of: HashMap::default(),
            shadow_host: HashMap::default(),
            shadow_flags: HashMap::default(),
            slots: RefCell::new(None),
            template_content: HashMap::default(),
            watcher: None,
            custom: HashMap::default(),
            states: HashMap::default(),
            ui: HashMap::default(),
            ui_revision: 0,
            images: HashMap::default(),
            css_images: HashMap::default(),
            user: UserState::default(),
            on_name: false,
            sheet_names: [u32::MAX; 3],
            sheet_made: RefCell::new(Vec::new()),
            sheet_scanned: Cell::new(0),
            sheet_list: RefCell::new(None),
        };
        // The order matters: it must match `Atom::ID` and `Atom::CLASS`.
        doc.intern("id");
        doc.intern("class");
        doc
    }

    /// A number that moves at every public change that can alter the answer of a tree query: a node is linked into or
    /// detached from the tree (the parser, `insert_*`, `move_to`, `move_children`, `detach`), an attribute is set or removed
    /// (any name, any value), an element gets a namespace, a custom element state or a shadow root. A host that caches the nodes of a query keeps the number from
    /// when it built the list, and builds it again when the number is not the same. Changes that cannot alter a query leave it
    /// alone: a new node with no parent, a text edit, a new name, a new span. It only grows, and it is not a count of changes.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// A number that moves at every change that can alter which elements of the tree have an `id` or a `name`, or their tag or
    /// namespace: a node that has attributes or children joins or leaves the tree, an `id` or `name` attribute is set or removed,
    /// an element gets a namespace, the parser cuts or mends the tree. A node with neither attributes nor children joins and
    /// leaves the tree without moving it. A host that caches named elements builds them again when the number is not the same.
    pub fn name_revision(&self) -> u64 {
        self.name_revision
    }

    /// Is `name` the `id` or the `name` attribute?
    fn names_attr(&self, name: Atom) -> bool {
        name == Atom::ID || name == Atom::NAME
    }

    /// Record that an attribute `name` got the value `value`: a later `may_have_name` for it says yes.
    fn note_name(&mut self, name: Atom, value: &str) {
        if !value.is_empty() && self.names_attr(name) {
            let mut h = Fnv::default();
            h.write(value.as_bytes());
            self.seen_names.insert(h.finish());
        }
    }

    /// Could an element have an `id` or `name` attribute with the value `value` (given as chars)? `false` is final: no element has
    /// one now, and none had one that is gone, so a look-up for the name needs no walk of the tree. `true` means maybe: the value
    /// was set once (on an element that may have left, or had the value changed), or another value has the same hash. The set
    /// only grows, so it costs a hash look-up and no work when a node leaves the tree.
    pub fn may_have_name(&self, value: impl Iterator<Item = char>) -> bool {
        self.seen_names.contains(&name_value_hash(value))
    }

    /// Can `node` (with its subtree) change the named elements when it joins or leaves the tree?
    fn may_name(&self, node: NodeId) -> bool {
        let n = &self.nodes[node.index()];
        n.first_child.is_some() || matches!(n.data, NodeData::Element { attrs, .. } if attrs.len > 0)
    }

    /// Move `revision` for a change that the host keeps outside the tree, so a host cache of a query that reads it (the selected
    /// options of a select) is built again.
    pub fn touch(&mut self) {
        self.revision += 1;
        self.mark_full();
    }

    /// A number that moves at every edit of the text of a node that exists (`set_text`, text that joins a text node). With
    /// `revision`, a host can tell that nothing that layout reads has changed. It only grows.
    pub fn text_revision(&self) -> u64 {
        self.text_revision
    }

    /// The top node.
    pub fn root(&self) -> NodeId {
        NodeId::from_index(0)
    }

    /// What this node is.
    pub fn data(&self, id: NodeId) -> &NodeData {
        &self.nodes[id.index()].data
        // JS: `nodes[id].data`. This crashes if `id` comes from another document. See the top of this file.
    }

    /// The parent of this node. `None` for the top node.
    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.nodes[id.index()].parent
    }

    /// The shadow root of the host `host`, if it has one (open or closed).
    #[inline]
    pub fn shadow_root(&self, host: NodeId) -> Option<NodeId> {
        // Most pages have no shadow root: skip the hash.
        if self.shadow_of.is_empty() {
            return None;
        }
        self.shadow_of.get(&(host.index() as u32)).copied()
    }

    /// Set the custom element state of `el` (`None`: no state, the "undefined" state of an element with a valid name). The value is a
    /// definition number (the state "custom"), `PRECUSTOMIZED` or `FAILED`. The host numbers the definitions.
    pub fn set_custom_state(&mut self, el: NodeId, state: Option<u32>) {
        let old = match state {
            Some(s) => self.custom.insert(el.index() as u32, s),
            None => self.custom.remove(&(el.index() as u32)),
        };
        // `:defined` may match another element now.
        self.revision += u64::from(old != state);
        if old != state {
            self.mark_full();
        }
    }

    /// The custom element state of `el`, see `set_custom_state`.
    pub fn custom_state(&self, el: NodeId) -> Option<u32> {
        // A page with no custom element pays one `is_empty` check.
        if self.custom.is_empty() {
            return None;
        }
        self.custom.get(&(el.index() as u32)).copied()
    }

    /// The custom states of `el` (`ElementInternals.states`), in the order they were added.
    pub fn states(&self, el: NodeId) -> &[Box<str>] {
        if self.states.is_empty() {
            return &[];
        }
        self.states.get(&(el.index() as u32)).map_or(&[], |v| v)
    }

    /// Replace the custom states of `el`. A rule with `:state()` may match another element now, so `revision` moves.
    pub fn set_states(&mut self, el: NodeId, states: Vec<Box<str>>) {
        if states.is_empty() {
            self.states.remove(&(el.index() as u32));
        } else {
            self.states.insert(el.index() as u32, states);
        }
        self.revision += 1;
        self.mark_full();
    }

    /// Set the form state of `el` that the host keeps outside the tree (`UI_*` bits; 0 clears it). Selectors read it for `:checked`,
    /// `:placeholder-shown` and `:indeterminate`. It moves `revision`, as a change of the tree would, when the bits change.
    pub fn set_ui(&mut self, el: NodeId, bits: u8) {
        let old = if bits == 0 { self.ui.remove(&(el.index() as u32)) } else { self.ui.insert(el.index() as u32, bits) };
        if old.unwrap_or(0) != bits {
            self.revision += 1;
            self.ui_revision += 1;
            self.mark(Mark::Ui { el });
        }
    }

    /// Give the `<img>` `el` its decoded picture (`None` takes it away). Layout reads its size (the hints of the style) and paint draws it.
    pub fn set_image(&mut self, el: NodeId, image: Option<Bitmap>) {
        let key = el.index() as u32;
        match image {
            Some(i) => self.images.insert(key, i),
            None => self.images.remove(&key),
        };
        self.revision += 1;
        self.mark(Mark::Image { el });
    }

    /// The picture of `el`, see `set_image`. A page with no picture pays one `is_empty` check.
    pub fn image(&self, el: NodeId) -> Option<&Bitmap> {
        if self.images.is_empty() {
            return None;
        }
        self.images.get(&(el.index() as u32))
    }

    /// Give the picture of address number `id` to the styles that name it. Layout does not read it, so the document logs no change: the host
    /// builds the display list again.
    pub fn set_css_image(&mut self, id: u32, image: Bitmap) {
        self.css_images.insert(id, image);
    }

    /// The picture of address number `id`, see `set_css_image`.
    pub fn css_image(&self, id: u32) -> Option<&Bitmap> {
        if self.css_images.is_empty() {
            return None;
        }
        self.css_images.get(&id)
    }

    /// Does any element have a picture?
    pub fn has_images(&self) -> bool {
        !self.images.is_empty()
    }

    /// The changes of the form state (`set_ui`). They are in `revision` too: a host that keeps a layout takes them out, because only a
    /// rule with a form pseudo-class reads them.
    pub fn ui_revision(&self) -> u64 {
        self.ui_revision
    }

    /// The `UI_*` bits of `el`, see `set_ui`.
    pub fn ui(&self, el: NodeId) -> u8 {
        // A page with no form state pays one `is_empty` check.
        if self.ui.is_empty() {
            return 0;
        }
        self.ui.get(&(el.index() as u32)).copied().unwrap_or(0)
    }

    /// What the user points at, presses and focuses (see `UserState`).
    pub fn user(&self) -> &UserState {
        &self.user
    }

    /// Is `el` the node `from` or one of its shadow-including ancestors?
    pub fn in_chain(&self, el: NodeId, from: Option<NodeId>) -> bool {
        let mut cur = from;
        while let Some(c) = cur {
            if c == el {
                return true;
            }
            let node = &self.nodes[c.index()];
            // Only a shadow root has a host: the other nodes with no parent skip the map.
            cur = node.parent.or_else(|| if matches!(node.data, NodeData::ShadowRoot) { self.shadow_host(c) } else { None });
        }
        false
    }

    /// The pointer is over `n` now. Gives whether `:hover` matches other elements now.
    pub fn set_hover(&mut self, n: Option<NodeId>) -> bool {
        if self.user.hover == n {
            return false;
        }
        self.user.hover = n;
        self.user.revision[0] += 1;
        true
    }

    /// The primary button pressed `n` (`None`: it went up). Gives whether `:active` matches other elements now.
    pub fn set_active(&mut self, n: Option<NodeId>) -> bool {
        if self.user.active == n {
            return false;
        }
        self.user.active = n;
        self.user.revision[1] += 1;
        true
    }

    /// `n` has the focus now, and `visible` says whether it shows a focus ring. Gives whether a focus pseudo-class matches other elements now.
    pub fn set_focus(&mut self, n: Option<NodeId>, visible: bool) -> bool {
        if self.user.focus == n && self.user.focus_visible == visible {
            return false;
        }
        self.user.focus = n;
        self.user.focus_visible = visible;
        self.user.revision[2] += 1;
        true
    }

    /// Log the changes of the tree from now on, for a host that keeps a view of the page and updates only the changed part
    /// (`take_damage`). A page that is not tracked pays one check at a change.
    /// `reader` (`DAMAGE_FRAME`, `DAMAGE_STYLE`) reads from now on.
    pub fn track_damage(&mut self, reader: usize) {
        let d = self.watcher.get_or_insert_default().damage.get_or_insert_default();
        d.at[reader] = Some(d.start + d.marks.len() as u64);
        d.trim();
    }

    /// The changes since the last call of `reader`, oldest first. `None` when the reader is off, or when a change happened that
    /// the log does not describe (a custom element state, a shadow root, the parser cut, `touch`), or more than `MAX_MARKS`
    /// changes: then the host must update all of its view, and call `track_damage` again.
    pub fn take_damage(&mut self, reader: usize) -> Option<Vec<Mark>> {
        let d = self.watcher.as_mut()?.damage.as_mut()?;
        let from = (d.at[reader]? - d.start) as usize;
        let marks = d.marks[from..].to_vec();
        d.at[reader] = Some(d.start + d.marks.len() as u64);
        d.trim();
        Some(marks)
    }

    /// A copy of the log (for a host test that looks at the page without taking the changes).
    pub fn peek_damage(&self) -> SavedDamage {
        SavedDamage(self.watcher.as_ref().and_then(|h| h.damage.clone()))
    }

    /// Set the log back to `saved` (from `peek_damage`).
    pub fn restore_damage(&mut self, saved: SavedDamage) {
        let h = self.watcher.get_or_insert_default();
        h.damage = saved.0;
        if h.f.is_none() && h.damage.is_none() {
            self.watcher = None;
        }
    }

    /// Log a change that the watcher is not told of. Most changes are logged by `notify`.
    #[inline(always)]
    fn mark(&mut self, m: Mark) {
        if self.watcher.is_some() {
            self.log(Some(m));
        }
    }

    /// A change that the log does not describe.
    #[inline(always)]
    fn mark_full(&mut self) {
        if self.watcher.is_some() {
            self.log(None);
        }
    }

    /// `mark` and `mark_full` when a hook is set: out of line, so the parser and the scripts that run with no hook pay one test.
    #[cold]
    #[inline(never)]
    fn log(&mut self, m: Option<Mark>) {
        if let Some(d) = self.watcher.as_mut().and_then(|h| h.damage.as_mut()) {
            match m {
                Some(m) => d.add(m),
                None => d.lose(),
            }
        }
    }

    /// The `link`, `style` and `base` elements that are in the tree, in the order of the page (a shadow tree comes right after its host, before the
    /// light children). It is the list that the style sheet code needs, so it does not walk the tree. The name of an element never changes, so
    /// `sheet_made` has every candidate. The list keeps nodes that are in the tree only (the top node, or a shadow root of a host that is), in order, and it is
    /// made again at the next read after a node that can hold one joins or leaves the tree. It costs the depth of each of the L elements, and the
    /// children of their ancestors up to the last one that leads to an element. An attribute or the text of an element is not kept here: read it when you need it.
    /// In a debug build and in tests, each read checks the list against a walk of the whole tree (`sheet_nodes_walk`).
    pub fn sheet_nodes(&self) -> Rc<[SheetNode]> {
        self.scan_sheets();
        let mut slot = self.sheet_list.borrow_mut();
        let list = slot.get_or_insert_with(|| {
            // A walk costs the nodes of the page, the other way costs the depth of each element and a table: the small page takes the walk.
            if self.nodes.len() < 32 * self.sheet_made.borrow().len() { self.sheet_nodes_walk().into() } else { self.order_sheets() }
        });
        #[cfg(any(debug_assertions, test))]
        {
            let walk = self.sheet_nodes_walk();
            assert_eq!(**list, *walk, "the sheet list is not the list of a walk");
            assert_eq!(*self.order_sheets(), *walk, "the pruned sheet list is not the list of a walk");
        }
        list.clone()
    }

    /// Add the `link`, `style` and `base` elements of the nodes made since the last read to `sheet_made`. One pass over the new part of the `Vec`, so the
    /// whole life of a document costs one pass, and a document that nobody asks pays nothing.
    fn scan_sheets(&self) {
        let from = self.sheet_scanned.replace(self.nodes.len());
        let mut made = self.sheet_made.borrow_mut();
        for (i, n) in self.nodes.iter().enumerate().skip(from) {
            if matches!(n.data, NodeData::Element { name, .. } if self.is_sheet_name(name)) {
                made.push(NodeId::from_index(i));
            }
        }
    }

    /// `sheet_nodes` without a walk of the tree: climb from each element that was made.
    fn order_sheets(&self) -> Rc<[SheetNode]> {
        self.scan_sheets();
        let made = &self.sheet_made.borrow()[..];
        if made.is_empty() {
            return Rc::from([]);
        }
        let template = self.find_atom("template");
        // `up[p]`: the shadow root and the light children of `p` that lead to an element, each once.
        let mut up: HashMap<NodeId, (Option<NodeId>, HashSet<NodeId, BuildHasherDefault<Fnv>>), BuildHasherDefault<Fnv>> = HashMap::default();
        let mut found: HashMap<NodeId, (bool, bool), BuildHasherDefault<Fnv>> = HashMap::default();
        let mut path = Vec::new();
        'each: for &el in made {
            let (mut light, mut under_template, mut cur) = (true, false, el);
            path.clear();
            loop {
                match self.nodes[cur.index()].parent {
                    Some(p) => {
                        path.push((p, cur, false));
                        under_template |= light && matches!(self.nodes[p.index()].data, NodeData::Element { name, .. } if Some(name) == template) && !self.is_foreign(p);
                        cur = p;
                    }
                    None if cur == self.root() => break,
                    None => match self.shadow_host.get(&(cur.index() as u32)) {
                        Some(&(host, _)) => {
                            path.push((host, cur, true));
                            light = false;
                            cur = host;
                        }
                        None => continue 'each,
                    },
                }
            }
            for &(p, child, shadow) in &path {
                let e = up.entry(p).or_default();
                if shadow {
                    e.0 = Some(child);
                } else {
                    e.1.insert(child);
                }
            }
            found.insert(el, (light, light && !under_template && !self.is_foreign(el)));
        }
        let mut out = Vec::with_capacity(found.len());
        let mut stack = vec![self.root()];
        while let Some(n) = stack.pop() {
            if let Some(&(light, html)) = found.get(&n) {
                out.push(SheetNode { node: n, light, html });
            }
            let Some((shadow, kids)) = up.get(&n) else { continue };
            // The marked children in the order of the page: walk the children until each one is seen.
            let mut ordered = Vec::with_capacity(kids.len());
            let mut c = self.nodes[n.index()].first_child;
            while let Some(k) = c.filter(|_| ordered.len() < kids.len()) {
                if kids.contains(&k) {
                    ordered.push(k);
                }
                c = self.nodes[k.index()].next;
            }
            stack.extend(ordered.into_iter().rev());
            stack.extend(*shadow);
        }
        out.into()
    }

    /// The list of `sheet_nodes`, made by a walk of the whole tree. It is the check of `sheet_nodes`.
    pub fn sheet_nodes_walk(&self) -> Vec<SheetNode> {
        let template = self.find_atom("template");
        let mut out = Vec::new();
        // The node, whether it is in the light tree, and whether an HTML `template` is above it.
        let mut stack = vec![(self.root(), true, false)];
        while let Some((n, light, under_template)) = stack.pop() {
            if let NodeData::Element { name, .. } = self.nodes[n.index()].data {
                if self.is_sheet_name(name) {
                    out.push(SheetNode { node: n, light, html: light && !under_template && !self.is_foreign(n) });
                }
                let is_template = Some(name) == template && !self.is_foreign(n);
                let mut c = self.nodes[n.index()].last_child;
                while let Some(k) = c {
                    stack.push((k, light, under_template || is_template));
                    c = self.nodes[k.index()].prev;
                }
            } else {
                let mut c = self.nodes[n.index()].last_child;
                while let Some(k) = c {
                    stack.push((k, light, under_template));
                    c = self.nodes[k.index()].prev;
                }
            }
            if let Some(&root) = self.shadow_of.get(&(n.index() as u32)) {
                stack.push((root, false, under_template));
            }
        }
        out
    }

    /// Does any element have a shadow root?
    pub fn has_shadow_roots(&self) -> bool {
        !self.shadow_of.is_empty()
    }

    /// Does any element have a custom element state?
    pub fn has_custom(&self) -> bool {
        !self.custom.is_empty()
    }

    /// The `:defined` pseudo-class: every element, except an HTML element with a valid custom element name that no constructor has run for.
    pub fn is_defined(&self, el: NodeId) -> bool {
        !(self.element_ns(el) == Ns::Html && valid_custom_name(self.element_name(el))) || self.custom_state(el).is_some_and(|s| s < PRECUSTOMIZED)
    }

    /// The host of the shadow root `root`. `None` for any other node.
    pub fn shadow_host(&self, root: NodeId) -> Option<NodeId> {
        self.shadow_host.get(&(root.index() as u32)).map(|&(h, _)| h)
    }

    /// The `delegatesFocus` (1), `clonable` (2) and `serializable` (4) bits of the shadow root `root`.
    pub fn shadow_flags(&self, root: NodeId) -> u8 {
        self.shadow_flags.get(&(root.index() as u32)).copied().unwrap_or(0)
    }

    pub fn set_shadow_flags(&mut self, root: NodeId, flags: u8) {
        if flags != 0 {
            self.shadow_flags.insert(root.index() as u32, flags);
        }
    }

    /// Is the shadow root `root` in open mode? `false` for a closed root and for any other node.
    pub fn shadow_is_open(&self, root: NodeId) -> bool {
        self.shadow_host.get(&(root.index() as u32)).is_some_and(|&(_, open)| open)
    }

    /// A new fragment node: the top of a tree with no host (JS: `document.createDocumentFragment()`).
    pub fn create_fragment(&mut self) -> NodeId {
        self.new_node(NodeData::ShadowRoot)
    }

    /// A new `Document` node that is not the top of this tree: the host uses it for the inert document that owns the contents of the templates. It has no children.
    pub fn create_document(&mut self) -> NodeId {
        self.new_node(NodeData::Document)
    }

    /// True when some `template` of this document has contents.
    #[inline(never)]
    pub fn has_template_content(&self) -> bool {
        !self.template_content.is_empty()
    }

    /// The contents of the HTML `template` `tpl`, if it has any yet. A page with no template contents pays one `is_empty` check.
    #[inline]
    pub fn template_content(&self, tpl: NodeId) -> Option<NodeId> {
        if self.template_content.is_empty() {
            return None;
        }
        self.template_content.get(&(tpl.index() as u32)).copied()
    }

    /// Is `n` in the tree of the contents of a `template`? That tree belongs to an inert document, which has no scripting. A page with no template contents pays one `is_empty` check.
    #[inline]
    pub fn in_template_content(&self, n: NodeId) -> bool {
        if self.template_content.is_empty() {
            return false;
        }
        let mut top = n;
        while let Some(p) = self.parent(top) {
            top = p;
        }
        self.template_content.values().any(|&c| c == top)
    }

    /// The contents of the HTML `template` `tpl`; an empty fragment is made if it has none. The caller checked that `tpl` is an HTML `template`.
    pub fn make_template_content(&mut self, tpl: NodeId) -> NodeId {
        if let Some(c) = self.template_content(tpl) {
            return c;
        }
        let c = self.create_fragment();
        self.template_content.insert(tpl.index() as u32, c);
        c
    }

    /// The parent of this node in the shadow-including tree: its parent, or the host when this node is a shadow root. JS: the step of `getRootNode({composed: true})`.
    pub fn parent_or_host(&self, id: NodeId) -> Option<NodeId> {
        self.nodes[id.index()].parent.or_else(|| self.shadow_host(id))
    }

    /// Give the element `host` a new, empty shadow root. The caller checked that `host` can be a host and has no root yet. The root is not in the light
    /// tree of the host. Gives the root.
    pub fn attach_shadow(&mut self, host: NodeId, open: bool) -> NodeId {
        debug_assert!(matches!(self.nodes[host.index()].data, NodeData::Element { .. }) && !self.shadow_of.contains_key(&(host.index() as u32)));
        let root = self.new_node(NodeData::ShadowRoot);
        self.shadow_of.insert(host.index() as u32, root);
        self.shadow_host.insert(root.index() as u32, (host, open));
        // The flat tree changed: the light children of the host are not rendered now.
        self.revision += 1;
        self.sheets_moved();
        self.mark_full();
        root
    }

    /// The parent of this node in the flat tree, the tree that is rendered (Blink `FlatTreeTraversal::Parent`). A child of a shadow root has
    /// the host as its parent. A light child of a host has the parent of the slot that takes it (a slot makes no box), or none when no slot
    /// takes it: it is not rendered. A page with no shadow root pays one `is_empty` check.
    #[inline]
    pub fn flat_parent(&self, id: NodeId) -> Option<NodeId> {
        let parent = self.parent(id)?;
        if self.shadow_of.is_empty() {
            return Some(parent);
        }
        self.shadowed_parent(id, parent, false)
    }

    /// The children of this node in the flat tree: the children of its shadow root if it is a host, else its own children. A slot is replaced by
    /// the nodes that it takes, or by its fallback children when it takes none (see `flat_next`).
    #[inline]
    pub fn flat_children(&self, id: NodeId) -> FlatChildren<'_> {
        if self.shadow_of.is_empty() {
            return FlatChildren { doc: self, next: self.nodes[id.index()].first_child, shadow: false };
        }
        FlatChildren { doc: self, next: self.shadowed_first(id), shadow: true }
    }

    /// The children of this node, in order.
    pub fn children(&self, id: NodeId) -> Children<'_> {
        Children { doc: self, next: self.nodes[id.index()].first_child }
    }

    /// Does the element `el` have `class` in its `class` attribute? A short attribute is read word by word.
    /// A long one (128 bytes or more) becomes a set the first time,
    /// so that 5,000 rules on one element do not read the 5,000 words 5,000 times.
    #[inline(always)]
    pub fn has_class(&self, el: NodeId, class: &str) -> bool {
        let Some(value) = self.attr(el, Atom::CLASS) else { return false };
        if value.len() < 128 {
            return value.split_ascii_whitespace().any(|c| c == class);
        }
        self.class_sets.borrow_mut().entry(el.index()).or_insert_with(|| value.split_ascii_whitespace().map(Box::from).collect()).contains(class)
    }

    /// The place of the element `el` among its sibling elements: 1 for the first. It counts from the end with
    /// `from_end`, and counts only the elements with the same name with `of_type`. This is what `:nth-child`
    /// and `:nth-of-type` ask. All the places are made in one pass the first time, so each answer is a look-up.
    /// (Counting the siblings for each element is slow for a parent with 50,000 children.)
    pub fn sibling_place(&self, el: NodeId, of_type: bool, from_end: bool) -> u32 {
        let places = self.places[of_type as usize].get_or_init(|| self.make_places(of_type));
        let (start, end) = places[el.index()];
        if from_end { end } else { start }
    }

    fn make_places(&self, of_type: bool) -> Vec<(u32, u32)> {
        let mut places = vec![(0, 0); self.nodes.len()];
        let mut seen: HashMap<Atom, u32, BuildHasherDefault<Fnv>> = HashMap::default();
        for parent in 0..self.nodes.len() {
            if self.nodes[parent].first_child.is_none() {
                continue;
            }
            let parent = NodeId::from_index(parent);
            seen.clear();
            let mut total = 0;
            for child in self.children(parent) {
                if let NodeData::Element { name, .. } = self.nodes[child.index()].data {
                    let count = if of_type { seen.entry(name).or_insert(0) } else { &mut total };
                    *count += 1;
                    places[child.index()].0 = *count;
                }
            }
            // The place from the end is the count of the group, less the place from the start, plus one.
            for child in self.children(parent) {
                if let NodeData::Element { name, .. } = self.nodes[child.index()].data {
                    let count = if of_type { seen[&name] } else { total };
                    places[child.index()].1 = count - places[child.index()].0 + 1;
                }
            }
        }
        places
    }

    /// The number of different names (tags and attributes) in the name table. It only grows.
    pub fn name_count(&self) -> usize {
        self.used.len()
    }

    /// Has the name table ever had a name that starts with `on` (an event handler attribute may exist)? It never goes back to false.
    #[inline]
    pub fn has_on_name(&self) -> bool {
        self.on_name
    }

    /// The atom of the `i`-th name of the table (`i < name_count()`). Names are never removed, so the atoms from an earlier
    /// `name_count()` up are the names added since.
    pub fn atom_at(&self, i: usize) -> Atom {
        Atom(self.used[i])
    }

    /// The number of nodes, with the top node.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Always false: a document has its top node.
    pub fn is_empty(&self) -> bool {
        false
    }

    /// The last child of this node. JS: `lastChild`.
    pub fn last_child(&self, id: NodeId) -> Option<NodeId> {
        self.nodes[id.index()].last_child
    }

    /// The atom for a name, if the document has it. It does not add the name.
    pub fn find_atom(&self, name: &str) -> Option<Atom> {
        let atom = self.names.with(|n| n.atoms.get(name).copied())?;
        self.uses(atom).then_some(Atom(atom))
    }

    /// Does this document use the atom?
    #[inline]
    fn uses(&self, atom: u32) -> bool {
        self.used_bits.get((atom >> 6) as usize).is_some_and(|w| w >> (atom & 63) & 1 == 1)
    }

    /// Add an atom that this document does not use yet to its names.
    #[cold]
    fn first_use(&mut self, atom: u32, name: &str) {
        self.use_atom(atom);
        self.on_name |= is_on_name(name);
        if let Some(i) = ["link", "style", "base"].iter().position(|&n| n == name) {
            self.sheet_names[i] = atom;
        }
    }

    /// Add the atom to the names of this document. Gives `true` when it was not there.
    fn use_atom(&mut self, atom: u32) -> bool {
        let word = (atom >> 6) as usize;
        if word >= self.used_bits.len() {
            self.used_bits.resize(word + 1, 0);
        }
        let bit = 1u64 << (atom & 63);
        if self.used_bits[word] & bit != 0 {
            return false;
        }
        self.used_bits[word] |= bit;
        self.used.push(atom);
        true
    }

    /// The sibling before this node. JS: `previousSibling`.
    pub fn prev_sibling(&self, id: NodeId) -> Option<NodeId> {
        self.nodes[id.index()].prev
    }

    /// The sibling after this node. JS: `nextSibling`.
    pub fn next_sibling(&self, id: NodeId) -> Option<NodeId> {
        self.nodes[id.index()].next
    }

    /// The value of an attribute of an element. `None` if there is none, or if this is not an element.
    /// With a repeated name, the first one wins (the tree builder never stores a repeat).
    pub fn attr(&self, id: NodeId, name: Atom) -> Option<&str> {
        let NodeData::Element { attrs, .. } = self.nodes[id.index()].data else { return None };
        self.attrs(attrs).iter().find(|a| a.name == name).map(|a| self.text(a.value))
    }

    /// The text of a `Span`.
    pub fn text(&self, span: Span) -> &str {
        &self.text[span.start as usize..span.end()]
    }

    /// The attributes of an element, in the order of the page.
    pub fn attrs(&self, list: AttrList) -> &[Attr] {
        // An empty list can start after the end of the list of all attributes (a removal shortened it). It has nothing to read.
        if list.len == 0 {
            return &[];
        }
        &self.attrs[list.start as usize..(list.start + list.len) as usize]
    }

    /// The atom for a name. A name that is new goes in the table.
    pub fn intern(&mut self, name: &str) -> Atom {
        let atom = self.names.with(|n| n.intern(name));
        if !self.uses(atom) {
            self.first_use(atom, name);
        }
        Atom(atom)
    }

    /// The text of a name.
    pub fn atom_str(&self, atom: Atom) -> &str {
        self.names.text(atom.0)
    }

    /// Add a new node as a child of `parent`, just before `before`. With `None`, add it at the end.
    /// `before` must be a child of `parent`. If it is not, the node goes at the end.
    /// Gives the new node. The tree builder needs this to put text before a table.
    fn insert(&mut self, parent: NodeId, before: Option<NodeId>, data: NodeData) -> NodeId {
        let id = NodeId::from_index(self.nodes.len());
        // The new node goes at the end of the `Vec`, so its number is the length now.
        self.nodes.push(Node { parent: None, first_child: None, last_child: None, prev: None, next: None, data });
        self.link(id, parent, before);
        id
    }

    /// Is `name` the atom of `link`, `style` or `base`?
    #[inline(always)]
    fn is_sheet_name(&self, name: Atom) -> bool {
        let [a, b, c] = self.sheet_names;
        name.0 == a || name.0 == b || name.0 == c
    }

    /// Can `node` (with its subtree) change `sheet_nodes` when it joins or leaves the tree? A `link`, `style` or `base` can, and so can a node
    /// that has children. A node with no children that is none of the three cannot. A list that is not made (no read yet) has nothing to drop, so a page that never reads it pays one test.
    fn may_hold_sheet(&mut self, node: NodeId) -> bool {
        let n = &self.nodes[node.index()];
        self.sheet_list.get_mut().is_some() && (n.first_child.is_some() || matches!(n.data, NodeData::Element { name, .. } if self.is_sheet_name(name)))
    }

    /// The list of `sheet_nodes` is out of date.
    fn sheets_moved(&mut self) {
        *self.sheet_list.get_mut() = None;
    }

    /// Put `node`, which has no parent, in the list of children of `parent`, just before `before` (or at the end).
    fn link(&mut self, node: NodeId, parent: NodeId, before: Option<NodeId>) {
        self.places = Default::default();
        self.revision += 1;
        self.name_revision += u64::from(self.may_name(node));
        if self.may_hold_sheet(node) {
            self.sheets_moved();
        }
        let before = before.filter(|&b| b != node && self.parent(b) == Some(parent));
        let prev = self.child_before(parent, before);
        let n = &mut self.nodes[node.index()];
        (n.parent, n.prev, n.next) = (Some(parent), prev, before);
        if self.watcher.is_some() {
            self.notify(Change::Link { parent, node, prev, next: before });
        }
        // Fix the links of the neighbours. JS: like inserting into a doubly linked list.
        match prev {
            Some(p) => self.nodes[p.index()].next = Some(node),
            None => self.nodes[parent.index()].first_child = Some(node),
        }
        match before {
            Some(b) => self.nodes[b.index()].prev = Some(node),
            None => self.nodes[parent.index()].last_child = Some(node),
        }
    }

    /// Take `node` out of its parent. Its children go with it. Does nothing for a node that has no parent.
    /// JS: like `node.remove()`.
    pub fn detach(&mut self, node: NodeId) {
        let Node { parent, prev, next, .. } = self.nodes[node.index()];
        let Some(parent) = parent else { return };
        if self.watcher.is_some() {
            self.notify(Change::Unlink { parent, node, prev, next });
        }
        self.places = Default::default();
        self.revision += 1;
        self.name_revision += u64::from(self.may_name(node));
        if self.may_hold_sheet(node) {
            self.sheets_moved();
        }
        match prev {
            Some(p) => self.nodes[p.index()].next = next,
            None => self.nodes[parent.index()].first_child = next,
        }
        match next {
            Some(n) => self.nodes[n.index()].prev = prev,
            None => self.nodes[parent.index()].last_child = prev,
        }
        let n = &mut self.nodes[node.index()];
        (n.parent, n.prev, n.next) = (None, None, None);
    }

    /// The view of the tree that a parser has while it makes the element `el`: `el` has no parent, no children and no attributes, and nothing
    /// that follows `el` in the tree exists yet. Nothing is reported to the watcher. `parse_open` puts `el` back at the end of its parent and
    /// `parse_close` gives back everything else. The host uses it to run the constructor of a custom element that the parser would make.
    pub fn parse_cut(&mut self, el: NodeId) -> Cut {
        let mut cut = Cut { el, attrs: None, kids: None, tails: Vec::new(), parent: self.nodes[el.index()].parent };
        if let NodeData::Element { name, attrs } = self.nodes[el.index()].data {
            cut.attrs = Some(attrs);
            self.nodes[el.index()].data = NodeData::Element { name, attrs: AttrList { start: to_u32(self.attrs.len()), len: 0 } };
        }
        let n = &mut self.nodes[el.index()];
        if let (Some(f), Some(l)) = (n.first_child, n.last_child) {
            cut.kids = Some((f, l));
            (n.first_child, n.last_child) = (None, None);
        }
        let mut cur = el;
        while let Some(p) = self.nodes[cur.index()].parent {
            // The first step takes `el` out of `p`; every other step keeps the ancestor and hides what follows it.
            let tail = if cur == el { self.nodes[el.index()].next } else { self.nodes[cur.index()].next };
            let before = if cur == el { self.nodes[el.index()].prev } else { Some(cur) };
            if let (Some(first), Some(last)) = (tail, self.nodes[p.index()].last_child) {
                cut.tails.push((p, first, last));
            }
            match before {
                Some(b) => self.nodes[b.index()].next = None,
                None => self.nodes[p.index()].first_child = None,
            }
            self.nodes[p.index()].last_child = before;
            if cur == el {
                let n = &mut self.nodes[el.index()];
                (n.parent, n.prev, n.next) = (None, None, None);
            }
            cur = p;
        }
        self.places = Default::default();
        self.revision += 1;
        self.name_revision += 1;
        self.sheets_moved();
        self.mark_full();
        cut
    }

    /// Put the element of `cut` back as the last child of its parent, with its attributes (the parser has made the start tag).
    pub fn parse_open(&mut self, cut: &Cut) {
        let el = cut.el;
        // A constructor that added children is refused (the element is made again, without them).
        (self.nodes[el.index()].first_child, self.nodes[el.index()].last_child) = (None, None);
        if let (Some(attrs), NodeData::Element { name, .. }) = (cut.attrs, self.nodes[el.index()].data) {
            self.nodes[el.index()].data = NodeData::Element { name, attrs };
        }
        if let Some(p) = cut.parent {
            let last = self.nodes[p.index()].last_child;
            let n = &mut self.nodes[el.index()];
            (n.parent, n.prev, n.next) = (Some(p), last, None);
            match last {
                Some(l) => self.nodes[l.index()].next = Some(el),
                None => self.nodes[p.index()].first_child = Some(el),
            }
            self.nodes[p.index()].last_child = Some(el);
        }
        self.places = Default::default();
        self.revision += 1;
        self.name_revision += 1;
        self.sheets_moved();
        self.mark_full();
    }

    /// Give back what `parse_cut` hid: the children of the element go after the ones it has now, and what followed it after the last child of each
    /// ancestor. A node that left the tree while it was hidden (a script that held it) is not put back.
    pub fn parse_close(&mut self, cut: Cut) {
        if let Some((f, l)) = cut.kids.filter(|&(f, l)| self.nodes[f.index()].parent == Some(cut.el) && self.nodes[l.index()].parent == Some(cut.el)) {
            self.append_chain(cut.el, f, l);
        }
        for (p, f, l) in cut.tails {
            if self.nodes[f.index()].parent == Some(p) && self.nodes[l.index()].parent == Some(p) {
                self.append_chain(p, f, l);
            }
        }
        self.places = Default::default();
        self.revision += 1;
        self.name_revision += 1;
        self.sheets_moved();
        self.mark_full();
    }

    /// Put the linked children `first` to `last` of `parent` after its last child.
    fn append_chain(&mut self, parent: NodeId, first: NodeId, last: NodeId) {
        let at = self.nodes[parent.index()].last_child;
        self.nodes[first.index()].prev = at;
        match at {
            Some(a) => self.nodes[a.index()].next = Some(first),
            None => self.nodes[parent.index()].first_child = Some(first),
        }
        self.nodes[parent.index()].last_child = Some(last);
    }

    /// Move `node` (with its children) to be a child of `parent`, just before `before`, or at the end for `None`.
    /// `parent` must not be `node` or inside it. The tree builder never does that. JS: like `parent.insertBefore(node, before)`.
    pub fn move_to(&mut self, node: NodeId, parent: NodeId, before: Option<NodeId>) {
        self.detach(node);
        self.link(node, parent, before);
    }

    /// Move all the children of `from` to the end of `to`, in the same order. JS: `to.append(...from.childNodes)`.
    pub fn move_children(&mut self, from: NodeId, to: NodeId) {
        while let Some(child) = self.nodes[from.index()].first_child {
            self.move_to(child, to, None);
        }
    }

    /// A new element with the same name and attributes as `node`, with no parent and no children. JS: `node.cloneNode(false)`.
    /// The values of the attributes are shared with `node`, not copied.
    pub fn clone_element(&mut self, node: NodeId) -> NodeId {
        let NodeData::Element { name, attrs } = self.nodes[node.index()].data else { panic!("clone_element: not an element") };
        let start = self.attrs.len();
        if attrs.len != 0 {
            self.attrs.extend_from_within(attrs.start as usize..(attrs.start + attrs.len) as usize);
        }
        let id = NodeId::from_index(self.nodes.len());
        let data = NodeData::Element { name, attrs: AttrList { start: to_u32(start), len: attrs.len } };
        self.places = Default::default();
        self.nodes.push(Node { parent: None, first_child: None, last_child: None, prev: None, next: None, data });
        if let Some(&f) = self.foreign.get(&(node.index() as u32)) {
            self.foreign.insert(id.index() as u32, f);
        }
        id
    }

    /// Add an element, with its name and attributes as text. The names go in the name table, and
    /// the values in the text buffer. `before` works as in `insert`; `None` adds it at the end.
    pub fn insert_element<K: AsRef<str>, V: AsRef<str>>(&mut self, parent: NodeId, before: Option<NodeId>, name: &str, attrs: &[(K, V)]) -> NodeId {
        let name = self.intern(name);
        let start = self.attrs.len();
        for (key, value) in attrs {
            let attr = Attr { name: self.intern(key.as_ref()), value: self.push_text(value.as_ref()) };
            self.note_name(attr.name, value.as_ref());
            self.attrs.push(attr);
        }
        let attrs = AttrList { start: to_u32(start), len: to_u32(attrs.len()) };
        self.insert(parent, before, NodeData::Element { name, attrs })
    }

    /// A new element with no parent and no attributes. JS: `document.createElement(name)`. The caller
    /// lowercases and checks the name. Put it in the tree with `move_to`.
    pub fn create_element(&mut self, name: &str) -> NodeId {
        let name = self.intern(name);
        let attrs = AttrList { start: to_u32(self.attrs.len()), len: 0 };
        self.new_node(NodeData::Element { name, attrs })
    }

    /// A new element with no parent and no attributes in the SVG or MathML namespace. JS: `document.createElementNS(ns, prefix + ':' + local)`.
    /// `local` keeps its case. The caller checked the names and the namespace rules. With `Ns::Html` it is `create_element`,
    /// and `prefix` must be `None`.
    pub fn create_element_ns(&mut self, ns: Ns, prefix: Option<&str>, local: &str) -> NodeId {
        let id = self.create_element(local);
        self.set_foreign(id, ns, prefix);
        id
    }

    fn set_foreign(&mut self, id: NodeId, ns: Ns, prefix: Option<&str>) {
        debug_assert!(matches!(ns, Ns::Html | Ns::Svg | Ns::MathMl));
        if ns != Ns::Html {
            self.revision += 1;
            self.name_revision += 1;
            self.sheets_moved();
            let prefix = prefix.map(|p| self.intern(p));
            self.foreign.insert(id.index() as u32, Foreign { ns, prefix });
        }
    }

    /// Like `insert_element`, for an element in `ns` (the parser makes SVG and MathML this way). Each attribute has
    /// its namespace, if it has one; its name is the qualified name (`xlink:href`).
    pub fn insert_element_ns<K: AsRef<str>, V: AsRef<str>>(&mut self, parent: NodeId, before: Option<NodeId>, ns: Ns, name: &str, attrs: &[(Option<Ns>, K, V)]) -> NodeId {
        let name = self.intern(name);
        let start = self.attrs.len();
        for (attr_ns, key, value) in attrs {
            let attr = Attr { name: self.intern_ns(*attr_ns, key.as_ref()), value: self.push_text(value.as_ref()) };
            self.note_name(attr.name, value.as_ref());
            self.attrs.push(attr);
        }
        let attrs = AttrList { start: to_u32(start), len: to_u32(attrs.len()) };
        let id = self.insert(parent, before, NodeData::Element { name, attrs });
        self.set_foreign(id, ns, None);
        id
    }

    /// The namespace of an element. `Ns::Html` for an element made by the HTML rules, and for a node that is not an element.
    #[inline]
    pub fn element_ns(&self, el: NodeId) -> Ns {
        if self.foreign.is_empty() {
            return Ns::Html;
        }
        self.foreign.get(&(el.index() as u32)).map_or(Ns::Html, |f| f.ns)
    }

    /// Does the document have any SVG or MathML element? The tree builder asks, so that a page with none pays one compare.
    #[inline]
    pub fn has_foreign(&self) -> bool {
        !self.foreign.is_empty()
    }

    /// The name of an element (the local name, with its case), or an empty text for another node.
    pub fn element_name(&self, el: NodeId) -> &str {
        match self.nodes[el.index()].data {
            NodeData::Element { name, .. } => self.atom_str(name),
            _ => "",
        }
    }

    /// Is this node an element that is not in the HTML namespace?
    #[inline]
    pub fn is_foreign(&self, el: NodeId) -> bool {
        !self.foreign.is_empty() && self.foreign.contains_key(&(el.index() as u32))
    }

    /// Is this an HTML element with this (lowercase) name? An SVG `script` is not an HTML `script`.
    pub fn is_html(&self, el: NodeId, name: &str) -> bool {
        matches!(self.nodes[el.index()].data, NodeData::Element { name: n, .. } if self.atom_str(n) == name) && !self.is_foreign(el)
    }

    /// The prefix of an element: `svg` for `svg:rect`. `None` if there is none. The local name is `atom_str` of the name.
    pub fn element_prefix(&self, el: NodeId) -> Option<&str> {
        self.foreign.get(&(el.index() as u32)).and_then(|f| f.prefix).map(|p| self.atom_str(p))
    }

    /// The atom for an attribute name in a namespace. With `None` it is `intern`. A namespaced name is a different atom from
    /// the same name with no namespace.
    pub fn intern_ns(&mut self, ns: Option<Ns>, qname: &str) -> Atom {
        let Some(ns) = ns else { return self.intern(qname) };
        let atom = self.names.with(|n| {
            if let Some(&atom) = n.ns_atoms.get(&(ns, Box::from(qname))) {
                atom
            } else {
                let atom = n.add(qname);
                n.ns_atoms.insert((ns, qname.into()), atom);
                n.atom_ns.insert(atom, ns);
                atom
            }
        });
        self.any_ns = true;
        if self.use_atom(atom) {
            self.on_name |= is_on_name(qname);
        }
        Atom(atom)
    }

    /// The atom for an attribute name in a namespace, if the document has it. It does not add the name.
    pub fn find_ns_atom(&self, ns: Option<Ns>, qname: &str) -> Option<Atom> {
        match ns {
            None => self.find_atom(qname),
            Some(ns) => {
                let atom = self.names.with(|n| n.ns_atoms.get(&(ns, Box::from(qname))).copied())?;
                self.uses(atom).then_some(Atom(atom))
            }
        }
    }

    /// The namespace of an attribute. `None` for no namespace.
    pub fn attr_ns(&self, name: Atom) -> Option<Ns> {
        if !self.any_ns { None } else { self.names.with(|n| n.atom_ns.get(&name.0).copied()) }
    }

    /// The prefix of an attribute: `xlink` for `xlink:href`. Only a namespaced attribute has one. (A plain `setAttribute('a:b')`
    /// makes an attribute with no namespace, no prefix, and the local name `a:b`.)
    pub fn attr_prefix(&self, name: Atom) -> Option<&str> {
        self.attr_ns(name)?;
        self.atom_str(name).split_once(':').map(|(p, _)| p)
    }

    /// The local name of an attribute.
    pub fn attr_local(&self, name: Atom) -> &str {
        let q = self.atom_str(name);
        match self.attr_ns(name) {
            Some(_) => q.split_once(':').map_or(q, |(_, l)| l),
            None => q,
        }
    }

    /// The first attribute of `el` whose qualified name is `qname`, in any namespace. JS: `getAttribute(qname)`.
    pub fn find_attr_qname(&self, el: NodeId, qname: &str) -> Option<Atom> {
        let NodeData::Element { attrs, .. } = self.nodes[el.index()].data else { return None };
        if !self.any_ns {
            // No attribute has a namespace: a plain atom compare finds it.
            let atom = self.find_atom(qname)?;
            return self.attrs(attrs).iter().any(|a| a.name == atom).then_some(atom);
        }
        self.attrs(attrs).iter().map(|a| a.name).find(|&n| self.atom_str(n) == qname)
    }

    /// The attribute of `el` with this namespace and local name. JS: `getAttributeNS(ns, local)`.
    pub fn find_attr_ns(&self, el: NodeId, ns: Option<Ns>, local: &str) -> Option<Atom> {
        let NodeData::Element { attrs, .. } = self.nodes[el.index()].data else { return None };
        self.attrs(attrs).iter().map(|a| a.name).find(|&n| self.attr_ns(n) == ns && self.attr_local(n) == local)
    }

    /// Copy the node `n` of `src` into this document, with no parent and no children. An element keeps its name, its namespace, its prefix, and
    /// each attribute keeps its namespace; the names go in this name table and the values in this text buffer. The caller checked the caps
    /// first (`new_names_in`, `text_bytes`, `attr_slots`). Gives `None` for the top node and for a shadow root, which cannot be copied.
    pub fn import_node(&mut self, src: &Document, n: NodeId) -> Option<NodeId> {
        Some(match *src.data(n) {
            NodeData::Document | NodeData::ShadowRoot => return None,
            NodeData::Text(sp) => self.create_text(src.text(sp)),
            NodeData::Comment(sp) => self.create_comment(src.text(sp)),
            NodeData::Doctype(sp) => self.create_doctype(src.text(sp)),
            NodeData::Element { name, attrs } => {
                let name = self.intern(src.atom_str(name));
                let start = self.attrs.len();
                for a in src.attrs(attrs) {
                    let attr = Attr { name: self.intern_ns(src.attr_ns(a.name), src.atom_str(a.name)), value: self.push_text(src.text(a.value)) };
                    self.note_name(attr.name, src.text(a.value));
                    self.attrs.push(attr);
                }
                let list = AttrList { start: to_u32(start), len: attrs.len };
                let id = self.new_node(NodeData::Element { name, attrs: list });
                if src.is_foreign(n) {
                    self.set_foreign(id, src.element_ns(n), src.element_prefix(n));
                }
                id
            }
        })
    }

    /// How many names of this document (tags and attribute names) the table of `target` does not have yet. A host checks it against its name cap
    /// before `import_node`. It counts every name of this table, also one that no node of the tree uses any more, so it is an upper bound.
    pub fn new_names_in(&self, target: &Document) -> usize {
        self.used.iter().filter(|&&atom| !target.uses(atom)).count()
    }

    /// A new text node with no parent. It does not join a neighbour (only the parser joins texts).
    pub fn create_text(&mut self, text: &str) -> NodeId {
        let span = self.push_text(text);
        self.new_node(NodeData::Text(span))
    }

    /// A new comment with no parent.
    pub fn create_comment(&mut self, text: &str) -> NodeId {
        let span = self.push_text(text);
        self.new_node(NodeData::Comment(span))
    }

    /// A new doctype with no parent.
    pub fn create_doctype(&mut self, name: &str) -> NodeId {
        let span = self.push_text(name);
        self.new_node(NodeData::Doctype(span))
    }

    fn new_node(&mut self, data: NodeData) -> NodeId {
        self.places = Default::default();
        // The sibling-place table has one entry for each node, so a new node makes it too short.
        let id = NodeId::from_index(self.nodes.len());
        self.nodes.push(Node { parent: None, first_child: None, last_child: None, prev: None, next: None, data });
        id
    }

    /// Set an attribute of an element: change the value, or add it at the end of the list. The caller
    /// lowercases the name. Does nothing for a node that is not an element. The old value stays in the text
    /// buffer (see `text_bytes`).
    pub fn set_attr(&mut self, el: NodeId, name: &str, value: &str) {
        if matches!(self.nodes[el.index()].data, NodeData::Element { .. }) {
            let name = self.intern(name);
            self.set_attr_atom(el, name, value);
        }
    }

    /// Like `set_attr`, for a name that is an atom already (see `intern_ns`).
    pub fn set_attr_atom(&mut self, el: NodeId, name: Atom, value: &str) {
        let NodeData::Element { name: tag, attrs } = self.nodes[el.index()].data else { return };
        let span = self.push_text(value);
        self.revision += 1;
        self.name_revision += u64::from(self.names_attr(name));
        self.note_name(name, value);
        self.class_sets.get_mut().remove(&el.index());
        // An empty list can start after the end (see `attrs`): it begins again at the end.
        let (start, len) = if attrs.len == 0 { (self.attrs.len(), 0) } else { (attrs.start as usize, attrs.len as usize) };
        if let Some(a) = self.attrs[start..start + len].iter_mut().find(|a| a.name == name) {
            let old = std::mem::replace(&mut a.value, span);
            if self.watcher.is_some() {
                self.notify(Change::Attr { el, name, old: Some(old), new: Some(span) });
            }
            return;
        }
        if self.watcher.is_some() {
            self.notify(Change::Attr { el, name, old: None, new: Some(span) });
        }
        let new_start = if start + len == self.attrs.len() {
            start
        } else {
            // Another element's attributes follow: copy ours to the end. The old slots are wasted.
            self.attrs.extend_from_within(start..start + len);
            self.attrs.len() - len
        };
        self.attrs.push(Attr { name, value: span });
        self.nodes[el.index()].data = NodeData::Element { name: tag, attrs: AttrList { start: to_u32(new_start), len: to_u32(len + 1) } };
    }

    /// Remove an attribute. Gives whether it was there.
    pub fn remove_attr(&mut self, el: NodeId, name: &str) -> bool {
        let Some(name) = self.find_atom(name) else { return false };
        self.remove_attr_atom(el, name)
    }

    /// Like `remove_attr`, for a name that is an atom already (see `intern_ns`).
    pub fn remove_attr_atom(&mut self, el: NodeId, name: Atom) -> bool {
        let NodeData::Element { name: tag, attrs } = self.nodes[el.index()].data else { return false };
        let (start, len) = (attrs.start as usize, attrs.len as usize);
        if len == 0 {
            return false;
        }
        let Some(at) = self.attrs[start..start + len].iter().position(|a| a.name == name) else { return false };
        self.revision += 1;
        self.name_revision += u64::from(self.names_attr(name));
        self.class_sets.get_mut().remove(&el.index());
        let value = self.attrs[start + at].value;
        if self.watcher.is_some() {
            self.notify(Change::Attr { el, name, old: Some(value), new: None });
        }
        // A watched attribute is logged once: it leaves the watch set here, so the log cannot grow without a new `watch_attr`.
        if !self.watched.is_empty() && self.watched.remove(&(el.index() as u32, name.0)) {
            self.removed.push(RemovedAttr { el, name, value });
        }
        self.attrs.copy_within(start + at + 1..start + len, start + at);
        if start + len == self.attrs.len() {
            self.attrs.pop();
        }
        self.nodes[el.index()].data = NodeData::Element { name: tag, attrs: AttrList { start: attrs.start, len: attrs.len - 1 } };
        true
    }

    /// Tell `watcher` of every change of the tree, attributes and text from now on (`None` stops it). One watcher at a time. A page that
    /// sets none pays one check at a change. `Link` is sent after the node is in place, the other three before the change is made, so
    /// the old value is still readable. The watcher must not change this document.
    pub fn set_watcher(&mut self, watcher: Option<Watcher>) {
        let h = self.watcher.get_or_insert_default();
        h.f = watcher;
        if h.f.is_none() && h.damage.is_none() {
            self.watcher = None;
        }
    }

    /// Is a watcher set? (For tests of the host: a page that uses none must have none.)
    pub fn has_watcher(&self) -> bool {
        self.watcher.as_ref().is_some_and(|h| h.f.is_some())
    }

    /// Log the change (`track_damage`) and tell the watcher of it.
    fn notify(&mut self, change: Change) {
        let Some(mut h) = self.watcher.take() else { return };
        if let Some(d) = &mut h.damage {
            d.add(match change {
                Change::Link { parent, node, .. } => Mark::Link { parent, node },
                Change::Unlink { parent, node, .. } => Mark::Unlink { parent, node },
                Change::Attr { el, name, old, .. } => Mark::Attr { el, name, old },
                Change::Text { node, .. } => Mark::Text { node },
            });
        }
        if let Some(f) = &mut h.f {
            f(self, change);
        }
        self.watcher = Some(h);
    }

    /// Ask to be told when the attribute `name` of `el` is removed (by any path: `remove_attr`, `remove_attr_atom`). A host
    /// that keeps an object for one attribute calls this, so that the object can become detached with the old value.
    /// A removal is logged once. Call it again for the next object of an attribute that was added back.
    pub fn watch_attr(&mut self, el: NodeId, name: Atom) {
        self.watched.insert((el.index() as u32, name.0));
    }

    /// Stop watching. Does nothing for an attribute that is not watched.
    pub fn unwatch_attr(&mut self, el: NodeId, name: Atom) {
        self.watched.remove(&(el.index() as u32, name.0));
    }

    /// Is a removal waiting in the log? A cheap check for hot paths.
    pub fn has_removed_attrs(&self) -> bool {
        !self.removed.is_empty()
    }

    /// The removals of watched attributes since the last call, oldest first. The log is empty after it.
    pub fn take_removed_attrs(&mut self) -> Vec<RemovedAttr> {
        std::mem::take(&mut self.removed)
    }

    /// The value of an attribute as a span (`None` if the element has no such attribute). Unlike `attr`, it does not borrow the text.
    pub fn attr_span(&self, el: NodeId, name: Atom) -> Option<Span> {
        let NodeData::Element { attrs, .. } = self.nodes[el.index()].data else { return None };
        self.attrs(attrs).iter().find(|a| a.name == name).map(|a| a.value)
    }

    /// Put `text` in the text buffer and give its span, for a value that no element holds (a detached attribute).
    /// The caller checks its text cap first (see `text_bytes`).
    pub fn new_span(&mut self, text: &str) -> Span {
        self.push_text(text)
    }

    /// How many attributes the element has. 0 for a node that is not an element.
    pub fn attr_count(&self, el: NodeId) -> usize {
        match self.nodes[el.index()].data {
            NodeData::Element { attrs, .. } => attrs.len as usize,
            _ => 0,
        }
    }

    /// Change the text of a text node, a comment or a doctype. Does nothing for other nodes.
    /// The old bytes stay in the text buffer (see `text_bytes`).
    pub fn set_text(&mut self, node: NodeId, text: &str) {
        self.text_revision += 1;
        if self.watcher.is_some()
            && let NodeData::Text(old) | NodeData::Comment(old) | NodeData::Doctype(old) = self.nodes[node.index()].data
        {
            self.notify(Change::Text { node, old });
        }
        match self.nodes[node.index()].data {
            NodeData::Text(_) => self.nodes[node.index()].data = NodeData::Text(self.push_text(text)),
            NodeData::Comment(_) => self.nodes[node.index()].data = NodeData::Comment(self.push_text(text)),
            NodeData::Doctype(_) => self.nodes[node.index()].data = NodeData::Doctype(self.push_text(text)),
            _ => {}
        }
    }

    /// Bytes in the text buffer, with the bytes of old values. The buffer never shrinks, so a host that lets a
    /// script change text must check this before it grows.
    pub fn text_bytes(&self) -> usize {
        self.text.len()
    }

    /// Slots in the attribute list, with the slots that no element uses any more. Same reason as `text_bytes`.
    pub fn attr_slots(&self) -> usize {
        self.attrs.len()
    }

    /// Put a text at the end of the text buffer. Gives its `Span`.
    fn push_text(&mut self, text: &str) -> Span {
        let start = self.text.len();
        self.text.push_str(text);
        Span { start: to_u32(start), len: to_u32(text.len()) }
    }

    /// The child of `parent` in front of `before`, or the last child when `before` is `None`.
    fn child_before(&self, parent: NodeId, before: Option<NodeId>) -> Option<NodeId> {
        match before {
            Some(b) => self.nodes[b.index()].prev,
            None => self.nodes[parent.index()].last_child,
        }
    }

    /// Add a comment as the last child of `parent`.
    pub fn append_comment_to(&mut self, parent: NodeId, text: &str) {
        let span = self.push_text(text);
        self.insert(parent, None, NodeData::Comment(span));
    }

    /// Add a comment at the end of the top node. Comments before and after the `html` element go there.
    pub fn append_comment(&mut self, text: &str) {
        self.append_comment_to(self.root(), text);
    }

    /// Add the doctype at the end of the top node.
    pub fn append_doctype(&mut self, name: &str) {
        let span = self.push_text(name);
        self.insert(self.root(), None, NodeData::Doctype(span));
    }

    /// Add text as the last child of `parent`. If the last child is text already, add to it.
    /// The DOM never has two text nodes side by side: `"a"` then `"b"` is one node `"ab"`.
    pub fn append_text(&mut self, parent: NodeId, text: &str) {
        self.insert_text(parent, None, text);
    }

    /// Like `append_text`, but just before `before`. The text joins the child in front of that place,
    /// if it is text.
    pub fn insert_text(&mut self, parent: NodeId, before: Option<NodeId>, text: &str) {
        let before = before.filter(|&b| self.parent(b) == Some(parent));
        if let Some(previous) = self.child_before(parent, before)
            && let NodeData::Text(old) = self.nodes[previous.index()].data
        {
            // Join the two texts. If the old text is the last one in the buffer, the new text just follows
            // it. If not, we copy the old text to the end first. The old bytes are wasted.
            let start = if old.end() == self.text.len() {
                old.start as usize
            } else {
                let start = self.text.len();
                self.text.extend_from_within(old.start as usize..old.end());
                start
            };
            self.text.push_str(text);
            self.text_revision += 1;
            self.mark(Mark::Text { node: previous });
            let joined = Span { start: to_u32(start), len: to_u32(self.text.len() - start) };
            self.nodes[previous.index()].data = NodeData::Text(joined);
            return;
        }
        let span = self.push_text(text);
        self.insert(parent, before, NodeData::Text(span));
    }

    /// Give back the spare room of the lists. Call it when the tree is complete.
    /// A `Vec` doubles when it is full, so after 400,000 pushes it can hold 524,288 nodes.
    pub fn shrink_to_fit(&mut self) {
        self.nodes.shrink_to_fit();
        self.used.shrink_to_fit();
        self.attrs.shrink_to_fit();
        self.text.shrink_to_fit();
    }

    /// The tree as text, one line per node, in the format of the html5lib tests.
    /// Each level is indented 2 more spaces. Attributes come after their element, sorted by name.
    pub fn dump(&self) -> String {
        let mut out = String::new();
        for child in self.children(self.root()) {
            self.dump_node(child, 0, false, &mut out);
        }
        out
    }

    /// The same, and the contents of each HTML `template` show as a `content` line under it, as the html5lib tests write them.
    pub fn dump_with_contents(&self) -> String {
        let mut out = String::new();
        for child in self.children(self.root()) {
            self.dump_node(child, 0, true, &mut out);
        }
        out
    }

    fn dump_node(&self, id: NodeId, depth: usize, contents: bool, out: &mut String) {
        // `&mut String` means we borrow the string and may add to it. JS: pass an array and `push`.
        let indent = "  ".repeat(depth);
        match self.data(id) {
            NodeData::Document | NodeData::ShadowRoot => {}
            NodeData::Doctype(name) => out.push_str(&format!("| {indent}<!DOCTYPE {}>\n", self.text(*name))),
            NodeData::Comment(text) => out.push_str(&format!("| {indent}<!-- {} -->\n", self.text(*text))),
            NodeData::Text(text) => out.push_str(&format!("| {indent}\"{}\"\n", self.text(*text))),
            NodeData::Element { name, attrs } => {
                // The html5lib format: an SVG or MathML element is `<svg path>`, an attribute in a namespace is `xlink href`.
                match self.element_ns(id) {
                    Ns::Svg => out.push_str(&format!("| {indent}<svg {}>\n", self.atom_str(*name))),
                    Ns::MathMl => out.push_str(&format!("| {indent}<math {}>\n", self.atom_str(*name))),
                    _ => out.push_str(&format!("| {indent}<{}>\n", self.atom_str(*name))),
                }
                let mut sorted: Vec<_> = self
                    .attrs(*attrs)
                    .iter()
                    .map(|a| match self.attr_ns(a.name) {
                        None => (self.atom_str(a.name).to_string(), self.text(a.value)),
                        Some(ns) => (format!("{} {}", self.attr_prefix(a.name).unwrap_or(if ns == Ns::Xmlns { "xmlns" } else { "" }), self.attr_local(a.name)), self.text(a.value)),
                    })
                    .collect();
                sorted.sort();
                // Sorted, so that two trees with the same attributes in another order look the same.
                for (key, value) in sorted {
                    out.push_str(&format!("| {indent}  {key}=\"{value}\"\n"));
                }
            }
        }
        if contents && matches!(self.data(id), NodeData::Element { name, .. } if self.atom_str(*name) == "template") && self.element_ns(id) == Ns::Html {
            out.push_str(&format!("| {indent}  content\n"));
            for child in self.template_content(id).into_iter().flat_map(|c| self.children(c)) {
                self.dump_node(child, depth + 2, contents, out);
            }
        }
        for child in self.children(id) {
            self.dump_node(child, depth + 1, contents, out);
            // A function that calls itself. It walks down the tree: one call for each level.
        }
    }
}

/// A position in the lists of a `Document`, as a `u32`.
fn to_u32(n: usize) -> u32 {
    u32::try_from(n).expect("more than 4 GB of text, or 4 billion attributes")
}

impl Default for Document {
    fn default() -> Document {
        Document::new()
    }
}

#[cfg(test)]
// Only compiled for `cargo test`.
mod tests {
    use super::*;

    /// Add an element at the end of `parent`.
    fn element(doc: &mut Document, parent: NodeId, name: &str, attrs: &[(&str, &str)]) -> NodeId {
        let attrs: Vec<_> = attrs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
        doc.insert_element(parent, None, name, &attrs)
    }

    fn kids(doc: &Document, id: NodeId) -> Vec<NodeId> {
        doc.children(id).collect()
    }

    /// The text of a text node.
    fn text_of(doc: &Document, id: NodeId) -> &str {
        let NodeData::Text(span) = doc.data(id) else { panic!("not text") };
        doc.text(*span)
    }

    /// `sheet_nodes` checks itself against a walk at each read (in tests). This makes random trees with `link`, `style`, `base`, `template`, SVG,
    /// shadow roots and fragments, changes them at random, and reads at random, so a change that the list misses fails.
    #[test]
    fn the_sheet_list_follows_random_changes() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = move |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as usize
        };
        for _ in 0..40 {
            let mut doc = Document::new();
            let mut boxes = vec![doc.root(), doc.create_fragment()];
            let mut all: Vec<NodeId> = Vec::new();
            for _ in 0..150 {
                match next(10) {
                    0..=2 => {
                        let name = ["link", "style", "base", "div", "template", "p"][next(6)];
                        let el = doc.create_element(name);
                        all.push(el);
                        boxes.push(el);
                    }
                    3 => {
                        let name = ["style", "link", "template", "g"][next(4)];
                        let el = doc.create_element_ns(if next(2) == 0 { Ns::Svg } else { Ns::MathMl }, None, name);
                        all.push(el);
                        boxes.push(el);
                    }
                    4..=6 if !all.is_empty() => {
                        let (node, parent) = (all[next(all.len())], boxes[next(boxes.len())]);
                        let mut up = Some(parent);
                        while up.is_some_and(|u| u != node) {
                            up = doc.parent_or_host(up.unwrap());
                        }
                        if up.is_none() {
                            let before = doc.children(parent).nth(next(4));
                            doc.move_to(node, parent, before);
                        }
                    }
                    7 if !all.is_empty() => doc.detach(all[next(all.len())]),
                    8 if !all.is_empty() => {
                        let host = all[next(all.len())];
                        if matches!(doc.data(host), NodeData::Element { .. }) && doc.shadow_root(host).is_none() {
                            boxes.push(doc.attach_shadow(host, true));
                        }
                    }
                    _ => {
                        if let Some(&n) = all.get(next(all.len().max(1))) {
                            let copy = doc.clone_element(n);
                            all.push(copy);
                            boxes.push(copy);
                        }
                    }
                }
                if next(3) == 0 {
                    doc.sheet_nodes();
                }
            }
            doc.sheet_nodes();
        }
    }

    #[test]
    fn the_sheet_list_is_in_tree_order_and_marks_what_the_loader_may_use() {
        let mut doc = crate::tree::parse("<head><base href=/x/><link rel=stylesheet href=a.css><template><link href=t.css></template></head><body><svg><style>a{}</style></svg><div><style>b{}</style></div></body>");
        // The contents of a parsed template are not in the tree. A `link` that a host puts below a `template` element is, but a loader must not take it.
        let template = doc.find_atom("template").and_then(|t| (0..doc.len()).map(NodeId::from_index).find(|&n| matches!(doc.data(n), NodeData::Element { name, .. } if *name == t))).unwrap();
        doc.insert_element(template, None, "link", &[("href", "u.css")]);
        let kinds: Vec<_> = doc.sheet_nodes().iter().map(|s| (doc.element_name(s.node).to_string(), s.light, s.html)).collect();
        let want = |n: &str, h| (n.to_string(), true, h);
        assert_eq!(kinds, [want("base", true), want("link", true), want("link", false), want("style", false), want("style", true)]);
    }

    #[test]
    fn two_readers_of_the_damage_log_see_every_mark() {
        let mut doc = crate::tree::parse("<p id=a class=x>t</p>");
        let p = doc.children(doc.children(doc.root()).last().unwrap()).last().and_then(|b| doc.children(b).next()).unwrap();
        doc.track_damage(DAMAGE_FRAME);
        doc.set_attr(p, "class", "y");
        doc.track_damage(DAMAGE_STYLE);
        doc.set_attr(p, "class", "z");
        // The old value comes with the mark.
        let old = |doc: &Document, m: &Mark| match *m {
            Mark::Attr { old, .. } => old.map(|s| doc.text(s).to_string()),
            _ => None,
        };
        let frame = doc.take_damage(DAMAGE_FRAME).unwrap();
        assert_eq!(frame.iter().map(|m| old(&doc, m)).collect::<Vec<_>>(), [Some("x".into()), Some("y".into())]);
        let style = doc.take_damage(DAMAGE_STYLE).unwrap();
        assert_eq!(style.iter().map(|m| old(&doc, m)).collect::<Vec<_>>(), [Some("y".into())]);
        assert_eq!(doc.take_damage(DAMAGE_FRAME).unwrap().len(), 0);
        // A reader that does not read loses its place when the log is full. The other one keeps every mark.
        for i in 0..MAX_MARKS + 10 {
            doc.set_attr(p, "data-n", &i.to_string());
            if i % 100 == 0 {
                assert_eq!(doc.take_damage(DAMAGE_FRAME).map(|m| m.len()), Some(if i == 0 { 1 } else { 100 }));
            }
        }
        assert!(doc.take_damage(DAMAGE_STYLE).is_none());
        assert_eq!(doc.take_damage(DAMAGE_FRAME).map(|m| m.len()), Some(MAX_MARKS + 10 - 4101));
    }

    #[test]
    fn detached_nodes_do_not_break_the_sibling_cache() {
        let mut doc = Document::new();
        let root = doc.root();
        let a = element(&mut doc, root, "a", &[]);
        assert_eq!(doc.sibling_place(a, false, false), 1); // the cache is built now
        let n = doc.create_element("p");
        assert_eq!(doc.sibling_place(n, false, false), 0); // a detached node is in no sibling list; it must not crash
        doc.move_to(n, root, None);
        assert_eq!(doc.sibling_place(n, false, false), 2);
    }

    #[test]
    fn an_empty_attribute_list_survives_the_removal_of_the_last_attribute_slot() {
        // `c` is made while `a` holds the last attribute slot. Removing it shortens the list below `c`'s start.
        let mut doc = Document::new();
        let a = doc.create_element("a");
        doc.set_attr(a, "x", "1");
        let c = doc.create_element("c");
        assert!(doc.remove_attr(a, "x"));
        assert_eq!(doc.attr_count(c), 0);
        assert_eq!(doc.attr(c, Atom::ID), None);
        assert!(!doc.remove_attr(c, "id"));
        doc.set_attr(c, "id", "k");
        assert_eq!(doc.attr(c, Atom::ID), Some("k"));
        assert_eq!(doc.attr_count(a), 0);
        // `clone_element` of an empty list that starts after the end
        let d = doc.create_element("d");
        doc.set_attr(a, "x", "1");
        let e = doc.create_element("e");
        doc.remove_attr(a, "x");
        let copy = doc.clone_element(e);
        assert_eq!((doc.attr_count(copy), doc.attr_count(d)), (0, 0));
    }

    #[test]
    fn a_watched_attribute_is_logged_once_with_its_value_and_by_every_removal_path() {
        let mut doc = Document::new();
        let a = doc.create_element("a");
        doc.set_attr(a, "x", "1");
        doc.set_attr(a, "y", "2");
        let (x, y) = (doc.find_atom("x").unwrap(), doc.find_atom("y").unwrap());
        doc.watch_attr(a, x);
        doc.watch_attr(a, y);
        assert!(!doc.has_removed_attrs());
        assert!(doc.remove_attr(a, "x"));
        doc.set_attr(a, "x", "again");
        assert!(doc.remove_attr(a, "x")); // not watched any more: not logged again
        assert!(doc.remove_attr_atom(a, y));
        let log = doc.take_removed_attrs();
        assert_eq!(log.iter().map(|r| (doc.atom_str(r.name), doc.text(r.value))).collect::<Vec<_>>(), [("x", "1"), ("y", "2")]);
        assert!(!doc.has_removed_attrs());
        // an unwatched attribute and a replaced value leave no log
        doc.set_attr(a, "z", "3");
        doc.set_attr(a, "z", "4");
        doc.remove_attr(a, "z");
        assert!(!doc.has_removed_attrs());
        // `unwatch_attr` stops the log
        doc.set_attr(a, "w", "5");
        let w = doc.find_atom("w").unwrap();
        doc.watch_attr(a, w);
        doc.unwatch_attr(a, w);
        doc.remove_attr(a, "w");
        assert!(!doc.has_removed_attrs());
        assert_eq!(doc.attr_span(a, w), None);
    }

    #[test]
    fn set_and_remove_attributes_keep_the_others() {
        let mut doc = Document::new();
        let root = doc.root();
        let a = element(&mut doc, root, "a", &[("x", "1"), ("y", "2")]);
        let b = element(&mut doc, root, "b", &[("z", "3")]); // `a` is not at the end of the list now
        doc.set_attr(a, "w", "4");
        doc.set_attr(a, "x", "10");
        let list = |doc: &Document, el| match doc.data(el) {
            NodeData::Element { attrs, .. } => doc.attrs(*attrs).iter().map(|t| format!("{}={}", doc.atom_str(t.name), doc.text(t.value))).collect::<Vec<_>>().join(","),
            _ => panic!(),
        };
        assert_eq!(list(&doc, a), "x=10,y=2,w=4");
        assert_eq!(list(&doc, b), "z=3");
        assert!(doc.remove_attr(a, "y"));
        assert!(!doc.remove_attr(a, "y"));
        assert_eq!(list(&doc, a), "x=10,w=4");
        doc.set_attr(b, "q", "5");
        assert_eq!(list(&doc, b), "z=3,q=5");
        assert_eq!(list(&doc, a), "x=10,w=4");
        assert_eq!(doc.attr_count(a), 2);
    }

    #[test]
    fn a_namespaced_attribute_is_not_the_plain_attribute_of_the_same_name() {
        let mut doc = Document::new();
        let root = doc.root();
        let el = doc.create_element_ns(Ns::Svg, Some("s"), "use");
        doc.move_to(el, root, None);
        doc.set_attr(el, "xlink:href", "plain");
        let plain = doc.find_atom("xlink:href").unwrap();
        let ns = doc.intern_ns(Some(Ns::Xlink), "xlink:href");
        assert_ne!(ns, plain);
        assert_eq!(doc.find_ns_atom(Some(Ns::Xlink), "xlink:href"), Some(ns));
        assert_eq!(doc.find_ns_atom(Some(Ns::Xml), "xlink:href"), None);
        doc.set_attr_atom(el, ns, "ns");
        assert_eq!(doc.attr_count(el), 2);
        // by qualified name the first one wins; by namespace and local name each is found
        assert_eq!(doc.find_attr_qname(el, "xlink:href"), Some(plain));
        assert_eq!(doc.find_attr_ns(el, Some(Ns::Xlink), "href"), Some(ns));
        assert_eq!(doc.find_attr_ns(el, None, "xlink:href"), Some(plain));
        assert_eq!(doc.find_attr_ns(el, None, "href"), None);
        assert_eq!((doc.attr_ns(ns), doc.attr_prefix(ns), doc.attr_local(ns)), (Some(Ns::Xlink), Some("xlink"), "href"));
        assert_eq!((doc.attr(el, ns), doc.attr(el, plain)), (Some("ns"), Some("plain")));
        assert_eq!((doc.attr_ns(plain), doc.attr_prefix(plain), doc.attr_local(plain)), (None, None, "xlink:href"));
        assert_eq!((doc.element_ns(el), doc.element_prefix(el), doc.element_name(el)), (Ns::Svg, Some("s"), "use"));
        // a clone keeps the element's identity and the attribute identities
        let c = doc.clone_element(el);
        assert_eq!((doc.element_ns(c), doc.element_prefix(c)), (Ns::Svg, Some("s")));
        assert_eq!(doc.find_attr_ns(c, Some(Ns::Xlink), "href"), Some(ns));
        assert!(doc.remove_attr_atom(el, ns));
        assert_eq!(doc.attr_count(el), 1);
        assert_eq!(doc.find_attr_ns(c, Some(Ns::Xlink), "href"), Some(ns));
        assert!(!doc.remove_attr_atom(el, ns));
    }

    #[test]
    fn changing_a_long_class_attribute_drops_the_cached_set() {
        let mut doc = Document::new();
        let root = doc.root();
        let long = format!("{} target", "filler ".repeat(30));
        let a = element(&mut doc, root, "a", &[("class", &long)]);
        assert!(doc.has_class(a, "target")); // builds the cached set
        doc.set_attr(a, "class", &"other ".repeat(30));
        assert!(!doc.has_class(a, "target"));
        assert!(doc.has_class(a, "other"));
        doc.remove_attr(a, "class");
        assert!(!doc.has_class(a, "other"));
    }

    #[test]
    fn text_nodes_can_be_made_detached_and_changed() {
        let mut doc = Document::new();
        let root = doc.root();
        let p = element(&mut doc, root, "p", &[]);
        let t1 = doc.create_text("ab");
        let t2 = doc.create_text("cd");
        doc.move_to(t1, p, None);
        doc.move_to(t2, p, None);
        assert_eq!(kids(&doc, p), [t1, t2]); // two text nodes stay two (only the parser merges)
        doc.set_text(t1, "xyz");
        assert_eq!(text_of(&doc, t1), "xyz");
        assert_eq!(text_of(&doc, t2), "cd");
        let c = doc.create_comment("hi");
        doc.set_text(c, "yo");
        assert!(matches!(doc.data(c), NodeData::Comment(s) if doc.text(*s) == "yo"));
        assert!(doc.text_bytes() >= 9);
    }

    #[test]
    fn nodes_are_small() {
        // A big page has hundreds of thousands of nodes. Each byte here is multiplied by that.
        assert_eq!(std::mem::size_of::<NodeId>(), 4);
        assert_eq!(std::mem::size_of::<Option<NodeId>>(), 4);
        assert!(std::mem::size_of::<NodeData>() <= 16, "node data is {} bytes", std::mem::size_of::<NodeData>());
        assert!(std::mem::size_of::<Node>() <= 36, "a node is {} bytes", std::mem::size_of::<Node>());
    }

    #[test]
    fn a_new_document_has_only_the_root() {
        let doc = Document::new();
        assert_eq!(doc.data(doc.root()), &NodeData::Document);
        assert_eq!(doc.parent(doc.root()), None);
        assert_eq!(doc.children(doc.root()).count(), 0);
        assert_eq!(doc.dump(), "");
    }

    #[test]
    fn links_parents_and_children() {
        let mut doc = Document::new();
        let root = doc.root();
        let html = element(&mut doc, root, "html", &[]);
        let head = element(&mut doc, html, "head", &[]);
        let body = element(&mut doc, html, "body", &[]);
        assert_eq!(kids(&doc, html), [head, body]);
        // Children keep the order in which we added them.
        assert_eq!(doc.parent(head), Some(html));
        assert_eq!(doc.parent(html), Some(doc.root()));
        assert_eq!(doc.parent(body), Some(html));
    }

    #[test]
    fn names_are_stored_once() {
        let mut doc = Document::new();
        let root = doc.root();
        let a = element(&mut doc, root, "div", &[("class", "x")]);
        let b = element(&mut doc, root, "div", &[("class", "y")]);
        let (&NodeData::Element { name: na, attrs: la }, &NodeData::Element { name: nb, attrs: lb }) = (doc.data(a), doc.data(b)) else {
            panic!("not elements");
        };
        let (aa, ab) = (doc.attrs(la), doc.attrs(lb));
        assert_eq!(na, nb);
        assert_eq!(aa[0].name, ab[0].name);
        assert_eq!(doc.atom_str(na), "div");
        assert_eq!(doc.atom_str(aa[0].name), "class");
        assert_eq!((doc.text(aa[0].value), doc.text(ab[0].value)), ("x", "y"));
        assert_eq!(doc.name_count(), 3);
        // "id" and "class" from the start, and "div". Not 4, although we made two elements.
        assert_eq!(doc.atom_str(Atom::ID), "id");
        assert_eq!(aa[0].name, Atom::CLASS);
        assert_eq!(doc.attr(a, Atom::CLASS), Some("x"));
        assert_eq!(doc.attr(a, Atom::ID), None);
    }

    #[test]
    fn short_names_get_the_same_atom_as_the_table_gives() {
        // Many names of 0 to 9 bytes, so that slots collide and the names that share bytes at the ends differ.
        let mut doc = Document::new();
        let mut names = vec![String::new()];
        for len in 1..=9usize {
            for k in 0..60u8 {
                let mut s: Vec<u8> = (0..len).map(|i| b'a' + (i as u8 * 7 + k) % 26).collect();
                s[len / 2] = b'0' + k % 10;
                names.push(String::from_utf8(s).unwrap());
            }
        }
        names.extend(["abcdefgh", "abcdXfgh", "abcdefgX", "Xbcdefgh", "abcXefgh"].map(String::from));
        let first: Vec<_> = names.iter().map(|n| doc.intern(n)).collect();
        for _ in 0..3 {
            for (n, &a) in names.iter().zip(&first) {
                assert_eq!(doc.intern(n), a, "{n}");
                assert_eq!(doc.atom_str(a), n);
            }
        }
        let mut sorted = first.clone();
        sorted.sort_by_key(|a| doc.atom_str(*a).to_string());
        sorted.dedup();
        assert_eq!(sorted.len(), names.iter().collect::<std::collections::HashSet<_>>().len(), "one atom for each different name");
    }

    #[test]
    fn two_documents_share_atoms_and_keep_their_own_names() {
        let mut a = Document::new();
        let mut b = Document::new();
        // The same name is the same atom in both documents, whatever each document added before.
        let first = a.intern("x-first-only-in-a");
        let (a_div, b_div) = (a.intern("x-shared"), b.intern("x-shared"));
        assert_eq!(a_div, b_div);
        assert_eq!(a.atom_str(a_div), "x-shared");
        // A document finds only the names that it uses, and counts only those.
        assert_eq!(a.find_atom("x-first-only-in-a"), Some(first));
        assert_eq!(b.find_atom("x-first-only-in-a"), None);
        assert_eq!((a.name_count(), b.name_count()), (4, 3));
        assert_eq!(a.atom_at(3), a_div);
        assert_eq!(b.atom_at(2), b_div);
        // Names that the other document used before are not new for it.
        assert_eq!(a.new_names_in(&b), 1);
        assert_eq!(b.new_names_in(&a), 0);
        // A namespaced name is another atom than the same name with no namespace, in each document.
        let plain = a.intern("xlink:href");
        let ns = a.intern_ns(Some(Ns::Xlink), "xlink:href");
        assert_ne!(plain, ns);
        assert_eq!(b.find_ns_atom(Some(Ns::Xlink), "xlink:href"), None);
        assert_eq!(b.intern_ns(Some(Ns::Xlink), "xlink:href"), ns);
        assert_eq!(b.attr_ns(ns), Some(Ns::Xlink));
        assert_eq!(b.attr_ns(plain), None);
    }

    #[test]
    fn joins_text_next_to_text() {
        let mut doc = Document::new();
        let root = doc.root();
        let p = element(&mut doc, root, "p", &[]);
        doc.append_text(p, "a");
        doc.append_text(p, "b");
        assert_eq!(doc.children(p).count(), 1);
        assert_eq!(text_of(&doc, doc.children(p).next().unwrap()), "ab");
        doc.append_comment_to(p, "c");
        doc.append_text(p, "d");
        assert_eq!(doc.children(p).count(), 3);
        // A comment sits between "ab" and "d", so "d" is a new text node.
    }

    #[test]
    fn dumps_a_tree() {
        let mut doc = Document::new();
        let root = doc.root();
        doc.append_doctype("html");
        let html = element(&mut doc, root, "html", &[]);
        let body = element(&mut doc, html, "body", &[("id", "x"), ("class", "a b")]);
        doc.append_text(body, "Hi");
        doc.append_comment_to(body, " c ");
        let expected = "\
| <!DOCTYPE html>
| <html>
|   <body>
|     class=\"a b\"
|     id=\"x\"
|     \"Hi\"
|     <!--  c  -->
";
        assert_eq!(doc.dump(), expected);
        // The attributes are sorted: "class" comes before "id", although we added "id" first.
    }

    #[test]
    fn inserts_before_a_node() {
        let mut doc = Document::new();
        let root = doc.root();
        let body = element(&mut doc, root, "body", &[]);
        let table = element(&mut doc, body, "table", &[]);
        let div = doc.insert_element(body, Some(table), "div", &[] as &[(&str, &str)]);
        assert_eq!(kids(&doc, body), [div, table]);
        // The new node is before the table, not after it.
        assert_eq!(doc.parent(div), Some(body));
        doc.insert_text(body, Some(table), "a");
        doc.insert_text(body, Some(table), "b");
        let list = kids(&doc, body);
        assert_eq!(list.len(), 3);
        assert_eq!(text_of(&doc, list[1]), "ab");
        // The two pieces of text joined, and sit between the div and the table.
        assert_eq!((list[0], list[2]), (div, table));
    }

    #[test]
    fn joins_text_that_is_not_the_last_in_the_buffer() {
        // "ab" and "cd" are two texts. Joining "x" to "ab" must copy it, because "cd" comes after it in the buffer.
        let mut doc = Document::new();
        let root = doc.root();
        let p = element(&mut doc, root, "p", &[]);
        let q = element(&mut doc, root, "q", &[]);
        doc.append_text(p, "ab");
        doc.append_text(q, "cd");
        doc.append_text(p, "x");
        assert_eq!(text_of(&doc, doc.children(p).next().unwrap()), "abx");
        assert_eq!(text_of(&doc, doc.children(q).next().unwrap()), "cd");
        // The other text did not change.
    }

    #[test]
    fn reads_attributes_comments_and_doctype() {
        let mut doc = Document::new();
        let root = doc.root();
        doc.append_doctype("html");
        let a = element(&mut doc, root, "a", &[("href", "/x"), ("id", ""), ("title", "é€")]);
        doc.append_comment_to(a, "hi");
        let NodeData::Element { attrs, .. } = *doc.data(a) else { panic!("not an element") };
        let list: Vec<_> = doc.attrs(attrs).iter().map(|x| (doc.atom_str(x.name), doc.text(x.value))).collect();
        assert_eq!(list, [("href", "/x"), ("id", ""), ("title", "é€")]);
        let kinds: Vec<_> = doc.children(root).map(|c| doc.data(c)).collect();
        assert!(matches!(kinds[0], NodeData::Doctype(span) if doc.text(*span) == "html"));
        assert!(matches!(doc.data(doc.children(a).next().unwrap()), NodeData::Comment(span) if doc.text(*span) == "hi"));
    }

    #[test]
    fn inserts_before_the_first_child() {
        let mut doc = Document::new();
        let root = doc.root();
        let a = element(&mut doc, root, "a", &[]);
        let b = doc.insert_element(root, Some(a), "b", &[] as &[(&str, &str)]);
        let c = doc.insert_element(root, Some(a), "c", &[] as &[(&str, &str)]);
        assert_eq!(kids(&doc, root), [b, c, a]);
        // `b` was the first child, and `c` goes after `b` and before `a`.
        assert_eq!(doc.children(root).count(), 3);
    }

    #[test]
    fn a_before_that_is_not_a_child_adds_at_the_end() {
        let mut doc = Document::new();
        let root = doc.root();
        let a = element(&mut doc, root, "a", &[]);
        let inner = element(&mut doc, a, "i", &[]);
        let b = doc.insert_element(root, Some(inner), "b", &[] as &[(&str, &str)]);
        // `inner` is a child of `a`, not of the root. We do not link into the wrong list.
        assert_eq!(kids(&doc, root), [a, b]);
        assert_eq!(kids(&doc, a), [inner]);
    }

    /// The names of the children of a node, as text: `["a", "b"]`.
    fn names(doc: &Document, id: NodeId) -> Vec<String> {
        doc.children(id)
            .map(|c| match doc.data(c) {
                NodeData::Element { name, .. } => doc.atom_str(*name).to_string(),
                _ => "#".to_string(),
            })
            .collect()
    }

    /// Check that every link of the tree agrees: the parent of each child, and both directions of the sibling links.
    fn check(doc: &Document, id: NodeId) {
        let list = kids(doc, id);
        assert_eq!(doc.last_child(id), list.last().copied());
        for (i, &c) in list.iter().enumerate() {
            assert_eq!(doc.parent(c), Some(id));
            assert_eq!(doc.prev_sibling(c), i.checked_sub(1).map(|j| list[j]));
            assert_eq!(doc.next_sibling(c), list.get(i + 1).copied());
            check(doc, c);
        }
    }

    #[test]
    fn move_and_detach() {
        let mut doc = Document::new();
        let root = doc.root();
        let (a, b, c) = (element(&mut doc, root, "a", &[]), element(&mut doc, root, "b", &[]), element(&mut doc, root, "c", &[]));
        let inner = element(&mut doc, a, "inner", &[]);
        // Move to the end, to the front, to the middle, and into another node.
        doc.move_to(a, root, None);
        assert_eq!(names(&doc, root), ["b", "c", "a"]);
        doc.move_to(a, root, Some(b));
        assert_eq!(names(&doc, root), ["a", "b", "c"]);
        doc.move_to(c, root, Some(b));
        assert_eq!(names(&doc, root), ["a", "c", "b"]);
        doc.move_to(b, a, None);
        assert_eq!(names(&doc, root), ["a", "c"]);
        assert_eq!(names(&doc, a), ["inner", "b"]);
        // A `before` that is not a child of the parent, or is the node itself, puts it at the end.
        doc.move_to(c, a, Some(root));
        assert_eq!(names(&doc, a), ["inner", "b", "c"]);
        doc.move_to(c, a, Some(c));
        assert_eq!(names(&doc, a), ["inner", "b", "c"]);
        check(&doc, root);
        // `detach` takes a node and its children out. Doing it twice, or for a node with no parent, is fine.
        doc.detach(a);
        doc.detach(a);
        assert_eq!(names(&doc, root), Vec::<String>::new());
        assert_eq!(doc.parent(a), None);
        assert_eq!(names(&doc, a), ["inner", "b", "c"]);
        let _ = inner;
        check(&doc, a);
    }

    #[test]
    fn move_children_keeps_the_order() {
        let mut doc = Document::new();
        let root = doc.root();
        let (from, to) = (element(&mut doc, root, "from", &[]), element(&mut doc, root, "to", &[]));
        for name in ["x", "y", "z"] {
            element(&mut doc, from, name, &[]);
        }
        element(&mut doc, to, "first", &[]);
        doc.append_text(from, "t");
        doc.move_children(from, to);
        assert_eq!(names(&doc, from), Vec::<String>::new());
        assert_eq!(names(&doc, to), ["first", "x", "y", "z", "#"]);
        check(&doc, root);
        // Nothing to move is fine.
        doc.move_children(from, to);
        check(&doc, root);
    }

    #[test]
    fn clone_has_the_attributes_and_no_children() {
        let mut doc = Document::new();
        let root = doc.root();
        let a = element(&mut doc, root, "a", &[("href", "x"), ("class", "y z")]);
        element(&mut doc, a, "child", &[]);
        let copy = doc.clone_element(a);
        assert_eq!(doc.parent(copy), None);
        assert_eq!(names(&doc, copy), Vec::<String>::new());
        let (NodeData::Element { name: n1, attrs: a1 }, NodeData::Element { name: n2, attrs: a2 }) = (*doc.data(a), *doc.data(copy)) else { panic!() };
        assert_eq!(n1, n2);
        let read = |list| doc.attrs(list).iter().map(|x| (doc.atom_str(x.name).to_string(), doc.text(x.value).to_string())).collect::<Vec<_>>();
        assert_eq!(read(a1), read(a2));
        assert_eq!(read(a2), [("href".to_string(), "x".to_string()), ("class".to_string(), "y z".to_string())]);
        // The copy can be put in the tree.
        doc.move_to(copy, root, None);
        assert_eq!(names(&doc, root), ["a", "a"]);
        check(&doc, root);
    }

    #[test]
    fn sibling_places_follow_the_tree() {
        let mut doc = crate::tree::parse("<ul><i></i><li></li><i></i><li></li></ul>");
        let (html, ul) = (doc.children(doc.root()).last().unwrap(), |doc: &Document, n| doc.children(n).last().unwrap());
        let ul = ul(&doc, ul(&doc, html));
        let kids: Vec<NodeId> = doc.children(ul).collect();
        let places = |doc: &Document, of_type, from_end| doc.children(ul).map(|c| doc.sibling_place(c, of_type, from_end)).collect::<Vec<_>>();
        assert_eq!(places(&doc, false, false), [1, 2, 3, 4]);
        assert_eq!(places(&doc, false, true), [4, 3, 2, 1]);
        assert_eq!(places(&doc, true, false), [1, 1, 2, 2]);
        assert_eq!(places(&doc, true, true), [2, 2, 1, 1]);
        // A change in the tree must drop the places that we made.
        doc.detach(kids[1]);
        assert_eq!(places(&doc, false, false), [1, 2, 3]);
        assert_eq!(places(&doc, true, true), [2, 1, 1]);
    }

    #[test]
    fn has_class_reads_short_and_long_lists() {
        let long: String = (0..100).map(|i| format!("c{i}")).collect::<Vec<_>>().join("  ");
        let doc = crate::tree::parse(&format!("<i class='a  b\tc'></i><i class='{long}'></i><i></i>"));
        let html = doc.children(doc.root()).last().unwrap();
        let body = doc.children(html).last().unwrap();
        let (short, big, none): (Vec<NodeId>, Vec<NodeId>, Vec<NodeId>) = {
            let kids: Vec<NodeId> = doc.children(body).collect();
            (vec![kids[0]], vec![kids[1]], vec![kids[2]])
        };
        assert!(doc.has_class(short[0], "b") && doc.has_class(short[0], "c") && !doc.has_class(short[0], "d"));
        // The long list is a set. Ask twice: the second time it comes from the set.
        for _ in 0..2 {
            assert!(doc.has_class(big[0], "c0") && doc.has_class(big[0], "c99"));
            assert!(!doc.has_class(big[0], "c100") && !doc.has_class(big[0], "c"));
        }
        assert!(!doc.has_class(none[0], "a"));
    }

    /// Every public change that can alter which nodes are in the tree, or what their attributes or namespaces are, moves
    /// `revision`. A host cache of a tree query (`getElementsByName`) is valid only while it is the same.
    #[test]
    fn revision_moves_on_every_tree_attribute_and_namespace_change() {
        let mut doc = Document::new();
        let root = doc.root();
        let r0 = doc.revision();
        let mut seen = vec![r0];
        let mut moved = |doc: &Document, what: &str| {
            let r = doc.revision();
            assert!(r != *seen.last().unwrap(), "{what} did not change the revision");
            seen.push(r);
        };
        let a = doc.insert_element(root, None, "div", &[("name", "x")]);
        moved(&doc, "insert_element");
        let t = doc.create_element("p");
        let c = doc.create_text("t");
        doc.move_to(t, a, None);
        moved(&doc, "move_to (link)");
        doc.move_to(t, root, Some(a));
        moved(&doc, "move_to (reparent)");
        doc.detach(t);
        moved(&doc, "detach");
        doc.move_children(root, a);
        moved(&doc, "move_children");
        let name = doc.intern("name");
        doc.set_attr_atom(a, name, "y");
        moved(&doc, "set_attr_atom (change)");
        doc.set_attr(a, "other", "1");
        moved(&doc, "set_attr (add)");
        assert!(doc.remove_attr(a, "other"));
        moved(&doc, "remove_attr");
        assert!(doc.remove_attr_atom(a, name));
        moved(&doc, "remove_attr_atom");
        let svg = doc.insert_element_ns::<&str, &str>(a, None, Ns::Svg, "g", &[]);
        moved(&doc, "insert_element_ns");
        doc.append_text(a, "more");
        moved(&doc, "append_text (new node)");
        doc.append_comment_to(a, "c");
        moved(&doc, "append_comment_to");
        doc.append_comment("c2");
        moved(&doc, "append_comment");
        doc.append_doctype("html");
        moved(&doc, "append_doctype");
        let ns = doc.create_element_ns(Ns::Svg, None, "rect");
        moved(&doc, "create_element_ns (set_foreign)");
        let _ = (svg, c, ns);
    }

    /// Changes that cannot alter any tree query leave the revision alone, so a cache is not rebuilt for nothing.
    #[test]
    fn revision_stays_for_changes_outside_the_tree() {
        let mut doc = Document::new();
        let root = doc.root();
        let a = doc.insert_element(root, None, "div", &[("name", "x")]);
        let r = doc.revision();
        let n = doc.create_element("i");
        let c = doc.clone_element(a);
        let t = doc.create_text("t");
        let _ = doc.create_comment("c");
        let _ = doc.create_doctype("d");
        doc.set_text(t, "u");
        let _ = doc.intern("other");
        let _ = doc.new_span("s");
        doc.detach(n);
        assert!(!doc.remove_attr(a, "absent"));
        let _ = c;
        assert_eq!(doc.revision(), r);
    }

    #[test]
    fn may_have_name_never_misses_a_name_that_was_set() {
        let mut doc = Document::new();
        let root = doc.root();
        let has = |doc: &Document, v: &str| doc.may_have_name(v.chars());
        assert!(!has(&doc, "a"));
        let el = doc.insert_element(root, None, "div", &[("id", "a"), ("class", "c"), ("name", "")]);
        doc.insert_element_ns(root, None, Ns::Svg, "rect", &[(None, "id", "ns")]);
        assert!(has(&doc, "a") && has(&doc, "ns"));
        // A class is no name, and an empty value is none.
        assert!(!has(&doc, "c") && !has(&doc, ""));
        doc.set_attr(el, "name", "b\u{e9}\u{1f600}");
        assert!(has(&doc, "b\u{e9}\u{1f600}"));
        // The set only grows: a removed or changed name stays "maybe".
        doc.set_attr(el, "id", "z");
        doc.remove_attr(el, "name");
        assert!(has(&doc, "a") && has(&doc, "b\u{e9}\u{1f600}") && has(&doc, "z"));
        // A node copied from another document brings its names.
        let mut other = Document::new();
        let r = other.root();
        let src = other.insert_element(r, None, "p", &[("id", "copied")]);
        assert!(!has(&doc, "copied"));
        doc.import_node(&other, src).unwrap();
        assert!(has(&doc, "copied"));
    }

    #[test]
    fn name_revision_moves_only_for_named_changes() {
        let mut doc = crate::tree::parse("<!doctype html><body><p></p></body>");
        let body = doc.children(doc.children(doc.root()).last().unwrap()).last().unwrap();
        let p = doc.children(body).next().unwrap();
        let r = doc.name_revision();
        // A bare element or text joins and leaves, a class changes: no move.
        let i = doc.create_element("i");
        doc.move_to(i, body, None);
        doc.detach(i);
        doc.set_attr(p, "class", "x");
        doc.remove_attr(p, "class");
        assert_eq!(doc.name_revision(), r);
        doc.set_attr(p, "id", "a");
        let r1 = doc.name_revision();
        assert!(r1 > r);
        doc.set_attr(i, "name", "n");
        let r2 = doc.name_revision();
        assert!(r2 > r1);
        // A node with attributes or with children joins or leaves: a move.
        doc.move_to(i, body, None);
        assert!(doc.name_revision() > r2);
        let r3 = doc.name_revision();
        let (b, c) = (doc.create_element("b"), doc.create_element("u"));
        doc.move_to(c, b, None);
        doc.move_to(b, body, None);
        assert!(doc.name_revision() > r3);
        let r4 = doc.name_revision();
        doc.remove_attr(p, "id");
        assert!(doc.name_revision() > r4);
    }

    #[test]
    fn revision_follows_the_parser_and_import() {
        let doc = crate::tree::parse("<!doctype html><body><i name=a></i></body>");
        assert!(doc.revision() > 0);
        let mut other = Document::new();
        let r = other.revision();
        let i = doc.children(doc.children(doc.root()).last().unwrap()).last().unwrap();
        let body = doc.children(doc.children(doc.root()).last().unwrap()).last().unwrap();
        let imported = other.import_node(&doc, body).unwrap();
        let _ = (i, imported);
        // an imported node has no parent: not a tree change; linking it is.
        assert_eq!(other.revision(), r);
        let root = other.root();
        other.move_to(imported, root, None);
        assert!(other.revision() != r);
    }
}

/// `Document::ui`: the checkedness is set by script or the user, so it no longer follows the `checked` attribute.
pub const UI_CHECKED_DIRTY: u8 = 1;
/// `Document::ui`: the checkedness, when `UI_CHECKED_DIRTY` is set.
pub const UI_CHECKED: u8 = 2;
/// `Document::ui`: the value is set by script or the user, so it no longer follows the `value` attribute or the text of a `textarea`.
pub const UI_VALUE_DIRTY: u8 = 4;
/// `Document::ui`: the value is not empty, when `UI_VALUE_DIRTY` is set.
pub const UI_VALUE_SET: u8 = 8;
/// `Document::ui`: the `indeterminate` property.
pub const UI_INDETERMINATE: u8 = 16;

/// The custom element state "precustomized": a constructor is running for the element.
pub const PRECUSTOMIZED: u32 = u32::MAX - 1;
/// The custom element state "failed".
pub const FAILED: u32 = u32::MAX;

/// `PCENChar` of the HTML Standard (valid custom element name), after the first character.
fn pcen(c: char) -> bool {
    matches!(c, '-' | '.' | '0'..='9' | '_' | 'a'..='z' | '\u{b7}' | '\u{c0}'..='\u{d6}' | '\u{d8}'..='\u{f6}' | '\u{f8}'..='\u{37d}' | '\u{37f}'..='\u{1fff}' | '\u{200c}'..='\u{200d}' | '\u{203f}'..='\u{2040}' | '\u{2070}'..='\u{218f}' | '\u{2c00}'..='\u{2fef}' | '\u{3001}'..='\u{d7ff}' | '\u{f900}'..='\u{fdcf}' | '\u{fdf0}'..='\u{fffd}' | '\u{10000}'..='\u{effff}')
}

/// HTML Standard "valid custom element name".
pub fn valid_custom_name(name: &str) -> bool {
    const RESERVED: [&str; 8] = ["annotation-xml", "color-profile", "font-face", "font-face-src", "font-face-uri", "font-face-format", "font-face-name", "missing-glyph"];
    name.starts_with(|c: char| c.is_ascii_lowercase()) && name.contains('-') && name.chars().all(pcen) && !RESERVED.contains(&name)
}

#[cfg(test)]
mod pixels_tests {
    use super::Pixels;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn lazy_pixels_are_made_once_at_the_first_read() {
        static MADE: AtomicUsize = AtomicUsize::new(0);
        let px = Pixels::lazy(|| {
            MADE.fetch_add(1, Ordering::Relaxed);
            vec![1, 2, 3]
        });
        let clone = px.clone();
        assert!(!px.is_ready() && MADE.load(Ordering::Relaxed) == 0, "nothing is made before a read");
        assert_eq!((&*px, clone.addr() == px.addr(), clone.is_ready()), (&[1, 2, 3][..], true, true));
        assert_eq!(clone.shared()[..], [1, 2, 3]);
        assert_eq!(MADE.load(Ordering::Relaxed), 1);
        assert!(Pixels::from(vec![7]).is_ready());
    }
}
