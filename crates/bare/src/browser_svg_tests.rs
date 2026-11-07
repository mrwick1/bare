//! Tests of the box of an `<svg>` (SVG 2 sizing, read through `getBoundingClientRect`).
//! The Chrome cases (`tools/fixtures/ui/svg-cases.txt`): place and size of each box against headless Chrome 155.

use crate::browser_topic_tests::topic_test;

topic_test!(chrome_cases, "svg", 7);
