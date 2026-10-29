//! The tokenizer: cut the text of a page into tokens.
//!
//! A token is one piece of HTML: a start tag (`<p class="a">`), an end tag (`</p>`),
//! a comment, a doctype, or a run of text.
//!
//! The tokenizer is a state machine. It reads the page and is always in one state,
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
//! The tokenizer gives one token at a time, like a JS generator. The tree builder uses each token
//! and drops it. The page text is read in place, so the list of all tokens never exists.
//!
//! A token does not copy the page either. Its names and texts are a `Cow<str>`: either a slice of
//! the page (`Borrowed`, no allocation) or a new `String` (`Owned`). A slice is enough when the
//! text is the same as in the page. A new `String` is only made when the tokenizer changes the
//! text: a tag name with capital letters (`<DIV>` becomes `div`), a character reference
//! (`&amp;` becomes `&`), or a NUL character. Most of a page has none of these.
//! JS: like `str.slice(a, b)`, which in the engine can point into the big string.
//!
//! Left out: the states for the doctype's public and system names, and the script data
//! escapes (`<!--<script>` inside a script).
//!
//! Comments compare Rust with JavaScript, because you know JS.

use std::borrow::Cow;
use std::collections::VecDeque;

use crate::charref;

/// One attribute: its name and its value.
pub type Attr<'a> = (Cow<'a, str>, Cow<'a, str>);

/// One piece of HTML. `'a` is the life of the page text: a token can point into it.
#[derive(Debug, PartialEq, Clone)]
pub enum Token<'a> {
    // JS: like a TypeScript union of objects, each with its own fields.
    /// `<!DOCTYPE html>`. The name is in lowercase, and it is empty when the page gives none.
    Doctype(Cow<'a, str>),
    /// `<p class="a">`. Names are in lowercase. `self_closing` is true for `<br/>`.
    StartTag { name: Cow<'a, str>, attrs: Vec<Attr<'a>>, self_closing: bool },
    /// `</p>`.
    EndTag { name: Cow<'a, str> },
    /// `<!-- text -->`.
    Comment(Cow<'a, str>),
    /// A run of text. Character references like `&amp;` are already decoded.
    Text(Cow<'a, str>),
}

/// Cut a page into tokens. Gives an iterator: it reads the page only as far as the caller asks.
/// JS: like a generator, `function* tokenize(input) { ... yield token ... }`.
/// The caller can use each token and throw it away, so the whole list is never in memory.
/// Tests that want the whole list call `.collect::<Vec<_>>()`.
pub fn tokenize(input: &str) -> Tokens<'_> {
    // `'_` says: the iterator borrows `input`, so `input` must stay alive while we use it.
    Tokens {
        input,
        pos: 0,
        last_len: 0,
        state: State::Data,
        tokens: VecDeque::new(),
        finished: false,
        text: Pending::default(),
        name: Cow::Borrowed(""),
        is_end: false,
        attrs: Vec::new(),
        skip_value: false,
        value: Pending::default(),
        self_closing: false,
        raw_name: "",
    }
}

/// The states we use. Each one has the same name as a state in the standard.
#[derive(Clone, Copy, PartialEq)]
enum State {
    Data,
    TagOpen,
    EndTagOpen,
    TagName,
    BeforeAttrName,
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

/// A text that we are collecting. It is a slice of the page, as long as we can: each time we add
/// the next part of the page, the slice grows. Only when we add something that is not in the page
/// (a decoded `&amp;`), or a part that is not next to the slice, the text becomes a `String`.
#[derive(Default)]
struct Pending {
    range: Option<(usize, usize)>,
    // The slice of the page: from byte `.0` to byte `.1`.
    owned: Option<String>,
    // The text as a `String`, when a slice is not enough.
}

impl Pending {
    /// Add `input[a..b]` to the text.
    fn slice(&mut self, input: &str, a: usize, b: usize) {
        if a == b {
            return;
        }
        if let Some(owned) = &mut self.owned {
            owned.push_str(&input[a..b]);
            return;
        }
        match self.range {
            None => self.range = Some((a, b)),
            Some((start, end)) if end == a => self.range = Some((start, b)),
            // The new part is next to the slice: the slice grows.
            Some((start, end)) => {
                self.owned = Some(format!("{}{}", &input[start..end], &input[a..b]));
                self.range = None;
            }
        }
    }

    /// Add a text that is not from the page.
    fn text(&mut self, input: &str, text: &str) {
        let owned = self.owned.get_or_insert_with(|| {
            let (start, end) = self.range.take().unwrap_or((0, 0));
            input[start..end].to_string()
        });
        owned.push_str(text);
    }

    /// Give the text, and start again with an empty one. `None` when it is empty.
    fn take<'a>(&mut self, input: &'a str) -> Option<Cow<'a, str>> {
        if let Some(owned) = self.owned.take() {
            self.range = None;
            return (!owned.is_empty()).then_some(Cow::Owned(owned));
        }
        let (a, b) = self.range.take()?;
        Some(Cow::Borrowed(&input[a..b]))
    }
}

/// A tag or attribute name in lowercase. A NUL becomes "�". We only make a new `String` when
/// something changes.
fn lower(name: &str) -> Cow<'_, str> {
    if name.bytes().any(|b| b.is_ascii_uppercase() || b == 0) {
        Cow::Owned(name.chars().map(|c| if c == '\0' { '\u{FFFD}' } else { c.to_ascii_lowercase() }).collect())
    } else {
        Cow::Borrowed(name)
    }
}

