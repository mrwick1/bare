//! Custom elements: `window.customElements` (`CustomElementRegistry`), the constructors of the HTML element interfaces for `class X extends
//! HTMLElement`, upgrades and the four lifecycle callbacks. The model is the HTML Standard "custom elements" section, checked against Chrome 155
//! (`tools/fixtures/ui/custom-cases.txt`, `custom-page-cases.txt`).
//!
//! State. One definition per `define` call (`CeSt::defs`). The state of an element is in the native document (`Document::set_custom_state`): a
//! definition number is "custom", `PRECUSTOMIZED` is "precustomized" (a constructor runs), `FAILED` is "failed". An element with no entry is
//! "undefined" (or "uncustomized" when its name is not a valid custom element name). `:defined` reads the same table.
//!
//! Reactions. A tree change tells the native watcher (`Dom::sync_watcher`), which cannot run script. The watcher puts a reaction on the
//! queue of the element (`CeSt::queues`) and the element on the top element queue (`CeSt::stack`). `Host::call` opens one stack entry around
//! each host call once the first `define` ran (`Dom::call_ce`), and `ce_pop` runs the reactions of that entry when the call ends, as the
//! `[CEReactions]` of the standard do. A page that never calls `define` pays one `Cell` read per host call. When the stack is empty (a change
//! that does not come from a host call), the element goes on the backup queue and a microtask runs it (`flush`).
//!
//! Hooks. `createElement` and `createElementNS` (`create`), `cloneNode` (`clone_upgrades`), the fragment parser of `innerHTML`,
//! `outerHTML` and `insertAdjacentHTML` (`fragment_upgrades`), `wrap` (the prototype of a custom element after its wrapper was collected)
//! and `Host::call`.
//!
//! The constructor. Each HTML interface that bare has is a constructor that reads `new.target` (`html_ctor::<K>`). The standard algorithm:
//! no definition for `new.target` is an illegal constructor; a definition with an empty construction stack makes a new element; with one
//! entry it takes that element (an upgrade) and marks the entry as constructed.
//!
//! Parse time. The page is parsed before any script runs, so a defined element that the parser reaches is made by `parse_construct`: the tree is cut
//! (`Document::parse_cut`) so the constructor sees what the parser has, the element with no attributes, no children and no parent, in a document that
//! stops before it. Then the attributes and the element go back, the reactions run, and the children and the later nodes come back.
//!
//! `HTMLUnknownElement` is the interface of an element that has no known name and is not a custom name (`is_unknown`), and of an element whose
//! constructor failed (`CeSt::unk`). The contents of a `template` belong to an inert `Document` node (`Ce.tpl`); `adoptedCallback` runs when an element
//! enters or leaves it, and for `adoptNode`. Form-associated elements are in `browser_face.rs`.
//!
//! Deviations from Chrome (each is a decision, not a gap that was not seen):
//! - A script that holds a wrapper of a node that the parse cut hides can reach it, and a hidden node that a constructor detaches is not put back.
//! - Only the interfaces that bare has can be extended: `HTMLButtonElement` and the like. `extends: 'div'` defines, but `new` of it is an
//!   illegal constructor (bare has no `HTMLDivElement`).
//! - The `is` value of an element made by `createElement(name, { is })` is kept (`CeSt::is`). Any other element reads its `is` attribute
//!   when it needs one, so `setAttribute('is', ...)` after creation upgrades at the next insertion (Chrome never does).
//! - `connectedMoveCallback` is read and checked and never called (no `moveBefore`).
//! - No `new CustomElementRegistry()`, `initialize` or scoped registries.
//! - The `outerHTML` of an element made with `is` does not show the `is` value.

use crate::browser::*;
use crate::browser_dom::{arg, make_element, next_composed};
use bare_html::dom::{Change, Document, FAILED, NodeData, NodeId, Ns, PRECUSTOMIZED, valid_custom_name};
use bare_js::eval::{Error, Interp};
use bare_js::value::{ObjId, Value};
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

type V = R<Value>;

/// Most reactions that wait at one time, and most definitions. A page that goes past one gets a limit error, not a slow loop.
const MAX_PENDING: usize = 1 << 20;
const MAX_DEFS: usize = 1 << 16;

/// The place of each callback in `Def::cb`.
const CONNECTED: usize = 0;
const DISCONNECTED: usize = 1;
const ADOPTED: usize = 2;
const ATTRIBUTE: usize = 3;
const FORM_ASSOCIATED: usize = 4;
const FORM_RESET: usize = 5;
const FORM_DISABLED: usize = 6;

/// The interfaces that have a constructor for custom elements, in the order of `html_ctor::<K>`.
const IFACES: [&str; 15] = [
    "HTMLElement", "HTMLAnchorElement", "HTMLAreaElement", "HTMLButtonElement", "HTMLFormElement", "HTMLIFrameElement", "HTMLInputElement", "HTMLLinkElement",
    "HTMLOptionElement", "HTMLOptGroupElement", "HTMLScriptElement", "HTMLSelectElement", "HTMLTemplateElement", "HTMLTextAreaElement", "HTMLLabelElement",
];

pub(crate) const HANDLERS: [H; 15] = [
    html_ctor::<0>, html_ctor::<1>, html_ctor::<2>, html_ctor::<3>, html_ctor::<4>, html_ctor::<5>, html_ctor::<6>, html_ctor::<7>,
    html_ctor::<8>, html_ctor::<9>, html_ctor::<10>, html_ctor::<11>, html_ctor::<12>, html_ctor::<13>, html_ctor::<14>,
];

/// The HTML elements whose interface is `HTMLElement` itself (Chrome 155).
const PLAIN: &str = "abbr acronym address article aside b basefont bdi bdo big center cite code dd dfn dt em figcaption figure footer header hgroup i kbd main mark nav nobr noembed noframes noscript plaintext rb rp rt rtc ruby s samp search section small strike strong sub summary sup tt u var wbr";


/// Every HTML element name that has an interface other than `HTMLUnknownElement` (Chrome 155), sorted. `define` with `extends` checks it, and so does the
/// prototype of a new wrapper (`is_unknown`).
/// `KNOWN` and `is_known`, from one list. The `match` is a switch on the length and the bytes: a wrapper of a plain element pays no search.
macro_rules! known {
    ($($s:literal),* $(,)?) => {
        const KNOWN: [&str; [$($s),*].len()] = [$($s),*];
        #[inline]
        fn is_known(n: &str) -> bool {
            matches!(n, $($s)|*)
        }
    };
}

known! {
    "a", "abbr", "acronym", "address", "area", "article", "aside", "audio", "b", "base", "basefont", "bdi", "bdo", "big", "blockquote", "body", "br",
    "button", "canvas", "caption", "center", "cite", "code", "col", "colgroup", "data", "datalist", "dd", "del", "details", "dfn", "dialog", "dir",
    "div", "dl", "dt", "em", "embed", "fieldset", "figcaption", "figure", "font", "footer", "form", "frame", "frameset", "h1", "h2", "h3", "h4", "h5",
    "h6", "head", "header", "hgroup", "hr", "html", "i", "iframe", "img", "input", "ins", "kbd", "label", "legend", "li", "link", "listing", "main",
    "map", "mark", "marquee", "menu", "meta", "meter", "nav", "nobr", "noembed", "noframes", "noscript", "object", "ol", "optgroup", "option",
    "output", "p", "param", "picture", "plaintext", "pre", "progress", "q", "rb", "rp", "rt", "rtc", "ruby", "s", "samp", "script", "search",
    "section", "select", "selectedcontent", "slot", "small", "source", "span", "strike", "strong", "style", "sub", "summary", "sup", "table", "tbody",
    "td", "template", "textarea", "tfoot", "th", "thead", "time", "title", "tr", "track", "tt", "u", "ul", "var", "video", "wbr", "xmp"
}

/// Is `n`, an HTML element name, an `HTMLUnknownElement`? Not when it has an interface of its own, nor when it is a valid custom element name.
pub(crate) fn is_unknown(n: &str) -> bool {
    !is_known(n) && !(n.contains('-') && valid_custom_name(n))
}

/// The index in `IFACES` of the interface of the HTML element `local`, or `usize::MAX` when bare has none for it.
fn iface_of(local: &str) -> usize {
    match local {
        "a" => 1,
        "area" => 2,
        "button" => 3,
        "form" => 4,
        "iframe" => 5,
        "input" => 6,
        "link" => 7,
        "option" => 8,
        "optgroup" => 9,
        "script" => 10,
        "select" => 11,
        "template" => 12,
        "textarea" => 13,
        "label" => 14,
        n if PLAIN.split(' ').any(|p| p == n) => 0,
        _ => usize::MAX,
    }
}

