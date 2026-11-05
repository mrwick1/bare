//! CDATA sections (HTML 13.2.5.69 to 13.2.5.72). They exist only in foreign content: the tree builder says so with `set_foreign`.
//! The section is plain text. No character reference is decoded and no tag is read. It ends at the first `]]>`.
//! A page that ends before that keeps the text that it has (with the brackets that came before the end).

use super::Tokens;

const OPEN: &str = "[CDATA[";
const CLOSE: &str = "]]>";

impl Tokens<'_> {
    /// The tokenizer just read `<!` in foreign content. Is this `<![CDATA[`? The letters must be upper case.
    /// Gives true when the input at this place is a CDATA section (now read into the pending text, and the state is `Data`),
    /// or may become one when the next part of the page arrives (the tokenizer stalls, and starts again here).
    /// Gives false for anything else: it is a bogus comment.
    pub(super) fn cdata(&mut self) -> bool {
        let rest = &self.input[self.pos..];
        let Some(body) = rest.strip_prefix(OPEN) else {
            // `<![CDA` at the end of a part of the page: we cannot know yet.
            if self.more && OPEN.starts_with(rest) {
                self.stalled = true;
                return true;
            }
            return false;
        };
        let from = self.pos + OPEN.len();
        let len = match body.find(CLOSE) {
            Some(len) => len,
            None if self.need_more(true) => return true,
            None => body.len(),
            // No end: the rest of the page is text. The standard emits each character that it reads.
        };
        let input = self.input;
        self.text.slice(input, from, from + len);
        self.pos = from + len + if len < body.len() { CLOSE.len() } else { 0 };
        self.state = super::State::Data;
        true
    }
}
