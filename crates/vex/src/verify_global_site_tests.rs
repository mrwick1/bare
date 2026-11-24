use super::*;
use crate::bytecode::{Ins, encode_in};
use crate::roots::RootMaps;

#[test]
fn global_site_must_be_in_range_in_both_encodings() {
    for op in [Op::GetGlobal, Op::SetGlobal, Op::SetGlobalS] {
        for wide in [false, true] {
            let mut code = Code::new(1, 1);
            code.roots = RootMaps::new(1);
            code.roots.add(0, [0]);
            encode_in(&mut code.words, &Ins::new(op, 0, if wide { 65_536 } else { 0 }, 0), wide).unwrap();
            encode_in(&mut code.words, &Ins::new(Op::RetUndef, 0, 0, 0), false).unwrap();
            let err = verify(&code).unwrap_err();
            assert!(err.contains("no such site"), "{err}");
        }
    }
}
