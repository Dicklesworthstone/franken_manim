//! fm-fmd-math-fuzz-budgets-k8fe fuzz target: fmd-font's CFF1 path — the
//! Type 2 charstring interpreter (`Font::cff_outline`) and the CFF subsetter
//! (`Font::try_subset_glyphs` on an `OTTO` font), which fmd uses to embed
//! OpenType-CFF fonts in PDFs.
//!
//! Resource-budget contract (§16.5/R14): for ANY font bytes, decoding and
//! subsetting either refuse with a typed `SubsetError` or succeed within the
//! interpreter's fuel, stack, subroutine-depth and command budgets — never a
//! panic, a hang, or an unbounded allocation. A subset that succeeds parses
//! again, and every kept glyph decodes to the same outline it had before.
//!
//! Input: a first byte below 0x40 means the rest is a whole font. Otherwise
//! the rest is a list of byte edits on the minimal valid CFF font built
//! below, as in `ttf_glyf_outlines`: `[offset: 3 bytes, big-endian, modulo
//! the base length][len: 1 byte]` then `len` replacement bytes; `len == 0`
//! truncates the font at `offset`.
#![no_main]

use std::sync::LazyLock;

use fmd_font::{Font, MISSING_GLYPH_REMAP};
use libfuzzer_sys::fuzz_target;

/// No bundled face is CFF and fmd-font has no public CFF writer, so the base
/// is assembled here: four glyphs (an empty `.notdef`, a square of
/// `rlineto`s, an `rrcurveto`, and one through a local subroutine) in a CFF1
/// table, with the `head`/`hhea`/`maxp`/`hmtx`/`cmap` tables `Font::parse`
/// requires.
static BASE: LazyLock<Vec<u8>> = LazyLock::new(cff_font);

/// Cap on glyphs decoded per input. `cff_outline` re-reads the CFF table on
/// every call, so decoding all of a 65 535-glyph font would be quadratic.
const MAX_DECODED: u16 = 64;

/// A one-byte Type 2 / DICT integer (-107..=107 encodes as `v + 139`).
fn small(v: i8) -> u8 {
    u8::try_from(i16::from(v) + 139).expect("-107..=107")
}

/// A three-byte DICT integer (operator 28, big-endian i16).
fn short(out: &mut Vec<u8>, v: usize) {
    let v = i16::try_from(v).expect("CFF offsets fit an i16");
    out.push(28);
    out.extend_from_slice(&v.to_be_bytes());
}

/// A CFF INDEX with one-byte offsets.
fn index(items: &[Vec<u8>]) -> Vec<u8> {
    let count = u16::try_from(items.len()).expect("small index");
    let mut out = count.to_be_bytes().to_vec();
    if items.is_empty() {
        return out;
    }
    out.push(1);
    let mut offset = 1_usize;
    out.push(1);
    for item in items {
        offset += item.len();
        out.push(u8::try_from(offset).expect("one-byte offsets"));
    }
    for item in items {
        out.extend_from_slice(item);
    }
    out
}

fn cff_table() -> Vec<u8> {
    let charstrings = index(&[
        vec![14],
        vec![
            small(50),
            small(50),
            21,
            small(100),
            small(0),
            5,
            small(0),
            small(100),
            5,
            small(-100),
            small(0),
            5,
            14,
        ],
        vec![
            small(0),
            small(0),
            21,
            small(10),
            small(20),
            small(30),
            small(40),
            small(50),
            small(60),
            8,
            14,
        ],
        // callsubr 0: fewer than 1240 subroutines bias the index by 107.
        vec![small(0), small(0), 21, small(-107), 10, 14],
    ]);
    let subrs = index(&[vec![small(100), small(0), 5, 11]]);
    let name = index(&[b"FuzzCff".to_vec()]);
    // Top DICT: CharStrings (17) and Private (18), all three-byte integers,
    // so its length is known before the offsets are.
    let top_len = 11;
    let header = 4;
    let top_index_len = 2 + 1 + 2 + top_len;
    let charstrings_at = header + name.len() + top_index_len + 2 + 2;
    let private_at = charstrings_at + charstrings.len();
    // Private DICT: Subrs (19), relative to the Private DICT's start.
    let private_len = 4;
    let mut private = Vec::new();
    short(&mut private, private_len);
    private.push(19);
    let mut top = Vec::new();
    short(&mut top, charstrings_at);
    top.push(17);
    short(&mut top, private_len);
    short(&mut top, private_at);
    top.push(18);
    assert_eq!(top.len(), top_len);
    assert_eq!(private.len(), private_len);
    let mut cff = vec![1, 0, 4, 1];
    cff.extend_from_slice(&name);
    cff.extend_from_slice(&index(&[top]));
    cff.extend_from_slice(&[0, 0, 0, 0]); // empty String and Global Subr INDEXes
    assert_eq!(cff.len(), charstrings_at);
    cff.extend_from_slice(&charstrings);
    cff.extend_from_slice(&private);
    cff.extend_from_slice(&subrs);
    cff
}

