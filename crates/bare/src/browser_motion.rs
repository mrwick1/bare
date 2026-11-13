//! CSS transition and animation events: `transitionrun`, `transitionstart`, `transitionend`, `transitioncancel`, `animationstart`,
//! `animationiteration`, `animationend`, `animationcancel`.
//! Sources: CSS Transitions Level 1 ("Starting of transitions", "Application of transitions", "Transition Events") and CSS Animations
//! Level 1 ("Animation Events"), read with Ladybird `Libraries/LibWeb/CSS/CSSTransition.cpp` and `CSSAnimation.cpp` (the event times and
//! the `elapsedTime` of each event) and Servo `components/style/animation.rs` (the choice of the properties that start a transition).
//!
//! How it works
//! - Nothing is drawn, so values are not interpolated: the new value applies at once. Only the events keep the timing of the page.
//! - A new cascade of the whole document (`Dom::cascade_with`) calls `motion_seen`. It compares the new styles with the copy (`StyleMap`)
//!   of the last cascade. That copy is the "before-change style". A property that changes and that the new `transition-property` covers,
//!   with `transition-duration` + `transition-delay` > 0, starts a transition. An element that gets an `animation-name` for which
//!   a `@keyframes` rule exists starts an animation. The elements are read in tree order, so the order of the events is stable.
//! - Each event is a scheduler task (`Task::Motion`) on the scheduler clock, so the virtual clock moves it. `transitionrun` is due at once,
//!   `transitionstart` after the delay and `transitionend` after the delay and the duration. `animationiteration` is queued one at a time:
//!   the next one when the last one ran. A finished transition queues nothing more; an animation that ended is kept as "finished" until
//!   the element loses the `animation-name`, so it does not start again.
//! - A change of the document makes a rendering step (`Dom::motion_wants_frame`), as for `ResizeObserver`, so a change that no script
//!   reads is seen too. A page with no `transition-*`, `animation-*` or `@keyframes` anywhere never asks for a frame.
//! - A running transition or animation is cancelled (`*cancel` event) when its element has no style (removed from the document, or in a
//!   `display: none` subtree), when the element itself is `display: none`, and when the transition property or the animation name is no
//!   longer in the list. The check runs at each new cascade, and the task of each event runs it first.
//! - The settle rule skips the `animationiteration` tasks of an `infinite` animation (`Task::Motion(_, ITER_INFINITE)`), as it skips a quiet
//!   timer: an infinite animation never ends, so it must not keep a session from settling.
//!
//! Deviations from Chrome
//! - The event objects are plain `Event` objects with own data properties (`propertyName`, `animationName`, `elapsedTime`,
//!   `pseudoElement`). `TransitionEvent` and `AnimationEvent` do not exist. They bubble, are not cancelable, and `pseudoElement` is `""`.
//! - A change on an element that had no style in the last cascade starts no transition (no before-change style); a transition that
//!   the same change adds to an element that had none counts, as the standard says.
//! - Timing functions, `animation-direction`, `-fill-mode` and `-play-state` are read and ignored. No pseudo-element is animated.
//! - The elements of a shadow tree are not visited.
//! - `transitionrun` and `transitionstart` have `elapsedTime` 0 for a positive delay and the part of a negative delay otherwise.
//!
//! Bound (`Limit(Host(name))`): `motion-live` (transitions and animations that live, finished animations included).

use crate::browser::*;
use bare_css::cascade::{ComputedStyle, RuleSet, StyleMap, Styles};
use bare_css::grid::Grid;
use bare_css::motion::{INFINITE, Motion};
use bare_css::values::{Keyword, Length, Property, Value as Cv, all_longhands};
use bare_html::dom::{NodeData, NodeId};
use crate::browser_scheduler::Task;
use bare_js::eval::{Error, Interp};
use bare_js::value::Value;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

/// The steps of a `Task::Motion`.
pub(crate) const RUN: u8 = 0;
pub(crate) const START: u8 = 1;
pub(crate) const END: u8 = 2;
pub(crate) const ITER: u8 = 3;
/// `ITER` of an animation that has no end: the settle rule skips it.
pub(crate) const ITER_INFINITE: u8 = 4;
pub(crate) const CANCEL: u8 = 5;

