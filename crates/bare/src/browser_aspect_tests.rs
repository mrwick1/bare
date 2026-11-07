//! Tests of `aspect-ratio` (CSS Sizing 4, read through `getBoundingClientRect`).
//! The Chrome cases (`tools/fixtures/ui/aspect-cases.txt`): width and height of each box against headless Chrome 155.

use crate::browser_topic_tests::topic_test;

topic_test!(chrome_cases, "aspect", 10);