/// One definition (`customElements.define`).
struct Def {
    name: Box<str>,
    /// The local name of a customized built-in element (`extends`).
    ext: Option<Box<str>>,
    ctor: ObjId,
    proto: ObjId,
    /// `connectedCallback`, `disconnectedCallback`, `adoptedCallback`, `attributeChangedCallback`, then (for a form-associated definition)
    /// `formAssociatedCallback`, `formResetCallback`, `formDisabledCallback` and `formStateRestoreCallback`; `Undefined` when absent.
    cb: [Value; 8],
    obs: Box<[Box<str>]>,
    /// `static formAssociated`, and whether `static disabledFeatures` has `internals`.
    form: bool,
    no_internals: bool,
    /// The construction stack: the element an upgrade runs for, or `None` once the constructor took it.
    stack: Vec<Option<NodeId>>,
}

enum Reaction {
    Upgrade(u32),
    Connected,
    Disconnected,
    /// The element changed document: `true` for the inert document of the template contents, `false` for the document.
    Adopted(bool),
    Attr(Box<AttrArgs>),
    /// A form-associated element: find its form owner and `disabled` state now, and call the callbacks for what changed (`form_check`).
    FormCheck,
    /// `formAssociatedCallback` with the owner that the element has after it left a tree (`Face::owner` is already that).
    FormAssoc(Option<NodeId>),
    FormReset,
}

struct AttrArgs {
    name: Box<str>,
    old: Option<Box<str>>,
    new: Option<Box<str>>,
    ns: Option<&'static str>,
}

#[derive(Default)]
pub(crate) struct CeSt {
    defs: Vec<Def>,
    by_name: HashMap<Box<str>, u32>,
    by_ctor: HashMap<ObjId, u32>,
    /// Some definition has `extends`: an element can need its `is` value.
    has_ext: bool,
    /// The "element definition is running" flag of the registry.
    defining: bool,
    /// The custom element reactions stack: one element queue for each host call that runs.
    stack: Vec<Vec<NodeId>>,
    /// The reaction queue of each element, by `NodeId::index`.
    queues: HashMap<u32, VecDeque<Reaction>>,
    /// The backup element queue, and whether a microtask must be queued for it (`want`) or is queued (`queued`).
    backup: Vec<NodeId>,
    want: bool,
    queued: bool,
    pending: usize,
    overflow: bool,
    /// Elements after the script that was running at `define`: the parser has not reached them yet. See `Dom::ce_release`.
    held: Vec<NodeId>,
    /// The `is` value of the elements made by `createElement(name, { is })`.
    is: HashMap<u32, Box<str>>,
    /// The elements whose constructor failed when the parser or `createElement` made them: they are `HTMLUnknownElement`.
    unk: std::collections::HashSet<u32>,
    /// The custom elements that a script moved into the contents of a template: their owner document is the inert one, also while they are out.
    inert: std::collections::HashSet<u32>,
    /// The state of the form-associated elements by `NodeId::index`, and the element of each `ElementInternals` and `CustomStateSet` (`browser_face.rs`).
    pub(crate) face: HashMap<u32, crate::browser_face::Face>,
    pub(crate) ints: HashMap<ObjId, (NodeId, bool)>,
    /// Some definition is form-associated: a tree change can move a form owner.
    has_face: bool,
    /// The promise of `whenDefined` for each name that is not defined yet.
    when: HashMap<Box<str>, Value>,
    flush: Option<ObjId>,
    /// The prototype, the constructor and the one registry object, once made.
    reg: Option<[ObjId; 3]>,
}

/// The state of custom elements of one realm. `on` is true after the first `define`.
#[derive(Default)]
pub(crate) struct Ce {
    pub(crate) on: Cell<bool>,
    /// The parser-blocking script that runs now while the page loads (see `release`).
    pub(crate) parsing: Cell<Option<NodeId>>,
    /// The prototype and the constructor of `HTMLUnknownElement`, once made.
    unk: OnceCell<[ObjId; 2]>,
    /// The prototypes and constructors of `ElementInternals` and `CustomStateSet`, once made.
    pub(crate) int: OnceCell<[ObjId; 4]>,
    /// The inert document that owns the contents of the templates (a `Document` node outside the tree), once needed.
    tpl: Cell<Option<NodeId>>,
    st: OnceCell<Rc<RefCell<CeSt>>>,
}

impl Ce {
    /// The state for the watcher of the document, once a definition was made.
    pub(crate) fn watching(&self) -> Option<Rc<RefCell<CeSt>>> {
        self.on.get().then(|| self.st.get().cloned()).flatten()
    }
}

pub(crate) fn st(d: &Dom) -> &Rc<RefCell<CeSt>> {
    d.ce.st.get_or_init(Default::default)
}

/// Is `n` in the document, also through shadow hosts?
fn connected(doc: &Document, n: NodeId) -> bool {
    let mut cur = n;
    while let Some(p) = doc.parent_or_host(cur) {
        cur = p;
    }
    cur == doc.root()
}

/// The elements of the shadow-including tree of `root` (`root` too), in tree order.
fn each_element(doc: &Document, root: NodeId, mut f: impl FnMut(NodeId)) {
    let mut cur = root;
    let mut depth = 0usize;
    if matches!(doc.data(root), NodeData::Element { .. }) {
        f(root);
    }
    while let Some((c, cd)) = next_composed(doc, root, cur, depth) {
        (cur, depth) = (c, cd);
        if matches!(doc.data(c), NodeData::Element { .. }) {
            f(c);
        }
    }
}

impl CeSt {
    fn enqueue(&mut self, el: NodeId, r: Reaction) {
        if self.pending >= MAX_PENDING {
            self.overflow = true;
            return;
        }
        self.pending += 1;
        self.queues.entry(el.index() as u32).or_default().push_back(r);
        let q = match self.stack.last_mut() {
            Some(q) => q,
            None => {
                self.want = true;
                &mut self.backup
            }
        };
        if q.last() != Some(&el) {
            q.push(el);
        }
    }

    /// The definition for an element made with local name `local` and `is` value `is`: "look up a custom element definition".
    fn def_for(&self, local: &str, is: Option<&str>) -> Option<u32> {
        if valid_custom_name(local)
            && let Some(&i) = self.by_name.get(local)
            && self.defs[i as usize].ext.is_none()
        {
            return Some(i);
        }
        let &i = self.by_name.get(is?)?;
        (self.defs[i as usize].ext.as_deref() == Some(local)).then_some(i)
    }

    /// The `is` value of `el`: the one `createElement` kept, else its `is` attribute.
    fn is_of<'a>(&'a self, doc: &'a Document, el: NodeId) -> Option<&'a str> {
        self.is.get(&(el.index() as u32)).map(|s| &**s).or_else(|| doc.attr(el, doc.find_atom("is")?))
    }

    fn def_of(&self, doc: &Document, el: NodeId) -> Option<u32> {
        if doc.element_ns(el) != Ns::Html {
            return None;
        }
        let is = if self.has_ext { self.is_of(doc, el) } else { None };
        self.def_for(doc.element_name(el), is)
    }