/// The text of a comment, with each NUL changed to "�".
fn no_nul(text: &str) -> Cow<'_, str> {
    if text.contains('\0') { Cow::Owned(text.replace('\0', "\u{FFFD}")) } else { Cow::Borrowed(text) }
}

/// The tokenizer. It is an iterator of tokens.
pub struct Tokens<'a> {
    input: &'a str,
    // We read the page where it is. We do not copy it.
    pos: usize,
    // The place of the next character to read, counted in bytes.
    last_len: usize,
    // The size in bytes of the character that we read last, so `back` knows how far to go.
    state: State,
    tokens: VecDeque<Token<'a>>,
    // Tokens that are ready and not yet given out. Never more than two or three:
    // the text before a tag, and the tag.
    finished: bool,
    text: Pending,
    // Text that we collect before we make a `Text` token. One token for a whole run of text.

    // The tag that we are reading now:
    name: Cow<'a, str>,
    is_end: bool,
    attrs: Vec<Attr<'a>>,
    skip_value: bool,
    // True when the attribute that we are reading is a repeat. We read its value, but we throw it away.
    value: Pending,
    // The value of the attribute that we are reading now.
    self_closing: bool,

    raw_name: &'static str,
    // The tag that started the raw text, like "script". Its end tag ends the raw text.
}

impl<'a> Iterator for Tokens<'a> {
    type Item = Token<'a>;

    /// Give the next token. Run the state machine only until one is ready.
    fn next(&mut self) -> Option<Token<'a>> {
        loop {
            if let Some(token) = self.tokens.pop_front() {
                return Some(token);
            }
            if self.finished {
                return None;
            }
            self.run();
            if self.tokens.is_empty() {
                // `run` stopped and has no token: this is the end of the input.
                self.finished = true;
                self.flush_text();
            }
        }
    }
}

impl<'a> Tokens<'a> {
    /// Look at the next character, and do not move.
    fn peek(&self) -> Option<char> {
        self.input[self.pos..].chars().next()
        // `chars()` reads UTF-8 one character at a time. We only decode the one that we need.
    }

    /// Read the next character. Gives `None` at the end of the input.
    fn read(&mut self) -> Option<char> {
        let c = self.peek();
        if let Some(c) = c {
            self.last_len = c.len_utf8();
            self.pos += self.last_len;
        }
        c
    }

    /// Go back one character, so the next state reads it again. The standard calls this "reconsume".
    fn back(&mut self) {
        self.pos -= self.last_len;
    }

    /// Move forward to the next byte in `stops` (or to the end). Gives the part that we passed over,
    /// as the places `(from, to)`. The `stops` are ASCII, so we always stop at the start of a character.
    fn scan(&mut self, stops: &[u8]) -> (usize, usize) {
        let from = self.pos;
        let rest = &self.input.as_bytes()[from..];
        self.pos += rest.iter().position(|b| stops.contains(b)).unwrap_or(rest.len());
        (from, self.pos)
    }

