//! Strict DER reader (ITU-T X.690 clause 8 and 10). Parsing only; no crypto, no trust.
//!
//! Every value is a borrowed slice of the input. Nothing is copied or allocated.
//! There is no object tree: callers read one TLV at a time with [`Reader`], so nesting
//! depth is the depth of the caller's own code, and work is linear in the input.
//!
//! Rejected: tags with number >= 31 (multi-byte tags), indefinite lengths, long-form
//! lengths that are not minimal, lengths of more than 4 octets, truncated values,
//! non-minimal INTEGERs, malformed BIT STRINGs, BOOLEANs other than 0x00/0xFF,
//! malformed OIDs and trailing bytes (when the caller calls [`Reader::finish`]).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The input ends before the TLV does.
    Truncated,
    /// Length octet 0x80 (BER indefinite length). DER forbids it.
    IndefiniteLength,
    /// Long-form length with a leading zero octet, or a value below 128.
    NonMinimalLength,
    /// Length octet count above 4 (or the reserved value 0xFF).
    LengthTooLarge,
    /// Tag number 31 or more (multi-byte tag).
    HighTag,
    /// A different tag than the one the caller expected.
    WrongTag,
    /// Bytes left over after the last expected element.
    TrailingData,
    /// INTEGER is empty, padded, or has the wrong sign.
    BadInteger,
    /// BIT STRING is empty, has unused bits above 7, or its unused bits are not zero.
    BadBitString,
    /// BOOLEAN is not exactly one octet of 0x00 or 0xFF.
    BadBoolean,
    /// OBJECT IDENTIFIER is empty or a sub-identifier is padded or unfinished.
    BadOid,
}

pub type Result<T> = core::result::Result<T, Error>;

pub const BOOLEAN: u8 = 0x01;
pub const INTEGER: u8 = 0x02;
pub const BIT_STRING: u8 = 0x03;
pub const OCTET_STRING: u8 = 0x04;
pub const NULL: u8 = 0x05;
pub const OID: u8 = 0x06;
pub const UTC_TIME: u8 = 0x17;
pub const GENERALIZED_TIME: u8 = 0x18;
pub const SEQUENCE: u8 = 0x30;
pub const SET: u8 = 0x31;

/// One tag-length-value element.
#[derive(Debug, Clone, Copy)]
pub struct Tlv<'a> {
    pub tag: u8,
    /// The value octets.
    pub content: &'a [u8],
    /// Tag, length and value octets: the exact bytes that were read.
    pub full: &'a [u8],
}

#[derive(Debug, Clone)]
pub struct Reader<'a> {
    buf: &'a [u8],
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Reader { buf }
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// The tag of the next element, if any bytes are left.
    pub fn peek_tag(&self) -> Option<u8> {
        self.buf.first().copied()
    }

    /// Read the next TLV.
    pub fn read(&mut self) -> Result<Tlv<'a>> {
        let b = self.buf;
        if b.len() < 2 {
            return Err(Error::Truncated);
        }
        let tag = b[0];
        if tag & 0x1f == 0x1f {
            return Err(Error::HighTag);
        }
        let l = b[1];
        let (hdr, len) = if l < 0x80 {
            (2, l as usize)
        } else {
            let n = (l & 0x7f) as usize;
            if n == 0 {
                return Err(Error::IndefiniteLength);
            }
            if n > 4 {
                return Err(Error::LengthTooLarge);
            }
            if b.len() < 2 + n {
                return Err(Error::Truncated);
            }
            if b[2] == 0 {
                return Err(Error::NonMinimalLength);
            }
            let v = b[2..2 + n].iter().fold(0u32, |a, &x| (a << 8) | x as u32);
            if v < 128 {
                return Err(Error::NonMinimalLength);
            }
            (2 + n, v as usize)
        };
        if len > b.len() - hdr {
            return Err(Error::Truncated);
        }
        let (full, rest) = b.split_at(hdr + len);
        self.buf = rest;
        Ok(Tlv { tag, content: &full[hdr..], full })
    }

    /// Read the next TLV and require this tag.
    pub fn read_tag(&mut self, tag: u8) -> Result<Tlv<'a>> {
        let t = self.read()?;
        if t.tag != tag {
            return Err(Error::WrongTag);
        }
        Ok(t)
    }

    /// Read the next TLV only if it has this tag. Reads nothing otherwise.
    pub fn read_opt(&mut self, tag: u8) -> Result<Option<Tlv<'a>>> {
        if self.peek_tag() == Some(tag) { self.read().map(Some) } else { Ok(None) }
    }

    /// Fail if any bytes are left.
    pub fn finish(&self) -> Result<()> {
        if self.buf.is_empty() { Ok(()) } else { Err(Error::TrailingData) }
    }
}