const MAX_LIVE: usize = 8192;

enum Kind {
    Transition { name: String, to: String },
    Animation { name: String, dur_ms: f64, count: i64 },
}

struct Live {
    node: NodeId,
    kind: Kind,
    /// Scheduler time (ms) when the active phase starts: the change plus the delay.
    start: f64,
    delay_ms: f64,
    dur_ms: f64,
    /// Iterations that ended (animations).
    iter: i64,
    cancelled: bool,
}

#[derive(Default)]
struct State {
    /// The last cascade had a transition, an animation or a `@keyframes`, or something still lives.
    armed: bool,
    /// A rendering step was asked for and did not run yet.
    pending: bool,
    last_key: Option<Key>,
    prev: Option<StyleMap>,
    next: u64,
    live: HashMap<u64, Live>,
    /// The transition of a property of an element, and the animation of a name (`None`: it finished).
    transitions: HashMap<(NodeId, String), u64>,
    animations: HashMap<(NodeId, String), Option<u64>>,
}

pub(crate) type Key = (u64, u64, (u32, u32), u64, [u64; 3]);

#[derive(Default)]
pub(crate) struct MotionState {
    s: RefCell<State>,
}

/// Properties that `transition-property: all` does not cover: they cannot be interpolated.
fn discrete(p: Property) -> bool {
    use Property::*;
    crate::browser_motion::is_motion(p)
        || matches!(
            p,
            Display | Float | Clear | WhiteSpace | TextAlign | TextDecoration | TextTransform | ListStyleType | BoxSizing | OverflowX | OverflowY | Position
                | FlexDirection | FlexWrap | JustifyContent | AlignContent | AlignItems | AlignSelf | JustifyItems | JustifySelf | GridTemplateColumns
                | GridTemplateRows | GridTemplateAreas | GridAutoColumns | GridAutoRows | GridAutoFlow | GridRowStart | GridRowEnd | GridColumnStart
                | GridColumnEnd | BorderCollapse | TableLayout | CaptionSide | Direction | Appearance | PointerEvents | Cursor | UserSelect | ObjectFit
                | FontStyle | BorderTopStyle | BorderRightStyle | BorderBottomStyle | BorderLeftStyle
        )
}

fn is_motion(p: Property) -> bool {
    bare_css::motion::is_motion(p)
}

fn names(styles: &Styles, s: &ComputedStyle, p: Property) -> Option<Vec<String>> {
    match s.get(p) {
        Cv::Grid(i) => match styles.grid(i) {
            Grid::Motion(Motion::Names(n)) => Some(n.clone()),
            _ => None,
        },
        _ => None,
    }
}

fn times(styles: &Styles, s: &ComputedStyle, p: Property) -> Option<Vec<i64>> {
    match s.get(p) {
        Cv::Grid(i) => match styles.grid(i) {
            Grid::Motion(Motion::Times(t)) => Some(t.clone()),
            _ => None,
        },
        _ => None,
    }
}

fn has_motion(s: &ComputedStyle) -> bool {
    [Property::TransitionProperty, Property::TransitionDuration, Property::TransitionDelay, Property::AnimationName, Property::AnimationDuration, Property::AnimationDelay]
        .iter()
        .any(|&p| matches!(s.get(p), Cv::Grid(_)))
}

/// One covered property of a style: the name that the events carry, the properties that it stands for, and the times in microseconds.
struct Item {
    name: String,
    props: Vec<Property>,
    dur: i64,
    delay: i64,
}