    /// The watcher of the document: put the reactions of a change on the queues. It cannot run script.
    pub(crate) fn on_change(&mut self, doc: &Document, ch: Change) {
        match ch {
            Change::Link { parent, node, .. } => {
                let conn = connected(doc, parent);
                // The document that owns `node` after the change: the inert one for the contents of a template.
                let inert = doc.in_template_content(parent);
                if !conn && !inert && self.inert.is_empty() && !self.has_face {
                    return;
                }
                let mut els = Vec::new();
                each_element(doc, node, |e| els.push(e));
                let forms = self.has_face && els.iter().any(|&e| doc.is_html(e, "form"));
                for e in els {
                    match doc.custom_state(e) {
                        Some(s) if s < PRECUSTOMIZED => {
                            if self.defs[s as usize].form {
                                self.enqueue(e, Reaction::FormCheck);
                            }
                            // "Adopt": the owner document changes when the element enters or leaves the contents of a template.
                            if inert != self.inert.contains(&(e.index() as u32)) {
                                if inert { self.inert.insert(e.index() as u32) } else { self.inert.remove(&(e.index() as u32)) };
                                if !matches!(self.defs[s as usize].cb[ADOPTED], Value::Undefined) {
                                    self.enqueue(e, Reaction::Adopted(inert));
                                }
                            }
                            if conn && !matches!(self.defs[s as usize].cb[CONNECTED], Value::Undefined) {
                                self.enqueue(e, Reaction::Connected);
                            }
                        }
                        Some(_) => {}
                        None if conn => {
                            if let Some(i) = self.def_of(doc, e) {
                                self.enqueue(e, Reaction::Upgrade(i));
                            }
                        }
                        None => {}
                    }
                }
                if forms {
                    self.check_explicit(doc);
                }
            }
            Change::Unlink { parent, node, .. } => {
                let conn = connected(doc, parent);
                if !doc.has_custom() || (!conn && !self.has_face) {
                    return;
                }
                let mut els = Vec::new();
                each_element(doc, node, |e| els.push(e));
                let forms = self.has_face && els.iter().any(|&e| doc.is_html(e, "form"));
                for e in els {
                    let Some(s) = doc.custom_state(e).filter(|&s| s < PRECUSTOMIZED) else { continue };
                    if conn && !matches!(self.defs[s as usize].cb[DISCONNECTED], Value::Undefined) {
                        self.enqueue(e, Reaction::Disconnected);
                    }
                    if self.defs[s as usize].form {
                        // The owner after the change is the nearest form inside the removed tree. The rest is read when the reaction runs.
                        let owner = crate::browser_face::owner_within(doc, e, node);
                        let f = self.face.entry(e.index() as u32).or_insert_with(|| crate::browser_face::Face::new(e));
                        if f.owner.is_some() && f.owner != owner {
                            f.owner = owner;
                            self.enqueue(e, Reaction::FormAssoc(owner));
                        }
                        self.enqueue(e, Reaction::FormCheck);
                    }
                }
                if forms {
                    self.check_explicit(doc);
                }
            }
            Change::Attr { el, name, old, new } => {
                if self.has_face {
                    self.attr_face(doc, el, doc.attr_local(name));
                }
                let Some(s) = doc.custom_state(el) else { return };
                if s >= PRECUSTOMIZED {
                    return;
                }
                let def = &self.defs[s as usize];
                let local = doc.attr_local(name);
                if matches!(def.cb[ATTRIBUTE], Value::Undefined) || !def.obs.iter().any(|o| &**o == local) {
                    return;
                }
                let args = AttrArgs { name: local.into(), old: old.map(|s| doc.text(s).into()), new: new.map(|s| doc.text(s).into()), ns: doc.attr_ns(name).map(Ns::uri) };
                self.enqueue(el, Reaction::Attr(Box::new(args)));
            }
            Change::Text { .. } => {}
        }
    }

    /// A form-associated element, `el`, needs a new check.
    fn check_face(&mut self, doc: &Document, e: NodeId) {
        if let Some(s) = doc.custom_state(e).filter(|&s| s < PRECUSTOMIZED)
            && self.defs[s as usize].form
            && !self.queues.get(&(e.index() as u32)).is_some_and(|q| q.iter().any(|r| matches!(r, Reaction::FormCheck)))
        {
            self.enqueue(e, Reaction::FormCheck);
        }
    }

    /// A form came into the tree or left it: the elements that name their form with `form=` can have another owner.
    fn check_explicit(&mut self, doc: &Document) {
        let Some(form) = doc.find_atom("form") else { return };
        let els: Vec<NodeId> = self.face.values().map(|f| f.el).filter(|&e| doc.attr(e, form).is_some()).collect();
        for e in els {
            self.check_face(doc, e);
        }
    }

    /// The attribute `local` of `el` changed: `form` and `disabled` of a form-associated element, `disabled` of a `fieldset` (its form-associated
    /// descendants), `id` of a `form` (the elements with a `form=` attribute).
    fn attr_face(&mut self, doc: &Document, el: NodeId, local: &str) {
        match local {
            "form" | "disabled" => {
                self.check_face(doc, el);
                if local == "disabled" && doc.is_html(el, "fieldset") {
                    let mut els = Vec::new();
                    each_element(doc, el, |e| els.push(e));
                    for e in els {
                        self.check_face(doc, e);
                    }
                }
            }
            "id" if doc.is_html(el, "form") => self.check_explicit(doc),
            _ => {}
        }
    }

    /// Put an upgrade reaction on `el` (once) and `el` in the top element queue at `at`.
    fn enqueue_upgrade_at(&mut self, at: usize, el: NodeId, def: u32) {
        let q = self.queues.entry(el.index() as u32).or_default();
        if !q.iter().any(|r| matches!(r, Reaction::Upgrade(_))) {
            q.push_back(Reaction::Upgrade(def));
            self.pending += 1;
        }
        let list = match self.stack.last_mut() {
            Some(l) => l,
            None => {
                self.want = true;
                &mut self.backup
            }
        };
        list.insert(at.min(list.len()), el);
    }
}

impl Dom {
    /// `Host::call` with a scope of "custom element reactions" around the host function.
    pub(crate) fn call_ce(&self, it: &mut Interp<'_>, h: H, this: Value, args: &[Value]) -> V {
        st(self).borrow_mut().stack.push(Vec::new());
        let r = h(self, it, this, args);
        let q = self.ce_pop(it);
        match (r, q) {
            (Ok(v), Ok(())) => Ok(v),
            (Err(e), _) | (Ok(_), Err(e)) => Err(e),
        }
    }

    /// End a scope: run the reactions of its element queue, then queue the microtask of the backup queue when it is needed.
    fn ce_pop(&self, it: &mut Interp<'_>) -> R<()> {
        let els = st(self).borrow_mut().stack.pop().unwrap_or_default();
        if !els.is_empty() {
            self.invoke(it, els)?;
        }
        let f = {
            let mut s = st(self).borrow_mut();
            if s.overflow {
                return Err(self.lim("custom-reactions"));
            }
            if !s.want || s.queued {
                return Ok(());
            }
            (s.want, s.queued) = (false, true);
            s.flush
        };
        let f = match f {
            Some(f) => f,
            None => {
                let f = self.add_fn(it, flush, "flush", 0, false)?;
                st(self).borrow_mut().flush = Some(f);
                f
            }
        };
        it.queue_microtask(Value::Object(f))
    }

    /// "Invoke custom element reactions": for each element, in order, run its reactions until its queue is empty.
    fn invoke(&self, it: &mut Interp<'_>, els: Vec<NodeId>) -> R<()> {
        for e in els {
            loop {
                let r = {
                    let mut s = st(self).borrow_mut();
                    let key = e.index() as u32;
                    let Some(q) = s.queues.get_mut(&key) else { break };
                    let r = q.pop_front();
                    if q.is_empty() {
                        s.queues.remove(&key);
                    }
                    let Some(r) = r else { break };
                    s.pending -= 1;
                    r
                };
                it.charge(1)?;
                self.react(it, e, r)?;
            }
        }
        Ok(())
    }

