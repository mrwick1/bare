use super::Census;

#[test]
fn a_miss_gets_a_line_at_each_power_of_two() {
    let mut c = Census::new(Vec::new());
    for _ in 0..9 {
        c.add("Window".into(), "fooBar".into(), true);
    }
    c.add("Window".into(), "fooBar".into(), false);
    c.add("HTMLDivElement".into(), "x".into(), true);
    let out = String::from_utf8(c.out).unwrap();
    assert_eq!(
        out,
        "Window\tfooBar\tget\t1\nWindow\tfooBar\tget\t2\nWindow\tfooBar\tget\t4\nWindow\tfooBar\tget\t8\n\
         Window\tfooBar\thas\t1\nHTMLDivElement\tx\tget\t1\n"
    );
}
