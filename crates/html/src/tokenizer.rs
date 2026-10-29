//! The tokenizer: cut the text of a page into tokens.
//!
//! A token is one piece of HTML: a start tag (`<p class="a">`), an end tag (`</p>`),
//! a comment, a doctype, or a run of text.
//!
//! The tokenizer is a state machine. It reads one character at a time. It is always in one state,
//! like "inside a tag name" or "inside a quoted attribute value". Each state says what to do
//! with the next character, and which state comes next. The HTML standard lists every state and
//! every rule. This is how all browsers read HTML the same way, even broken HTML.
//!
//! Why not a regex? A regex cannot know that `>` inside `<a title=">">` does not end the tag,
//! or that `<b>` inside `<script>` is not a tag. A state machine keeps track of where it is.
//!
//! No HTML error stops the tokenizer. The standard says what to do for each one. For example,
//! `<p class=a class=b>` has the attribute twice: the first one wins.
//!
//! Some elements hold raw text: `<script>`, `<style>`, `<title>`, `<textarea>` and a few more.
//! Inside them `<b>` is plain text. The text only ends at the matching end tag.
//! In a real browser the tree builder tells the tokenizer to switch. We do it here, because
//! for us the result is the same.
//!
//! Left out: the states for the doctype's public and system names, and the script data
//! escapes (`<!--<script>` inside a script).
//!
//! Comments compare Rust with JavaScript, because you know JS.

use crate::charref;

/// One piece of HTML.
#[derive(Debug, PartialEq, Clone)]
pub enum Token {
    // JS: like a TypeScript union of objects, each with its own fields.
    /// `<!DOCTYPE html>`. The name is in lowercase, and it is empty when the page gives none.
    Doctype(String),
    /// `<p class="a">`. Names are in lowercase. `self_closing` is true for `<br/>`.
    StartTag { name: String, attrs: Vec<(String, String)>, self_closing: bool },
    /// `</p>`.
    EndTag { name: String },
    /// `<!-- text -->`.
    Comment(String),
    /// A run of text. Character references like `&amp;` are already decoded.
    Text(String),
}

/// Cut a page into tokens.
pub fn tokenize(input: &str) -> Vec<Token> {
    let mut t = Tokenizer {
        input: input.chars().collect(),
        // JS: `[...input]`. A list of single letters, so we can look at any position.
        pos: 0,
        state: State::Data,
        tokens: Vec::new(),
        text: String::new(),
        name: String::new(),
        is_end: false,
        attrs: Vec::new(),
        attr_name: String::new(),
        skip_value: false,
        self_closing: false,
        raw_name: String::new(),
    };
    t.run();
    t.flush_text();
    t.tokens
}

/// The states we use. Each one has the same name as a state in the standard.
#[derive(Clone, Copy, PartialEq)]
enum State {
    Data,
    TagOpen,
    EndTagOpen,
    TagName,
    BeforeAttrName,
    AttrName,
    AfterAttrName,
    BeforeAttrValue,
    AttrValueQuoted(char),
    // This state holds data: which quote opened the value, `"` or `'`.
    AttrValueUnquoted,
    AfterAttrValueQuoted,
    SelfClosing,
    BogusComment,
    MarkupDeclaration,
    Doctype,
    RawText(bool),
    // Inside `<script>` and the like. The `bool` says if character references work:
    // true for `<title>` and `<textarea>`, false for `<script>` and `<style>`.
}

struct Tokenizer {
    input: Vec<char>,
    pos: usize,
    // The place of the next character to read.
    state: State,
    tokens: Vec<Token>,
    text: String,
    // Text that we collect before we make a `Text` token. One token for a whole run of text.

    // The tag that we are reading now:
    name: String,
    is_end: bool,
    attrs: Vec<(String, String)>,
    attr_name: String,
    // The name of the attribute that we are reading now. We add it to `attrs` when it is complete.
    skip_value: bool,
    // True when this attribute is a repeat. We read its value, but we throw it away.
    self_closing: bool,

    raw_name: String,
    // The tag that started the raw text, like "script". Its end tag ends the raw text.
}

