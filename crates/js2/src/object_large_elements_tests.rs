use crate::atoms::{Atoms, SharedAtoms};
use crate::cell::{Layout, Shapes};
use crate::gc::{Marker, collect};
use crate::heap::{Heap, MAX_SMALL};
use crate::object::Obj;
use crate::value::Value;
use std::sync::Arc;

#[test]
fn growing_dense_arrays_releases_the_old_mapping_and_preserves_live_arrays() {
    let mut heap = Heap::default();
    let mut shapes = Shapes::default();
    let mut atoms = Atoms::new(Arc::new(SharedAtoms::default()));
    let shape = shapes.root(Layout::Indexed, None, 0);
    let n = MAX_SMALL / 4;
    let values = vec![Value::int(9); n];
    Obj::array(&mut heap, &shapes, shape, &values).unwrap();
    let a = Obj::array(&mut heap, &shapes, shape, &values).unwrap();
    let b = Obj::array(&mut heap, &shapes, shape, &values).unwrap();
    collect(&mut heap, &mut shapes, &mut atoms, &mut |m: &mut Marker| m.values(&[a.value(), b.value()]));

    let mapped = heap.mapped_bytes();
    for array in [a, b] {
        array.set_element(&mut heap, n as u32, Value::int(10)).unwrap();
        assert_eq!(heap.mapped_bytes(), mapped, "one large record replaces another");
        assert_eq!(array.len(), n as u32 + 1);
        assert_eq!(array.get_element(n as u32 - 1), Some(Value::int(9)));
        assert_eq!(array.get_element(n as u32), Some(Value::int(10)));
    }
}
