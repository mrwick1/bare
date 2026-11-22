use crate::intl;
use crate::packed::{Table, pack_table};

macro_rules! name_table {
    ($table:ident, $name:ident) => {
        fn $table() -> Table<4> {
            pack_table!(intl::$name, 4, (), |i| {
                let (code, long, short, narrow) = intl::$name[i];
                ([code, long, short, narrow], ())
            })
        }
    };
}

name_table!(language_names, NAMES_LANGUAGE);
name_table!(dialect_names, NAMES_DIALECT);
name_table!(region_names, NAMES_REGION);
name_table!(script_names, NAMES_SCRIPT);
name_table!(currency_names, NAMES_CURRENCY);
name_table!(calendar_names, NAMES_CALENDAR);
name_table!(field_names, NAMES_FIELD);

/// A display names object.
#[derive(Clone, Copy)]
pub struct IntlDn {
    pub loc: u8,
    pub style: u8,
    /// An index of `DN_TYPES`.
    pub ty: u8,
    /// `fallback: "code"` (true) or `"none"`.
    pub code: bool,
    /// `languageDisplay: "dialect"` (true) or `"standard"`.
    pub dialect: bool,
}


/// The name of `code` in a table of `intl_names_data.rs` for `style`, or `None`. The rows are sorted by code.
fn dn_lookup(table: Table<4>, code: &str, style: u8) -> Option<&'static str> {
    let i = table.search(code)?;
    // Short and narrow are empty when they equal the long name.
    let name = table.text(i, 1 + style as usize);
    Some(if name.is_empty() { table.text(i, 1) } else { name })
}

/// `s` with its first letter upper case and the rest lower case (a script code: "latn" is "Latn").
fn title_case(s: &str) -> String {
    let mut out = s.to_ascii_lowercase();
    out[..1].make_ascii_uppercase();
    out
}

/// The display name of a language tag, or `None` when `fallback: "none"` and a part is unknown. `Err` when `tag` is not a
/// `unicode_language_id` (an extension or a private use part is an error, as in V8).
fn dn_language(d: IntlDn, tag: &str) -> Result<Option<String>, ()> {
    let canon = intl::canonical_tag(tag).map_err(|_| ())?;
    let s = intl::loc_split(&canon);
    if !s.ext.is_empty() {
        return Err(());
    }
    // a closure (like an arrow function): the name of a code, else the code itself with fallback "code"
    let named = |table: Table<4>, code: &str| dn_lookup(table, code, d.style).map(str::to_string).or_else(|| d.code.then(|| code.to_string()));
    // a dialect name covers the language with its script or with its region ("zh-Hans", "en-US"), never with both
    let dialect = if d.dialect && (s.script.is_empty() || s.region.is_empty()) {
        let key: Vec<&str> = [s.lang, s.script, s.region].into_iter().filter(|p| !p.is_empty()).collect();
        dn_lookup(dialect_names(), &key.join("-"), d.style)
    } else {
        None
    };
    let mut quals: Vec<String> = Vec::new();
    let base = match dialect {
        Some(n) => n.to_string(),
        None => {
            for (table, code) in [(script_names(), s.script), (region_names(), s.region)] {
                if code.is_empty() {
                    continue;
                }
                let Some(n) = named(table, code) else { return Ok(None) };
                quals.push(n);
            }
            let Some(n) = named(language_names(), s.lang) else { return Ok(None) };
            n
        }
    };
    for v in s.variants.split('-').filter(|v| !v.is_empty()) {
        let Some(n) = d.code.then(|| v.to_string()) else { return Ok(None) };
        quals.push(n);
    }
    Ok(Some(if quals.is_empty() { base } else { format!("{base} ({})", quals.join(", ")) }))
}

/// `of(code)`: `Err` for a code that is not valid for the type (a RangeError), `Ok(None)` for undefined.
pub fn dn_of(d: IntlDn, code: &str) -> Result<Option<String>, ()> {
    let alpha = |n: usize| code.len() == n && code.bytes().all(|c| c.is_ascii_alphabetic());
    // the canonical code and its table
    let (canon, table): (String, Table<4>) = match d.ty {
        0 => return dn_language(d, code),
        1 if alpha(2) || (code.len() == 3 && code.bytes().all(|c| c.is_ascii_digit())) => (code.to_ascii_uppercase(), region_names()),
        2 if alpha(4) => (title_case(code), script_names()),
        3 if alpha(3) => (code.to_ascii_uppercase(), currency_names()),
        4 if !code.is_empty() && code.split('-').all(|p| (3..=8).contains(&p.len()) && p.bytes().all(|c| c.is_ascii_alphanumeric())) => (code.to_ascii_lowercase(), calendar_names()),
        5 if field_names().search(code).is_some() => (code.to_string(), field_names()),
        _ => return Err(()),
    };
    match dn_lookup(table, &canon, d.style) {
        Some(n) => Ok(Some(n.to_string())),
        None if d.code => Ok(Some(canon)),
        None => Ok(None),
    }
}