/// The transitions that `s` asks for, with duration + delay above 0. Each name once (the last item wins, as in the standard).
fn transition_items(styles: &Styles, s: &ComputedStyle) -> Vec<Item> {
    let props = names(styles, s, Property::TransitionProperty).unwrap_or_else(|| vec!["all".into()]);
    let durs = times(styles, s, Property::TransitionDuration).unwrap_or_else(|| vec![0]);
    let delays = times(styles, s, Property::TransitionDelay).unwrap_or_else(|| vec![0]);
    let mut out: Vec<Item> = Vec::new();
    for (i, n) in props.iter().enumerate() {
        let (dur, delay) = (durs[i % durs.len()], delays[i % delays.len()]);
        if dur.saturating_add(delay) <= 0 {
            continue;
        }
        let lower = n.to_ascii_lowercase();
        let mut found: Vec<(String, Vec<Property>)> = Vec::new();
        match lower.as_str() {
            "none" => {}
            "all" => found.extend(Property::ALL[..Property::CORE].iter().filter(|&&p| !discrete(p)).map(|&p| (display_name(p).to_string(), vec![p]))),
            _ => match (Property::from_name(&lower), all_longhands(&lower)) {
                (Some(p), _) => found.push((display_name(p).to_string(), vec![p])),
                (None, Some(list)) => found.extend(list.iter().map(|&p| (display_name(p).to_string(), vec![p]))),
                _ => {}
            },
        }
        for (name, props) in found {
            let item = Item { name, props, dur, delay };
            match out.iter_mut().find(|o| o.name == item.name) {
                Some(o) => {
                    o.dur = dur;
                    o.delay = delay;
                }
                None => out.push(item),
            }
        }
    }
    out
}

/// The two halves of `transform` are one property for the page.
fn display_name(p: Property) -> &'static str {
    match p {
        Property::TranslateX | Property::TranslateY => "transform",
        p => p.name(),
    }
}

/// The text of a value that compares two styles of two cascades. `None`: the value is not one that a transition reads.
fn norm(v: Cv, calc: &dyn Fn(u32) -> (f32, f32)) -> Option<String> {
    match v {
        Cv::Grid(_) | Cv::Var(_) | Cv::Inherit | Cv::Initial | Cv::Unset => None,
        Cv::Length(Length::Calc(i)) => Some(format!("calc{:?}", calc(i))),
        v => Some(format!("{v:?}")),
    }
}

/// The value of the properties of `item` in `s`, as one text.
fn read(item: &Item, s: &ComputedStyle, calc: &dyn Fn(u32) -> (f32, f32)) -> Option<String> {
    let parts: Option<Vec<String>> = item.props.iter().map(|&p| norm(s.get(p), calc)).collect();
    parts.map(|p| p.join("|"))
}

struct Anim {
    name: String,
    dur: i64,
    delay: i64,
    count: i64,
}

fn animation_items(styles: &Styles, rules: &RuleSet, s: &ComputedStyle) -> Vec<Anim> {
    let Some(list) = names(styles, s, Property::AnimationName) else { return Vec::new() };
    let durs = times(styles, s, Property::AnimationDuration).unwrap_or_else(|| vec![0]);
    let delays = times(styles, s, Property::AnimationDelay).unwrap_or_else(|| vec![0]);
    let counts = times(styles, s, Property::AnimationIterationCount).unwrap_or_else(|| vec![1000]);
    let mut out: Vec<Anim> = Vec::new();
    for (i, name) in list.iter().enumerate() {
        if name == "none" || !rules.has_keyframes(name) || out.iter().any(|a| &a.name == name) {
            continue;
        }
        out.push(Anim { name: name.clone(), dur: durs[i % durs.len()], delay: delays[i % delays.len()], count: counts[i % counts.len()] });
    }
    out
}

fn hidden(s: &ComputedStyle) -> bool {
    s.get(Property::Display) == Cv::Keyword(Keyword::None)
}

impl Dom {
    pub(crate) fn motion_key_changed(&self, key: Key) -> bool {
        self.motion.s.borrow().last_key != Some(key)
    }

    pub(crate) fn motion_key(&self) -> Key {
        let doc = self.doc.borrow();
        let vp = self.plat.s.borrow().viewport;
        (doc.revision(), doc.text_revision(), (vp.0.to_bits(), vp.1.to_bits()), self.computed.sheet_gen.get(), doc.user().revision)
    }

    /// Should the scheduler queue a rendering step now? Marks one as pending when it says yes.
    pub(crate) fn motion_wants_frame(&self) -> bool {
        if !self.motion.s.borrow().armed {
            return false;
        }
        let key = self.motion_key();
        let mut s = self.motion.s.borrow_mut();
        if s.pending || s.last_key == Some(key) {
            return false;
        }
        s.pending = true;
        true
    }

