//! Make the `::before` and `::after` nodes of a document from its styles (CSS Generated Content). The nodes are in `dom/pseudo.rs`.
//!
//! The cascade cannot change the document, so the host calls `sync` after it, before each layout. A page with no `::before` or `::after`
//! box pays two `is_empty` checks.

use bare_html::dom::Document;

use crate::cascade::Styles;
use crate::values::Property;

/// Show a pseudo node for each `::before` and `::after` box of `styles`, with the text of its `content`, and hide the others. Then
/// `styles` gives the style of each node.
pub fn sync(doc: &mut Document, styles: &Styles) {
    if !doc.has_pseudo() && !styles.has_generated() {
        return;
    }
    let pairs = styles.generated_pairs();
    for (el, which) in doc.shown_pseudo() {
        if pairs.binary_search(&(el.index() as u32, which)).is_err() {
            doc.hide_pseudo(el, which);
        }
    }
    let mut nodes = Vec::with_capacity(pairs.len());
    for (el, which) in pairs {
        let Some(el) = doc.node_at(el as usize) else { continue };
        let Some(style) = styles.generated(el, which) else { continue };
        let attr = |name: &str| doc.find_atom(name).and_then(|a| doc.attr(el, a)).map(str::to_string);
        let text = crate::content::text(style.picture(Property::Content), attr).unwrap_or_default();
        nodes.push((doc.show_pseudo(el, which, &text), el, which));
    }
    styles.set_pseudo_nodes(nodes);
}
