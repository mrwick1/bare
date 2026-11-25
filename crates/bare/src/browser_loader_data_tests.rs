//! Tests of parser-found `<script src="data:...">` (HTML Standard, "fetch a classic script" of a `data:` URL; Fetch, "data: URL
//! processor"). WhatsApp Web runs its boot steps from `data:text/javascript;base64,...` scripts.

use crate::browser_loader_tests::{PRE, big, page, quick};

#[test]
fn data_scripts_run_in_page_order() {
    big(|| {
        // `L('b')` and `L('c')` in base64; the defer script runs after the parse.
        let html = format!(
            "<head>{PRE}<script src=\"data:text/javascript;base64,TCgnYicp\"></script>\
             <script src=\"data:application/x-javascript; charset=utf-8;base64,TCgn Yycp\"></script>\
             <script defer src=\"data:text/javascript,L('d')\"></script>\
             <script src=\"data:text/javascript,L(%22e%22)\"></script>\
             <script>L('f')</script></head>"
        );
        let (log, _, _) = page(&html, vec![], quick());
        assert_eq!(log, "b:loading|c:loading|e:loading|f:loading|d:interactive");
    });
}

#[test]
fn data_script_fires_load_and_bad_one_fires_error() {
    big(|| {
        let html = format!(
            "<head>{PRE}<script id=a src=\"data:,L('a')\" onload=\"L('a-load')\"></script>\
             <script src=\"data:text/javascript;base64,%%%\" onerror=\"L('bad-error')\"></script></head>"
        );
        let (log, _, _) = page(&html, vec![], quick());
        assert_eq!(log, "a:loading|a-load:loading|bad-error:loading");
    });
}