    /// The part of the rendering step: style the document, so that a change is seen.
    pub(crate) fn motion_frame(&self, it: &mut Interp<'_>) -> R<()> {
        self.motion.s.borrow_mut().pending = false;
        if !self.motion.s.borrow().armed {
            return Ok(());
        }
        match self.cascade_now(it) {
            Err(Error::Unsupported(_)) => Ok(()),
            r => r.map(|_| ()),
        }
    }

    /// A cascade of the whole document is done: start and cancel what the change asks for.
    pub(crate) fn motion_seen(&self, it: &mut Interp<'_>, styles: &Styles) -> R<()> {
        let (keyframes, any) = {
            let cache = self.computed.cache.borrow();
            let Some(built) = cache.as_ref() else { return Ok(()) };
            (built.rules.has_motion(), styles.any_style(has_motion))
        };
        let key = self.motion_key();
        {
            let mut s = self.motion.s.borrow_mut();
            s.last_key = Some(key);
            s.armed = keyframes || any || !s.live.is_empty() || !s.animations.is_empty();
            if !s.armed {
                s.prev = None;
                return Ok(());
            }
        }
        let cache = self.computed.cache.borrow();
        let rules = &cache.as_ref().expect("checked").rules;
        let now = self.now_ms();
        let old = self.motion.s.borrow_mut().prev.take();
        let doc = self.doc.borrow();
        let mut seen_anims: HashSet<(NodeId, String)> = HashSet::new();
        let mut seen_props: HashSet<(NodeId, String)> = HashSet::new();
        let mut starts: Vec<(NodeId, Item, String)> = Vec::new();
        let mut begins: Vec<(NodeId, Anim)> = Vec::new();
        let mut stack = vec![doc.root()];
        while let Some(n) = stack.pop() {
            it.charge(1)?;
            let kids: Vec<NodeId> = doc.children(n).collect();
            stack.extend(kids.into_iter().rev());
            if !matches!(doc.data(n), NodeData::Element { .. }) {
                continue;
            }
            let Some(s) = styles.style(n) else {
                // Not rendered: its children are not either, and there is nothing to start.
                stack.truncate(stack.len() - doc.children(n).count());
                continue;
            };
            if hidden(s) {
                stack.truncate(stack.len() - doc.children(n).count());
                continue;
            }
            let new_calc = |i: u32| styles.calc(i);
            for item in transition_items(styles, s) {
                seen_props.insert((n, item.name.clone()));
                let Some(before) = old.as_ref().and_then(|o| o.style(n)) else { continue };
                let old_calc = |i: u32| old.as_ref().map_or((0.0, 0.0), |o| o.calc(i));
                let (Some(a), Some(b)) = (read(&item, before, &old_calc), read(&item, s, &new_calc)) else { continue };
                if a != b {
                    starts.push((n, item, b));
                }
            }
            for a in animation_items(styles, rules, s) {
                seen_anims.insert((n, a.name.clone()));
                begins.push((n, a));
            }
        }
        drop(doc);
        drop(cache);
        // What lives and is no longer asked for is cancelled.
        let mut gone: Vec<u64> = Vec::new();
        {
            let mut s = self.motion.s.borrow_mut();
            let (live, transitions, animations) = (&s.live, &s.transitions, &s.animations);
            let dead_transitions: Vec<(NodeId, String)> = transitions.iter().filter(|(k, id)| !seen_props.contains(*k) && live.get(id).is_some_and(|l| !l.cancelled)).map(|(k, _)| k.clone()).collect();
            let dead_animations: Vec<(NodeId, String)> = animations.keys().filter(|k| !seen_anims.contains(*k)).cloned().collect();
            for k in dead_transitions {
                if let Some(id) = s.transitions.remove(&k) {
                    gone.push(id);
                }
            }
            for k in dead_animations {
                if let Some(Some(id)) = s.animations.remove(&k) {
                    gone.push(id);
                }
            }
        }
        for id in gone {
            self.motion_cancel(id)?;
        }
        for (n, item, to) in starts {
            let existing = self.motion.s.borrow().transitions.get(&(n, item.name.clone())).copied();
            if let Some(id) = existing {
                let same = self.motion.s.borrow().live.get(&id).is_some_and(|l| matches!(&l.kind, Kind::Transition { to: t, .. } if *t == to));
                if same {
                    continue;
                }
                self.motion.s.borrow_mut().transitions.remove(&(n, item.name.clone()));
                self.motion_cancel(id)?;
            }
            self.motion_start_transition(n, &item, to, now)?;
        }
        for (n, a) in begins {
            if self.motion.s.borrow().animations.contains_key(&(n, a.name.clone())) {
                continue;
            }
            self.motion_start_animation(n, &a, now)?;
        }
        self.motion.s.borrow_mut().prev = Some(styles.map());
        Ok(())
    }

