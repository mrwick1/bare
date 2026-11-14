//! Web APIs that are written in JavaScript and made at the first read of one of their names: `Element.prototype.animate` and `Animation`
//! (`shims/animation.js`), `Path2D` (`shims/path2d.js`), `indexedDB` and the `IDB*` interfaces (`shims/idb.js`), `EventSource` (`shims/eventsource.js`),
//! `FileReader` (`shims/filereader.js`),
//! `WritableStream`, `TransformStream`, `TextDecoderStream`, `TextEncoderStream` and `ReadableStream.prototype.pipeTo` and `pipeThrough`
//! (`shims/streams.js`), `CompressionStream` and `DecompressionStream` (`shims/compression.js`, with the native `codec` here).
//!
//! A read of the name finds nothing, so the heap asks the host once (`Heap::browser_name`, `browser_handlers::lazy`). The host runs the
//! source as an indirect eval; the source defines the globals itself. A realm that never reads the names pays nothing: no object, no
//! function, no script. The source of a shim defines its own sentinel global, so it runs once.
//!
//! Gates: none of them has a timeline, a layout or a store on disk. `animate` ends the animation at once (one timer). `Path2D` records
//! nothing. `indexedDB` keeps its data in the realm only and runs the requests of a transaction one after another in
//! microtasks. Not compared with a real browser.

use crate::browser::*;
use bare_js::eval::Interp;
use bare_js::value::Value;

/// `(names that ask for it, the global that the source defines first, source)`.
const SHIMS: [(&[&str], &str, &str); 7] = [
    (&["animate", "Animation"], "Animation", include_str!("shims/animation.js")),
    (&["Path2D"], "Path2D", include_str!("shims/path2d.js")),
    (&["EventSource"], "EventSource", include_str!("shims/eventsource.js")),
    (&["FileReader"], "FileReader", include_str!("shims/filereader.js")),
    (&["indexedDB"], "indexedDB", include_str!("shims/idb.js")),
    (&STREAM_NAMES, "WritableStream", include_str!("shims/streams.js")),
    (&["CompressionStream", "DecompressionStream"], "CompressionStream", include_str!("shims/compression.js")),
];

/// The globals of `shims/streams.js`. `crates/js/src/heap.rs` (`browser_name`) lists the same names.
const STREAM_NAMES: [&str; 7] = ["WritableStream", "WritableStreamDefaultWriter", "WritableStreamDefaultController", "TransformStream", "TransformStreamDefaultController", "TextDecoderStream", "TextEncoderStream"];

/// Make the shim that `name` asks for. Gives whether `name` is one of them.
#[cold]
pub(crate) fn define(d: &Dom, it: &mut Interp<'_>, name: &str) -> R<bool> {
    let Some(&(_, sentinel, src)) = SHIMS.iter().find(|s| s.0.contains(&name) || (name.starts_with("IDB") && s.1 == "indexedDB")) else { return Ok(false) };
    let g = it.global_object();
    let k = d.key(it, sentinel)?;
    if it.heap.peek_own_property(g, k).map_err(|e| it.heap_error(e))?.is_none() {
        // Not `js_str`: the source is part of the host, not script data, and a small `max_string_bytes` must not stop the browser.
        let s = it.heap.intern_str(src).map_err(|e| it.heap_error(e))?;
        let made = it.eval_global(Value::String(s))?;
        if sentinel == "CompressionStream" {
            // The source gives its install function. It takes the native codec.
            let f = d.add_fn(it, codec, "codec", 3, false)?;
            it.call(made, Value::Undefined, &[Value::Object(f)])?;
        }
    }
    Ok(true)
}

/// The most bytes that one `DecompressionStream` gives. A small bomb that grows past it is a `TypeError`, after this much work.
const MAX_OUTPUT: usize = 256 << 20;

