//! The CSS parser: turn tokens into rules and declarations.
//!
//! ```text
//! p.big { margin: 10px !important; color: red }        a style rule
//! @media screen { p { color: red } }                    an at-rule with rules inside
//! @import "a.css";                                      an at-rule with no block
//! ```
//!
//! A style sheet is a list of rules. A rule is either:
//! - a style rule: a "prelude" (the selectors, `p.big`) and a block of declarations (`margin: 10px`), or
//! - an at-rule: it starts with `@`, then a name, a prelude, and a block or a `;`.
//!
//! This is CSS Syntax Level 3, section 5. It says exactly what to do with every kind of broken CSS.
//! That is why old pages with CSS errors look the same in all browsers: each browser throws away
//! the same broken piece, and keeps the rest.
//!
//! The error rules, in short:
//! - A declaration that is broken (no `:`, or no name) is skipped up to the next `;`. The ones before
//!   and after it are kept: `a { color: red; oops; margin: 0 }` keeps `color` and `margin`.
//! - A block that is not closed goes to the end of the style sheet, and its content is kept.
//! - A rule with no `{` at the end of the style sheet is thrown away.
//! - `;`, `)`, `]` and `}` inside a string, a url, or a nested block `(...)` `[...]` `{...}` do not end anything.
//!   The tokenizer already made strings and urls one token. The parser skips nested blocks as a whole.
//!
//! What we keep for later:
//! - The prelude of a rule and the value of a declaration stay as text: a slice of the style sheet.
//!   The selector parser (topic 3) and the value parser (topic 4) read them. This way the parser
//!   copies nothing, and it reads each token once.
//! - Names are lowercase. CSS names are not case sensitive, except custom properties (`--Name`).
//!
//! Left out:
//! - CSS nesting (a rule inside a style rule). The inner rule is skipped.
//! - Nothing here knows what a property or an at-rule means. `@foo {}` is a good at-rule here.
//!
//! Comments compare Rust with JavaScript, because you know JS.

use std::borrow::Cow;

use crate::tokenizer::{Token, Tokens, tokenize};

/// A rule of a style sheet.
#[derive(Debug, PartialEq)]
pub enum Rule<'a> {
    // JS: like a TypeScript union of objects, each with its own fields.
    Style(StyleRule<'a>),
    At(AtRule<'a>),
}

/// `p.big { margin: 10px }`.
#[derive(Debug, PartialEq)]
pub struct StyleRule<'a> {
    /// The text before the `{`, without the spaces at the ends: `p.big`. Not parsed yet.
    pub prelude: &'a str,
    pub declarations: Vec<Declaration<'a>>,
}

/// `@media screen { ... }` or `@import "a.css";`.
#[derive(Debug, PartialEq)]
pub struct AtRule<'a> {
    /// The name in lowercase, without the `@`: `media`.
    pub name: Cow<'a, str>,
    /// The text between the name and the block or the `;`, without the spaces at the ends: `screen`.
    pub prelude: &'a str,
    pub body: AtBody<'a>,
}

/// What is inside the `{ }` of an at-rule.
#[derive(Debug, PartialEq)]
pub enum AtBody<'a> {
    /// No block: the rule ended with `;`, or the style sheet ended.
    None,
    /// Rules, like in `@media` and `@supports`.
    Rules(Vec<Rule<'a>>),
    /// Declarations, like in `@font-face` and `@page`.
    Declarations(Vec<Declaration<'a>>),
    /// A block of an at-rule that we do not know. We read it and throw it away.
    Skipped,
}

/// `margin: 10px !important`.
#[derive(Debug, PartialEq)]
pub struct Declaration<'a> {
    /// The property name in lowercase: `margin`. A custom property (`--x`) keeps its letter case.
    pub name: Cow<'a, str>,
    /// The text after the `:`, without the spaces at the ends and without `!important`: `10px`. Not parsed yet.
    pub value: &'a str,
    pub important: bool,
}

