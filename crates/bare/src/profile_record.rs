//! The disk form of a clone record (`design-persistent-profile.md`, section 7). A record keeps its `Blob`, `File` and
//! `CryptoKey` objects outside its bytes (`Moved::Data` holding a `WebClone`), so each one gets a tag and its fields here.
//!
//! ```text
//! record  = u64 bytes length, bytes, u64 cost, u32 item count, items
//! item    = 1 blob: str type, u64 length, bytes, u8 has file, [str name, f64 last modified]
//!         | 2 key: u8 algorithm index, u8 extractable, u8 usage bits, u64 length, key bytes
//! str     = u64 length, UTF-8 bytes
//! ```
//!
//! Numbers are little endian. A `WebAssembly.Module`, a transferred buffer or a port has no disk form: `encode` gives false.
// The IndexedDB log (design step 3) is the first user.
#![cfg_attr(not(test), allow(dead_code))]

use crate::browser_web::WebClone;
use bare_js_api::clone::{Cloned, Moved};

/// Append the disk form of `c` to `out`. Gives false (and leaves `out` as it was) when the record holds an item with no disk form.
pub(crate) fn encode(c: &Cloned, out: &mut Vec<u8>) -> bool {
    let start = out.len();
    bytes(out, c.bytes());
    out.extend((c.cost() as u64).to_le_bytes());
    out.extend((c.moved().len() as u32).to_le_bytes());
    for m in c.moved() {
        let ok = match m {
            Moved::Data(d) => match d.downcast_ref::<WebClone>() {
                Some(WebClone::Blob { bytes: b, ty, file }) => {
                    out.push(1);
                    bytes(out, ty.as_bytes());
                    bytes(out, b);
                    out.push(file.is_some() as u8);
                    if let Some((name, modified)) = file {
                        bytes(out, name.as_bytes());
                        out.extend(modified.to_le_bytes());
                    }
                    true
                }
                Some(WebClone::Key(k)) => {
                    let (alg, b, extractable, usages) = k.parts();
                    out.extend([2, alg, extractable as u8, usages]);
                    bytes(out, b);
                    true
                }
                _ => false,
            },
            _ => false,
        };
        if !ok {
            out.truncate(start);
            return false;
        }
    }
    true
}

/// The record that `encode` wrote at the start of `b`, and the bytes after it. `None` for damaged input.
pub(crate) fn decode(b: &[u8]) -> Option<(Cloned, &[u8])> {
    let mut r = Reader(b);
    let data: Box<[u8]> = r.bytes()?.into();
    let cost = usize::try_from(r.u64()?).ok()?;
    let n = u32::from_le_bytes(r.take(4)?.try_into().ok()?);
    let mut moved = Vec::with_capacity(n.min(1024) as usize);
    for _ in 0..n {
        let item = match r.take(1)?[0] {
            1 => {
                let ty = String::from_utf8(r.bytes()?.to_vec()).ok()?;
                let bytes: std::sync::Arc<[u8]> = r.bytes()?.into();
                let file = match r.take(1)?[0] {
                    0 => None,
                    1 => Some((String::from_utf8(r.bytes()?.to_vec()).ok()?, f64::from_le_bytes(r.take(8)?.try_into().ok()?))),
                    _ => return None,
                };
                WebClone::Blob { bytes, ty, file }
            }
            2 => {
                let h = r.take(3)?;
                let (alg, extractable, usages) = (h[0], h[1] != 0, h[2]);
                WebClone::Key(crate::browser_subtle::Key::from_parts(alg, r.bytes()?.to_vec(), extractable, usages)?)
            }
            _ => return None,
        };
        moved.push(Moved::Data(Box::new(item)));
    }
    Some((Cloned::new(data, moved, cost), r.0))
}

fn bytes(out: &mut Vec<u8>, b: &[u8]) {
    out.extend((b.len() as u64).to_le_bytes());
    out.extend_from_slice(b);
}

struct Reader<'b>(&'b [u8]);

impl<'b> Reader<'b> {
    fn take(&mut self, n: usize) -> Option<&'b [u8]> {
        if self.0.len() < n {
            return None;
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Some(a)
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    fn bytes(&mut self) -> Option<&'b [u8]> {
        let n = usize::try_from(self.u64()?).ok()?;
        self.take(n)
    }
}