impl Tokenizer {
    /// Read the next character. Gives `None` at the end of the input.
    fn next(&mut self) -> Option<char> {
        let c = self.input.get(self.pos).copied();
        // `get` gives `None` past the end. `copied` turns `Option<&char>` into `Option<char>`.
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    /// Go back one character, so the next state reads it again. The standard calls this "reconsume".
    fn back(&mut self) {
        self.pos -= 1;
    }

    /// Does the input at the current place start with `text`? Letter case does not matter.
    fn looking_at(&self, text: &str) -> bool {
        let rest = &self.input[self.pos..];
        text.chars().count() <= rest.len() && text.chars().zip(rest).all(|(a, b)| a.eq_ignore_ascii_case(b))
        // `zip` walks two lists side by side. JS: `text.every((a, i) => same(a, rest[i]))`.
    }

    /// Turn the collected text into a `Text` token.
    fn flush_text(&mut self) {
        if !self.text.is_empty() {
            self.tokens.push(Token::Text(std::mem::take(&mut self.text)));
            // `take` gives us the text and leaves an empty one in its place.
            // JS: `const t = text; text = ""`.
        }
    }

    /// Start a new tag. The name starts empty, and the next state fills it.
    fn start_tag(&mut self, is_end: bool) {
        self.name.clear();
        self.is_end = is_end;
        self.attrs.clear();
        self.attr_name.clear();
        self.skip_value = false;
        self.self_closing = false;
    }

    /// The name of an attribute is complete. Add it, unless an attribute with this name came before.
    fn finish_attr_name(&mut self) {
        let name = std::mem::take(&mut self.attr_name);
        self.skip_value = self.attrs.iter().any(|(n, _)| *n == name);
        // The first attribute wins. `<p id=a id=b>` has `id=a`.
        if !self.skip_value {
            self.attrs.push((name, String::new()));
        }
    }

    /// Add a character to the value of the attribute that we are reading.
    fn push_value(&mut self, text: &str) {
        if !self.skip_value
            && let Some((_, value)) = self.attrs.last_mut()
        {
            value.push_str(text);
        }
    }

    /// Read a character reference after an "&". Give the text for it: the decoded text, or just "&".
    fn char_ref(&mut self, in_attribute: bool) -> String {
        match charref::decode(&self.input[self.pos..], in_attribute) {
            Some((text, used)) => {
                self.pos += used;
                text
            }
            None => "&".to_string(),
            // Not a reference: the "&" is plain text.
        }
    }

    /// The tag is complete. Make a token, and pick the next state.
    fn emit_tag(&mut self) {
        self.flush_text();
        let name = std::mem::take(&mut self.name);
        self.state = State::Data;
        if self.is_end {
            self.tokens.push(Token::EndTag { name });
            // An end tag may have attributes, like `</p class=x>`. The standard says to ignore them.
            return;
        }
        let raw = match name.as_str() {
            "title" | "textarea" => Some(true),
            // RCDATA: raw text, but `&amp;` still works.
            "style" | "xmp" | "iframe" | "noembed" | "noframes" | "script" | "plaintext" => Some(false),
            // RAWTEXT and script data: no references at all.
            _ => None,
        };
        if let Some(refs) = raw {
            self.state = State::RawText(refs);
            self.raw_name = name.clone();
        }
        self.tokens.push(Token::StartTag { name, attrs: std::mem::take(&mut self.attrs), self_closing: self.self_closing });
    }

    /// Run the state machine until the input ends.
    fn run(&mut self) {
        loop {
            // Each turn reads one character and does what the current state says.
            // At the end of the input (`None`), most states stop. A tag that is not complete
            // is thrown away. The standard says so: a page cut in the middle of a tag has no tag.
            match self.state {
                State::Data => match self.next() {
                    Some('<') => self.state = State::TagOpen,
                    Some('&') => {
                        let text = self.char_ref(false);
                        self.text.push_str(&text);
                    }
                    Some('\0') => {} // A NUL character in text is dropped, like the tree builder does.
                    Some(c) => self.text.push(c),
                    None => return,
                },

                State::TagOpen => match self.next() {
                    Some('!') => self.state = State::MarkupDeclaration,
                    Some('/') => self.state = State::EndTagOpen,
                    Some(c) if c.is_ascii_alphabetic() => {
                        self.start_tag(false);
                        self.back();
                        self.state = State::TagName;
                    }
                    Some('?') => {
                        // `<?xml ... ?>` is not HTML. It becomes a comment.
                        self.back();
                        self.state = State::BogusComment;
                    }
                    Some(_) => {
                        // "<" followed by something else, like "a < b" or "<3". It is plain text.
                        self.text.push('<');
                        self.back();
                        self.state = State::Data;
                    }
                    None => {
                        self.text.push('<');
                        return;
                    }
                },

                State::EndTagOpen => match self.next() {
                    Some(c) if c.is_ascii_alphabetic() => {
                        self.start_tag(true);
                        self.back();
                        self.state = State::TagName;
                    }
                    Some('>') => self.state = State::Data, // `</>` is ignored.
                    Some(_) => {
                        self.back();
                        self.state = State::BogusComment;
                    }
                    None => {
                        self.text.push_str("</");
                        return;
                    }
                },

                State::TagName => match self.next() {
                    Some(' ' | '\t' | '\n' | '\x0C') => self.state = State::BeforeAttrName,
                    // JS: `case " ": case "\t": ...`. These are the four spaces of HTML.
                    Some('/') => self.state = State::SelfClosing,
                    Some('>') => self.emit_tag(),
                    Some('\0') => self.name.push('\u{FFFD}'),
                    // A NUL in a name becomes "�".
                    Some(c) => self.name.push(c.to_ascii_lowercase()),
                    // Tag names are not case sensitive. We keep them in lowercase.
                    None => return,
                },

                State::BeforeAttrName => match self.next() {
                    Some(' ' | '\t' | '\n' | '\x0C') => {}
                    Some('/' | '>') => {
                        self.back();
                        self.state = State::AfterAttrName;
                    }
                    Some('=') => {
                        // `<p =x>`: the name of the attribute starts with "=". An error, but the standard allows it.
                        self.attr_name.push('=');
                        self.state = State::AttrName;
                    }
                    Some(_) => {
                        self.back();
                        self.state = State::AttrName;
                    }
                    None => return,
                },

                State::AttrName => match self.next() {
                    Some(' ' | '\t' | '\n' | '\x0C' | '/' | '>') => {
                        self.finish_attr_name();
                        self.back();
                        self.state = State::AfterAttrName;
                    }
                    Some('=') => {
                        self.finish_attr_name();
                        self.state = State::BeforeAttrValue;
                    }
                    Some('\0') => self.attr_name.push('\u{FFFD}'),
                    Some(c) => self.attr_name.push(c.to_ascii_lowercase()),
                    None => return,
                },

                State::AfterAttrName => match self.next() {
                    Some(' ' | '\t' | '\n' | '\x0C') => {}
                    Some('/') => self.state = State::SelfClosing,
                    Some('=') => self.state = State::BeforeAttrValue,
                    Some('>') => self.emit_tag(),
                    Some(_) => {
                        self.back();
                        self.state = State::AttrName;
                    }
                    None => return,
                },

                State::BeforeAttrValue => match self.next() {
                    Some(' ' | '\t' | '\n' | '\x0C') => {}
                    Some(q @ ('"' | '\'')) => self.state = State::AttrValueQuoted(q),
                    // `q @ (...)` takes the character that matched and calls it `q`.
                    Some('>') => self.emit_tag(), // `<p class=>`: no value.
                    Some(_) => {
                        self.back();
                        self.state = State::AttrValueUnquoted;
                    }
                    None => return,
                },

                State::AttrValueQuoted(quote) => match self.next() {
                    Some(c) if c == quote => self.state = State::AfterAttrValueQuoted,
                    Some('&') => {
                        let text = self.char_ref(true);
                        self.push_value(&text);
                    }
                    Some('\0') => self.push_value("\u{FFFD}"),
                    Some(c) => self.push_value(c.encode_utf8(&mut [0; 4])),
                    // `encode_utf8` turns one letter into a piece of text.
                    None => return,
                },

                State::AttrValueUnquoted => match self.next() {
                    Some(' ' | '\t' | '\n' | '\x0C') => self.state = State::BeforeAttrName,
                    Some('&') => {
                        let text = self.char_ref(true);
                        self.push_value(&text);
                    }
                    Some('>') => self.emit_tag(),
                    Some('\0') => self.push_value("\u{FFFD}"),
                    Some(c) => self.push_value(c.encode_utf8(&mut [0; 4])),
                    None => return,
                },

                State::AfterAttrValueQuoted => match self.next() {
                    Some(' ' | '\t' | '\n' | '\x0C') => self.state = State::BeforeAttrName,
                    Some('/') => self.state = State::SelfClosing,
                    Some('>') => self.emit_tag(),
                    Some(_) => {
                        // `<p a="b"c="d">`: no space between the attributes. We read it as two.
                        self.back();
                        self.state = State::BeforeAttrName;
                    }
                    None => return,
                },

                State::SelfClosing => match self.next() {
                    Some('>') => {
                        self.self_closing = true;
                        self.emit_tag();
                    }
                    Some(_) => {
                        self.back();
                        self.state = State::BeforeAttrName;
                    }
                    None => return,
                },

                State::BogusComment => {
                    // Everything up to the next ">" is a comment.
                    let mut data = String::new();
                    while let Some(c) = self.next() {
                        if c == '>' {
                            break;
                        }
                        data.push(if c == '\0' { '\u{FFFD}' } else { c });
                    }
                    self.flush_text();
                    self.tokens.push(Token::Comment(data));
                    self.state = State::Data;
                    // If the input ended, the next turn of the loop stops.
                }

                State::MarkupDeclaration => {
                    // We just read "<!". What follows tells us what this is.
                    if self.looking_at("--") {
                        self.pos += 2;
                        self.comment();
                    } else if self.looking_at("doctype") {
                        self.pos += 7;
                        self.state = State::Doctype;
                    } else {
                        self.state = State::BogusComment;
                        // `<![CDATA[x]]>` is also here. Outside of SVG and MathML it is a comment.
                    }
                }

                State::Doctype => {
                    while matches!(self.input.get(self.pos), Some(' ' | '\t' | '\n' | '\x0C')) {
                        self.pos += 1;
                    }
                    // Skip the spaces before the name.
                    let mut name = String::new();
                    while let Some(&c) = self.input.get(self.pos) {
                        if matches!(c, ' ' | '\t' | '\n' | '\x0C' | '>') {
                            break;
                        }
                        name.push(if c == '\0' { '\u{FFFD}' } else { c.to_ascii_lowercase() });
                        self.pos += 1;
                    }
                    while let Some(c) = self.next() {
                        if c == '>' {
                            break;
                        }
                        // After the name there may be a public and a system name. We skip them for now.
                    }
                    self.flush_text();
                    self.tokens.push(Token::Doctype(name));
                    self.state = State::Data;
                }

                State::RawText(refs) => match self.next() {
                    Some('<') if self.at_raw_end_tag() => {
                        // This is `</script` followed by a space, "/" or ">". The raw text is over.
                        self.pos += 1 + self.raw_name.chars().count();
                        // Skip "/" and the name. The next character is a space, "/" or ">", and `BeforeAttrName` reads it.
                        self.start_tag(true);
                        self.name = self.raw_name.clone();
                        self.state = State::BeforeAttrName;
                    }
                    Some('&') if refs => {
                        let text = self.char_ref(false);
                        self.text.push_str(&text);
                    }
                    Some('\0') => self.text.push('\u{FFFD}'),
                    Some(c) => self.text.push(c),
                    None => return,
                },
            }
        }
    }

    /// We just read "<" in raw text. Is this the end tag that closes the raw text?
    /// It is `</` plus the name, in any letter case, and then a space, "/" or ">".
    fn at_raw_end_tag(&mut self) -> bool {
        if self.raw_name == "plaintext" {
            return false;
            // `<plaintext>` is the one raw text element that has no end.
        }
        self.back();
        let name = self.raw_name.clone();
        let found = self.looking_at(&format!("</{name}"))
            && matches!(self.input.get(self.pos + 2 + name.chars().count()), Some(' ' | '\t' | '\n' | '\x0C' | '/' | '>'));
        self.pos += 1;
        // Go back to where we were: just after the "<".
        found
        // So `</scripts>` does not end a script, and neither does `</script` at the end of the input.
    }

    /// Read a comment. We just read "<!--".
    fn comment(&mut self) {
        let mut data = String::new();
        if self.looking_at(">") {
            self.pos += 1; // `<!-->` is an empty comment.
        } else if self.looking_at("->") {
            self.pos += 2; // `<!--->` too.
        } else {
            while self.pos < self.input.len() && !self.looking_at("-->") && !self.looking_at("--!>") {
                // A comment ends with "-->" or with "--!>". The second one is an old mistake that browsers allow.
                let c = self.input[self.pos];
                data.push(if c == '\0' { '\u{FFFD}' } else { c });
                self.pos += 1;
            }
            self.pos += if self.looking_at("-->") { 3 } else if self.looking_at("--!>") { 4 } else { 0 };
            // If the input ended inside the comment, the comment ends there.
        }
        self.flush_text();
        self.tokens.push(Token::Comment(data));
        self.state = State::Data;
    }
}

#[cfg(test)]
// Only compiled for `cargo test`.
mod tests {
    use super::*;