/// Blocks inside blocks go this deep. Deeper ones are skipped. This protects us from a style sheet
/// like `@media{@media{@media{...` that would make the parser call itself until the stack is full.
const MAX_DEPTH: usize = 32;

/// Parse a style sheet.
pub fn parse_stylesheet(input: &str) -> Vec<Rule<'_>> {
    Parser::new(input).rule_list(false, 0)
}

/// Parse a list of declarations with no `{ }` around it: the `style="..."` attribute of an element.
pub fn parse_declarations(input: &str) -> Vec<Declaration<'_>> {
    Parser::new(input).declaration_list(false)
}

/// The three kinds of block.
#[derive(Clone, Copy, PartialEq)]
enum Block {
    Curly,
    Paren,
    Square,
}

/// A token, with the byte where it starts and the byte where it ends.
type Spanned<'a> = (Token<'a>, usize, usize);

struct Parser<'a> {
    input: &'a str,
    tokens: Tokens<'a>,
    peeked: Option<Spanned<'a>>,
    // The next token, when we have looked at it. JS: like `iterator.peek()`, which the tokenizer does not have.
    last_end: usize,
    // Where the last token that we read ends.
    stack: Vec<Block>,
    // Used by `skip_block`. It lives here, so that skipping many blocks does not allocate each time.
}

impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Parser<'a> {
        Parser { input, tokens: tokenize(input), peeked: None, last_end: 0, stack: Vec::new() }
    }

    // ---- Reading tokens ----

    /// Look at the next token, and do not read it.
    fn peek(&mut self) -> Option<&Token<'a>> {
        if self.peeked.is_none() {
            self.peeked = self.tokens.next_spanned();
        }
        self.peeked.as_ref().map(|(token, _, _)| token)
    }

    /// Read the next token.
    fn bump(&mut self) -> Option<Spanned<'a>> {
        self.peek();
        let spanned = self.peeked.take()?;
        self.last_end = spanned.2;
        Some(spanned)
    }

    /// The byte where the next token starts. At the end, the length of the style sheet.
    fn next_start(&mut self) -> usize {
        self.peek();
        self.peeked.as_ref().map_or(self.input.len(), |(_, start, _)| *start)
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(Token::Whitespace)) {
            self.bump();
        }
    }

    /// Skip a block that we just started. `open` is its kind. We stop after the token that closes it,
    /// or at the end of the style sheet. Gives the byte where the block ends.
    /// Blocks inside it are skipped too. A `]` does not close a `(`: only the right closing token does.
    fn skip_block(&mut self, open: Block) -> usize {
        self.stack.clear();
        self.stack.push(open);
        // A list of the blocks that are open now, and not a function that calls itself: a style sheet with
        // 100,000 `(` in a row must not fill the stack.
        while let Some((token, _, end)) = self.bump() {
            if block_end(&token) == self.stack.last().copied() {
                self.stack.pop();
                if self.stack.is_empty() {
                    return end;
                }
            } else if let Some(inner) = block_start(&token) {
                self.stack.push(inner);
            }
        }
        self.last_end
    }

    // ---- Rules ----

    /// Parse rules until the end of the style sheet, or, when `nested`, until the `}` that closes the block we are in.
    fn rule_list(&mut self, nested: bool, depth: usize) -> Vec<Rule<'a>> {
        let mut rules = Vec::new();
        loop {
            match self.peek() {
                None => return rules,
                Some(Token::Whitespace) => {
                    self.bump();
                }
                Some(Token::Cdo | Token::Cdc) if !nested => {
                    // `<!--` and `-->` at the top are ignored. Old pages put CSS inside HTML comments.
                    self.bump();
                }
                Some(Token::CloseCurly) if nested => {
                    self.bump();
                    return rules;
                }
                Some(Token::AtKeyword(_)) => rules.extend(self.at_rule(nested, depth).map(Rule::At)),
                Some(_) => rules.extend(self.qualified_rule(nested, depth).map(Rule::Style)),
                // JS: `rules.push(...)` only when there is a rule. `extend` with an `Option` adds zero or one.
            }
        }
    }

    /// A style rule: the prelude up to `{`, and then the declarations. Gives `None` when the rule is broken.
    fn qualified_rule(&mut self, nested: bool, depth: usize) -> Option<StyleRule<'a>> {
        let start = self.next_start();
        let mut end = start;
        loop {
            match self.peek() {
                None => return None,
                // The style sheet ended before the `{`. The standard says to throw the rule away.
                Some(Token::CloseCurly) if nested => return None,
                // The block that holds us ends. The rule is cut: we throw it away, and the caller reads the `}`.
                Some(Token::OpenCurly) => break,
                Some(_) => {
                    let (token, _, token_end) = self.bump()?;
                    end = match block_start(&token) {
                        Some(block) => self.skip_block(block),
                        None => token_end,
                    };
                }
            }
        }
        let prelude = self.input[start..end].trim_matches(SPACES);
        self.bump(); // The `{`.
        if depth >= MAX_DEPTH {
            self.skip_block(Block::Curly);
            return None;
        }
        let declarations = self.declaration_list(true);
        // We read the block even if the prelude is empty, so that we are after its `}`.
        (!prelude.is_empty()).then_some(StyleRule { prelude, declarations })
    }

    /// An at-rule. We are at the `@name` token.
    fn at_rule(&mut self, nested: bool, depth: usize) -> Option<AtRule<'a>> {
        let (Token::AtKeyword(name), _, _) = self.bump()? else { return None };
        let name = lowercase(name);
        let start = self.next_start();
        let mut end = start;
        let has_block = loop {
            match self.peek() {
                None => break false,
                Some(Token::Semicolon) => {
                    self.bump();
                    break false;
                }
                Some(Token::CloseCurly) if nested => break false,
                // The `}` of the block that holds us. We do not read it.
                Some(Token::OpenCurly) => {
                    self.bump();
                    break true;
                }
                Some(_) => {
                    let (token, _, token_end) = self.bump()?;
                    end = match block_start(&token) {
                        Some(block) => self.skip_block(block),
                        None => token_end,
                    };
                }
            }
        };
        let prelude = self.input[start..end].trim_matches(SPACES);
        let body = if !has_block {
            AtBody::None
        } else if depth >= MAX_DEPTH {
            self.skip_block(Block::Curly);
            AtBody::Skipped
        } else {
            match &*name {
                "media" | "supports" | "layer" | "container" | "document" | "-moz-document" | "scope" | "starting-style" => AtBody::Rules(self.rule_list(true, depth + 1)),
                n if n.ends_with("keyframes") => AtBody::Rules(self.rule_list(true, depth + 1)),
                "font-face" | "page" | "property" | "counter-style" | "viewport" | "-ms-viewport" | "font-palette-values" => AtBody::Declarations(self.declaration_list(true)),
                _ => {
                    self.skip_block(Block::Curly);
                    AtBody::Skipped
                }
            }
        };
        Some(AtRule { name, prelude, body })
    }

    // ---- Declarations ----

    /// Parse declarations until the end of the style sheet, or, when `nested`, until the `}` of our block.
    fn declaration_list(&mut self, nested: bool) -> Vec<Declaration<'a>> {
        let mut declarations = Vec::new();
        loop {
            match self.peek() {
                None => return declarations,
                Some(Token::Whitespace | Token::Semicolon) => {
                    self.bump();
                }
                Some(Token::CloseCurly) if nested => {
                    self.bump();
                    return declarations;
                }
                Some(Token::AtKeyword(_)) => self.skip_statement(nested, true),
                // An at-rule inside a block: we do not use it. We skip it, with its block.
                Some(Token::Ident(_)) => declarations.extend(self.declaration(nested)),
                Some(_) => self.skip_statement(nested, false),
                // Something that cannot start a declaration. The standard says: skip to the next `;`.
            }
        }
    }

    /// `name: value`. We are at the name. Gives `None` and skips to the next `;` when it is broken.
    fn declaration(&mut self, nested: bool) -> Option<Declaration<'a>> {
        let Some((Token::Ident(name), _, _)) = self.bump() else { return None };
        self.skip_whitespace();
        if !matches!(self.peek(), Some(Token::Colon)) {
            self.skip_statement(nested, false);
            return None;
        }
        self.bump();
        let (start, end, important) = self.value(nested);
        let name = if name.starts_with("--") { name } else { lowercase(name) };
        // The name of a custom property is case sensitive. The others are not.
        Some(Declaration { name, value: &self.input[start..end], important })
    }

    /// Read the value of a declaration: everything up to the next `;`, or the end of the block, or the end of the
    /// style sheet. Gives where it starts and ends, and if it ends with `!important`.
    fn value(&mut self, nested: bool) -> (usize, usize, bool) {
        let mut start = None;
        let mut end = 0;
        // `end` is where the last token that counts ends. White space does not count.
        let mut bang = None;
        // After a `!`: where the value ended before it. If `important` follows, the value ends there.
        let mut important = false;
        loop {
            match self.peek() {
                None | Some(Token::Semicolon) => break,
                Some(Token::CloseCurly) if nested => break,
                Some(Token::Whitespace) => {
                    self.bump();
                    continue;
                }
                Some(_) => {}
            }
            let Some((token, token_start, token_end)) = self.bump() else { break };
            start.get_or_insert(token_start);
            let before = end;
            end = token_end;
            if let Some(block) = block_start(&token) {
                // `(...)`, `[...]`, `{...}` or `name(...)`: one piece, whatever is inside.
                end = self.skip_block(block);
                bang = None;
                important = false;
                continue;
            }
            match token {
                Token::Delim('!') => {
                    bang = Some(before);
                    important = false;
                }
                Token::Ident(word) if bang.is_some() && !important && word.eq_ignore_ascii_case("important") => important = true,
                _ => {
                    bang = None;
                    important = false;
                }
            }
        }
        if important && let Some(before) = bang {
            end = before;
            // The `!important` is not part of the value.
        }
        (start.unwrap_or(end).min(end), end, important)
    }

    /// Skip tokens up to the next `;` (we do not read it), or up to the `}` of our block (we do not read it either),
    /// or up to the end. Blocks inside are skipped as a whole, so a `;` or `}` in them does not stop us.
    /// With `stop_after_block`, the end of a `{ }` block also ends the statement. This is for an at-rule.
    fn skip_statement(&mut self, nested: bool, stop_after_block: bool) {
        loop {
            match self.peek() {
                None | Some(Token::Semicolon) => return,
                Some(Token::CloseCurly) if nested => return,
                _ => {}
            }
            let Some((token, _, _)) = self.bump() else { return };
            if let Some(block) = block_start(&token) {
                self.skip_block(block);
                if stop_after_block && block == Block::Curly {
                    return;
                }
            }
        }
    }
}

