//! Tests of baseline alignment in flex and grid containers (CSS Box Alignment 3, CSS Flexbox 1 section 8.5 and CSS Grid 2 section 6.6), read through
//! `getBoundingClientRect`. The Chrome cases (`tools/fixtures/ui/baseline-cases.txt`): the box of each item against headless Chrome 155.

use crate::browser_topic_tests::topic_test;

topic_test!(chrome_cases, "baseline", 70);
