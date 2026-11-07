//! Tests of `document.cookie` (`browser_cookie.rs`) against headless Chrome 155 on the document `https://a.test/`
//! (`tools/fixtures/ui/cookie-cases.txt`, `chrome-cookie.txt`).

use crate::browser_topic_tests::topic_test;

topic_test!(chrome_cases, "cookie", 19);