    fn react(&self, it: &mut Interp<'_>, el: NodeId, r: Reaction) -> R<()> {
        let (slot, args) = match r {
            Reaction::Upgrade(i) => return self.upgrade(it, el, i),
            Reaction::Connected => (CONNECTED, None),
            Reaction::Disconnected => (DISCONNECTED, None),
            Reaction::Adopted(to) => return self.react_adopted(it, el, to),
            Reaction::Attr(a) => (ATTRIBUTE, Some(a)),
            Reaction::FormCheck => return self.form_check(it, el),
            Reaction::FormAssoc(o) => return self.form_assoc(it, el, o),
            Reaction::FormReset => (FORM_RESET, None),
        };
        let Some(s) = self.doc.borrow().custom_state(el).filter(|&s| s < PRECUSTOMIZED) else { return Ok(()) };
        let f = st(self).borrow().defs[s as usize].cb[slot];
        let mut argv = Vec::new();
        if let Some(a) = args {
            let opt = |it: &mut Interp<'_>, v: &Option<Box<str>>| match v {
                Some(s) => self.js_str(it, s),
                None => Ok(Value::Null),
            };
            argv.push(self.js_str(it, &a.name)?);
            argv.push(opt(it, &a.old)?);
            argv.push(opt(it, &a.new)?);
            argv.push(match a.ns {
                Some(u) => self.js_str(it, u)?,
                None => Value::Null,
            });
        }
        let this = self.wrap(it, el)?;
        self.guarded(it, el, |it| it.call(f, this, &argv).map(|_| ()))
    }

    /// Is `n` a form-associated custom element (also while its constructor runs)?
    #[inline]
    pub(crate) fn is_face(&self, n: NodeId) -> bool {
        self.ce.on.get() && self.is_face_slow(n)
    }

    fn is_face_slow(&self, n: NodeId) -> bool {
        let (doc, s) = (self.doc.borrow(), st(self).borrow());
        match doc.custom_state(n) {
            Some(i) if i < PRECUSTOMIZED => s.defs[i as usize].form,
            Some(PRECUSTOMIZED) => s.def_of(&doc, n).is_some_and(|i| s.defs[i as usize].form),
            _ => false,
        }
    }

    /// Does the element `n` have a definition (it is custom, or a constructor runs for it)? Gives whether `disabledFeatures` has `internals`.
    pub(crate) fn ce_def(&self, n: NodeId) -> Option<bool> {
        if !self.ce.on.get() {
            return None;
        }
        let (doc, s) = (self.doc.borrow(), st(self).borrow());
        let i = match doc.custom_state(n) {
            Some(i) if i < PRECUSTOMIZED => i,
            Some(PRECUSTOMIZED) => s.def_of(&doc, n)?,
            _ => return None,
        };
        Some(s.defs[i as usize].no_internals)
    }

    /// Call the callback `slot` of the custom element `el` when its definition has one.
    fn form_cb(&self, it: &mut Interp<'_>, el: NodeId, slot: usize, args: &[Value]) -> R<()> {
        let Some(s) = self.doc.borrow().custom_state(el).filter(|&s| s < PRECUSTOMIZED) else { return Ok(()) };
        let f = st(self).borrow().defs[s as usize].cb[slot];
        if matches!(f, Value::Undefined) {
            return Ok(());
        }
        let this = self.wrap(it, el)?;
        self.guarded(it, el, |it| it.call(f, this, args).map(|_| ()))
    }

    fn form_assoc(&self, it: &mut Interp<'_>, el: NodeId, owner: Option<NodeId>) -> R<()> {
        let form = self.wrap_opt(it, owner)?;
        self.form_cb(it, el, FORM_ASSOCIATED, &[form])
    }

    /// `formAssociatedCallback` and `formDisabledCallback` for what changed since the last check of `el`.
    fn form_check(&self, it: &mut Interp<'_>, el: NodeId) -> R<()> {
        if !self.is_face(el) {
            return Ok(());
        }
        let (owner, disabled) = (self.form_owner(it, el)?, self.is_disabled_control(it, el)?);
        let (assoc, dis) = {
            let mut s = st(self).borrow_mut();
            let f = s.face.entry(el.index() as u32).or_insert_with(|| crate::browser_face::Face::new(el));
            (std::mem::replace(&mut f.owner, owner) != owner, std::mem::replace(&mut f.disabled, disabled) != disabled)
        };
        if assoc {
            self.form_assoc(it, el, owner)?;
        }
        if dis {
            self.form_cb(it, el, FORM_DISABLED, &[Value::Bool(disabled)])?;
        }
        Ok(())
    }

    /// `form.reset()` reaches the form-associated element `el`.
    pub(crate) fn ce_form_reset(&self, el: NodeId) {
        let mut s = st(self).borrow_mut();
        if let Some(i) = self.doc.borrow().custom_state(el).filter(|&i| i < PRECUSTOMIZED)
            && !matches!(s.defs[i as usize].cb[FORM_RESET], Value::Undefined)
        {
            s.enqueue(el, Reaction::FormReset);
        }
    }

    /// `adoptedCallback(oldDocument, newDocument)`.
    fn react_adopted(&self, it: &mut Interp<'_>, el: NodeId, to_inert: bool) -> R<()> {
        let Some(s) = self.doc.borrow().custom_state(el).filter(|&s| s < PRECUSTOMIZED) else { return Ok(()) };
        let f = st(self).borrow().defs[s as usize].cb[ADOPTED];
        let tpl = template_document(self);
        let main = self.doc.borrow().root();
        let (old, new) = if to_inert { (main, tpl) } else { (tpl, main) };
        let argv = [self.wrap(it, old)?, self.wrap(it, new)?];
        let this = self.wrap(it, el)?;
        self.guarded(it, el, |it| it.call(f, this, &argv).map(|_| ()))
    }

    /// Run `f`. An exception is reported (the page goes on, as a listener that throws); any other error ends the run.
    pub(crate) fn guarded(&self, it: &mut Interp<'_>, el: NodeId, f: impl FnOnce(&mut Interp<'_>) -> R<()>) -> R<()> {
        match f(it) {
            Err(Error::Throw(v)) => {
                self.report(it, v, "custom-element", Target::Node(el));
                Ok(())
            }
            r => r,
        }
    }

    /// "Upgrade an element".
    fn upgrade(&self, it: &mut Interp<'_>, el: NodeId, idx: u32) -> R<()> {
        if self.doc.borrow().custom_state(el).is_some() {
            return Ok(());
        }
        let wrapper = self.wrap(it, el)?;
        let ctor = {
            let (mut doc, mut s) = (self.doc.borrow_mut(), st(self).borrow_mut());
            doc.set_custom_state(el, Some(FAILED));
            let conn = connected(&doc, el);
            let reactions = initial_reactions(&doc, &s.defs[idx as usize], el, conn);
            for r in reactions {
                s.enqueue(el, r);
            }
            let def = &mut s.defs[idx as usize];
            def.stack.push(Some(el));
            doc.set_custom_state(el, Some(PRECUSTOMIZED));
            def.ctor
        };
        let r = self.construct_checked(it, ctor, wrapper);
        {
            let mut s = st(self).borrow_mut();
            s.defs[idx as usize].stack.pop();
        }
        match r {
            Ok(()) => {
                self.doc.borrow_mut().set_custom_state(el, Some(idx));
                Ok(())
            }
            Err(e) => {
                self.doc.borrow_mut().set_custom_state(el, Some(FAILED));
                let gone = st(self).borrow_mut().queues.remove(&(el.index() as u32));
                if let Some(q) = gone {
                    st(self).borrow_mut().pending -= q.len();
                }
                self.guarded(it, el, |_| Err(e))
            }
        }
    }

    /// `Construct(ctor)` and the check that the result is `wrapper`.
    fn construct_checked(&self, it: &mut Interp<'_>, ctor: ObjId, wrapper: Value) -> R<()> {
        match (it.host_construct(Value::Object(ctor), &[], None)?, wrapper) {
            (Value::Object(a), Value::Object(b)) if a == b => Ok(()),
            _ => Err(it.type_error("Custom element constructor did not return the element that is being upgraded")),
        }
    }

    /// The parser reaches the script `upto` (`None`: the end of the page): make the held elements before it, in tree order, as the parser would have
    /// made them (`parse_construct`). The loader calls it before each parser-blocking script.
    pub(crate) fn ce_release(&self, it: &mut Interp<'_>, upto: Option<NodeId>) -> R<()> {
        if self.ce.st.get().is_none_or(|s| s.borrow().held.is_empty()) {
            return Ok(());
        }
        let found = {
            let (doc, mut s) = (self.doc.borrow(), st(self).borrow_mut());
            let held: std::collections::HashSet<NodeId> = s.held.iter().copied().collect();
            let mut found = Vec::new();
            let mut stop = false;
            each_element(&doc, doc.root(), |e| {
                stop |= Some(e) == upto;
                if !stop && held.contains(&e) {
                    found.push(e);
                }
            });
            let gone: std::collections::HashSet<NodeId> = found.iter().copied().collect();
            s.held.retain(|e| !gone.contains(e));
            found
        };
        for e in found {
            self.parse_construct(it, e)?;
        }
        Ok(())
    }

    /// "Create an element for a token" with the synchronous custom elements flag, for the held element `el` that has a definition. The tree is
    /// already built, so `el` is taken out of it (`Document::parse_cut`): the constructor sees what the parser has when it calls it, an element with
    /// no attributes, no children and no parent, in a document that stops before it. Then the parser adds the attributes and inserts the element,
    /// and the reactions run (`attributeChangedCallback`, `connectedCallback`) while the element has no children yet. Its children come back last.
    fn parse_construct(&self, it: &mut Interp<'_>, el: NodeId) -> R<()> {
        let found = {
            let (doc, s) = (self.doc.borrow(), st(self).borrow());
            if doc.custom_state(el).is_some() || !connected(&doc, el) { None } else { s.def_of(&doc, el).map(|i| (i, s.defs[i as usize].ctor, doc.element_name(el).to_string())) }
        };
        let Some((idx, ctor, name)) = found else { return Ok(()) };
        let wrapper = self.wrap(it, el)?;
        let cut = self.doc.borrow_mut().parse_cut(el);
        {
            let mut s = st(self).borrow_mut();
            s.stack.push(Vec::new());
            s.defs[idx as usize].stack.push(Some(el));
            self.doc.borrow_mut().set_custom_state(el, Some(PRECUSTOMIZED));
        }
        let r = self.construct_checked(it, ctor, wrapper).and_then(|()| check_result(self, it, wrapper, &name).map(drop));
        st(self).borrow_mut().defs[idx as usize].stack.pop();
        let r = match r {
            Err(Error::Throw(e)) => Err(e),
            Err(e) => {
                st(self).borrow_mut().stack.pop();
                self.doc.borrow_mut().parse_close(cut);
                return Err(e);
            }
            Ok(()) => Ok(()),
        };
        self.doc.borrow_mut().parse_open(&cut);
        match r {
            Ok(()) => {
                let mut s = st(self).borrow_mut();
                let mut doc = self.doc.borrow_mut();
                doc.set_custom_state(el, Some(idx));
                for r in initial_reactions(&doc, &s.defs[idx as usize], el, true) {
                    s.enqueue(el, r);
                }
            }
            Err(e) => {
                self.make_unknown(it, el, wrapper)?;
                let gone = st(self).borrow_mut().queues.remove(&(el.index() as u32));
                if let Some(q) = gone {
                    st(self).borrow_mut().pending -= q.len();
                }
                self.report(it, e, "custom-element", Target::Node(el));
            }
        }
        // The constructor was the only script on the stack: its microtask checkpoint comes before the reactions.
        let r = it.drain_microtasks().map(drop).and_then(|()| self.ce_pop(it));
        self.doc.borrow_mut().parse_close(cut);
        r
    }

    /// Where the next element goes in the top element queue. See `fragment_upgrades`.
    pub(crate) fn ce_mark(&self) -> usize {
        let s = st(self).borrow();
        s.stack.last().map_or(s.backup.len(), Vec::len)
    }

    /// The elements `els` that the fragment parser made are in the document tree of `doc` at `mark`: put an upgrade for each that has a
    /// definition before the reactions of the nodes that the same change removed (`mark` was taken before it).
    pub(crate) fn fragment_upgrades(&self, doc: &Document, mut mark: usize, els: &[NodeId]) {
        let mut s = st(self).borrow_mut();
        for &e in els {
            if let Some(i) = s.def_of(doc, e) {
                s.enqueue_upgrade_at(mark, e, i);
                mark += 1;
            }
        }
    }

    /// `cloneNode`: each element of the clone `pairs` (`(source, copy)`) that has a definition gets an upgrade, and keeps the `is` value of its source.
    pub(crate) fn clone_upgrades(&self, doc: &Document, pairs: &[(NodeId, NodeId)]) {
        let mut s = st(self).borrow_mut();
        for &(src, c) in pairs {
            let is = s.is_of(doc, src).map(Box::<str>::from);
            let found = s.def_for(doc.element_name(src), is.as_deref());
            if let (Some(is), true) = (is, s.is.contains_key(&(src.index() as u32))) {
                s.is.insert(c.index() as u32, is);
            }
            if let Some(i) = found.filter(|_| doc.element_ns(src) == Ns::Html) {
                s.enqueue(c, Reaction::Upgrade(i));
            }
        }
    }

    /// The prototype that a new wrapper of the element `n` must have: that of its definition when it is a custom element.
    pub(crate) fn ce_proto(&self, doc: &Document, n: NodeId, natural: ObjId) -> ObjId {
        match (doc.custom_state(n), self.ce.st.get()) {
            (Some(s), Some(st)) if s < PRECUSTOMIZED => st.borrow().defs[s as usize].proto,
            (Some(FAILED), Some(st)) if st.borrow().unk.contains(&(n.index() as u32)) => self.ce.unk.get().map_or(natural, |u| u[0]),
            _ => natural,
        }
    }

    pub(crate) fn ce_roots(&self, out: &mut Vec<Value>) {
        let Some(st) = self.ce.st.get() else { return };
        let s = st.borrow();
        for d in &s.defs {
            out.extend([Value::Object(d.ctor), Value::Object(d.proto)]);
            out.extend(d.cb.iter().copied().filter(|v| !matches!(v, Value::Undefined)));
        }
        out.extend(s.when.values().copied());
        out.extend(s.flush.map(Value::Object));
        out.extend(s.reg.iter().flatten().copied().map(Value::Object));
        out.extend(self.ce.unk.get().into_iter().flatten().copied().map(Value::Object));
        out.extend(self.ce.int.get().into_iter().flatten().copied().map(Value::Object));
        out.extend(s.ints.keys().copied().map(Value::Object));
        for f in s.face.values() {
            f.roots(out);
        }
    }
}

