//! Chrome-checked cases of CSS and layout fixes: huge flex factors and the alignment of anonymous grid and flex items
//! (`tools/fixtures/ui/<topic>-cases.txt` against `chrome-<topic>.txt`, `node tools/ui/chrome_topic.mjs <topic>`).

use crate::browser_topic_tests::topic_test;

topic_test!(flexhuge_cases, "flexhuge", 7);
topic_test!(anonalign_cases, "anonalign", 23);
