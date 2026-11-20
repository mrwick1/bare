// Tests of `atoms.rs`. Miri runs them in CI, so the counts shrink under it.
use super::*;
use std::collections::HashSet;

const N: usize = if cfg!(miri) { 120 } else { 5_000 };

fn realm() -> (Heap, Atoms) {
    (Heap::default(), Atoms::new(Arc::new(SharedAtoms::default())))
}

fn name(i: usize) -> String {
    format!("name{i}")
}

#[test]
fn one_cell_for_each_content_and_form() {
    let (mut h, mut a) = realm();
    let x = a.intern(&mut h, Chars::One(b"foo")).unwrap();
    assert_eq!(a.intern(&mut h, Chars::One(b"foo")).unwrap(), x);
    // The same units as 16-bit units find the same atom.
    assert_eq!(a.intern(&mut h, Chars::Two(&[102, 111, 111])).unwrap(), x);
    assert!(x.is_atom() && !x.is_shared() && x.cached_hash() != 0);
    let w = a.intern(&mut h, Chars::Two(&[0x20AC])).unwrap();
    assert_eq!(a.intern(&mut h, Chars::Two(&[0x20AC])).unwrap(), w);
    assert_ne!(w, x);
    assert_eq!(a.runtime().len(), 2);
}

#[test]
fn the_table_grows_and_finds_every_atom() {
    let (mut h, mut a) = realm();
    let all: Vec<Str> = (0..N).map(|i| a.intern(&mut h, Chars::One(name(i).as_bytes())).unwrap()).collect();
    assert_eq!(all.iter().map(|x| x.cell()).collect::<HashSet<_>>().len(), N);
    for (i, &x) in all.iter().enumerate() {
        assert_eq!(a.intern(&mut h, Chars::One(name(i).as_bytes())).unwrap(), x);
        assert_eq!(a.runtime().find(&Chars::One(name(i).as_bytes())), Some(x));
    }
    assert_eq!(a.runtime().find(&Chars::One(b"absent")), None);
    assert_eq!(a.runtime().len(), N);
}

#[test]
fn a_string_becomes_an_atom_in_place() {
    let (mut h, mut a) = realm();
    let s = Str::from_str(&mut h, "key").unwrap();
    assert!(!s.is_atom());
    let at = a.atomize(&mut h, s).unwrap();
    assert_eq!(at, s);
    assert!(s.is_atom());
    assert_eq!(a.atomize(&mut h, s).unwrap(), s);
    // An equal string that is a different cell gives the atom.
    let t = Str::from_str(&mut h, "key").unwrap();
    assert_eq!(a.atomize(&mut h, t).unwrap(), s);
    assert!(!t.is_atom());
    // A rope is flattened and its copy becomes the atom.
    let (p, q) = (Str::from_str(&mut h, &"p".repeat(40)).unwrap(), Str::from_str(&mut h, "q").unwrap());
    let r = Str::concat(&mut h, p, q).unwrap();
    let ra = a.atomize(&mut h, r).unwrap();
    assert!(ra.is_atom() && !ra.is_rope() && r.resolve() == ra);
    assert_eq!(a.intern(&mut h, Chars::One(format!("{}q", "p".repeat(40)).as_bytes())).unwrap(), ra);
}

#[test]
fn source_atoms_are_shared_by_the_realms_and_outside_their_heaps() {
    let shared = Arc::new(SharedAtoms::default());
    let (mut h1, mut h2) = (Heap::default(), Heap::default());
    let (mut r1, mut r2) = (Atoms::new(shared.clone()), Atoms::new(shared.clone()));
    let x = shared.intern(Chars::One(b"length")).unwrap();
    assert!(x.is_shared() && x.is_atom() && x.cached_hash() != 0);
    assert_eq!(shared.intern(Chars::Two(&[108, 101, 110, 103, 116, 104])).unwrap(), x);
    assert_eq!(shared.len(), 1);
    // Both realms get the shared cell, and neither makes a copy in its own heap.
    assert_eq!(r1.intern(&mut h1, Chars::One(b"length")).unwrap(), x);
    assert_eq!(r2.intern(&mut h2, Chars::One(b"length")).unwrap(), x);
    assert_eq!((r1.runtime().len(), r2.runtime().len()), (0, 0));
    assert_eq!(r1.adopt(x), x);
    assert_eq!(r1.atomize(&mut h1, x).unwrap(), x);
    // A string equal to a shared atom atomizes to the shared atom.
    let s = Str::from_str(&mut h2, "length").unwrap();
    assert_eq!(r2.atomize(&mut h2, s).unwrap(), x);
    assert!(!s.is_atom());
    // Realm 1 allocated nothing; realm 2 allocated only the plain string `s`.
    assert!(shared.mapped_bytes() > 0 && h1.mapped_bytes() == 0 && h2.mapped_bytes() > 0);
}