/// A format 4 `cmap` (platform 3, encoding 1) mapping 'A'..='C' to 1..=3.
fn cmap_table() -> Vec<u8> {
    let mut sub = Vec::new();
    let segments: [(u16, u16, u16); 2] =
        [(0x41, 0x43, 1_u16.wrapping_sub(0x41)), (0xFFFF, 0xFFFF, 1)];
    let seg_x2 = u16::try_from(segments.len() * 2).expect("two segments");
    for v in [4_u16, 0, 0, seg_x2, 4, 1, 0] {
        sub.extend_from_slice(&v.to_be_bytes());
    }
    for (_, end, _) in segments {
        sub.extend_from_slice(&end.to_be_bytes());
    }
    sub.extend_from_slice(&[0, 0]);
    for (start, _, _) in segments {
        sub.extend_from_slice(&start.to_be_bytes());
    }
    for (_, _, delta) in segments {
        sub.extend_from_slice(&delta.to_be_bytes());
    }
    for _ in segments {
        sub.extend_from_slice(&[0, 0]);
    }
    let length = u16::try_from(sub.len()).expect("small subtable");
    sub[2..4].copy_from_slice(&length.to_be_bytes());
    let mut cmap = vec![0, 0, 0, 1, 0, 3, 0, 1, 0, 0, 0, 12];
    cmap.extend_from_slice(&sub);
    cmap
}

fn cff_font() -> Vec<u8> {
    let glyphs = 4_u16;
    let mut head = vec![0_u8; 54];
    head[18..20].copy_from_slice(&1000_u16.to_be_bytes());
    let mut hhea = vec![0_u8; 36];
    hhea[4..6].copy_from_slice(&800_i16.to_be_bytes());
    hhea[6..8].copy_from_slice(&(-200_i16).to_be_bytes());
    hhea[34..36].copy_from_slice(&glyphs.to_be_bytes());
    let mut maxp = vec![0, 0, 0x50, 0, 0, 0];
    maxp[4..6].copy_from_slice(&glyphs.to_be_bytes());
    let mut hmtx = Vec::new();
    for _ in 0..glyphs {
        hmtx.extend_from_slice(&500_u16.to_be_bytes());
        hmtx.extend_from_slice(&0_i16.to_be_bytes());
    }
    let tables: [(&[u8; 4], Vec<u8>); 6] = [
        (b"CFF ", cff_table()),
        (b"cmap", cmap_table()),
        (b"head", head),
        (b"hhea", hhea),
        (b"hmtx", hmtx),
        (b"maxp", maxp),
    ];
    let count = u16::try_from(tables.len()).expect("six tables");
    let mut out = 0x4F54_544F_u32.to_be_bytes().to_vec();
    out.extend_from_slice(&count.to_be_bytes());
    out.extend_from_slice(&[0; 6]);
    let mut offset = 12 + tables.len() * 16;
    let mut body = Vec::new();
    for (tag, bytes) in &tables {
        out.extend_from_slice(&tag[..]);
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&u32::try_from(offset).expect("small font").to_be_bytes());
        out.extend_from_slice(
            &u32::try_from(bytes.len())
                .expect("small table")
                .to_be_bytes(),
        );
        offset += bytes.len();
        body.extend_from_slice(bytes);
    }
    out.extend_from_slice(&body);
    let font = Font::parse(out.clone()).expect("the CFF base parses");
    for gid in 0..glyphs {
        font.cff_outline(gid).expect("the CFF base decodes");
    }
    out
}

/// The font bytes an input stands for.
fn font_bytes(data: &[u8]) -> Option<Vec<u8>> {
    let (&mode, rest) = data.split_first()?;
    if mode < 0x40 {
        return Some(rest.to_vec());
    }
    let mut font = BASE.clone();
    let mut edits = rest;
    while let [a, b, c, len, tail @ ..] = edits {
        if font.is_empty() {
            break;
        }
        let offset = (usize::from(*a) << 16 | usize::from(*b) << 8 | usize::from(*c)) % font.len();
        let len = usize::from(*len);
        if len == 0 {
            font.truncate(offset);
            edits = tail;
            continue;
        }
        let take = len.min(tail.len());
        let end = (offset + take).min(font.len());
        font[offset..end].copy_from_slice(&tail[..end - offset]);
        edits = &tail[take..];
    }
    Some(font)
}

fuzz_target!(|data: &[u8]| {
    let Some(bytes) = font_bytes(data) else {
        return;
    };
    let Ok(font) = Font::parse(bytes) else {
        return;
    };
    let decoded: Vec<_> = (0..font.num_glyphs.min(MAX_DECODED))
        .map(|gid| font.cff_outline(gid).ok())
        .collect();
    for outline in decoded.iter().flatten() {
        for command in &outline.commands {
            let points = match command {
                fmd_font::cff::Command::Move(p) | fmd_font::cff::Command::Line(p) => vec![p],
                fmd_font::cff::Command::Curve(a, b, c) => vec![a, b, c],
                fmd_font::cff::Command::Close => Vec::new(),
            };
            for p in points {
                assert!(p.x.is_finite() && p.y.is_finite(), "non-finite CFF point");
            }
        }
    }
    let keep: Vec<u16> = (0..font.num_glyphs.min(MAX_DECODED)).collect();
    if let Ok(subset) = font.try_subset_glyphs(&keep, &['A', 'B', 'C']) {
        let again = Font::parse(subset.bytes).expect("a CFF subset parses again");
        for (old, outline) in keep.iter().zip(&decoded) {
            let Some(outline) = outline else { continue };
            let new = subset.glyph_map[usize::from(*old)];
            if new == MISSING_GLYPH_REMAP {
                continue;
            }
            let round_trip = again.cff_outline(new).expect("a subset glyph decodes");
            assert_eq!(
                &round_trip, outline,
                "glyph {old} changed through the subset"
            );
        }
    }
});