/// The microtask of the backup element queue.
fn flush(d: &Dom, it: &mut Interp<'_>, _: Value, _: &[Value]) -> V {
    let els = {
        let mut s = st(d).borrow_mut();
        s.queued = false;
        std::mem::take(&mut s.backup)
    };
    d.invoke(it, els)?;
    Ok(Value::Undefined)
}

// ------------------------------------------------------------------------------------------ the constructors

/// The constructor of the interface `IFACES[K]`: "the HTMLElement constructor" of the standard.
fn html_ctor<const K: usize>(d: &Dom, it: &mut Interp<'_>, _: Value, _: &[Value]) -> V {
    let iface = IFACES[K];
    let bad = |it: &mut Interp<'_>, why: &str| it.type_error(&format!("Failed to construct '{iface}': Illegal constructor{why}"));
    let Some(nt) = it.host_new_target() else { return Err(bad(it, "")) };
    let Some(sr) = d.ce.st.get() else { return Err(bad(it, "")) };
    let found = {
        let s = sr.borrow();
        s.by_ctor.get(&nt).map(|&i| (i, s.defs[i as usize].ext.clone(), s.defs[i as usize].name.clone()))
    };
    let Some((idx, ext, name)) = found else { return Err(bad(it, "")) };
    match &ext {
        None if K != 0 => return Err(bad(it, ": autonomous custom elements must extend HTMLElement")),
        Some(e) if iface_of(e) != K => return Err(bad(it, ": localName does not match the HTML element interface")),
        _ => {}
    }
    let taken = sr.borrow_mut().defs[idx as usize].stack.last_mut().map(Option::take);
    let wrapper = match taken {
        Some(Some(e)) => d.wrap(it, e)?,
        Some(None) => return Err(it.type_error(&format!("Failed to construct '{iface}': This instance is already constructed"))),
        None => {
            let v = make_element(d, it, ext.as_deref().unwrap_or(&name))?;
            let n = d.node_of(v).expect("an element");
            d.doc.borrow_mut().set_custom_state(n, Some(idx));
            v
        }
    };
    let Value::Object(w) = wrapper else { unreachable!("a wrapper is an object") };
    let natural = it.heap.get_prototype_of(w).map_err(|e| it.heap_error(e))?.unwrap_or(d.p().html);
    let proto = it.host_proto_from_new_target(natural)?;
    it.heap.set_prototype_of(w, Some(proto)).map_err(|e| it.heap_error(e))?;
    Ok(wrapper)
}

/// The constructor object of the interface `name`: for an HTML element interface it takes `new.target` (`class extends`, `Reflect.construct`), for any
/// other it is an illegal constructor (`new_ok`: whether `new` reaches it).
pub(crate) fn interface_ctor(d: &Dom, it: &mut Interp<'_>, name: &str, new_ok: bool) -> R<ObjId> {
    match IFACES.iter().position(|&n| n == name) {
        Some(k) => d.add_ctor_nt(it, HANDLERS[k], name, 0),
        None => d.add_fn(it, illegal_constructor, name, 0, new_ok),
    }
}

/// The error of `HTMLElement()` without `new`: `h` is the host function that was called.
pub(crate) fn needs_new(it: &mut Interp<'_>, h: H) -> Error {
    match HANDLERS.iter().position(|&x| x as usize == h as usize) {
        Some(k) => it.type_error(&format!("Failed to construct '{}': Please use the 'new' operator, this DOM object constructor cannot be called as a function.", IFACES[k])),
        None if h as usize == unknown_ctor as *const () as usize => it.type_error("Illegal constructor"),
        None => crate::browser_geom::needs_new(it, h).unwrap_or_else(|| it.type_error("Failed to construct: please use the 'new' operator")),
    }
}