    fn text(s: &str) -> Token {
        Token::Text(s.to_string())
    }

    fn start(name: &str, attrs: &[(&str, &str)]) -> Token {
        Token::StartTag { name: name.to_string(), attrs: attrs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect(), self_closing: false }
    }

    fn end(name: &str) -> Token {
        Token::EndTag { name: name.to_string() }
    }

    #[test]
    fn reads_tags_and_text() {
        assert_eq!(tokenize("<p>Hi <b>you</b></p>"), [start("p", &[]), text("Hi "), start("b", &[]), text("you"), end("b"), end("p")]);
    }

    #[test]
    fn tag_names_are_lowercase() {
        assert_eq!(tokenize("<DIV></Div>"), [start("div", &[]), end("div")]);
    }

    #[test]
    fn reads_attributes() {
        assert_eq!(
            tokenize("<a href=\"/x?a=1&b=2\" Class='big one' id=main hidden data-x = 5>"),
            [start("a", &[("href", "/x?a=1&b=2"), ("class", "big one"), ("id", "main"), ("hidden", ""), ("data-x", "5")])]
        );
        // Double quotes, single quotes, no quotes, no value, and spaces around "=".
    }

    #[test]
    fn first_duplicate_attribute_wins() {
        assert_eq!(tokenize("<p id=a id=b class=c>"), [start("p", &[("id", "a"), ("class", "c")])]);
    }

