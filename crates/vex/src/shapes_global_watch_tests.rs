use super::Shapes;

#[test]
fn changing_the_watched_global_invalidates_cached_slots() {
    let mut shapes = Shapes::default();
    shapes.watch(1);
    let first = shapes.watch_tag();
    shapes.watch(1);
    assert_eq!(shapes.watch_tag(), first);
    shapes.watch(2);
    let second = shapes.watch_tag();
    assert_ne!(second, first);
    shapes.changed(1);
    assert_eq!(shapes.watch_tag(), second);
    shapes.changed(2);
    assert_ne!(shapes.watch_tag(), second);
}