    /// Does the input at the current place start with `text`? Letter case does not matter.
    /// `text` must be ASCII. All the texts that we look for are.
    fn looking_at(&self, text: &str) -> bool {
        let rest = &self.input.as_bytes()[self.pos..];
        rest.len() >= text.len() && rest[..text.len()].eq_ignore_ascii_case(text.as_bytes())
    }

    /// Turn the collected text into a `Text` token.
    fn flush_text(&mut self) {
        if let Some(text) = self.text.take(self.input) {
            self.tokens.push_back(Token::Text(text));
        }
    }

    /// Start a new tag. The name starts empty, and the next state fills it.
    fn start_tag(&mut self, is_end: bool) {
        self.name = Cow::Borrowed("");
        self.is_end = is_end;
        self.attrs.clear();
        self.skip_value = false;
        self.self_closing = false;
    }

    /// Read the name of an attribute. The first character is part of the name, whatever it is
    /// (`<p =x>` has an attribute named "=x"). Then we read up to a space, "/", ">" or "=".
    /// Gives false when the input ends in the name: the tag is dropped.
    fn attr_name(&mut self) -> bool {
        let start = self.pos;
        let Some(first) = self.peek() else { return false };
        self.pos += first.len_utf8();
        let (_, end) = self.scan(b" \t\n\x0C/>=");
        let input = self.input;
        let name = lower(&input[start..end]);
        // The attribute is complete. Add it, unless an attribute with this name came before.
        // The first attribute wins: `<p id=a id=b>` has `id=a`.
        self.skip_value = self.attrs.iter().any(|(n, _)| *n == name);
        if !self.skip_value {
            self.attrs.push((name, Cow::Borrowed("")));
        }
        match self.read() {
            None => return false,
            Some('=') => self.state = State::BeforeAttrValue,
            Some(_) => {
                self.back();
                self.state = State::AfterAttrName;
            }
        }
        true
    }

    /// The value of the attribute is complete. Put it in the attribute, unless it is a repeat.
    fn finish_value(&mut self) {
        let value = self.value.take(self.input);
        if !self.skip_value
            && let (Some(value), Some(last)) = (value, self.attrs.last_mut())
        {
            last.1 = value;
        }
    }

    /// We just read an "&". Decode the reference, and add it to the text (or to the value of the
    /// attribute, when `in_value` is true). If it is not a reference, the "&" is plain text.
    fn char_ref(&mut self, in_value: bool) {
        let input = self.input;
        let decoded = charref::decode(&input[self.pos..], in_value);
        let pending = if in_value { &mut self.value } else { &mut self.text };
        match decoded {
            Some((text, used)) => {
                pending.text(input, &text);
                self.pos += used;
            }
            None => pending.slice(input, self.pos - 1, self.pos),
            // Not a reference: the "&" stays as it is. It is just before us in the page.
        }
    }

    /// The tag is complete. Make a token, and pick the next state.
    fn emit_tag(&mut self) {
        self.flush_text();
        let name = std::mem::replace(&mut self.name, Cow::Borrowed(""));
        self.state = State::Data;
        if self.is_end {
            self.tokens.push_back(Token::EndTag { name });
            // An end tag may have attributes, like `</p class=x>`. The standard says to ignore them.
            return;
        }
        if let Some((refs, raw_name)) = raw_text_kind(&name) {
            self.state = State::RawText(refs);
            self.raw_name = raw_name;
        }
        self.tokens.push_back(Token::StartTag { name, attrs: std::mem::take(&mut self.attrs), self_closing: self.self_closing });
    }

