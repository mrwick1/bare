//! HPACK (RFC 7541): header compression for HTTP/2.
//!
//! The decoder keeps the dynamic table and checks every length. The encoder keeps no state: it
//! uses the static table and plain literals, and never adds to the dynamic table of the peer, so
//! a `SETTINGS_HEADER_TABLE_SIZE` from the peer never changes what it writes.

use std::collections::VecDeque;

/// Why a header block cannot be decoded. Every one is a `COMPRESSION_ERROR` of the connection.
#[derive(Debug, PartialEq)]
pub struct Error(pub &'static str);

/// The static table (RFC 7541 appendix A). Index 1 is the first entry.
const STATIC: [(&str, &str); 61] = [
    (":authority", ""), (":method", "GET"), (":method", "POST"), (":path", "/"), (":path", "/index.html"), (":scheme", "http"),
    (":scheme", "https"), (":status", "200"), (":status", "204"), (":status", "206"), (":status", "304"), (":status", "400"),
    (":status", "404"), (":status", "500"), ("accept-charset", ""), ("accept-encoding", "gzip, deflate"), ("accept-language", ""),
    ("accept-ranges", ""), ("accept", ""), ("access-control-allow-origin", ""), ("age", ""), ("allow", ""), ("authorization", ""),
    ("cache-control", ""), ("content-disposition", ""), ("content-encoding", ""), ("content-language", ""), ("content-length", ""),
    ("content-location", ""), ("content-range", ""), ("content-type", ""), ("cookie", ""), ("date", ""), ("etag", ""), ("expect", ""),
    ("expires", ""), ("from", ""), ("host", ""), ("if-match", ""), ("if-modified-since", ""), ("if-none-match", ""), ("if-range", ""),
    ("if-unmodified-since", ""), ("last-modified", ""), ("link", ""), ("location", ""), ("max-forwards", ""), ("proxy-authenticate", ""),
    ("proxy-authorization", ""), ("range", ""), ("referer", ""), ("refresh", ""), ("retry-after", ""), ("server", ""), ("set-cookie", ""),
    ("strict-transport-security", ""), ("transfer-encoding", ""), ("user-agent", ""), ("vary", ""), ("via", ""), ("www-authenticate", ""),
];

/// The Huffman code of each byte, and of the end of string (256): (number of bits, bits). RFC 7541 appendix B.
/// Copied from the `h2` crate (MIT, `src/hpack/huffman/table.rs`). The tests check that it is a complete prefix code.
const CODES: [(u8, u32); 257] = [
    (13, 0x1ff8), (23, 0x7fffd8), (28, 0xfffffe2), (28, 0xfffffe3), (28, 0xfffffe4), (28, 0xfffffe5), (28, 0xfffffe6), (28, 0xfffffe7),
    (28, 0xfffffe8), (24, 0xffffea), (30, 0x3ffffffc), (28, 0xfffffe9), (28, 0xfffffea), (30, 0x3ffffffd), (28, 0xfffffeb), (28, 0xfffffec),
    (28, 0xfffffed), (28, 0xfffffee), (28, 0xfffffef), (28, 0xffffff0), (28, 0xffffff1), (28, 0xffffff2), (30, 0x3ffffffe), (28, 0xffffff3),
    (28, 0xffffff4), (28, 0xffffff5), (28, 0xffffff6), (28, 0xffffff7), (28, 0xffffff8), (28, 0xffffff9), (28, 0xffffffa), (28, 0xffffffb),
    (6, 0x14), (10, 0x3f8), (10, 0x3f9), (12, 0xffa), (13, 0x1ff9), (6, 0x15), (8, 0xf8), (11, 0x7fa),
    (10, 0x3fa), (10, 0x3fb), (8, 0xf9), (11, 0x7fb), (8, 0xfa), (6, 0x16), (6, 0x17), (6, 0x18),
    (5, 0x0), (5, 0x1), (5, 0x2), (6, 0x19), (6, 0x1a), (6, 0x1b), (6, 0x1c), (6, 0x1d),
    (6, 0x1e), (6, 0x1f), (7, 0x5c), (8, 0xfb), (15, 0x7ffc), (6, 0x20), (12, 0xffb), (10, 0x3fc),
    (13, 0x1ffa), (6, 0x21), (7, 0x5d), (7, 0x5e), (7, 0x5f), (7, 0x60), (7, 0x61), (7, 0x62),
    (7, 0x63), (7, 0x64), (7, 0x65), (7, 0x66), (7, 0x67), (7, 0x68), (7, 0x69), (7, 0x6a),
    (7, 0x6b), (7, 0x6c), (7, 0x6d), (7, 0x6e), (7, 0x6f), (7, 0x70), (7, 0x71), (7, 0x72),
    (8, 0xfc), (7, 0x73), (8, 0xfd), (13, 0x1ffb), (19, 0x7fff0), (13, 0x1ffc), (14, 0x3ffc), (6, 0x22),
    (15, 0x7ffd), (5, 0x3), (6, 0x23), (5, 0x4), (6, 0x24), (5, 0x5), (6, 0x25), (6, 0x26),
    (6, 0x27), (5, 0x6), (7, 0x74), (7, 0x75), (6, 0x28), (6, 0x29), (6, 0x2a), (5, 0x7),
    (6, 0x2b), (7, 0x76), (6, 0x2c), (5, 0x8), (5, 0x9), (6, 0x2d), (7, 0x77), (7, 0x78),
    (7, 0x79), (7, 0x7a), (7, 0x7b), (15, 0x7ffe), (11, 0x7fc), (14, 0x3ffd), (13, 0x1ffd), (28, 0xffffffc),
    (20, 0xfffe6), (22, 0x3fffd2), (20, 0xfffe7), (20, 0xfffe8), (22, 0x3fffd3), (22, 0x3fffd4), (22, 0x3fffd5), (23, 0x7fffd9),
    (22, 0x3fffd6), (23, 0x7fffda), (23, 0x7fffdb), (23, 0x7fffdc), (23, 0x7fffdd), (23, 0x7fffde), (24, 0xffffeb), (23, 0x7fffdf),
    (24, 0xffffec), (24, 0xffffed), (22, 0x3fffd7), (23, 0x7fffe0), (24, 0xffffee), (23, 0x7fffe1), (23, 0x7fffe2), (23, 0x7fffe3),
    (23, 0x7fffe4), (21, 0x1fffdc), (22, 0x3fffd8), (23, 0x7fffe5), (22, 0x3fffd9), (23, 0x7fffe6), (23, 0x7fffe7), (24, 0xffffef),
    (22, 0x3fffda), (21, 0x1fffdd), (20, 0xfffe9), (22, 0x3fffdb), (22, 0x3fffdc), (23, 0x7fffe8), (23, 0x7fffe9), (21, 0x1fffde),
    (23, 0x7fffea), (22, 0x3fffdd), (22, 0x3fffde), (24, 0xfffff0), (21, 0x1fffdf), (22, 0x3fffdf), (23, 0x7fffeb), (23, 0x7fffec),
    (21, 0x1fffe0), (21, 0x1fffe1), (22, 0x3fffe0), (21, 0x1fffe2), (23, 0x7fffed), (22, 0x3fffe1), (23, 0x7fffee), (23, 0x7fffef),
    (20, 0xfffea), (22, 0x3fffe2), (22, 0x3fffe3), (22, 0x3fffe4), (23, 0x7ffff0), (22, 0x3fffe5), (22, 0x3fffe6), (23, 0x7ffff1),
    (26, 0x3ffffe0), (26, 0x3ffffe1), (20, 0xfffeb), (19, 0x7fff1), (22, 0x3fffe7), (23, 0x7ffff2), (22, 0x3fffe8), (25, 0x1ffffec),
    (26, 0x3ffffe2), (26, 0x3ffffe3), (26, 0x3ffffe4), (27, 0x7ffffde), (27, 0x7ffffdf), (26, 0x3ffffe5), (24, 0xfffff1), (25, 0x1ffffed),
    (19, 0x7fff2), (21, 0x1fffe3), (26, 0x3ffffe6), (27, 0x7ffffe0), (27, 0x7ffffe1), (26, 0x3ffffe7), (27, 0x7ffffe2), (24, 0xfffff2),
    (21, 0x1fffe4), (21, 0x1fffe5), (26, 0x3ffffe8), (26, 0x3ffffe9), (28, 0xffffffd), (27, 0x7ffffe3), (27, 0x7ffffe4), (27, 0x7ffffe5),
    (20, 0xfffec), (24, 0xfffff3), (20, 0xfffed), (21, 0x1fffe6), (22, 0x3fffe9), (21, 0x1fffe7), (21, 0x1fffe8), (23, 0x7ffff3),
    (22, 0x3fffea), (22, 0x3fffeb), (25, 0x1ffffee), (25, 0x1ffffef), (24, 0xfffff4), (24, 0xfffff5), (26, 0x3ffffea), (23, 0x7ffff4),
    (26, 0x3ffffeb), (27, 0x7ffffe6), (26, 0x3ffffec), (26, 0x3ffffed), (27, 0x7ffffe7), (27, 0x7ffffe8), (27, 0x7ffffe9), (27, 0x7ffffea),
    (27, 0x7ffffeb), (28, 0xffffffe), (27, 0x7ffffec), (27, 0x7ffffed), (27, 0x7ffffee), (27, 0x7ffffef), (27, 0x7fffff0), (26, 0x3ffffee),
    (30, 0x3fffffff),
];

/// The decoding tree, made from `CODES` at compile time. `TREE[node][bit]` is the next node, or `LEAF | byte` at the end of a code.
/// A node has at most 256 inner nodes for 257 leaves.
const LEAF: u16 = 0x8000;
const TREE: [[u16; 2]; 256] = {
    let mut tree = [[0u16; 2]; 256];
    let mut used = 1;
    let mut sym = 0;
    while sym < 257 {
        let (len, code) = CODES[sym];
        let mut node = 0;
        let mut at = len;
        while at > 0 {
            at -= 1;
            let bit = ((code >> at) & 1) as usize;
            if at == 0 {
                tree[node][bit] = LEAF | sym as u16;
            } else {
                if tree[node][bit] == 0 {
                    tree[node][bit] = used;
                    used += 1;
                }
                node = tree[node][bit] as usize;
            }
        }
        sym += 1;
    }
    tree
};

/// Decode a Huffman string into `out`. The padding must be at most 7 bits, all ones, and the end of string must not appear (section 5.2).
fn huffman_decode(data: &[u8], out: &mut Vec<u8>) -> Result<(), Error> {
    let (mut node, mut pad, mut ones) = (0usize, 0u32, true);
    for &byte in data {
        for shift in (0..8).rev() {
            let bit = (byte >> shift) & 1;
            let next = TREE[node][bit as usize];
            pad += 1;
            ones &= bit == 1;
            if next & LEAF != 0 {
                let sym = next & !LEAF;
                if sym == 256 {
                    return Err(Error("the end of string inside a Huffman string"));
                }
                out.push(sym as u8);
                (node, pad, ones) = (0, 0, true);
            } else {
                node = next as usize;
            }
        }
    }
    if pad > 7 || !ones {
        return Err(Error("bad padding in a Huffman string"));
    }
    Ok(())
}

fn huffman_len(data: &[u8]) -> usize {
    data.iter().map(|&b| CODES[b as usize].0 as usize).sum::<usize>().div_ceil(8)
}

fn huffman_encode(data: &[u8], out: &mut Vec<u8>) {
    let (mut acc, mut bits) = (0u64, 0u32);
    for &b in data {
        let (len, code) = CODES[b as usize];
        acc = acc << len | code as u64;
        bits += len as u32;
        while bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    if bits > 0 {
        // The padding is the first bits of the end of string: ones.
        out.push((acc << (8 - bits) | (0xff >> bits)) as u8);
    }
}

/// An integer with an `n` bit prefix (section 5.1). `first` has the flag bits above the prefix.
fn put_int(out: &mut Vec<u8>, first: u8, n: u32, mut value: usize) {
    let max = (1usize << n) - 1;
    if value < max {
        out.push(first | value as u8);
        return;
    }
    out.push(first | max as u8);
    value -= max;
    while value >= 128 {
        out.push((value & 127) as u8 | 128);
        value >>= 7;
    }
    out.push(value as u8);
}

fn put_string(out: &mut Vec<u8>, s: &[u8]) {
    if huffman_len(s) < s.len() {
        put_int(out, 0x80, 7, huffman_len(s));
        huffman_encode(s, out);
    } else {
        put_int(out, 0, 7, s.len());
        out.extend_from_slice(s);
    }
}

/// Add one header to a header block. A value of the static table is sent as an index, a name of it as an index and a literal value.
/// `secret` values (cookies, credentials) are never indexed by an intermediary (section 7.1.3).
/// A name must be lowercase: the caller makes it so.
pub fn encode(out: &mut Vec<u8>, name: &[u8], value: &[u8], secret: bool) {
    let mut name_index = 0;
    for (i, (n, v)) in STATIC.iter().enumerate() {
        if n.as_bytes() == name {
            if v.as_bytes() == value && !secret {
                return put_int(out, 0x80, 7, i + 1);
            }
            if name_index == 0 {
                name_index = i + 1;
            }
        }
    }
    put_int(out, if secret { 0x10 } else { 0 }, 4, name_index);
    if name_index == 0 {
        put_string(out, name);
    }
    put_string(out, value);
}

/// The decoder of one connection. The dynamic table is the state that both ends share: a block that fails to decode ends the connection.
pub struct Decoder {
    table: VecDeque<(Vec<u8>, Vec<u8>)>,
    /// The size of the entries now: name + value + 32 each.
    size: usize,
    /// The limit that the encoder set by a size update.
    limit: usize,
    /// The limit that we allow (`SETTINGS_HEADER_TABLE_SIZE`).
    allowed: usize,
    name: Vec<u8>,
    value: Vec<u8>,
}

/// An integer with an `n` bit prefix. At most 4 bytes more: a value over 2^28 is an error, whatever the caller expects.
fn get_int(block: &[u8], at: &mut usize, n: u32) -> Result<usize, Error> {
    let max = (1usize << n) - 1;
    let first = *block.get(*at).ok_or(Error("the block ends inside an integer"))?;
    *at += 1;
    let mut value = first as usize & max;
    if value < max {
        return Ok(value);
    }
    for shift in (0..28).step_by(7) {
        let b = *block.get(*at).ok_or(Error("the block ends inside an integer"))?;
        *at += 1;
        value += ((b & 127) as usize) << shift;
        if b & 128 == 0 {
            return Ok(value);
        }
    }
    Err(Error("an integer is too long"))
}

/// A string: the flag for Huffman, a length, and the bytes. Fills `out`.
fn get_string(block: &[u8], at: &mut usize, out: &mut Vec<u8>) -> Result<(), Error> {
    let huffman = block.get(*at).is_some_and(|b| b & 0x80 != 0);
    let len = get_int(block, at, 7)?;
    let raw = block.get(*at..*at + len).ok_or(Error("a string is longer than the block"))?;
    *at += len;
    out.clear();
    if huffman { huffman_decode(raw, out) } else { Ok(out.extend_from_slice(raw)) }
}

impl Decoder {
    pub fn new(allowed: usize) -> Decoder {
        Decoder { table: VecDeque::new(), size: 0, limit: allowed, allowed, name: Vec::new(), value: Vec::new() }
    }

    fn entry(&self, index: usize) -> Result<(&[u8], &[u8]), Error> {
        match index {
            0 => Err(Error("index 0")),
            1..=61 => Ok((STATIC[index - 1].0.as_bytes(), STATIC[index - 1].1.as_bytes())),
            _ => self.table.get(index - 62).map(|(n, v)| (&n[..], &v[..])).ok_or(Error("an index past the table")),
        }
    }

    fn shrink(&mut self, to: usize) {
        while self.size > to {
            let (n, v) = self.table.pop_back().unwrap();
            self.size -= n.len() + v.len() + 32;
        }
    }

    fn insert(&mut self, name: &[u8], value: &[u8]) {
        let size = name.len() + value.len() + 32;
        // An entry that does not fit empties the table and is not added (section 4.4).
        self.shrink(self.limit.saturating_sub(size));
        if size <= self.limit {
            self.table.push_front((name.to_vec(), value.to_vec()));
            self.size += size;
        }
    }

    /// Decode one whole header block, and call `emit(name, value)` for each header. Names may have capitals: the caller checks them.
    pub fn decode(&mut self, block: &[u8], mut emit: impl FnMut(&[u8], &[u8])) -> Result<(), Error> {
        let (mut at, mut updates) = (0, true);
        while at < block.len() {
            let b = block[at];
            if b & 0x80 != 0 {
                updates = false;
                let (n, v) = self.entry(get_int(block, &mut at, 7)?)?;
                emit(n, v);
            } else if b & 0xe0 == 0x20 {
                if !updates {
                    return Err(Error("a table size update after a header"));
                }
                let size = get_int(block, &mut at, 5)?;
                if size > self.allowed {
                    return Err(Error("a table size above the setting"));
                }
                self.limit = size;
                self.shrink(size);
            } else {
                updates = false;
                let indexed = b & 0x40 != 0;
                let index = get_int(block, &mut at, if indexed { 6 } else { 4 })?;
                if index == 0 {
                    get_string(block, &mut at, &mut self.name)?;
                } else {
                    let mut name = std::mem::take(&mut self.name);
                    name.clear();
                    name.extend_from_slice(self.entry(index)?.0);
                    self.name = name;
                }
                get_string(block, &mut at, &mut self.value)?;
                emit(&self.name, &self.value);
                if indexed {
                    let (name, value) = (std::mem::take(&mut self.name), std::mem::take(&mut self.value));
                    self.insert(&name, &value);
                    (self.name, self.value) = (name, value);
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unhex(s: &str) -> Vec<u8> {
        let s: String = s.split_whitespace().collect();
        (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
    }

    fn decode(d: &mut Decoder, hex: &str) -> Vec<(String, String)> {
        let mut out = Vec::new();
        d.decode(&unhex(hex), |n, v| out.push((String::from_utf8_lossy(n).into_owned(), String::from_utf8_lossy(v).into_owned()))).unwrap();
        out
    }

    fn pair(n: &str, v: &str) -> (String, String) {
        (n.to_string(), v.to_string())
    }

    #[test]
    fn the_huffman_code_is_complete_and_has_no_prefix_of_another_code() {
        // Kraft sum of 2^-len is exactly 1 for a complete code: in units of 2^-30.
        assert_eq!(CODES.iter().map(|&(len, _)| 1u64 << (30 - len)).sum::<u64>(), 1 << 30);
        assert_eq!(CODES[256], (30, 0x3fff_ffff));
        for (i, &(la, ca)) in CODES.iter().enumerate() {
            for &(lb, cb) in &CODES[i + 1..] {
                let l = la.min(lb);
                assert_ne!(ca >> (la - l), cb >> (lb - l), "a code is a prefix of another");
            }
        }
    }

    #[test]
    fn rfc_7541_c3_requests_without_huffman() {
        let mut d = Decoder::new(4096);
        let first = decode(&mut d, "8286 8441 0f77 7777 2e65 7861 6d70 6c65 2e63 6f6d");
        assert_eq!(first, [pair(":method", "GET"), pair(":scheme", "http"), pair(":path", "/"), pair(":authority", "www.example.com")]);
        assert_eq!(d.size, 57);
        let second = decode(&mut d, "8286 84be 5808 6e6f 2d63 6163 6865");
        assert_eq!(second.last().unwrap(), &pair("cache-control", "no-cache"));
        assert_eq!(second[3], pair(":authority", "www.example.com"), "index 62 is the first entry that was added");
        assert_eq!(d.size, 110);
    }

    #[test]
    fn rfc_7541_c4_requests_with_huffman() {
        let mut d = Decoder::new(4096);
        assert_eq!(decode(&mut d, "8286 8441 8cf1 e3c2 e5f2 3a6b a0ab 90f4 ff")[3], pair(":authority", "www.example.com"));
        assert_eq!(decode(&mut d, "8286 84be 5886 a8eb 1064 9cbf")[4], pair("cache-control", "no-cache"));
        let third = decode(&mut d, "8287 85bf 4088 25a8 49e9 5ba9 7d7f 8925 a849 e95b b8e8 b4bf");
        assert_eq!(third, [pair(":method", "GET"), pair(":scheme", "https"), pair(":path", "/index.html"), pair(":authority", "www.example.com"), pair("custom-key", "custom-value")]);
        assert_eq!(d.size, 164);
    }

    #[test]
    fn rfc_7541_c6_responses_with_a_table_of_256_bytes_evict_entries() {
        let mut d = Decoder::new(256);
        d.limit = 256;
        let first = decode(&mut d, "4882 6402 5885 aec3 771a 4b61 96d0 7abe 9410 54d4 44a8 2005 9504 0b81 66e0 82a6 2d1b ff6e 919d 29ad 1718 63c7 8f0b 97c8 e9ae 82ae 43d3");
        assert_eq!(first[0], pair(":status", "302"));
        assert_eq!(first[3], pair("location", "https://www.example.com"));
        assert_eq!(d.size, 222);
        let second = decode(&mut d, "4883 640e ffc1 c0bf");
        assert_eq!(second[0], pair(":status", "307"));
        assert_eq!(d.size, 222, ":status 302 was evicted to make room");
        let third = decode(&mut d, "88c1 6196 d07a be94 1054 d444 a820 0595 040b 8166 e084 a62d 1bff c05a 839b d9ab 77ad 94e7 821d d7f2 e6c7 b335 dfdf cd5b 3960 d5af 2708 7f36 72c1 ab27 0fb5 291f 9587 3160 65c0 03ed 4ee5 b106 3d50 07");
        assert_eq!(third[0], pair(":status", "200"));
        assert_eq!(third.last().unwrap().0, "set-cookie");
        assert_eq!(d.size, 215);
    }

    #[test]
    fn what_the_encoder_writes_the_decoder_reads() {
        let headers: [(&str, &str, bool); 9] = [
            (":method", "GET", false),
            (":method", "PUT", false),
            (":path", "/a/b?c=d", false),
            (":scheme", "https", false),
            ("accept-encoding", "gzip, deflate", false),
            ("accept-encoding", "gzip, deflate, br", false),
            ("cookie", "a=b; c=d", true),
            ("x-custom", "", false),
            ("user-agent", "bare/0.1 \u{e9}\u{4e2d}\u{1f600}", false),
        ];
        let mut block = Vec::new();
        for (n, v, secret) in headers {
            encode(&mut block, n.as_bytes(), v.as_bytes(), secret);
        }
        let mut got = Vec::new();
        Decoder::new(4096).decode(&block, |n, v| got.push((String::from_utf8(n.to_vec()).unwrap(), String::from_utf8(v.to_vec()).unwrap()))).unwrap();
        assert_eq!(got, headers.iter().map(|&(n, v, _)| pair(n, v)).collect::<Vec<_>>());
        // An exact entry of the static table is one byte, and a secret is never an index and never added to a table.
        let (mut one, mut cookie) = (Vec::new(), Vec::new());
        encode(&mut one, b":method", b"GET", false);
        encode(&mut cookie, b"cookie", b"a=b", true);
        assert_eq!(one, [0x82]);
        assert_eq!(cookie[..2], [0x1f, 32 - 15], "never indexed, name index 32 in a 4 bit prefix");
        let mut d = Decoder::new(4096);
        d.decode(&block, |_, _| {}).unwrap();
        assert_eq!(d.size, 0, "the encoder never adds to the table");
    }

    #[test]
    fn every_byte_value_goes_through_huffman_and_back() {
        let all: Vec<u8> = (0..=255).collect();
        let (mut wire, mut back) = (Vec::new(), Vec::new());
        huffman_encode(&all, &mut wire);
        huffman_decode(&wire, &mut back).unwrap();
        assert_eq!(back, all);
        assert_eq!(huffman_len(&all), wire.len());
        for len in 0..40 {
            let text = vec![b'e'; len];
            let (mut w, mut b) = (Vec::new(), Vec::new());
            huffman_encode(&text, &mut w);
            huffman_decode(&w, &mut b).unwrap();
            assert_eq!(b, text);
        }
    }

    #[test]
    fn bad_huffman_strings_are_errors() {
        let mut out = Vec::new();
        // Padding of 8 bits (a whole byte of ones), padding that is not ones, and the end of string.
        assert!(huffman_decode(&[0x1f, 0xff], &mut out).is_err());
        assert!(huffman_decode(&[0x18], &mut out).is_err(), "'a' is 00011 and the padding is 000");
        assert!(huffman_decode(&[0xff, 0xff, 0xff, 0xff], &mut out).is_err());
        assert!(huffman_decode(&[0x1f], &mut out).is_ok(), "'a' is 00011 and the padding is 111");
    }

    #[test]
    fn broken_blocks_are_errors_and_never_panic() {
        let mut d = Decoder::new(4096);
        for bad in ["80", "ff", "ff ff ff ff ff ff", "40", "40 03 61", "4001 61 05 62", "7f", "00 85 aa", "0f"] {
            assert!(d.decode(&unhex(bad), |_, _| {}).is_err(), "{bad}");
        }
        // A size update after a header, and a size update above the setting.
        assert!(d.decode(&unhex("82 20"), |_, _| {}).is_err());
        assert!(d.decode(&unhex("3f e2 1f"), |_, _| {}).is_err());
        // An index past the end of the table.
        assert!(d.decode(&unhex("be"), |_, _| {}).is_err());
        // Size updates at the start are fine, and a size of 0 empties the table.
        d.decode(&unhex("40 01 61 01 62"), |_, _| {}).unwrap();
        assert_eq!(d.size, 34);
        d.decode(&unhex("20 82"), |_, _| {}).unwrap();
        assert_eq!(d.size, 0);
    }

    #[test]
    fn an_entry_bigger_than_the_table_empties_it() {
        let mut d = Decoder::new(100);
        d.decode(&unhex("40 01 61 01 62"), |_, _| {}).unwrap();
        assert_eq!(d.size, 34);
        let mut big = unhex("40 01 63 50");
        big.extend(vec![b'x'; 80]);
        d.decode(&big, |_, _| {}).unwrap();
        assert_eq!((d.size, d.table.len()), (0, 0));
    }

    #[test]
    fn random_blocks_never_panic() {
        let mut seed = 0x1234_5678_9abc_def0u64;
        let mut d = Decoder::new(4096);
        for _ in 0..20_000 {
            let len = (seed % 48) as usize;
            let block: Vec<u8> = (0..len)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    seed as u8
                })
                .collect();
            let _ = d.decode(&block, |_, _| {});
            assert!(d.size <= 4096);
        }
    }
}