/// The five spaces of CSS.
const SPACES: [char; 5] = [' ', '\t', '\n', '\r', '\x0C'];

/// The kind of block that this token opens. A function (`rgb(`) opens a parenthesis.
fn block_start(token: &Token<'_>) -> Option<Block> {
    match token {
        Token::OpenCurly => Some(Block::Curly),
        Token::OpenParen | Token::Function(_) => Some(Block::Paren),
        Token::OpenSquare => Some(Block::Square),
        _ => None,
    }
}

/// The kind of block that this token closes.
fn block_end(token: &Token<'_>) -> Option<Block> {
    match token {
        Token::CloseCurly => Some(Block::Curly),
        Token::CloseParen => Some(Block::Paren),
        Token::CloseSquare => Some(Block::Square),
        _ => None,
    }
}

/// A name in lowercase. A new `String` is made only when there is a capital letter.
fn lowercase(name: Cow<'_, str>) -> Cow<'_, str> {
    if name.bytes().any(|b| b.is_ascii_uppercase()) { Cow::Owned(name.to_ascii_lowercase()) } else { name }
}

#[cfg(test)]
// Only compiled for `cargo test`.
mod tests {
    use super::*;

    /// The rules as one line of text, so that a test can compare it with a string.
    /// `p{color:red;margin:0!}` is a style rule. A `!` after a value means `!important`.
    fn show(rules: &[Rule<'_>]) -> String {
        rules
            .iter()
            .map(|rule| match rule {
                Rule::Style(r) => format!("{}{{{}}}", r.prelude, show_declarations(&r.declarations)),
                Rule::At(r) => {
                    let body = match &r.body {
                        AtBody::None => ";".to_string(),
                        AtBody::Rules(rules) => format!("{{{}}}", show(rules)),
                        AtBody::Declarations(d) => format!("{{{}}}", show_declarations(d)),
                        AtBody::Skipped => "{skipped}".to_string(),
                    };
                    format!("@{} {}{}", r.name, r.prelude, body)
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn show_declarations(declarations: &[Declaration<'_>]) -> String {
        declarations.iter().map(|d| format!("{}:{}{}", d.name, d.value, if d.important { "!" } else { "" })).collect::<Vec<_>>().join(";")
    }

    fn sheet(input: &str) -> String {
        show(&parse_stylesheet(input))
    }

    #[test]
    fn reads_style_rules() {
        assert_eq!(sheet("p.big { margin: 10px 2em; color: red }"), "p.big{margin:10px 2em;color:red}");
        assert_eq!(sheet("a{} b{c:d}"), "a{} b{c:d}");
        assert_eq!(sheet("  /* c */ a , b > c {x:y}  "), "a , b > c{x:y}");
        // The prelude is the text as it is, with the spaces at the ends cut off. The selector parser reads it.
    }

    #[test]
    fn property_names_are_lowercase_but_custom_ones_keep_their_case() {
        assert_eq!(sheet("a{COLOR:Red;--Foo:Bar}"), "a{color:Red;--Foo:Bar}");
    }

    #[test]
    fn important() {
        assert_eq!(sheet("a{x:red!important}"), "a{x:red!}");
        assert_eq!(sheet("a{x:red ! important}"), "a{x:red!}");
        assert_eq!(sheet("a{x:red !IMPORTANT ;y:1}"), "a{x:red!;y:1}");
        assert_eq!(sheet("a{x:red !important z}"), "a{x:red !important z}");
        // `important` must be the last token.
        assert_eq!(sheet("a{x:!important}"), "a{x:!}");
        assert_eq!(sheet("a{x:red !important !important}"), "a{x:red !important!}");
        assert_eq!(sheet("a{x:red !imp}"), "a{x:red !imp}");
        assert_eq!(sheet("a{x:red!}"), "a{x:red!}");
        // That last `!` is a plain `!` in the value, and not a flag. The text only looks like the flag.
    }

    #[test]
    fn broken_declarations_are_skipped_and_the_others_are_kept() {
        assert_eq!(sheet("a{color:red;oops;margin:0}"), "a{color:red;margin:0}");
        assert_eq!(sheet("a{color:red;;;margin:0}"), "a{color:red;margin:0}");
        assert_eq!(sheet("a{:red;margin:0}"), "a{margin:0}");
        assert_eq!(sheet("a{1x:y;margin:0}"), "a{margin:0}");
        assert_eq!(sheet("a{color red;margin:0}"), "a{margin:0}");
        assert_eq!(sheet("a{color:red margin:0}"), "a{color:red margin:0}");
        // No `;` between them: this is one declaration with a long value. The value parser will reject it.
    }

    #[test]
    fn a_block_that_is_not_closed_keeps_its_content() {
        assert_eq!(sheet("a{color:red"), "a{color:red}");
        assert_eq!(sheet("a{color:red;"), "a{color:red}");
        assert_eq!(sheet("a{b:c} d{e:f"), "a{b:c} d{e:f}");
    }

    #[test]
    fn a_rule_with_no_block_is_dropped() {
        assert_eq!(sheet("a b"), "");
        assert_eq!(sheet("a{b:c} d e"), "a{b:c}");
        assert_eq!(sheet("{a:b}"), "");
        // No prelude: no rule.
    }

    #[test]
    fn strings_urls_and_blocks_hide_their_semicolons_and_braces() {
        assert_eq!(sheet("a{content:\";}\";x:y}"), "a{content:\";}\";x:y}");
        assert_eq!(sheet("a{background:url(a;b}c);x:y}"), "a{background:url(a;b}c);x:y}");
        assert_eq!(sheet("a{x:(1;2);y:z}"), "a{x:(1;2);y:z}");
        assert_eq!(sheet("a{x:[1;2];y:z}"), "a{x:[1;2];y:z}");
        assert_eq!(sheet("a{x:f(1;2);y:z}"), "a{x:f(1;2);y:z}");
        assert_eq!(sheet("a[title=\"{\"]{x:y}"), "a[title=\"{\"]{x:y}");
        assert_eq!(sheet("a[b={]{x:y}"), "");
        // The `{` inside the `[` opens a block. The `]` after it cannot close the `[` while that block is open.
        // Then the `}` closes the block, and the `[` is still open at the end of the style sheet. So there is no rule.
    }

    #[test]
    fn a_closing_token_of_the_wrong_kind_does_not_close_a_block() {
        assert_eq!(sheet("a{x:(1]2);y:z}"), "a{x:(1]2);y:z}");
        assert_eq!(sheet("a{x:(1}2);y:z}"), "a{x:(1}2);y:z}");
    }

    #[test]
    fn custom_properties_can_hold_blocks() {
        assert_eq!(sheet("a{--x: { a: b };--y:;z:1}"), "a{--x:{ a: b };--y:;z:1}");
    }

    #[test]
    fn broken_junk_inside_a_block_is_skipped_to_the_next_semicolon() {
        assert_eq!(sheet("a{ ] ; x:y}"), "a{x:y}");
        assert_eq!(sheet("a{ {x:y} ; z:1}"), "a{z:1}");
        // A `{ }` block with no name in front is junk. We skip it whole.
        assert_eq!(sheet("a{ b {c:d} e:f}"), "a{}");
        // `b {c:d} e:f` has no `;`: one broken statement. (CSS nesting will change this later.)
    }

    #[test]
    fn at_rules_without_a_block() {
        assert_eq!(sheet("@import \"a.css\"; a{b:c}"), "@import \"a.css\"; a{b:c}");
        assert_eq!(sheet("@charset \"utf-8\";"), "@charset \"utf-8\";");
        assert_eq!(sheet("@import url(a.css) screen"), "@import url(a.css) screen;");
        // The style sheet ended: the at-rule is complete. The standard says so.
        assert_eq!(sheet("@layer a, b;"), "@layer a, b;");
        assert_eq!(sheet("@IMPORT 'a';"), "@import 'a';");
    }

    #[test]
    fn at_rules_with_rules_inside() {
        assert_eq!(sheet("@media screen and (min-width:10px){a{b:c} d{e:f}} g{h:i}"), "@media screen and (min-width:10px){a{b:c} d{e:f}} g{h:i}");
        assert_eq!(sheet("@media print{@media screen{a{b:c}}}"), "@media print{@media screen{a{b:c}}}");
        assert_eq!(sheet("@keyframes x{from{a:b}50%{a:c}to{a:d}}"), "@keyframes x{from{a:b} 50%{a:c} to{a:d}}");
        assert_eq!(sheet("@-webkit-keyframes x{from{a:b}}"), "@-webkit-keyframes x{from{a:b}}");
        assert_eq!(sheet("@media screen{a{b:c}"), "@media screen{a{b:c}}");
        // The block is not closed. The rules in it are kept.
    }

    #[test]
    fn at_rules_with_declarations_inside() {
        assert_eq!(sheet("@font-face{font-family:x;src:url(a.woff)}"), "@font-face {font-family:x;src:url(a.woff)}");
        assert_eq!(sheet("@page :first{margin:1cm}"), "@page :first{margin:1cm}");
    }

    #[test]
    fn unknown_at_rules_are_skipped_with_their_block() {
        assert_eq!(sheet("@foo bar{a{b:c}} x{y:z}"), "@foo bar{skipped} x{y:z}");
        assert_eq!(sheet("@foo{ } x{y:z}"), "@foo {skipped} x{y:z}");
    }

    #[test]
    fn at_rules_do_not_read_past_the_end_of_their_block() {
        assert_eq!(sheet("@media screen{a{b:c} @import 'x' } d{e:f}"), "@media screen{a{b:c} @import 'x';} d{e:f}");
        assert_eq!(sheet("@media screen{a{b:c}"), "@media screen{a{b:c}}");
    }

    #[test]
    fn cdo_and_cdc_are_ignored_at_the_top() {
        assert_eq!(sheet("<!-- a{b:c} -->"), "a{b:c}");
        assert_eq!(sheet("<!-- --> <!--"), "");
    }

    #[test]
    fn a_stray_closing_brace_at_the_top_joins_the_next_prelude() {
        assert_eq!(sheet("a{b:c}} d{e:f}"), "a{b:c} } d{e:f}");
        // The first `}` closes `a`. The second one is a plain token at the top, so it is in the prelude of the next rule.
        // The selector parser will reject that prelude, and the rule is lost. This is what the standard says.
    }

    #[test]
    fn style_attribute() {
        let d = parse_declarations("color: red; margin: 0 auto !important; oops; }; padding: 1px");
        assert_eq!(show_declarations(&d), "color:red;margin:0 auto!;padding:1px");
        assert_eq!(show_declarations(&parse_declarations("")), "");
    }

    #[test]
    fn comments_in_values() {
        assert_eq!(sheet("a{x:/* c */red/* d */;y:1}"), "a{x:red;y:1}");
        // The value starts at the first token and ends at the last one, so the comments at the ends are not in it.
        assert_eq!(sheet("a{x:1/* c */2}"), "a{x:1/* c */2}");
        // A comment in the middle stays in the slice. The value parser skips comments.
    }

    #[test]
    fn deep_nesting_does_not_overflow_the_stack() {
        let sheet_text = "@media{".repeat(10_000) + "a{b:c}";
        let rules = parse_stylesheet(&sheet_text);
        assert_eq!(rules.len(), 1);
        let parens = format!("a{{x:{}1{}}}", "(".repeat(100_000), ")".repeat(100_000));
        assert_eq!(parse_stylesheet(&parens).len(), 1);
        // 100,000 `(` in a row: `skip_block` uses a list, and not a function that calls itself.
    }

    #[test]
    fn never_panics_or_loops_on_junk() {
        let pieces = [
            "a", "{", "}", "(", ")", "[", "]", ";", ":", ",", "!", "important", "@media", "@import", "@font-face", "url(", "url(x)", "\"", "'", "\\", "/*", "*/", "<!--", "-->", " ", "\n", "1px", "#fff", "rgb(", "--x", "é", "\0", "!important", "@", ".", "&",
        ];
        let mut seed: u32 = 12345;
        for _ in 0..6000 {
            let mut text = String::new();
            for _ in 0..(seed % 30) {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                // A simple random number generator. The same numbers every run, so a failure can be repeated.
                text.push_str(pieces[(seed >> 16) as usize % pieces.len()]);
            }
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            // The slices must be valid, and the parser must end. `show` reads every slice.
            show(&parse_stylesheet(&text));
            show_declarations(&parse_declarations(&text));
        }
    }
}