/// BOOLEAN content (X.690 11.1: TRUE is 0xFF).
pub fn boolean(content: &[u8]) -> Result<bool> {
    match content {
        [0x00] => Ok(false),
        [0xff] => Ok(true),
        _ => Err(Error::BadBoolean),
    }
}

/// Check INTEGER content: non-empty and minimal (X.690 8.3.2).
pub fn check_integer(c: &[u8]) -> Result<()> {
    match c {
        [] => Err(Error::BadInteger),
        [0x00, b, ..] if b & 0x80 == 0 => Err(Error::BadInteger),
        [0xff, b, ..] if b & 0x80 != 0 => Err(Error::BadInteger),
        _ => Ok(()),
    }
}

/// Non-negative INTEGER content as big-endian magnitude. The sign octet is removed.
/// Zero is returned as `[0]`.
pub fn unsigned(c: &[u8]) -> Result<&[u8]> {
    check_integer(c)?;
    if c[0] & 0x80 != 0 {
        return Err(Error::BadInteger);
    }
    Ok(if c.len() > 1 && c[0] == 0 { &c[1..] } else { c })
}

/// BIT STRING content: (unused bit count, data octets). The unused bits must be zero.
pub fn bit_string(c: &[u8]) -> Result<(u8, &[u8])> {
    let (&unused, data) = c.split_first().ok_or(Error::BadBitString)?;
    if unused > 7 || (data.is_empty() && unused != 0) {
        return Err(Error::BadBitString);
    }
    if let Some(&last) = data.last()
        && last & ((1u8 << unused) - 1) != 0
    {
        return Err(Error::BadBitString);
    }
    Ok((unused, data))
}