    #[test]
    fn a_quoted_value_can_hold_a_greater_than_sign() {
        assert_eq!(tokenize("<a title=\">\">x"), [start("a", &[("title", ">")]), text("x")]);
        // A regex that looks for ">" would end the tag too early.
    }

    #[test]
    fn handles_odd_attribute_syntax() {
        assert_eq!(tokenize("<p a=\"b\"c=d>"), [start("p", &[("a", "b"), ("c", "d")])]);
        assert_eq!(tokenize("<p =x>"), [start("p", &[("=x", "")])]);
        assert_eq!(tokenize("<p class=>"), [start("p", &[("class", "")])]);
        assert_eq!(tokenize("<p a/b>"), [start("p", &[("a", ""), ("b", "")])]);
    }

    #[test]
    fn self_closing_flag() {
        assert_eq!(tokenize("<br/><img src=a />"), [
            Token::StartTag { name: "br".into(), attrs: vec![], self_closing: true },
            Token::StartTag { name: "img".into(), attrs: vec![("src".into(), "a".into())], self_closing: true },
        ]);
    }

    #[test]
    fn end_tag_attributes_are_ignored() {
        assert_eq!(tokenize("</p class=x>"), [end("p")]);
    }

    #[test]
    fn decodes_references_in_text_and_attributes() {
        assert_eq!(tokenize("a &lt; b &amp; c &copy 2024"), [text("a < b & c © 2024")]);
        // One `Text` token, even though the text has references in it.
        assert_eq!(tokenize("<a href=\"?x=1&copy=2&amp;y=3\">"), [start("a", &[("href", "?x=1&copy=2&y=3")])]);
        // In an attribute, "&copy=" stays. "&amp;" is decoded.
    }