    /// Run the state machine until a token is ready, or until the input ends.
    fn run(&mut self) {
        let input = self.input;
        loop {
            // Each turn does what the current state says.
            // At the end of the input (`None`), most states stop. A tag that is not complete
            // is thrown away. The standard says so: a page cut in the middle of a tag has no tag.
            if !self.tokens.is_empty() {
                return;
            }
            match self.state {
                State::Data => {
                    let (a, b) = self.scan(b"<&\0");
                    // A run of plain text is one slice of the page. We do not look at each letter.
                    self.text.slice(input, a, b);
                    match self.read() {
                        Some('<') => self.state = State::TagOpen,
                        Some('&') => self.char_ref(false),
                        Some(_) => {} // A NUL character in text is dropped, like the tree builder does.
                        None => return,
                    }
                }

                State::TagOpen => match self.read() {
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
                        self.back();
                        self.text.slice(input, self.pos - 1, self.pos);
                        self.state = State::Data;
                    }
                    None => {
                        self.text.slice(input, self.pos - 1, self.pos);
                        return;
                    }
                },

                State::EndTagOpen => match self.read() {
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
                        self.text.slice(input, self.pos - 2, self.pos);
                        return;
                    }
                },

                State::TagName => {
                    let (a, b) = self.scan(b" \t\n\x0C/>");
                    // Tag names are not case sensitive. `lower` keeps them in lowercase.
                    // A NUL in a name becomes "�".
                    let Some(end) = self.read() else { return };
                    self.name = lower(&input[a..b]);
                    match end {
                        '/' => self.state = State::SelfClosing,
                        '>' => self.emit_tag(),
                        _ => self.state = State::BeforeAttrName,
                        // JS: the `_` is `default:`. The other four are the spaces of HTML.
                    }
                }

                State::BeforeAttrName => match self.read() {
                    Some(' ' | '\t' | '\n' | '\x0C') => {}
                    Some('/' | '>') => {
                        self.back();
                        self.state = State::AfterAttrName;
                    }
                    Some(_) => {
                        self.back();
                        if !self.attr_name() {
                            return;
                        }
                    }
                    None => return,
                },

                State::AfterAttrName => match self.read() {
                    Some(' ' | '\t' | '\n' | '\x0C') => {}
                    Some('/') => self.state = State::SelfClosing,
                    Some('=') => self.state = State::BeforeAttrValue,
                    Some('>') => self.emit_tag(),
                    Some(_) => {
                        self.back();
                        if !self.attr_name() {
                            return;
                        }
                    }
                    None => return,
                },

                State::BeforeAttrValue => match self.read() {
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

                State::AttrValueQuoted(quote) => {
                    let (a, b) = self.scan(if quote == '"' { b"\"&\0" } else { b"'&\0" });
                    self.value.slice(input, a, b);
                    match self.read() {
                        Some('&') => self.char_ref(true),
                        Some('\0') => self.value.text(input, "\u{FFFD}"),
                        Some(_) => {
                            // The closing quote.
                            self.finish_value();
                            self.state = State::AfterAttrValueQuoted;
                        }
                        None => return,
                    }
                }

                State::AttrValueUnquoted => {
                    let (a, b) = self.scan(b" \t\n\x0C>&\0");
                    self.value.slice(input, a, b);
                    match self.read() {
                        Some(' ' | '\t' | '\n' | '\x0C') => {
                            self.finish_value();
                            self.state = State::BeforeAttrName;
                        }
                        Some('&') => self.char_ref(true),
                        Some('>') => {
                            self.finish_value();
                            self.emit_tag();
                        }
                        Some(_) => self.value.text(input, "\u{FFFD}"),
                        None => return,
                    }
                }

                State::AfterAttrValueQuoted => match self.read() {
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

                State::SelfClosing => match self.read() {
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
                    let rest = &input[self.pos..];
                    let (data, used) = match rest.find('>') {
                        Some(i) => (&rest[..i], i + 1),
                        None => (rest, rest.len()),
                    };
                    self.pos += used;
                    self.flush_text();
                    self.tokens.push_back(Token::Comment(no_nul(data)));
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
                    while matches!(self.peek(), Some(' ' | '\t' | '\n' | '\x0C')) {
                        self.pos += 1;
                    }
                    // Skip the spaces before the name.
                    let (a, b) = self.scan(b" \t\n\x0C>");
                    let name = lower(&input[a..b]);
                    // After the name there may be a public and a system name. We skip them for now.
                    self.pos += input[self.pos..].find('>').map_or(input.len() - self.pos, |i| i + 1);
                    self.flush_text();
                    self.tokens.push_back(Token::Doctype(name));
                    self.state = State::Data;
                }

                State::RawText(refs) => {
                    let (a, b) = self.scan(if refs { b"<&\0" } else { b"<\0" });
                    self.text.slice(input, a, b);
                    match self.read() {
                        Some('<') if self.at_raw_end_tag() => {
                            // This is `</script` followed by a space, "/" or ">". The raw text is over.
                            self.pos += 1 + self.raw_name.len();
                            // Skip "/" and the name. The next character is a space, "/" or ">", and `BeforeAttrName` reads it.
                            self.start_tag(true);
                            self.name = Cow::Borrowed(self.raw_name);
                            self.state = State::BeforeAttrName;
                        }
                        Some('<') => self.text.slice(input, self.pos - 1, self.pos),
                        // A "<" that does not end the raw text is plain text.
                        Some('&') => self.char_ref(false),
                        Some(_) => self.text.text(input, "\u{FFFD}"),
                        None => return,
                    }
                }
            }
        }
    }