    fn motion_new(&self, live: Live) -> R<u64> {
        let mut s = self.motion.s.borrow_mut();
        if s.live.len() + s.animations.len() >= MAX_LIVE {
            return Err(self.lim("motion-live"));
        }
        s.next += 1;
        let id = s.next;
        s.live.insert(id, live);
        Ok(id)
    }

    fn motion_start_transition(&self, node: NodeId, item: &Item, to: String, now: f64) -> R<()> {
        let (dur_ms, delay_ms) = (item.dur as f64 / 1000.0, item.delay as f64 / 1000.0);
        let start = now + delay_ms.max(0.0);
        let id = self.motion_new(Live { node, kind: Kind::Transition { name: item.name.clone(), to }, start: now + delay_ms, delay_ms, dur_ms, iter: 0, cancelled: false })?;
        self.motion.s.borrow_mut().transitions.insert((node, item.name.clone()), id);
        self.schedule(now, Task::Motion(id, RUN))?;
        self.schedule(start, Task::Motion(id, START))?;
        self.schedule(now + delay_ms + dur_ms, Task::Motion(id, END))?;
        Ok(())
    }

    fn motion_start_animation(&self, node: NodeId, a: &Anim, now: f64) -> R<()> {
        let (dur_ms, delay_ms) = (a.dur.max(0) as f64 / 1000.0, a.delay as f64 / 1000.0);
        let id = self.motion_new(Live { node, kind: Kind::Animation { name: a.name.clone(), dur_ms, count: a.count }, start: now + delay_ms, delay_ms, dur_ms, iter: 0, cancelled: false })?;
        self.motion.s.borrow_mut().animations.insert((node, a.name.clone()), Some(id));
        self.schedule(now + delay_ms.max(0.0), Task::Motion(id, START))?;
        Ok(())
    }

    /// Mark `id` as cancelled and queue its `*cancel` event.
    fn motion_cancel(&self, id: u64) -> R<()> {
        {
            let mut s = self.motion.s.borrow_mut();
            let Some(l) = s.live.get_mut(&id) else { return Ok(()) };
            if l.cancelled {
                return Ok(());
            }
            l.cancelled = true;
        }
        self.schedule(self.now_ms(), Task::Motion(id, CANCEL))?;
        Ok(())
    }

    /// Queue what follows an animation's start or iteration: the next iteration, or the end.
    fn motion_next_iteration(&self, id: u64) -> R<()> {
        let (start, dur, k, count) = {
            let s = self.motion.s.borrow();
            let Some(l) = s.live.get(&id) else { return Ok(()) };
            let Kind::Animation { count, .. } = l.kind else { return Ok(()) };
            (l.start, l.dur_ms, l.iter + 1, count)
        };
        let end = if count == INFINITE { f64::INFINITY } else { dur * count as f64 / 1000.0 };
        if dur > 0.0 && (count == INFINITE || (k as f64) < count as f64 / 1000.0) {
            self.schedule(start + dur * k as f64, Task::Motion(id, if count == INFINITE { ITER_INFINITE } else { ITER }))?;
        } else if end.is_finite() {
            self.schedule(start + end, Task::Motion(id, END))?;
        }
        Ok(())
    }

