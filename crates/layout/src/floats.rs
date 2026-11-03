//! Floats: boxes with `float: left` or `float: right` (CSS 2.1, section 9.5). A float leaves the normal flow: it goes to
//! the left or the right edge of its containing block, and the lines next to it get shorter, so text flows around it.
//! Blocks do not move for a float: only their lines do. A block that makes its own block formatting context (BFC), like
//! an inline-block or a float, does not let floats from outside in: its content has its own floats.
//!
//! `Floats` is the "exclusion space" of one BFC: the margin boxes of the floats that are placed so far, in the
//! coordinates of the BFC (from the top-left of its root's border box). Layout asks it two questions:
//! - `band`: how much room is left between the floats, at this height? A line or a BFC root goes there.
//! - `place`: where does a new float go?
//!
//! The rules of `place` (CSS 2.1, 9.5.1), for a left float (a right float is the mirror):
//! - It is as high as it can go, but not higher than the line or block where it is in the source, and not higher than
//!   a float placed before it.
//! - It is as far left as it can go: against the containing block's edge, or against a left float before it.
//! - If it does not fit next to the floats at that height, it goes down, to the bottom of the next float that ends.
//!
//! How the real engines keep floats: Firefox keeps a list of the floats of a BFC (`nsFloatManager`) and walks it for each
//! question. That costs the number of floats for each line: 20,000 floats with a paragraph each took 840 ms here, and
//! 200,000 took more than 20 s. Chrome (LayoutNG) keeps an `NGExclusionSpace`: "shelves" that it updates when a float
//! is added, so that a question costs about the number of shelves.
//!
//! Our choice is close to Chrome's. A question never needs the floats themselves, only the free room at each height:
//! a step function of `y`. `Floats` keeps it as a sorted list of `Step`s: from its `y` down to the next step, the room
//! is `l..r`. A question is a binary search and a look at the steps that the box covers. A new float splits at most
//! two steps and narrows the steps that it covers. A float never goes higher than the one before it, so the splits are
//! near the end of the list, and the copy that an insert in a `Vec` makes is short. 200,000 floats with a paragraph each:
//! 213 ms (`docs/resources.md`).
//!
//! Comments compare Rust with JavaScript, because you know JS.

/// A rounding margin: sums of `f32` are not exact.
const EPS: f32 = 0.01;

/// The free room from `y` down to the next step: `l` is the right edge of the left floats there, `r` is the left edge
/// of the right floats. With no float, they are -infinity and +infinity.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Step {
    y: f32,
    l: f32,
    r: f32,
}

/// The floats of one block formatting context. `Default` is empty: a new BFC starts with no floats.
#[derive(Debug)]
pub struct Floats {
    /// Sorted by `y`. The first step starts at -infinity, so every `y` is in one step.
    steps: Vec<Step>,
    /// The top of the last float. A new float may not go higher (rule 6 of CSS 2.1, 9.5.1).
    last_top: f32,
    /// The bottom of the lowest left float and right float, for `clear`.
    bottom_left: Option<f32>,
    bottom_right: Option<f32>,
}

impl Default for Floats {
    fn default() -> Floats {
        Floats { steps: Vec::new(), last_top: f32::NEG_INFINITY, bottom_left: None, bottom_right: None }
    }
}

impl Floats {
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// The index of the step that holds `y`.
    fn at(&self, y: f32) -> usize {
        // `partition_point`: the number of steps that start at or above `y` (a binary search). The last of them holds `y`.
        self.steps.partition_point(|s| s.y <= y).saturating_sub(1)
    }

    /// The steps that a box from `y` to `y + h` covers. A box with no height still meets a float that starts at its `y`.
    fn covered(&self, y: f32, h: f32) -> impl Iterator<Item = &Step> {
        let end = y + h.max(EPS);
        self.steps[self.at(y)..].iter().take_while(move |s| s.y < end)
    }

    /// The room between the floats for a box from `y` to `y + h`, inside the edges `left..right`: (left, right) of the
    /// free part. With no float there, it is `(left, right)`.
    pub fn band(&self, y: f32, h: f32, left: f32, right: f32) -> (f32, f32) {
        let (mut l, mut r) = (left, right);
        for s in self.covered(y, h) {
            l = l.max(s.l);
            r = r.min(s.r);
        }
        (l, r.max(l))
    }

    /// The first place below `y` where the room for a box from `y` to `y + h` can get wider: the end of the first step it
    /// covers. (It can be the top of a float, where the room gets narrower. The caller asks again from there, and finds
    /// the same place as if it went to the next bottom of a float at once: no float that covers the box ends before it.)
    /// `None` if no step ends below `y`.
    pub fn next_bottom(&self, y: f32, _h: f32) -> Option<f32> {
        self.steps.get(self.at(y) + 1).map(|s| s.y)
    }

    /// The bottom of the lowest float on the given sides: where a box with `clear` starts. `None` if there is none.
    pub fn clear(&self, left: bool, right: bool) -> Option<f32> {
        let l = self.bottom_left.filter(|_| left);
        let r = self.bottom_right.filter(|_| right);
        l.into_iter().chain(r).reduce(f32::max)
    }

    /// Make a step start at `y`, and give its index.
    fn split(&mut self, y: f32) -> usize {
        let i = self.at(y);
        if self.steps[i].y == y {
            return i;
        }
        let step = Step { y, ..self.steps[i] };
        self.steps.insert(i + 1, step);
        i + 1
    }