#[test]
fn a_run_time_atom_wins_over_a_later_source_atom_in_its_own_realm() {
    let shared = Arc::new(SharedAtoms::default());
    let (mut h1, mut h2) = (Heap::default(), Heap::default());
    let (mut r1, mut r2) = (Atoms::new(shared.clone()), Atoms::new(shared.clone()));
    let own = r1.intern(&mut h1, Chars::One(b"late")).unwrap();
    assert!(!own.is_shared());
    let src = shared.intern(Chars::One(b"late")).unwrap();
    assert_ne!(src, own);
    // Realm 1 keeps one atom for the content. Realm 2 never saw the run-time atom and uses the shared one.
    assert_eq!(r1.adopt(src), own);
    assert_eq!(r1.intern(&mut h1, Chars::One(b"late")).unwrap(), own);
    assert_eq!(r1.atomize(&mut h1, src).unwrap(), own);
    assert_eq!(r2.adopt(src), src);
    assert_eq!(r2.intern(&mut h2, Chars::One(b"late")).unwrap(), src);
}

#[test]
fn threads_that_intern_the_same_names_get_the_same_cells() {
    let shared = Arc::new(SharedAtoms::default());
    let n = if cfg!(miri) { 20 } else { 500 };
    let run = |shared: Arc<SharedAtoms>| {
        move || (0..n).map(|i| shared.intern(Chars::One(name(i).as_bytes())).unwrap().cell().as_ptr() as usize).collect::<Vec<_>>()
    };
    let hs: Vec<_> = (0..3).map(|_| std::thread::spawn(run(shared.clone()))).collect();
    let results: Vec<Vec<usize>> = hs.into_iter().map(|h| h.join().unwrap()).collect();
    assert!(results.windows(2).all(|w| w[0] == w[1]));
    assert_eq!(results[0].iter().collect::<HashSet<_>>().len(), n);
    assert_eq!(shared.len(), n);
}

#[test]
fn the_sweep_hook_drops_only_the_atoms_that_the_mark_phase_left_unmarked() {
    let (mut h, mut a) = realm();
    let all: Vec<Str> = (0..N).map(|i| a.intern(&mut h, Chars::One(name(i).as_bytes())).unwrap()).collect();
    h.start_mark();
    for x in all.iter().step_by(2) {
        // SAFETY: a live cell of `h`.
        unsafe { h.mark(x.cell()) };
    }
    let dropped = a.sweep(&h);
    h.finish_mark();
    assert_eq!(dropped, N / 2);
    assert_eq!(a.runtime().len(), N - N / 2);
    for (i, &x) in all.iter().enumerate() {
        let found = a.runtime().find(&Chars::One(name(i).as_bytes()));
        assert_eq!(found, (i % 2 == 0).then_some(x), "{i}");
    }
    // A dropped name makes a new atom. The table still works after the rebuild.
    let again = a.intern(&mut h, Chars::One(name(1).as_bytes())).unwrap();
    assert_eq!(a.intern(&mut h, Chars::One(name(1).as_bytes())).unwrap(), again);
    assert_eq!(a.runtime().len(), N - N / 2 + 1);
}

#[test]
fn a_sweep_of_a_mostly_dead_table_shrinks_it() {
    let (mut h, mut a) = realm();
    for i in 0..N {
        a.intern(&mut h, Chars::One(name(i).as_bytes())).unwrap();
    }
    let big = a.runtime.table.slots.len();
    h.start_mark();
    assert_eq!(a.sweep(&h), N);
    h.finish_mark();
    assert!(a.runtime().is_empty() && a.runtime.table.slots.len() < big / 8);
    assert!(a.intern(&mut h, Chars::One(b"x")).is_ok());
}
