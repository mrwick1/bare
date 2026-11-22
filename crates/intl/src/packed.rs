//! Store generated text as UTF-8 and byte ranges. The compiler packs the source
//! tables, so the loader has no string pointers to fix up at process startup.

#[derive(Clone, Copy)]
struct Span {
    start: u16,
    end: u16,
}

pub(crate) struct Packed<const N: usize, const C: usize, const B: usize, T> {
    pub(crate) bytes: [u8; B],
    rows: [([Span; C], T); N],
}

pub(crate) const fn byte_len<const N: usize, const C: usize, T>(rows: &[([&str; C], T); N]) -> usize {
    let mut len = 0;
    let mut i = 0;
    while i < N {
        let mut col = 0;
        while col < C {
            len += rows[i].0[col].len();
            col += 1;
        }
        i += 1;
    }
    len
}

impl<const N: usize, const C: usize, const B: usize, T: Copy> Packed<N, C, B, T> {
    pub(crate) const fn new(rows: [([&str; C], T); N]) -> Self {
        assert!(N != 0 && B == byte_len(&rows) && B <= u16::MAX as usize);
        let mut bytes = [0; B];
        let empty = Span { start: 0, end: 0 };
        let mut packed = [([empty; C], rows[0].1); N];
        let mut end = 0;
        let mut i = 0;
        while i < N {
            let mut col = 0;
            while col < C {
                let text = rows[i].0[col].as_bytes();
                let start = end;
                let mut j = 0;
                while j < text.len() {
                    bytes[end] = text[j];
                    end += 1;
                    j += 1;
                }
                packed[i].0[col] = Span { start: start as u16, end: end as u16 };
                col += 1;
            }
            packed[i].1 = rows[i].1;
            i += 1;
        }
        Self { bytes, rows: packed }
    }

    pub(crate) const fn view(&'static self, text: &'static str) -> Table<C, T> {
        Table { text, rows: &self.rows }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Table<const C: usize, T: 'static = ()> {
    text: &'static str,
    rows: &'static [([Span; C], T)],
}

impl<const C: usize, T: Copy> Table<C, T> {
    pub(crate) fn len(self) -> usize {
        self.rows.len()
    }

    pub(crate) fn text(self, i: usize, col: usize) -> &'static str {
        let span = self.rows[i].0[col];
        &self.text[span.start as usize..span.end as usize]
    }

    pub(crate) fn meta(self, i: usize) -> T {
        self.rows[i].1
    }

    pub(crate) fn search_by(self, mut compare: impl FnMut(T) -> core::cmp::Ordering) -> Option<usize> {
        self.rows.binary_search_by(|row| compare(row.1)).ok()
    }

    /// Look up a key in the first column of a sorted table.
    pub(crate) fn search(self, key: &str) -> Option<usize> {
        self.rows.binary_search_by(|r| {
            let span = r.0[0];
            self.text.as_bytes()[span.start as usize..span.end as usize].cmp(key.as_bytes())
        }).ok()
    }
}

/// Map source rows to (strings, scalar data) and emit one pointer-free static.
/// The UTF-8 check runs at compile time. Only the selected span is read at runtime.
macro_rules! pack_table {
    ($source:expr, $cols:literal, $meta:ty, |$i:ident| $row:expr) => {{
        const N: usize = ($source).len();
        const ROWS: [([&str; $cols], $meta); N] = {
            let $i = 0;
            let mut rows = [$row; N];
            let mut i = 1;
            while i < N {
                let $i = i;
                rows[i] = $row;
                i += 1;
            }
            rows
        };
        static PACKED: $crate::packed::Packed<N, $cols, { $crate::packed::byte_len(&ROWS) }, $meta> =
            $crate::packed::Packed::new(ROWS);
        const TEXT: &str = match core::str::from_utf8(&PACKED.bytes) {
            Ok(text) => text,
            Err(_) => panic!("invalid packed text"),
        };
        PACKED.view(TEXT)
    }};
}

pub(crate) use pack_table;