// ------------------------------------------------------------------------------------------ createElement

/// `createElement(name, options)` and `createElementNS` of the HTML namespace: "create an element" with the synchronous custom elements flag.
/// `name` is lowercase. `options` is the second argument (`Undefined` for none).
pub(crate) fn create(d: &Dom, it: &mut Interp<'_>, name: &str, options: Value) -> V {
    let is = match options {
        o @ Value::Object(_) => {
            let k = d.key(it, "is")?;
            match it.get_value(o, k)? {
                Value::Undefined => None,
                v => Some(d.string_arg(it, v)?),
            }
        }
        _ => None,
    };
    let def = d.ce.st.get().and_then(|s| s.borrow().def_for(name, is.as_deref()));
    let Some(i) = def else {
        let v = make_element(d, it, name)?;
        if let Some(is) = is {
            let n = d.node_of(v).expect("an element");
            st(d).borrow_mut().is.insert(n.index() as u32, is.into());
        }
        return Ok(v);
    };
    let ctor = st(d).borrow().defs[i as usize].ctor;
    let r = it.host_construct(Value::Object(ctor), &[], None).and_then(|v| check_result(d, it, v, name));
    match r {
        Ok(v) => Ok(v),
        Err(Error::Throw(e)) => {
            // "Failed": report the exception and make an element that no constructor will run for.
            let v = make_element(d, it, name)?;
            let n = d.node_of(v).expect("an element");
            d.make_unknown(it, n, v)?;
            d.report(it, e, "custom-element", Target::Node(n));
            Ok(v)
        }
        Err(e) => Err(e),
    }
}

/// The inert document that owns the contents of the templates, made by the first need.
pub(crate) fn template_document(d: &Dom) -> NodeId {
    d.ce.tpl.get().unwrap_or_else(|| {
        let n = d.doc.borrow_mut().create_document();
        d.ce.tpl.set(Some(n));
        n
    })
}

impl Dom {
    /// Does the inert document own `n`: is it in the contents of a template, or was it moved out of them by a script (a custom element)?
    pub(crate) fn in_inert(&self, n: NodeId) -> bool {
        self.doc.borrow().in_template_content(n) || (self.ce.on.get() && st(self).borrow().inert.contains(&(n.index() as u32)))
    }

    /// The element `n` (wrapper `v`) failed to construct: the state "failed", and its interface is `HTMLUnknownElement`.
    fn make_unknown(&self, it: &mut Interp<'_>, n: NodeId, v: Value) -> R<()> {
        self.doc.borrow_mut().set_custom_state(n, Some(FAILED));
        st(self).borrow_mut().unk.insert(n.index() as u32);
        let (Value::Object(w), proto) = (v, unknown_proto(self, it)?) else { unreachable!("a wrapper is an object") };
        it.heap.set_prototype_of(w, Some(proto)).map_err(|e| it.heap_error(e))?;
        Ok(())
    }
}

/// The prototype of `HTMLUnknownElement`, made by the first element that needs it.
pub(crate) fn unknown_proto(d: &Dom, it: &mut Interp<'_>) -> R<ObjId> {
    if let Some(u) = d.ce.unk.get() {
        return Ok(u[0]);
    }
    let (kp, kc) = (d.key(it, "prototype")?, d.key(it, "constructor")?);
    let proto = d.obj(it, d.p().html)?;
    let f = d.add_fn(it, unknown_ctor, "HTMLUnknownElement", 0, true)?;
    it.define_data(f, kp, Value::Object(proto), false, false, false)?;
    it.define_data(proto, kc, Value::Object(f), true, false, true)?;
    crate::browser_lazy::tag(d, it, proto, "HTMLUnknownElement")?;
    // Like every HTML interface, its constructor inherits from `HTMLElement`.
    let g = it.global_object();
    let k = d.key(it, "HTMLElement")?;
    if let Value::Object(h) = it.get_value(Value::Object(g), k)? {
        it.heap.set_prototype_of(f, Some(h)).map_err(|e| it.heap_error(e))?;
    }
    let _ = d.ce.unk.set([proto, f]);
    Ok(proto)
}

/// `new HTMLUnknownElement()` is an error (a call without `new` is one too: see `needs_new`).
fn unknown_ctor(_: &Dom, it: &mut Interp<'_>, _: Value, _: &[Value]) -> V {
    Err(it.type_error("Failed to construct 'HTMLUnknownElement': Illegal constructor"))
}

fn ensure_unknown(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    unknown_proto(d, it).map(|_| ())
}

crate::browser_lazy::lazy_global!(lazy_unknown, "HTMLUnknownElement", ensure_unknown, |d| d.ce.unk.get().expect("made")[1]);

/// The reactions of a new custom element, in the order of the standard: `attributeChangedCallback` for each observed attribute that it has, then
/// `connectedCallback` when it is connected.
fn initial_reactions(doc: &Document, def: &Def, el: NodeId, conn: bool) -> Vec<Reaction> {
    let mut reactions = Vec::new();
    if !matches!(def.cb[ATTRIBUTE], Value::Undefined) && !def.obs.is_empty() {
        let NodeData::Element { attrs, .. } = *doc.data(el) else { unreachable!("only elements are upgraded") };
        for a in doc.attrs(attrs) {
            let local = doc.attr_local(a.name);
            if def.obs.iter().any(|o| &**o == local) {
                reactions.push(Reaction::Attr(Box::new(AttrArgs { name: local.into(), old: None, new: Some(doc.text(a.value).into()), ns: doc.attr_ns(a.name).map(Ns::uri) })));
            }
        }
    }
    if conn && !matches!(def.cb[CONNECTED], Value::Undefined) {
        reactions.push(Reaction::Connected);
    }
    if def.form {
        reactions.push(Reaction::FormCheck);
    }
    reactions
}

/// The checks of the standard on what the constructor gave.
fn check_result(d: &Dom, it: &mut Interp<'_>, v: Value, name: &str) -> V {
    let msg = {
        let doc = d.doc.borrow();
        match d.node_of(v) {
            Some(n) if matches!(doc.data(n), NodeData::Element { .. }) && doc.element_ns(n) == Ns::Html => {
                if doc.attr_count(n) != 0 {
                    Some("The result must not have attributes")
                } else if doc.children(n).next().is_some() {
                    Some("The result must not have children")
                } else if doc.parent(n).is_some() {
                    Some("The result must not have a parent")
                } else if doc.element_name(n) != name {
                    Some("The result must have the same local name as the element being created")
                } else {
                    None
                }
            }
            _ => return Err(it.type_error("The result must implement HTMLElement")),
        }
    };
    match msg {
        Some(m) => Err(d.dom_err(it, "NotSupportedError", m)),
        None => Ok(v),
    }
}

// ------------------------------------------------------------------------------------------ the registry

fn registry(d: &Dom, it: &mut Interp<'_>, this: Value) -> R<()> {
    match (this, d.ce.st.get().and_then(|s| s.borrow().reg)) {
        (Value::Object(o), Some([_, _, r])) if o == r => Ok(()),
        _ => Err(it.type_error("Illegal invocation")),
    }
}

fn reg_err(it: &mut Interp<'_>, m: &str, msg: &str) -> Error {
    it.type_error(&format!("Failed to execute '{m}' on 'CustomElementRegistry': {msg}"))
}

fn reg_dom_err(d: &Dom, it: &mut Interp<'_>, kind: &str, m: &str, msg: &str) -> Error {
    d.dom_err(it, kind, &format!("Failed to execute '{m}' on 'CustomElementRegistry': {msg}"))
}

/// `customElements.define(name, constructor, options)`. The first call has no scope of reactions yet (`Host::call` opens it once `on` is set).
fn define(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    if d.ce.on.get() {
        return define_in(d, it, this, a);
    }
    d.ce.on.set(true);
    st(d).borrow_mut().stack.push(Vec::new());
    d.sync_watcher();
    d.def_method(it, d.p().html, "attachInternals", 0, crate::browser_face::attach_internals)?;
    let r = define_in(d, it, this, a);
    let q = d.ce_pop(it);
    r.and_then(|v| q.map(|()| v))
}

