//! Test support of the interpreter (S2.6): a builder for one function of a test program (`F`) and the root maps that a compiler
//! would write (`roots`: a liveness analysis). The unit tests of `interp_tests.rs` and the example `interp_load.rs` both include this
//! file with `#[path]`, so it takes its names from the `prelude` module of the includer. S2.5 (the compiler) replaces both.
#![allow(dead_code)]
use super::prelude::*;

/// One function under construction.
pub struct F {
    pub asm: Asm,
    pub nregs: u16,
    pub nparams: u16,
    pub numbers: Vec<f64>,
    pub atoms: Vec<Str>,
    pub funcs: Vec<u32>,
    pub recipes: Recipes,
    pub sites: Vec<u32>,
    pub handlers: Vec<(usize, usize, Label, u16, u16)>,
    /// The save lists of `GenInit` and `Yield` (`Code::tables`), and the kind of the function (`Code::gen_kind`).
    pub tables: Vec<u32>,
    pub gen_kind: u8,
    pub gen_regs: u16,
    /// Registers that stay in the root map of each safepoint although the program does not read them (a test of the map).
    pub pin: Vec<usize>,
    pub shared: Arc<SharedAtoms>,
}

impl F {
    pub fn new(shared: &Arc<SharedAtoms>, nregs: u16, nparams: u16) -> F {
        F { asm: Asm::default(), nregs, nparams, numbers: vec![], atoms: vec![], funcs: vec![], recipes: Recipes::default(), sites: vec![], handlers: vec![], tables: vec![], gen_kind: 0, gen_regs: 0, pin: vec![], shared: shared.clone() }
    }

    pub fn e(&mut self, op: Op, a: u32, b: u32, c: u32) -> usize {
        self.asm.emit(Ins::new(op, a, b, c))
    }

    pub fn ex(&mut self, op: Op, a: u32, b: u32, c: u32, x: u32) -> usize {
        self.asm.emit(Ins::new(op, a, b, c).with_x(x))
    }

    pub fn j(&mut self, op: Op, a: u32, b: u32, l: Label) -> usize {
        self.asm.jump(Ins::new(op, a, b, 0), l)
    }

    pub fn label(&mut self) -> Label {
        self.asm.label()
    }

    pub fn bind(&mut self, l: Label) {
        self.asm.bind(l)
    }

    pub fn atom(&mut self, s: &str) -> u32 {
        let a = self.shared.intern(Chars::One(s.as_bytes())).unwrap();
        if let Some(i) = self.atoms.iter().position(|x| *x == a) {
            return i as u32;
        }
        self.atoms.push(a);
        self.atoms.len() as u32 - 1
    }

    /// A new site for the name `s`: each site has one instruction.
    pub fn site(&mut self, s: &str) -> u32 {
        let a = self.atom(s);
        self.sites.push(a);
        self.sites.len() as u32 - 1
    }

    /// A jump of the forms with an immediate (`AsBxX`): `op a, imm -> l`.
    pub fn ji(&mut self, op: Op, a: u32, imm: i32, l: Label) -> usize {
        self.asm.jump(Ins::new(op, a, 0, 0).with_x(imm as u32), l)
    }

    pub fn li(&mut self, r: u32, v: i32) -> usize {
        self.asm.emit(Ins::new(Op::LoadI, r, v as u32, 0))
    }

    pub fn get(&mut self, dst: u32, obj: u32, name: &str) -> usize {
        let s = self.site(name);
        self.e(Op::GetField, dst, obj, s)
    }

    pub fn set(&mut self, obj: u32, val: u32, name: &str) -> usize {
        let s = self.site(name);
        self.e(Op::SetField, obj, val, s)
    }

    /// A save list in the table: the registers that the resume writes (`NO_REG`: none) and the registers that it keeps. Gives its offset.
    pub fn save_list(&mut self, sent: u32, mode: u32, regs: &[u32]) -> u32 {
        let at = self.tables.len() as u32;
        self.gen_regs = self.gen_regs.max(regs.len() as u16);
        self.tables.extend([sent, mode, regs.len() as u32]);
        self.tables.extend_from_slice(regs);
        at
    }

    /// `GenInit` with the save list `regs` (the parameters that the body reads).
    pub fn gen_init(&mut self, regs: &[u32]) -> usize {
        let t = self.save_list(NO_REG, NO_REG, regs);
        self.gen_kind = GK_GEN;
        self.e(Op::GenInit, 0, t, 0)
    }

    /// `Yield v` with the sent value in `sent`, the mode in `mode`, and the registers `regs` kept.
    pub fn yield_(&mut self, v: u32, sent: u32, mode: u32, regs: &[u32]) -> usize {
        let t = self.save_list(sent, mode, regs);
        self.gen_kind = GK_GEN;
        self.e(Op::Yield, v, t, 0)
    }

    pub fn build(self) -> Code {
        let code = self.build_unchecked();
        if let Err(e) = verify(&code) {
            panic!("the test program does not verify: {e}\n{}", code.disassemble());
        }
        code
    }