/// `codec(mode, format, bytes)` of `shims/compression.js`: mode 0 compresses, 1 decompresses. Formats: 0 gzip (RFC 1952), 1 deflate
/// (zlib, RFC 1950), 2 deflate-raw (RFC 1951). Bad, cut or too big data, and bytes after the end of the stream, are a `TypeError`, as
/// the Compression Standard says.
fn codec(_: &Dom, it: &mut Interp<'_>, _: Value, a: &[Value]) -> R<Value> {
    let num = |i: usize| match a.get(i) {
        Some(Value::Number(n)) => *n as u8,
        _ => 0,
    };
    let (mode, format) = (num(0), num(1));
    let input = match a.get(2) {
        Some(Value::Object(o)) => it.heap.bytes(*o).map(<[u8]>::to_vec).unwrap_or_default(),
        _ => Vec::new(),
    };
    it.charge((input.len() / 64) as u64)?;
    let out = if mode == 0 { Ok(compress(format, &input)) } else { decompress(format, &input) };
    match out {
        Ok(out) => {
            it.charge((out.len() / 64) as u64)?;
            it.new_uint8_array(out)
        }
        Err(why) => Err(it.type_error(&format!("Failed to execute 'flush' on 'DecompressionStream': {why}"))),
    }
}

fn compress(format: u8, input: &[u8]) -> Vec<u8> {
    use miniz_oxide::deflate::{compress_to_vec, compress_to_vec_zlib};
    match format {
        0 => {
            // A header with no name, no time and the "unknown" system, as browsers write it.
            let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 0xff];
            out.extend(compress_to_vec(input, 6));
            out.extend(crc32fast::hash(input).to_le_bytes());
            out.extend((input.len() as u32).to_le_bytes());
            out
        }
        1 => compress_to_vec_zlib(input, 6),
        _ => compress_to_vec(input, 6),
    }
}

fn decompress(format: u8, wire: &[u8]) -> Result<Vec<u8>, &'static str> {
    use miniz_oxide::inflate::TINFLStatus;
    use miniz_oxide::inflate::core::inflate_flags::{TINFL_FLAG_COMPUTE_ADLER32, TINFL_FLAG_PARSE_ZLIB_HEADER, TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF};
    use miniz_oxide::inflate::core::{DecompressorOxide, decompress};
    const CUT: &str = "the compressed data ends early";
    let (mut flags, mut at) = (TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF, 0);
    match format {
        0 => {
            // RFC 1952: 10 fixed bytes, then the optional extra field, name, comment and header checksum that the flags name.
            if wire.len() < 10 {
                return Err(CUT);
            }
            if wire[..3] != [0x1f, 0x8b, 8] {
                return Err("not a gzip stream");
            }
            at = 10;
            if wire[3] & 4 != 0 {
                let len = wire.get(at..at + 2).ok_or(CUT)?;
                at += 2 + usize::from(u16::from_le_bytes([len[0], len[1]]));
            }
            for flag in [8, 16] {
                if wire[3] & flag != 0 {
                    at += wire.get(at..).ok_or(CUT)?.iter().position(|&b| b == 0).ok_or(CUT)? + 1;
                }
            }
            if wire[3] & 2 != 0 {
                at += 2;
            }
            if at > wire.len() {
                return Err(CUT);
            }
        }
        1 => flags |= TINFL_FLAG_PARSE_ZLIB_HEADER | TINFL_FLAG_COMPUTE_ADLER32,
        _ => {}
    }
    let mut out = vec![0u8; (wire.len() * 4).clamp(4096, MAX_OUTPUT + 1)];
    let mut state = Box::<DecompressorOxide>::default();
    let (mut pos, mut input) = (0, &wire[at..]);
    loop {
        let (status, used, wrote) = decompress(&mut state, input, &mut out, pos, flags);
        input = &input[used..];
        pos += wrote;
        if pos > MAX_OUTPUT {
            return Err("the decompressed data is too big");
        }
        match status {
            TINFLStatus::Done => break,
            TINFLStatus::HasMoreOutput => out.resize((out.len() * 2).min(MAX_OUTPUT + 1), 0),
            TINFLStatus::NeedsMoreInput | TINFLStatus::FailedCannotMakeProgress => return Err(CUT),
            _ => return Err("the compressed data is bad"),
        }
    }
    if format == 0 {
        // The trailer: the CRC-32 and the length of the output (modulo 2^32).
        let tail = input.get(..8).ok_or(CUT)?;
        if u32::from_le_bytes(tail[..4].try_into().unwrap()) != crc32fast::hash(&out[..pos]) || u32::from_le_bytes(tail[4..].try_into().unwrap()) != pos as u32 {
            return Err("the gzip checksum is wrong");
        }
        input = &input[8..];
    }
    if !input.is_empty() {
        return Err("there are bytes after the end of the compressed data");
    }
    out.truncate(pos);
    Ok(out)
}
