//! Which of the two buffers to draw into. Plain logic with no Wayland objects, so a test can check it.
//!
//! A buffer is *busy* from the commit that attaches it until the compositor sends `release`. The program never draws
//! into a busy buffer. A free buffer of the right size is used again. A free buffer of another size is made again, and
//! only then, so a window that keeps its size allocates nothing after its first two frames.

pub const COUNT: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    /// Draw into slot `i`: it has the right size.
    Reuse(usize),
    /// Slot `i` is free but empty or of another size: make it again, then draw.
    Recreate(usize),
    /// Both slots are busy: wait for a `release`.
    Wait,
}

#[derive(Debug, Default)]
pub struct Slots {
    size: [Option<(u32, u32)>; COUNT],
    busy: [bool; COUNT],
}

impl Slots {
    pub fn pick(&self, size: (u32, u32)) -> Pick {
        if let Some(i) = (0..COUNT).find(|&i| !self.busy[i] && self.size[i] == Some(size)) {
            return Pick::Reuse(i);
        }
        match (0..COUNT).find(|&i| !self.busy[i]) {
            Some(i) => Pick::Recreate(i),
            None => Pick::Wait,
        }
    }

    /// Is a slot free? (A free slot may still need to be made again.)
    pub fn any_free(&self) -> bool {
        self.busy.iter().any(|b| !b)
    }

    /// Slot `i` now has a buffer of `size`.
    pub fn made(&mut self, i: usize, size: (u32, u32)) {
        debug_assert!(!self.busy[i], "a busy buffer is never made again");
        self.size[i] = Some(size);
    }

    /// Slot `i` was attached and committed.
    pub fn attached(&mut self, i: usize) {
        debug_assert!(!self.busy[i], "a busy buffer is never attached");
        self.busy[i] = true;
    }

    /// The compositor sent `release` for slot `i`.
    pub fn released(&mut self, i: usize) {
        self.busy[i] = false;
    }

    /// The number of buffers that exist (made and not dropped).
    pub fn live(&self) -> usize {
        self.size.iter().flatten().count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_frames_use_two_buffers_and_then_they_are_used_again() {
        let mut s = Slots::default();
        let size = (100, 50);
        let first = s.pick(size);
        assert_eq!(first, Pick::Recreate(0));
        s.made(0, size);
        s.attached(0);
        assert_eq!(s.pick(size), Pick::Recreate(1), "slot 0 is held: the second buffer is made");
        s.made(1, size);
        s.attached(1);
        assert_eq!(s.pick(size), Pick::Wait, "both are held");
        assert!(!s.any_free());
        s.released(0);
        assert_eq!(s.pick(size), Pick::Reuse(0));
        s.attached(0);
        s.released(1);
        assert_eq!(s.pick(size), Pick::Reuse(1));
        assert_eq!(s.live(), 2);
    }

    #[test]
    fn a_free_buffer_of_the_right_size_beats_a_free_one_of_the_wrong_size() {
        let mut s = Slots::default();
        s.made(0, (10, 10));
        s.made(1, (20, 20));
        assert_eq!(s.pick((20, 20)), Pick::Reuse(1));
        assert_eq!(s.pick((10, 10)), Pick::Reuse(0));
        assert_eq!(s.pick((30, 30)), Pick::Recreate(0));
    }

    #[test]
    fn a_resize_never_touches_a_held_buffer() {
        let mut s = Slots::default();
        s.made(0, (10, 10));
        s.attached(0);
        s.made(1, (10, 10));
        s.attached(1);
        s.released(1);
        // Slot 0 is still held by the compositor, slot 1 is free: the new size goes to slot 1.
        assert_eq!(s.pick((20, 20)), Pick::Recreate(1));
        s.made(1, (20, 20));
        s.attached(1);
        assert_eq!(s.pick((20, 20)), Pick::Wait);
        s.released(0);
        assert_eq!(s.pick((20, 20)), Pick::Recreate(0));
    }
}