fn define_in(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    registry(d, it, this)?;
    if a.len() < 2 {
        return Err(reg_err(it, "define", &format!("2 arguments required, but only {} present.", a.len())));
    }
    let name = d.string_arg(it, a[0])?;
    let ctor = a[1];
    let Value::Object(ctor_obj) = ctor else { return Err(reg_err(it, "define", "parameter 2 is not of type 'Function'.")) };
    if !it.is_callable_value(ctor) {
        return Err(reg_err(it, "define", "parameter 2 is not of type 'Function'."));
    }
    let ext = match arg(a, 2) {
        Value::Undefined | Value::Null => None,
        o @ Value::Object(_) => {
            let k = d.key(it, "extends")?;
            match it.get_value(o, k)? {
                Value::Undefined => None,
                v => Some(d.string_arg(it, v)?),
            }
        }
        _ => return Err(reg_err(it, "define", "The provided value is not of type 'ElementDefinitionOptions'.")),
    };
    if !it.is_constructor_value(ctor)? {
        return Err(reg_err(it, "define", "constructor argument is not a constructor"));
    }
    if !valid_custom_name(&name) {
        return Err(reg_dom_err(d, it, "SyntaxError", "define", &format!("\"{name}\" is not a valid custom element name")));
    }
    {
        let s = st(d).borrow();
        if s.by_name.contains_key(&*name) {
            drop(s);
            return Err(reg_dom_err(d, it, "NotSupportedError", "define", &format!("the name \"{name}\" has already been used with this registry")));
        }
        if s.by_ctor.contains_key(&ctor_obj) {
            drop(s);
            return Err(reg_dom_err(d, it, "NotSupportedError", "define", "this constructor has already been used with this registry"));
        }
        if s.defs.len() >= MAX_DEFS {
            drop(s);
            return Err(d.lim("custom-definitions"));
        }
    }
    if let Some(e) = &ext {
        if valid_custom_name(e) {
            return Err(reg_dom_err(d, it, "NotSupportedError", "define", &format!("\"{e}\" is a valid custom element name")));
        }
        if KNOWN.binary_search(&e.as_str()).is_err() {
            return Err(reg_dom_err(d, it, "NotSupportedError", "define", &format!("\"{e}\" is an HTMLUnknownElement")));
        }
    }
    if std::mem::replace(&mut st(d).borrow_mut().defining, true) {
        return Err(reg_dom_err(d, it, "NotSupportedError", "define", "an element definition is already being processed"));
    }
    let read = read_definition(d, it, ctor);
    st(d).borrow_mut().defining = false;
    let (proto, cb, obs, form, no_internals) = read?;

    let idx = {
        let mut s = st(d).borrow_mut();
        let idx = s.defs.len() as u32;
        s.has_ext |= ext.is_some();
        s.by_name.insert(name.as_str().into(), idx);
        s.by_ctor.insert(ctor_obj, idx);
        s.has_face |= form;
        s.defs.push(Def { name: name.as_str().into(), ext: ext.map(Into::into), ctor: ctor_obj, proto, cb, obs, form, no_internals, stack: Vec::new() });
        idx
    };
    // The upgrade candidates: the elements of the document (and its shadow trees) that have this definition, in tree order.
    // While the page loads, the parser has not made the elements after the running script: they wait (`ce_release`).
    let (mut found, mut held) = (Vec::new(), Vec::new());
    {
        let (doc, s) = (d.doc.borrow(), st(d).borrow());
        let (mut budget, parsing, mut after) = (0u64, d.ce.parsing.get(), false);
        each_element(&doc, doc.root(), |e| {
            budget += 1;
            after |= Some(e) == parsing;
            if doc.custom_state(e).is_none() && s.def_of(&doc, e) == Some(idx) {
                if after { held.push(e) } else { found.push(e) }
            }
        });
        drop(s);
        it.charge(budget)?;
    }
    {
        let mut s = st(d).borrow_mut();
        s.held.extend(held);
        for e in found {
            s.enqueue(e, Reaction::Upgrade(idx));
        }
    }
    let waiting = st(d).borrow_mut().when.remove(&*name);
    if let Some(p) = waiting {
        it.host_resolve_promise(p, ctor)?;
    }
    Ok(Value::Undefined)
}

/// The prototype, the callbacks, the observed attributes, `formAssociated` and "`disabledFeatures` has `internals`".
type Read = (ObjId, [Value; 8], Box<[Box<str>]>, bool, bool);

/// The callback `n` of `proto`: `Undefined` when absent, an error when it is not a function.
fn callback(d: &Dom, it: &mut Interp<'_>, proto: ObjId, n: &str) -> V {
    let k = d.key(it, n)?;
    let v = it.get_value(Value::Object(proto), k)?;
    if !matches!(v, Value::Undefined) && !it.is_callable_value(v) {
        return Err(reg_err(it, "define", &format!("\"{n}\" is not a function")));
    }
    Ok(v)
}

/// A `sequence<DOMString>` that `v` holds: `observedAttributes` and `disabledFeatures`.
fn strings(d: &Dom, it: &mut Interp<'_>, v: Value) -> R<Vec<Box<str>>> {
    let mut out: Vec<Box<str>> = Vec::new();
    if matches!(v, Value::Undefined) {
        return Ok(out);
    }
    let iter = if matches!(v, Value::Object(_)) { it.host_iter_open(v)? } else { None };
    let Some(mut iter) = iter else { return Err(reg_err(it, "define", "The provided value cannot be converted to a sequence.")) };
    loop {
        it.charge(1)?;
        match it.host_iter_next(&mut iter)? {
            Some(v) => match d.string_arg(it, v) {
                Ok(s) => out.push(s.into()),
                Err(e) => return Err(it.host_iter_fail(&mut iter, e)),
            },
            None => break,
        }
        if out.len() > d.lim.max_names {
            return Err(d.lim("custom-observed-attributes"));
        }
    }
    Ok(out)
}

/// What `define` reads from a constructor, in the order of the standard (and Chrome 155).
fn read_definition(d: &Dom, it: &mut Interp<'_>, ctor: Value) -> R<Read> {
    let kp = d.key(it, "prototype")?;
    let Value::Object(proto) = it.get_value(ctor, kp)? else { return Err(reg_err(it, "define", "constructor prototype is not an object")) };
    let mut cb = [Value::Undefined; 8];
    // `connectedMoveCallback` is read and checked as Chrome does; it is never called (no `moveBefore`).
    for (slot, n) in [Some(CONNECTED), Some(DISCONNECTED), None, Some(2), Some(ATTRIBUTE)].into_iter().zip(["connectedCallback", "disconnectedCallback", "connectedMoveCallback", "adoptedCallback", "attributeChangedCallback"]) {
        let v = callback(d, it, proto, n)?;
        if let Some(i) = slot {
            cb[i] = v;
        }
    }
    let mut obs = Vec::new();
    if !matches!(cb[ATTRIBUTE], Value::Undefined) {
        let ko = d.key(it, "observedAttributes")?;
        let v = it.get_value(ctor, ko)?;
        obs = strings(d, it, v)?;
    }
    let kd = d.key(it, "disabledFeatures")?;
    let v = it.get_value(ctor, kd)?;
    let no_internals = strings(d, it, v)?.iter().any(|f| &**f == "internals");
    let kf = d.key(it, "formAssociated")?;
    let v = it.get_value(ctor, kf)?;
    let form = it.to_js_bool(v)?;
    if form {
        for (slot, n) in [FORM_ASSOCIATED, FORM_RESET, FORM_DISABLED, 7].into_iter().zip(["formAssociatedCallback", "formResetCallback", "formDisabledCallback", "formStateRestoreCallback"]) {
            cb[slot] = callback(d, it, proto, n)?;
        }
        // Chrome reads `toolFillCallback` too. It is never called.
        callback(d, it, proto, "toolFillCallback")?;
    }
    Ok((proto, cb, obs.into(), form, no_internals))
}

fn get(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    registry(d, it, this)?;
    if a.is_empty() {
        return Err(reg_err(it, "get", "1 argument required, but only 0 present."));
    }
    let name = d.string_arg(it, a[0])?;
    let s = st(d).borrow();
    Ok(s.by_name.get(&*name).map_or(Value::Undefined, |&i| Value::Object(s.defs[i as usize].ctor)))
}

fn get_name(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    registry(d, it, this)?;
    if a.is_empty() {
        return Err(reg_err(it, "getName", "1 argument required, but only 0 present."));
    }
    let name = match a[0] {
        Value::Object(o) if it.is_callable_value(a[0]) => st(d).borrow().by_ctor.get(&o).map(|&i| st(d).borrow().defs[i as usize].name.clone()),
        _ => return Err(reg_err(it, "getName", "parameter 1 is not of type 'Function'.")),
    };
    match name {
        Some(n) => d.js_str(it, &n),
        None => Ok(Value::Null),
    }
}