    #[test]
    fn a_lone_less_than_sign_is_text() {
        assert_eq!(tokenize("a < b <3 <"), [text("a < b <3 <")]);
        assert_eq!(tokenize("x </"), [text("x </")]);
    }

    #[test]
    fn reads_comments() {
        assert_eq!(tokenize("a<!-- hi -->b"), [text("a"), Token::Comment(" hi ".into()), text("b")]);
        assert_eq!(tokenize("<!---->"), [Token::Comment("".into())]);
        assert_eq!(tokenize("<!-->x"), [Token::Comment("".into()), text("x")]);
        assert_eq!(tokenize("<!--->x"), [Token::Comment("".into()), text("x")]);
        assert_eq!(tokenize("<!-- a -- b --!>x"), [Token::Comment(" a -- b ".into()), text("x")]);
        assert_eq!(tokenize("<!-- cut"), [Token::Comment(" cut".into())]);
        // A comment that never ends takes the rest of the page.
    }

    #[test]
    fn bogus_comments() {
        assert_eq!(tokenize("<?xml version=1?>x"), [Token::Comment("?xml version=1?".into()), text("x")]);
        assert_eq!(tokenize("</ nope>x"), [Token::Comment(" nope".into()), text("x")]);
        assert_eq!(tokenize("<![CDATA[x]]>"), [Token::Comment("[CDATA[x]]".into())]);
        assert_eq!(tokenize("<!foo>"), [Token::Comment("foo".into())]);
    }

