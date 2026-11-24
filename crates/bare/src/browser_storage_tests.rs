//! Tests of the named properties of `localStorage` and `sessionStorage` (WebIDL `Storage`).
//! The Chrome cases (`tools/fixtures/ui/storage-cases.txt`) against headless Chrome 155, on the document `https://a.test/`.

use crate::browser_topic_tests::topic_test;

topic_test!(#[ignore = "vex: 6 of 45 cases differ (Storage named properties, freeze and seal)"] chrome_cases, "storage", 45);