    /// Place a float with a margin box of `w` by `h`, not higher than `y`, inside the edges `left..right`. Gives the
    /// top-left corner of its margin box.
    pub fn place(&mut self, is_left: bool, w: f32, h: f32, y: f32, left: f32, right: f32) -> (f32, f32) {
        let mut y = y.max(self.last_top);
        let (l, r) = loop {
            let (l, r) = self.band(y, h, left, right);
            if r - l >= w - EPS || (l, r) == (left, right) {
                // It fits, or no float is in the way: a float wider than its containing block sticks out.
                break (l, r);
            }
            match self.next_bottom(y, h) {
                Some(next) => y = next,
                None => break (l, r),
            }
        };
        let x = if is_left { l } else { r - w };
        if self.steps.is_empty() {
            self.steps.push(Step { y: f32::NEG_INFINITY, l: f32::NEG_INFINITY, r: f32::INFINITY });
        }
        // A margin box can have a negative height (negative margins): then it takes no room.
        let (from, to) = (self.split(y), self.split(y + h.max(0.0)));
        for s in &mut self.steps[from..to] {
            if is_left {
                s.l = s.l.max(x + w);
            } else {
                s.r = s.r.min(x);
            }
        }
        let side = if is_left { &mut self.bottom_left } else { &mut self.bottom_right };
        *side = Some(side.map_or(y + h, |b| b.max(y + h)));
        self.last_top = y;
        (x, y)
    }

    /// The bottom of the lowest float. A BFC root with `height: auto` grows to contain its floats (CSS 2.1, 10.6.7).
    pub fn bottom(&self) -> Option<f32> {
        self.clear(true, true)
    }
}

#[cfg(test)]
mod tests {
    use super::Floats;

    #[test]
    fn floats_go_to_the_edges_and_stack() {
        let mut f = Floats::default();
        assert_eq!(f.place(true, 100.0, 50.0, 0.0, 0.0, 500.0), (0.0, 0.0));
        assert_eq!(f.place(true, 100.0, 20.0, 0.0, 0.0, 500.0), (100.0, 0.0));
        assert_eq!(f.place(false, 100.0, 80.0, 0.0, 0.0, 500.0), (400.0, 0.0));
        // The room left at the top: 200 to 400. Below the second float (20), 100 to 400. Below all, everything.
        assert_eq!(f.band(0.0, 10.0, 0.0, 500.0), (200.0, 400.0));
        assert_eq!(f.band(30.0, 10.0, 0.0, 500.0), (100.0, 400.0));
        assert_eq!(f.band(80.0, 10.0, 0.0, 500.0), (0.0, 500.0));
        assert_eq!(f.clear(true, false), Some(50.0));
        assert_eq!(f.clear(false, true), Some(80.0));
        assert_eq!(f.bottom(), Some(80.0));
    }

    #[test]
    fn a_float_that_does_not_fit_goes_down() {
        let mut f = Floats::default();
        f.place(true, 300.0, 50.0, 0.0, 0.0, 500.0);
        f.place(true, 100.0, 30.0, 0.0, 0.0, 500.0);
        // 150 does not fit next to 400 at the top. At 30 the second float ends, and there are 200 next to the first.
        assert_eq!(f.place(true, 150.0, 10.0, 0.0, 0.0, 500.0), (300.0, 30.0));
        // Not higher than the float before it, even if there is room above.
        assert_eq!(f.place(false, 10.0, 10.0, 0.0, 0.0, 500.0), (490.0, 30.0));
        // 400 fits nowhere next to them: it goes below all of them.
        assert_eq!(f.place(true, 400.0, 10.0, 0.0, 0.0, 500.0), (0.0, 50.0));
    }

    #[test]
    fn a_float_wider_than_its_block_sticks_out() {
        let mut f = Floats::default();
        // A right float keeps its right edge on the right edge of the block, and sticks out on the left (as in Chrome).
        assert_eq!(f.place(false, 600.0, 10.0, 5.0, 0.0, 500.0), (-100.0, 5.0));
    }

    #[test]
    fn a_float_with_a_negative_height_takes_no_room() {
        // Found by the fuzzer: `margin-bottom: -50px` on a float 20 tall makes its margin box -30 tall.
        let mut f = Floats::default();
        // A step already starts at -30, where this float ends: the end of its range came before its start.
        f.place(false, 10.0, 10.0, -40.0, 0.0, 500.0);
        f.place(true, 10.0, -30.0, 0.0, 0.0, 500.0);
        assert_eq!(f.band(0.0, 10.0, 0.0, 500.0), (0.0, 500.0));
        assert_eq!(f.clear(true, false), Some(-30.0));
    }

    #[test]
    fn a_float_may_go_above_zero() {
        // Negative margins can put content, and so a float, above the top of the BFC.
        let mut f = Floats::default();
        assert_eq!(f.place(true, 10.0, 10.0, -20.0, 0.0, 500.0), (0.0, -20.0));
        assert_eq!(f.band(-15.0, 1.0, 0.0, 500.0), (10.0, 500.0));
        assert_eq!(f.band(-10.0, 1.0, 0.0, 500.0), (0.0, 500.0));
    }
}