    /// The code as it is, for a test of the verifier.
    pub fn build_unchecked(self) -> Code {
        let a = self.asm.finish().unwrap();
        let mut code = Code::new(self.nregs, self.nparams);
        code.words = a.words;
        code.loops = a.loops;
        code.numbers = self.numbers;
        code.atoms = self.atoms;
        code.funcs = self.funcs;
        code.recipes = self.recipes;
        code.sites = self.sites;
        code.tables = self.tables;
        code.gen_kind = self.gen_kind;
        if self.gen_kind != 0 {
            code.noctor = true;
            code.gen_regs = self.gen_regs;
        }
        for (s, e, l, reg, depth) in self.handlers {
            code.handlers.push(Handler { start: a.pcs[s], end: a.pcs[e], target: a.label_pcs[l_idx(l)], reg, depth });
        }
        code.roots = roots(&code, &self.pin);
        code
    }
}

fn l_idx(l: Label) -> usize {
    // `Label` has no accessor; its debug form is `Label(n)`.
    format!("{l:?}").trim_start_matches("Label(").trim_end_matches(')').parse().unwrap()
}

fn window(i: &Ins, code: &Code) -> Option<usize> {
    Some(match i.op {
        Op::Call => 2 + i.c as usize,
        Op::CallU | Op::New => 1 + i.c as usize,
        Op::Concat | Op::NewArrLit => i.c as usize,
        Op::NewObjLit => code.recipes.get(i.c as usize).map_or(0, <[u32]>::len),
        Op::IterOpen | Op::ForInOpen | Op::ForInNext | Op::IterNext | Op::IterClose | Op::AIterOpen | Op::AIterNext | Op::AIterClose | Op::AIterCloseT => 3,
        Op::YieldStarStep | Op::AYsCall => 5,
        _ => return None,
    })
}

/// The root maps of a program: at each safepoint the registers that are live (read later, or by the instruction itself), the way a
/// compiler would write them, plus `pin`.
pub fn roots(code: &Code, pin: &[usize]) -> RootMaps {
    let mut ins = vec![];
    let mut pc = 0;
    while pc < code.words.len() {
        let (i, len) = decode(&code.words, pc).unwrap();
        ins.push((pc, i, len));
        pc += len;
    }
    let at = |p: i64| ins.iter().position(|x| x.0 as i64 == p).unwrap();
    let k = ins.len();
    let (mut uses, mut defs) = (vec![BTreeSet::new(); k], vec![BTreeSet::new(); k]);
    let mut succ: Vec<Vec<(usize, Option<usize>)>> = vec![vec![]; k];
    for (x, &(pc, i, len)) in ins.iter().enumerate() {
        let win = window(&i, code);
        let base = (0..i.op.form().fields()).find(|&f| matches!(i.op.letter(f), 'R' | 'W')).map(|f| i.field(f) as usize);
        for f in 0..i.op.form().fields() {
            match i.op.letter(f) {
                'r' => {
                    uses[x].insert(i.field(f) as usize);
                }
                'w' => {
                    defs[x].insert(i.field(f) as usize);
                }
                'R' => uses[x].extend(base.unwrap()..base.unwrap() + win.unwrap()),
                'W' => defs[x].extend(base.unwrap()..base.unwrap() + win.unwrap()),
                _ => {}
            }
        }
        if matches!(i.op, Op::GenInit | Op::Yield | Op::Await) {
            // A suspension reads the registers of its save list, and the resume writes the sent value and the mode.
            let l = code.save_list(i.b).expect("a save list");
            uses[x].extend(l.regs.iter().map(|r| *r as usize));
            defs[x].extend([l.sent, l.mode].into_iter().filter(|r| *r != NO_REG).map(|r| r as usize));
        }
        if let Some(t) = i.target(pc, len) {
            succ[x].push((at(t), None));
        }
        if i.op.falls_through() && x + 1 < k {
            succ[x].push((x + 1, None));
        }
        for h in &code.handlers {
            if (h.start as usize..h.end as usize).contains(&pc) && i.op.is_safepoint() {
                succ[x].push((at(h.target as i64), Some(h.reg as usize)));
            }
        }
    }
    let mut live_in = vec![BTreeSet::<usize>::new(); k];
    loop {
        let mut changed = false;
        for x in (0..k).rev() {
            let mut out = BTreeSet::new();
            for &(s, kill) in &succ[x] {
                out.extend(live_in[s].iter().copied().filter(|r| Some(*r) != kill));
            }
            let mut new: BTreeSet<usize> = out.difference(&defs[x]).copied().collect();
            new.extend(uses[x].iter().copied());
            if new != live_in[x] {
                live_in[x] = new;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let mut maps = RootMaps::new(code.nregs as usize);
    for (x, &(pc, i, _)) in ins.iter().enumerate() {
        if i.op.is_safepoint() {
            // A pinned register is in the map from the first instruction after its first write (a straight-line test program).
            let pinned = pin.iter().copied().filter(|r| defs[..x].iter().any(|d| d.contains(r)));
            maps.add(pc as u32, live_in[x].iter().copied().chain(pinned).collect::<BTreeSet<_>>());
        }
    }
    maps
}
