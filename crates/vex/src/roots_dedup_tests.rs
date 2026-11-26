// Tests of the sealed root maps (`docs/internal/design-code-memory.md`, R1): each distinct map is kept once, and each safepoint finds
// the same registers as before the seal.
use super::*;

fn live(m: &RootMaps, pc: u32) -> Vec<usize> {
    m.find(pc).expect("a map").slots().collect()
}

#[test]
fn a_sealed_map_finds_the_same_registers_and_is_smaller() {
    for regs in [5, 64, 130] {
        let mut m = RootMaps::new(regs);
        let sets: [&[usize]; 4] = [&[0, 2], &[1], &[0, 2], &[regs - 1]];
        for pc in 0..400u32 {
            m.add(pc * 3, sets[pc as usize % 4].iter().copied());
        }
        let before: Vec<_> = (0..400).map(|pc| live(&m, pc * 3)).collect();
        let bytes = m.bytes();
        let m = m.seal();
        assert_eq!((0..400).map(|pc| live(&m, pc * 3)).collect::<Vec<_>>(), before, "{regs} registers");
        assert!(m.find(1).is_none(), "no safepoint at pc 1");
        assert!(m.bytes() * 2 < bytes, "{regs} registers: {} of {bytes} bytes", m.bytes());
    }
}

#[test]
fn a_code_keeps_its_live_values_across_collections() {
    // Many safepoints with repeated maps in one function; the collection must still keep each live register.
    let src = "function f(n) { var a = [1], b = {x: 2}, s = 0; for (var i = 0; i < n; i++) { var t = [i, i + 1]; s += t[1] + a[0] + b.x; } return s; }\n\
               var r = 0; for (var k = 0; k < 30; k++) r += f(2000);";
    let mut vm = crate::script::run(src).unwrap();
    assert_eq!(vm.global_text("r"), (30 * (0..2000).map(|i| i + 1 + 1 + 2).sum::<i64>()).to_string());
}