    #[test]
    fn reads_the_doctype() {
        assert_eq!(tokenize("<!DOCTYPE html>"), [Token::Doctype("html".into())]);
        assert_eq!(tokenize("<!doctype  HTML PUBLIC \"-//W3C//DTD HTML 4.01//EN\">x"), [Token::Doctype("html".into()), text("x")]);
        assert_eq!(tokenize("<!DOCTYPE>"), [Token::Doctype("".into())]);
    }

    #[test]
    fn script_holds_raw_text() {
        assert_eq!(tokenize("<script>if (a<b && c>d) { x = \"<b>&amp;\" }</script>"), [
            start("script", &[]),
            text("if (a<b && c>d) { x = \"<b>&amp;\" }"),
            end("script"),
        ]);
        // No tags and no references inside a script.
    }

    #[test]
    fn raw_text_ends_only_at_the_right_end_tag() {
        assert_eq!(tokenize("<style>a</styles></style >b"), [start("style", &[]), text("a</styles>"), end("style"), text("b")]);
        assert_eq!(tokenize("<SCRIPT>x</ScRiPt>"), [start("script", &[]), text("x"), end("script")]);
        assert_eq!(tokenize("<script>x</script"), [start("script", &[]), text("x</script")]);
        // The end tag is not complete, so it is still text.
    }

    #[test]
    fn title_and_textarea_decode_references_but_not_tags() {
        assert_eq!(tokenize("<title>a &amp; <b></title>"), [start("title", &[]), text("a & <b>"), end("title")]);
        assert_eq!(tokenize("<textarea><p>&lt;</textarea>"), [start("textarea", &[]), text("<p><"), end("textarea")]);
    }

    #[test]
    fn plaintext_never_ends() {
        assert_eq!(tokenize("<plaintext></plaintext><b>"), [start("plaintext", &[]), text("</plaintext><b>")]);
    }

    #[test]
    fn a_cut_tag_is_dropped() {
        assert_eq!(tokenize("a<p class=\"x"), [text("a")]);
        assert_eq!(tokenize("a<p"), [text("a")]);
        assert_eq!(tokenize("a</p"), [text("a")]);
        // The page ends in the middle of a tag. The standard says to drop that tag.
    }

    #[test]
    fn nul_characters() {
        assert_eq!(tokenize("a\0b"), [text("ab")]);
        assert_eq!(tokenize("<a\0b>"), [start("a\u{FFFD}b", &[])]);
    }

    #[test]
    fn empty_input() {
        assert_eq!(tokenize(""), []);
    }

    #[test]
    fn a_page() {
        let page = "<!DOCTYPE html>\n<html lang=en>\n<head><meta charset=utf-8><title>Hi</title></head>\n<body><p>x</p></body></html>";
        let tokens = tokenize(page);
        assert_eq!(tokens.len(), 17);
        assert_eq!(tokens[0], Token::Doctype("html".into()));
        assert_eq!(tokens[2], start("html", &[("lang", "en")]));
    }

    #[test]
    fn never_panics_or_loops_on_junk() {
        let pieces = ["<", ">", "/", "=", "\"", "'", "&", "&#", "&#x", "&amp", "!", "--", "-->", "?", "a", "script", "</script", "title", " ", "\n", "\0", "é", "doctype", "[CDATA["];
        let mut seed: u32 = 12345;
        for _ in 0..3000 {
            let mut page = String::new();
            for _ in 0..(seed % 20) {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                // A simple random number generator. The same numbers every run, so a failure can be repeated.
                page.push_str(pieces[(seed >> 16) as usize % pieces.len()]);
            }
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            tokenize(&page);
            // We only check that it ends and does not crash.
        }
    }
}