    /// We just read "<" in raw text. Is this the end tag that closes the raw text?
    /// It is `</` plus the name, in any letter case, and then a space, "/" or ">".
    fn at_raw_end_tag(&self) -> bool {
        if self.raw_name == "plaintext" {
            return false;
            // `<plaintext>` is the one raw text element that has no end.
        }
        let name = self.raw_name.as_bytes();
        let rest = &self.input.as_bytes()[self.pos - 1..];
        // `rest` starts at the "<" that we just read.
        rest.len() > name.len() + 2
            && rest.starts_with(b"</")
            && rest[2..2 + name.len()].eq_ignore_ascii_case(name)
            && matches!(rest[2 + name.len()], b' ' | b'\t' | b'\n' | b'\x0C' | b'/' | b'>')
        // So `</scripts>` does not end a script, and neither does `</script` at the end of the input.
    }

    /// Read a comment. We just read "<!--".
    fn comment(&mut self) {
        let rest = &self.input[self.pos..];
        let (data, used) = if rest.starts_with('>') {
            ("", 1) // `<!-->` is an empty comment.
        } else if rest.starts_with("->") {
            ("", 2) // `<!--->` too.
        } else {
            // A comment ends with "-->" or with "--!>". The second one is an old mistake that browsers allow.
            // We look for "--" and check what follows it.
            let mut from = 0;
            let mut end = None;
            while let Some(i) = rest[from..].find("--") {
                let at = from + i;
                if rest[at..].starts_with("-->") {
                    end = Some((at, 3));
                    break;
                }
                if rest[at..].starts_with("--!>") {
                    end = Some((at, 4));
                    break;
                }
                from = at + 1;
            }
            match end {
                Some((at, len)) => (&rest[..at], at + len),
                None => (rest, rest.len()),
                // If the input ended inside the comment, the comment ends there.
            }
        };
        self.pos += used;
        self.flush_text();
        self.tokens.push_back(Token::Comment(no_nul(data)));
        self.state = State::Data;
    }
}

/// Does a start tag with this name switch to raw text? Gives whether character references work in it,
/// and the name as a `'static` text.
fn raw_text_kind(name: &str) -> Option<(bool, &'static str)> {
    Some(match name {
        "title" => (true, "title"),
        "textarea" => (true, "textarea"),
        // RCDATA: raw text, but `&amp;` still works.
        "style" => (false, "style"),
        "xmp" => (false, "xmp"),
        "iframe" => (false, "iframe"),
        "noembed" => (false, "noembed"),
        "noframes" => (false, "noframes"),
        "script" => (false, "script"),
        "plaintext" => (false, "plaintext"),
        // RAWTEXT and script data: no references at all.
        _ => return None,
    })
}

#[cfg(test)]
// Only compiled for `cargo test`.
mod tests {
    use super::*;