    /// Run one step of the task of a transition or an animation.
    pub(crate) fn motion_task(&self, it: &mut Interp<'_>, id: u64, step: u8) -> R<()> {
        if step != CANCEL {
            // The element may be gone: the cascade finds it and cancels.
            match self.cascade_now(it) {
                Err(Error::Unsupported(_)) => {}
                r => r.map(|_| ())?,
            }
        }
        let (node, ty, name, elapsed, last) = {
            let mut s = self.motion.s.borrow_mut();
            let Some(l) = s.live.get_mut(&id) else { return Ok(()) };
            if l.cancelled != (step == CANCEL) {
                return Ok(());
            }
            let sec = |ms: f64| (ms / 1000.0 * 1e6).round() / 1e6;
            let active = |ms: f64| ms.clamp(0.0, l.dur_ms);
            match (&l.kind, step) {
                (Kind::Transition { name, .. }, RUN | START) => (l.node, if step == RUN { "transitionrun" } else { "transitionstart" }, name.clone(), sec(active(-l.delay_ms)), false),
                (Kind::Transition { name, .. }, END) => (l.node, "transitionend", name.clone(), sec(l.dur_ms), true),
                (Kind::Transition { name, .. }, CANCEL) => (l.node, "transitioncancel", name.clone(), sec(active(self.now_ms() - l.start)), true),
                (Kind::Animation { name, .. }, START) => (l.node, "animationstart", name.clone(), sec(active(-l.delay_ms)), false),
                (Kind::Animation { name, .. }, ITER | ITER_INFINITE) => {
                    l.iter += 1;
                    (l.node, "animationiteration", name.clone(), sec(l.dur_ms * l.iter as f64), false)
                }
                (Kind::Animation { name, count, dur_ms }, END) => (l.node, "animationend", name.clone(), sec(dur_ms * *count as f64 / 1000.0), true),
                (Kind::Animation { name, dur_ms, .. }, CANCEL) => (l.node, "animationcancel", name.clone(), sec((self.now_ms() - l.start).clamp(0.0, *dur_ms)), true),
                _ => return Ok(()),
            }
        };
        let animation = ty.starts_with("animation");
        if last {
            let mut s = self.motion.s.borrow_mut();
            let key = (node, name.clone());
            if animation {
                // It stays "finished" while the element has the name; a cancelled one is forgotten.
                if step == END {
                    s.animations.insert(key, None);
                } else if s.animations.get(&key) == Some(&Some(id)) {
                    s.animations.remove(&key);
                }
            } else if s.transitions.get(&key) == Some(&id) {
                s.transitions.remove(&key);
            }
            s.live.remove(&id);
        } else if animation {
            self.motion_next_iteration(id)?;
        }
        self.motion_fire(it, node, ty, if animation { "animationName" } else { "propertyName" }, &name, elapsed)
    }

    fn motion_fire(&self, it: &mut Interp<'_>, n: NodeId, ty: &str, key: &str, name: &str, elapsed: f64) -> R<()> {
        crate::browser_handlers::sync_path(self, n, ty);
        let listens = {
            let (doc, st) = (self.doc.borrow(), self.st.borrow());
            let on = |t: Target| st.listeners.get(&t).is_some_and(|v| v.iter().any(|l| &*l.ty == ty));
            let mut cur = n;
            let mut found = on(Target::Node(cur));
            while let Some(p) = doc.parent(cur) {
                it.charge(1)?;
                cur = p;
                found |= on(Target::Node(cur));
            }
            found || (cur == doc.root() && on(Target::Window))
        };
        if !listens {
            return Ok(());
        }
        let e = self.new_event(it, ty, true, false, false, None)?;
        self.st.borrow_mut().events.get_mut(&e).expect("just made").trusted = true;
        let s = self.js_str(it, name)?;
        let empty = self.js_str(it, "")?;
        for (k, v) in [(key, s), ("elapsedTime", Value::Number(elapsed)), ("pseudoElement", empty)] {
            let k = self.key(it, k)?;
            it.define_data(e, k, v, false, true, true)?;
        }
        self.dispatch(it, Target::Node(n), e).map(|_| ())
    }
}