fn when_defined(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    registry(d, it, this)?;
    if a.is_empty() {
        return Err(reg_err(it, "whenDefined", "1 argument required, but only 0 present."));
    }
    let name = d.string_arg(it, a[0])?;
    let invalid = !valid_custom_name(&name);
    let (done, existing, n) = {
        let s = st(d).borrow();
        (s.by_name.get(&*name).map(|&i| Value::Object(s.defs[i as usize].ctor)), s.when.get(&*name).copied(), s.when.len())
    };
    // One promise for a name that is not defined yet: a second call gets the same one.
    if let (Some(e), false) = (existing, invalid) {
        return Ok(e);
    }
    if n >= MAX_DEFS {
        return Err(d.lim("custom-when-defined"));
    }
    let p = it.host_new_promise()?;
    if invalid {
        let Error::Throw(e) = reg_dom_err(d, it, "SyntaxError", "whenDefined", &format!("\"{name}\" is not a valid custom element name")) else { unreachable!("a DOMException is thrown") };
        it.host_reject_promise(p, e)?;
    } else if let Some(c) = done {
        it.host_resolve_promise(p, c)?;
    } else {
        st(d).borrow_mut().when.insert(name.into(), p);
    }
    Ok(p)
}

fn upgrade(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    registry(d, it, this)?;
    if a.is_empty() {
        return Err(reg_err(it, "upgrade", "1 argument required, but only 0 present."));
    }
    let Some(root) = d.node_of(a[0]) else { return Err(reg_err(it, "upgrade", "parameter 1 is not of type 'Node'.")) };
    let mut els = Vec::new();
    {
        let doc = d.doc.borrow();
        let mut budget = 0u64;
        each_element(&doc, root, |e| {
            budget += 1;
            els.push(e);
        });
        it.charge(budget)?;
    }
    let mut s = st(d).borrow_mut();
    let doc = d.doc.borrow();
    for e in els {
        if doc.custom_state(e).is_none()
            && let Some(i) = s.def_of(&doc, e)
        {
            s.enqueue(e, Reaction::Upgrade(i));
        }
    }
    Ok(Value::Undefined)
}

// ------------------------------------------------------------------------------------------ importNode, adoptNode

/// The node argument of `importNode` and `adoptNode` (`m`): the count and type checks of WebIDL, then the refusal of a document and a shadow root.
fn move_arg(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value], m: &str, what: &str) -> R<NodeId> {
    d.nd(it, this)?;
    if a.is_empty() {
        return Err(it.type_error(&format!("Failed to execute '{m}' on 'Document': 1 argument required, but only 0 present.")));
    }
    let Some(n) = d.node_of(a[0]) else { return Err(it.type_error(&format!("Failed to execute '{m}' on 'Document': parameter 1 is not of type 'Node'."))) };
    match d.data(n) {
        NodeData::Document if m == "adoptNode" => Err(d.dom_err(it, "NotSupportedError", "Failed to execute 'adoptNode' on 'Document': The node provided is of type '#document', which may not be adopted.")),
        NodeData::Document => Err(d.dom_err(it, "NotSupportedError", &format!("Failed to execute '{m}' on 'Document': The node provided is a document, which may not be {what}."))),
        NodeData::ShadowRoot if d.doc.borrow().shadow_host(n).is_some() => {
            let kind = if m == "adoptNode" { "HierarchyRequestError" } else { "NotSupportedError" };
            Err(d.dom_err(it, kind, &format!("Failed to execute '{m}' on 'Document': The node provided is a shadow root, which may not be {what}.")))
        }
        _ => Ok(n),
    }
}

/// `document.importNode(node, deep)`: a clone that belongs to the document (its custom elements are upgraded, also when `node` is in a template).
fn import_node(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = move_arg(d, it, this, a, "importNode", "imported")?;
    let deep = it.to_js_bool(arg(a, 1))?;
    let c = crate::browser_dom::clone_tree(d, it, n, deep, true)?;
    d.wrap(it, c)
}

/// `document.adoptNode(node)`: `node` leaves its parent and belongs to the document. A custom element that came from the contents of a template gets
/// `adoptedCallback`.
fn adopt_node(d: &Dom, it: &mut Interp<'_>, this: Value, a: &[Value]) -> V {
    let n = move_arg(d, it, this, a, "adoptNode", "adopted")?;
    d.doc.borrow_mut().detach(n);
    if d.ce.on.get() {
        let mut s = st(d).borrow_mut();
        let doc = d.doc.borrow();
        let mut els = Vec::new();
        each_element(&doc, n, |e| els.push(e));
        it.charge(els.len() as u64)?;
        for e in els {
            if let Some(c) = doc.custom_state(e).filter(|&c| c < PRECUSTOMIZED) && s.inert.remove(&(e.index() as u32)) && !matches!(s.defs[c as usize].cb[ADOPTED], Value::Undefined) {
                s.enqueue(e, Reaction::Adopted(false));
            }
        }
    }
    Ok(a[0])
}

// ------------------------------------------------------------------------------------------ install

/// Make the prototype, the constructor and the registry (the first read of `customElements` or `CustomElementRegistry`).
fn ensure(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    if st(d).borrow().reg.is_none() {
        make(d, it)?;
    }
    Ok(())
}

#[cold]
#[inline(never)]
fn make(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    let op = it.object_prototype();
    let proto = d.obj(it, op)?;
    let (kp, kc) = (d.key(it, "prototype")?, d.key(it, "constructor")?);
    let f = d.add_fn(it, illegal_constructor, "CustomElementRegistry", 0, true)?;
    it.define_data(f, kp, Value::Object(proto), false, false, false)?;
    it.define_data(proto, kc, Value::Object(f), true, false, true)?;
    crate::browser_lazy::tag(d, it, proto, "CustomElementRegistry")?;
    for (name, len, h) in [("define", 2, define as H), ("get", 1, get), ("getName", 1, get_name), ("whenDefined", 1, when_defined), ("upgrade", 1, upgrade)] {
        let k = d.key(it, name)?;
        let m = d.add_fn(it, h, name, len, false)?;
        // WebIDL operations are enumerable.
        it.define_data(proto, k, Value::Object(m), true, true, true)?;
    }
    // `Object.prototype.toString` of an element made by a custom element constructor says `[object HTMLElement]`. The prototypes of the eager
    // interfaces get their tag now, so a page that never reads `customElements` does not pay for them.
    let p = d.p();
    for (proto, name) in [(p.html, "HTMLElement"), (p.input, "HTMLInputElement"), (p.iframe, "HTMLIFrameElement"), (p.link, "HTMLLinkElement"), (p.form, "HTMLFormElement"), (p.button, "HTMLButtonElement")] {
        crate::browser_lazy::tag(d, it, proto, name)?;
    }
    if let Some(&script) = d.scripts.proto.get() {
        crate::browser_lazy::tag(d, it, script, "HTMLScriptElement")?;
    }
    let reg = d.obj(it, proto)?;
    st(d).borrow_mut().reg = Some([proto, f, reg]);
    Ok(())
}

fn customelements(d: &Dom, it: &mut Interp<'_>, _: Value, _: &[Value]) -> V {
    ensure(d, it)?;
    let [_, _, r] = st(d).borrow().reg.expect("made");
    Ok(Value::Object(r))
}

crate::browser_lazy::lazy_global!(lazy_registry, "CustomElementRegistry", ensure, |d| st(d).borrow().reg.expect("made")[1]);

/// Install last: a new host function earlier in the list of `Dom::install` moves the id of each one after it.
pub(crate) fn install(d: &Dom, it: &mut Interp<'_>) -> R<()> {
    crate::browser_lazy::install(d, it, &&[&[("CustomElementRegistry", lazy_registry as H), ("HTMLUnknownElement", lazy_unknown)], &crate::browser_face::LAZY[..]].concat())?;
    // As in Chrome, `customElements` stays an accessor of `window` with no setter.
    let g = *d.global.get().expect("installed");
    let key = d.key(it, "customElements")?;
    let f = d.add_fn(it, customelements, "get customElements", 0, false)?;
    d.define_accessor(it, g, key, f, None)?;
    d.def_method(it, d.p().element, "toggleAttribute", 1, crate::browser_dom::toggle_attribute)?;
    d.def_method(it, d.p().document, "importNode", 1, import_node)?;
    d.def_method(it, d.p().document, "adoptNode", 1, adopt_node)?;
    Ok(())
}