    fn all(input: &str) -> Vec<Token<'_>> {
        tokenize(input).collect()
    }

    fn text(s: &str) -> Token<'_> {
        Token::Text(s.into())
    }

    fn start<'a>(name: &'a str, attrs: &[(&'a str, &'a str)]) -> Token<'a> {
        Token::StartTag { name: name.into(), attrs: attrs.iter().map(|&(a, b)| (a.into(), b.into())).collect(), self_closing: false }
    }

    fn end(name: &str) -> Token<'_> {
        Token::EndTag { name: name.into() }
    }

    #[test]
    fn reads_tags_and_text() {
        assert_eq!(all("<p>Hi <b>you</b></p>"), [start("p", &[]), text("Hi "), start("b", &[]), text("you"), end("b"), end("p")]);
    }

    #[test]
    fn tag_names_are_lowercase() {
        assert_eq!(all("<DIV></Div>"), [start("div", &[]), end("div")]);
    }

    #[test]
    fn reads_attributes() {
        assert_eq!(
            all("<a href=\"/x?a=1&b=2\" Class='big one' id=main hidden data-x = 5>"),
            [start("a", &[("href", "/x?a=1&b=2"), ("class", "big one"), ("id", "main"), ("hidden", ""), ("data-x", "5")])]
        );
        // Double quotes, single quotes, no quotes, no value, and spaces around "=".
    }

    #[test]
    fn first_duplicate_attribute_wins() {
        assert_eq!(all("<p id=a id=b class=c>"), [start("p", &[("id", "a"), ("class", "c")])]);
    }

    #[test]
    fn a_quoted_value_can_hold_a_greater_than_sign() {
        assert_eq!(all("<a title=\">\">x"), [start("a", &[("title", ">")]), text("x")]);
        // A regex that looks for ">" would end the tag too early.
    }

    #[test]
    fn handles_odd_attribute_syntax() {
        assert_eq!(all("<p a=\"b\"c=d>"), [start("p", &[("a", "b"), ("c", "d")])]);
        assert_eq!(all("<p =x>"), [start("p", &[("=x", "")])]);
        assert_eq!(all("<p class=>"), [start("p", &[("class", "")])]);
        assert_eq!(all("<p a/b>"), [start("p", &[("a", ""), ("b", "")])]);
    }

    #[test]
    fn self_closing_flag() {
        assert_eq!(all("<br/><img src=a />"), [
            Token::StartTag { name: "br".into(), attrs: vec![], self_closing: true },
            Token::StartTag { name: "img".into(), attrs: vec![("src".into(), "a".into())], self_closing: true },
        ]);
    }

    #[test]
    fn end_tag_attributes_are_ignored() {
        assert_eq!(all("</p class=x>"), [end("p")]);
    }

    #[test]
    fn decodes_references_in_text_and_attributes() {
        assert_eq!(all("a &lt; b &amp; c &copy 2024"), [text("a < b & c © 2024")]);
        // One `Text` token, even though the text has references in it.
        assert_eq!(all("<a href=\"?x=1&copy=2&amp;y=3\">"), [start("a", &[("href", "?x=1&copy=2&y=3")])]);
        // In an attribute, "&copy=" stays. "&amp;" is decoded.
    }

    #[test]
    fn a_lone_less_than_sign_is_text() {
        assert_eq!(all("a < b <3 <"), [text("a < b <3 <")]);
        assert_eq!(all("x </"), [text("x </")]);
    }

    #[test]
    fn reads_comments() {
        assert_eq!(all("a<!-- hi -->b"), [text("a"), Token::Comment(" hi ".into()), text("b")]);
        assert_eq!(all("<!---->"), [Token::Comment("".into())]);
        assert_eq!(all("<!-->x"), [Token::Comment("".into()), text("x")]);
        assert_eq!(all("<!--->x"), [Token::Comment("".into()), text("x")]);
        assert_eq!(all("<!-- a -- b --!>x"), [Token::Comment(" a -- b ".into()), text("x")]);
        assert_eq!(all("<!-- cut"), [Token::Comment(" cut".into())]);
        // A comment that never ends takes the rest of the page.
    }

    #[test]
    fn bogus_comments() {
        assert_eq!(all("<?xml version=1?>x"), [Token::Comment("?xml version=1?".into()), text("x")]);
        assert_eq!(all("</ nope>x"), [Token::Comment(" nope".into()), text("x")]);
        assert_eq!(all("<![CDATA[x]]>"), [Token::Comment("[CDATA[x]]".into())]);
        assert_eq!(all("<!foo>"), [Token::Comment("foo".into())]);
    }

    #[test]
    fn reads_the_doctype() {
        assert_eq!(all("<!DOCTYPE html>"), [Token::Doctype("html".into())]);
        assert_eq!(all("<!doctype  HTML PUBLIC \"-//W3C//DTD HTML 4.01//EN\">x"), [Token::Doctype("html".into()), text("x")]);
        assert_eq!(all("<!DOCTYPE>"), [Token::Doctype("".into())]);
    }

    #[test]
    fn script_holds_raw_text() {
        assert_eq!(all("<script>if (a<b && c>d) { x = \"<b>&amp;\" }</script>"), [
            start("script", &[]),
            text("if (a<b && c>d) { x = \"<b>&amp;\" }"),
            end("script"),
        ]);
        // No tags and no references inside a script.
    }

    #[test]
    fn raw_text_ends_only_at_the_right_end_tag() {
        assert_eq!(all("<style>a</styles></style >b"), [start("style", &[]), text("a</styles>"), end("style"), text("b")]);
        assert_eq!(all("<SCRIPT>x</ScRiPt>"), [start("script", &[]), text("x"), end("script")]);
        assert_eq!(all("<script>x</script"), [start("script", &[]), text("x</script")]);
        // The end tag is not complete, so it is still text.
    }

    #[test]
    fn title_and_textarea_decode_references_but_not_tags() {
        assert_eq!(all("<title>a &amp; <b></title>"), [start("title", &[]), text("a & <b>"), end("title")]);
        assert_eq!(all("<textarea><p>&lt;</textarea>"), [start("textarea", &[]), text("<p><"), end("textarea")]);
    }

    #[test]
    fn plaintext_never_ends() {
        assert_eq!(all("<plaintext></plaintext><b>"), [start("plaintext", &[]), text("</plaintext><b>")]);
    }

    #[test]
    fn a_cut_tag_is_dropped() {
        assert_eq!(all("a<p class=\"x"), [text("a")]);
        assert_eq!(all("a<p"), [text("a")]);
        assert_eq!(all("a</p"), [text("a")]);
        // The page ends in the middle of a tag. The standard says to drop that tag.
    }

    #[test]
    fn nul_characters() {
        assert_eq!(all("a\0b"), [text("ab")]);
        assert_eq!(all("<a\0b>"), [start("a\u{FFFD}b", &[])]);
    }

    #[test]
    fn gives_tokens_one_at_a_time() {
        // The iterator must not read the whole page to give the first token.
        let page = format!("<p>{}", "x<b>".repeat(1000));
        let mut tokens = tokenize(&page);
        assert_eq!(tokens.next(), Some(start("p", &[])));
        assert!(tokens.pos < 10, "read {} bytes for one token", tokens.pos);
        assert_eq!(tokens.next(), Some(text("x")));
        assert_eq!(tokens.next(), Some(start("b", &[])));
    }

    #[test]
    fn multibyte_characters_next_to_markup() {
        // `back` must step over the whole character, not one byte.
        assert_eq!(all("é<p>é€</p>😀"), [text("é"), start("p", &[]), text("é€"), end("p"), text("😀")]);
        assert_eq!(all("<é>"), [text("<é>")]); // A tag name starts with an ASCII letter.
        assert_eq!(all("<pé>"), [start("pé", &[])]);
        assert_eq!(all("a<€"), [text("a<€")]);
        assert_eq!(all("<!--é-->"), [Token::Comment("é".into())]);
    }

    #[test]
    fn borrows_from_the_page_when_it_can() {
        // A `Cow::Borrowed` is a slice of the page: no new `String`. We only make one when the text changes.
        let tokens = all("<p class=a id=\"b&amp;c\">text</p><P>x &amp; y</P>");
        let Token::StartTag { name, attrs, .. } = &tokens[0] else { panic!("not a start tag") };
        assert!(matches!(name, Cow::Borrowed("p")));
        assert!(matches!(attrs[0].0, Cow::Borrowed("class")));
        assert!(matches!(attrs[0].1, Cow::Borrowed("a")));
        assert!(matches!(attrs[1].1, Cow::Owned(_))); // `&amp;` was decoded.
        assert!(matches!(&tokens[1], Token::Text(Cow::Borrowed("text"))));
        assert!(matches!(&tokens[2], Token::EndTag { name: Cow::Borrowed("p") }));
        assert!(matches!(&tokens[3], Token::StartTag { name: Cow::Owned(_), .. })); // `P` became `p`.
        assert!(matches!(&tokens[4], Token::Text(Cow::Owned(_)))); // `&amp;` was decoded.
        assert_eq!(tokens[4], text("x & y"));
    }

    #[test]
    fn empty_input() {
        assert_eq!(all(""), []);
    }

    #[test]
    fn a_page() {
        let page = "<!DOCTYPE html>\n<html lang=en>\n<head><meta charset=utf-8><title>Hi</title></head>\n<body><p>x</p></body></html>";
        let tokens = all(page);
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
            all(&page);
            // We only check that it ends and does not crash.
        }
    }
}