/// Check OBJECT IDENTIFIER content: non-empty, no padded sub-identifier, last octet ends one.
pub fn check_oid(c: &[u8]) -> Result<()> {
    if c.is_empty() || c[c.len() - 1] & 0x80 != 0 {
        return Err(Error::BadOid);
    }
    let mut start = true;
    for &b in c {
        if start && b == 0x80 {
            return Err(Error::BadOid);
        }
        start = b & 0x80 == 0;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rd(b: &[u8]) -> Result<Tlv<'_>> {
        Reader::new(b).read()
    }

    #[test]
    fn lengths() {
        assert_eq!(rd(&[4, 0]).unwrap().content, &[] as &[u8]);
        assert_eq!(rd(&[4, 1, 9]).unwrap().content, &[9]);
        // 127 is the last short form; 128 is the first long form.
        let mut v = vec![4, 127];
        v.extend([0; 127]);
        assert_eq!(rd(&v).unwrap().content.len(), 127);
        let mut v = vec![4, 0x81, 128];
        v.extend([0; 128]);
        assert_eq!(rd(&v).unwrap().content.len(), 128);
        let mut v = vec![4, 0x81, 255];
        v.extend([0; 255]);
        assert_eq!(rd(&v).unwrap().content.len(), 255);
        let mut v = vec![4, 0x82, 1, 0];
        v.extend([0; 256]);
        assert_eq!(rd(&v).unwrap().content.len(), 256);
        // Non-minimal forms.
        assert_eq!(rd(&[4, 0x81, 5, 0, 0, 0, 0, 0]).unwrap_err(), Error::NonMinimalLength);
        assert_eq!(rd(&[4, 0x81, 127]).unwrap_err(), Error::NonMinimalLength);
        assert_eq!(rd(&[4, 0x82, 0, 200]).unwrap_err(), Error::NonMinimalLength);
        assert_eq!(rd(&[4, 0x83, 0, 1, 0]).unwrap_err(), Error::NonMinimalLength);
        // Indefinite, reserved, too many octets.
        assert_eq!(rd(&[0x30, 0x80, 0, 0]).unwrap_err(), Error::IndefiniteLength);
        assert_eq!(rd(&[4, 0xff]).unwrap_err(), Error::LengthTooLarge);
        assert_eq!(rd(&[4, 0x85, 1, 0, 0, 0, 0]).unwrap_err(), Error::LengthTooLarge);
        // Huge declared lengths: no overflow, just truncated.
        assert_eq!(rd(&[4, 0x84, 0xff, 0xff, 0xff, 0xff, 1]).unwrap_err(), Error::Truncated);
        assert_eq!(rd(&[4, 0x84, 0x80, 0, 0, 0]).unwrap_err(), Error::Truncated);
        // Truncation.
        assert_eq!(rd(&[]).unwrap_err(), Error::Truncated);
        assert_eq!(rd(&[4]).unwrap_err(), Error::Truncated);
        assert_eq!(rd(&[4, 2, 1]).unwrap_err(), Error::Truncated);
        assert_eq!(rd(&[4, 0x82, 1]).unwrap_err(), Error::Truncated);
        // High tag numbers.
        assert_eq!(rd(&[0x1f, 1, 0]).unwrap_err(), Error::HighTag);
        assert_eq!(rd(&[0xbf, 0x21, 0]).unwrap_err(), Error::HighTag);
    }

    #[test]
    fn reader_sequence_and_trailing() {
        let mut r = Reader::new(&[4, 1, 7, 5, 0, 9]);
        assert_eq!(r.read_tag(4).unwrap().content, &[7]);
        assert_eq!(r.read_tag(4).unwrap_err(), Error::WrongTag);
        let mut r = Reader::new(&[4, 1, 7, 9]);
        r.read().unwrap();
        assert_eq!(r.finish().unwrap_err(), Error::TrailingData);
        assert_eq!(r.read().unwrap_err(), Error::Truncated);
        assert!(Reader::new(&[4, 5]).read_opt(4).is_err());
        assert!(Reader::new(&[]).finish().is_ok());
        assert!(Reader::new(&[5, 0]).read_opt(4).unwrap().is_none());
    }

    #[test]
    fn integers() {
        assert!(check_integer(&[]).is_err());
        assert!(check_integer(&[0]).is_ok());
        assert!(check_integer(&[0, 0]).is_err());
        assert!(check_integer(&[0, 0x7f]).is_err());
        assert!(check_integer(&[0, 0x80]).is_ok());
        assert!(check_integer(&[0xff, 0x80]).is_err());
        assert!(check_integer(&[0xff, 0x7f]).is_ok());
        assert!(check_integer(&[0x80]).is_ok());
        assert_eq!(unsigned(&[0]).unwrap(), &[0]);
        assert_eq!(unsigned(&[0x7f]).unwrap(), &[0x7f]);
        assert_eq!(unsigned(&[0, 0x80, 1]).unwrap(), &[0x80, 1]);
        assert!(unsigned(&[0x80]).is_err());
        assert!(unsigned(&[0xff]).is_err());
    }

    #[test]
    fn bit_strings_booleans_oids() {
        assert!(bit_string(&[]).is_err());
        assert_eq!(bit_string(&[0]).unwrap(), (0, &[] as &[u8]));
        assert!(bit_string(&[1]).is_err());
        assert!(bit_string(&[8, 0]).is_err());
        assert_eq!(bit_string(&[7, 0x80]).unwrap(), (7, &[0x80][..]));
        assert!(bit_string(&[7, 0x81]).is_err());
        assert!(bit_string(&[3, 0b1010_1000]).is_ok());
        assert!(bit_string(&[3, 0b1010_1100]).is_err());
        assert_eq!(boolean(&[0]), Ok(false));
        assert_eq!(boolean(&[0xff]), Ok(true));
        assert!(boolean(&[1]).is_err());
        assert!(boolean(&[]).is_err());
        assert!(boolean(&[0, 0]).is_err());
        assert!(check_oid(&[0x2a, 0x86, 0x48]).is_ok());
        assert!(check_oid(&[]).is_err());
        assert!(check_oid(&[0x2a, 0x86]).is_err());
        assert!(check_oid(&[0x2a, 0x80, 0x01]).is_err());
        assert!(check_oid(&[0x80, 0x01]).is_err());
    }
}
