//! fm-17m acceptance, slice A: the owned DEFLATE — round-trips at all
//! levels, DEFLATE determinism (same input/params ⇒ same bytes), the
//! byte-aligned segment-composition interlock, cross-validation of
//! inflate against a CPython-zlib-compressed fixture, and the
//! decompression-bomb / malformed-stream refusals.

use fmn_codec::inflate::InflateError;
use fmn_codec::{
    CompressionLevel, deflate_bytes, deflate_segment, inflate_bytes, zlib_compress, zlib_decompress,
};

const LEVELS: [CompressionLevel; 3] = [
    CompressionLevel::Fast,
    CompressionLevel::Default,
    CompressionLevel::Best,
];

/// Deterministic pseudo-random bytes (LCG — no RNG dependency).
fn lcg_bytes(n: usize, mut state: u32) -> Vec<u8> {
    (0..n)
        .map(|_| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 24) as u8
        })
        .collect()
}

fn corpora() -> Vec<Vec<u8>> {
    vec![
        Vec::new(),
        b"a".to_vec(),
        b"hello, hello, hello world".to_vec(),
        b"ab".repeat(10_000),
        vec![0u8; 1 << 20],
        lcg_bytes(100_000, 0xdead_beef),
        ("the quick brown fox jumps over the lazy dog. ".repeat(500)).into_bytes(),
    ]
}

#[test]
fn round_trips_at_every_level() {
    for data in corpora() {
        for level in LEVELS {
            let packed = deflate_bytes(&data, level);
            let unpacked = inflate_bytes(&packed, data.len().max(1)).unwrap();
            assert_eq!(unpacked, data, "level {level:?}, len {}", data.len());
        }
    }
}

#[test]
fn deflate_is_deterministic() {
    for data in corpora() {
        for level in LEVELS {
            assert_eq!(
                deflate_bytes(&data, level),
                deflate_bytes(&data, level),
                "level {level:?}"
            );
        }
    }
}

#[test]
fn compressed_bytes_match_the_pre_dag_encoder() {
    // Captured from the unmodified encoder at 8717dc8b, before replacing
    // expanded package lists. These pin encoded bytes, not merely successful
    // round-trips; changing Huffman tie order must never re-bless this table.
    let alphabet: Vec<u8> = (0..256).map(|symbol| symbol as u8).collect();
    let skewed: Vec<u8> = (0..100_000)
        .map(|i| {
            let x = (i * 37 + i / 251) % 257;
            if x < 192 { b'a' } else { (x - 192) as u8 }
        })
        .collect();
    let cases = [
        (
            "empty",
            Vec::new(),
            1,
            ["9b4fb24edd6d1d8830e272398263cdbf026b97392cc35387b991dc0248a628f9"; 3],
        ),
        (
            "fixed",
            b"hello, hello, hello world".to_vec(),
            1,
            ["c0d356bb7819b6f40089703a23e02581921f2d1cf58ba587e59b62e5a88f1985"; 3],
        ),
        (
            "dense",
            alphabet.repeat(64),
            2,
            ["71f7a1a8eb26164bbfba8b3cdba80be62dd4b0d7d5bd64bd3a51382872d59732"; 3],
        ),
        (
            "skewed",
            skewed,
            2,
            [
                "c22df5132675bec14d5550fb1eb26e2d613ad7f5485209a8333edcc19f27e7b8",
                "62de1025d499e709f06c4a6a97a4a089b7ecccf15ee6ce216772283a1791bcb2",
                "61bcf8216059942014eddf097802d01779db320fc8a8425012bebed9adf37da3",
            ],
        ),
        (
            "stored",
            lcg_bytes(70_000, 0xdead_beef),
            0,
            ["744bc83b7bad9b7b3d1bc1bcbfc0b17de8d6838b9667b6b29b963b20ef83330a"; 3],
        ),
        (
            "long_run",
            vec![0; 1 << 20],
            2,
            ["987cc3a8b6175ec96b6ec48955ce98d08e2bf5748af18a91c71ae1806b799646"; 3],
        ),
    ];
    for (name, input, first_block_type, expected) in cases {
        for (level, digest) in LEVELS.into_iter().zip(expected) {
            let compressed = deflate_bytes(&input, level);
            assert_eq!((compressed[0] >> 1) & 3, first_block_type, "{name}");
            assert_eq!(
                fmn_hash::sha256(&compressed).to_hex(),
                digest,
                "case={name}, level={level:?}",
            );
        }
    }
}

#[test]
fn dictionary_segments_match_the_pre_dag_encoder() {
    let mut input = lcg_bytes(180_000, 42);
    input[30_000..60_000].fill(b'x');
    input.copy_within(25_000..65_000, 100_000);
    // Also captured from 8717dc8b. The middle segment begins inside a run,
    // requiring the previous plaintext dictionary and a nonfinal sync flush.
    const EXPECTED: &str = "3aafbabf1d1fafb9f608db4a3444b135f8b690d06f5f2d5b48a57d4236548736";
    for level in LEVELS {
        let mut compressed = Vec::new();
        for span in [0usize, 40_000, 100_000, input.len()].windows(2) {
            compressed.extend_from_slice(&deflate_segment(
                &input[..span[0]],
                &input[span[0]..span[1]],
                level,
                span[1] == input.len(),
            ));
        }
        assert_eq!(
            fmn_hash::sha256(&compressed).to_hex(),
            EXPECTED,
            "{level:?}"
        );
        assert_eq!(inflate_bytes(&compressed, input.len()).unwrap(), input);
    }
}

#[test]
fn compression_actually_compresses() {
    let data = b"ab".repeat(10_000);
    let packed = deflate_bytes(&data, CompressionLevel::Default);
    assert!(
        packed.len() < data.len() / 10,
        "20000 bytes of 'ab' compressed to {}",
        packed.len()
    );
    // Incompressible data falls back to stored blocks: bounded overhead.
    let noise = lcg_bytes(65_536, 1);
    let packed = deflate_bytes(&noise, CompressionLevel::Best);
    assert!(packed.len() < noise.len() + 64, "stored fallback overhead");
}

#[test]
fn segment_composition_is_a_valid_stream() {
    let data = lcg_bytes(200_000, 42);
    let mut mixed = data.clone();
    // Make the tail compressible so matches want to cross boundaries.
    mixed[100_000..150_000].fill(b'x');

    for level in LEVELS {
        let bounds = [0usize, 64_000, 130_000, mixed.len()];
        let mut stream = Vec::new();
        let mut segments = Vec::new();
        for w in bounds.windows(2) {
            let (start, end) = (w[0], w[1]);
            let last = end == mixed.len();
            let segment = deflate_segment(&mixed[..start], &mixed[start..end], level, last);
            if !last {
                // The sync flush leaves every non-final segment
                // byte-aligned, ending in the empty stored block.
                assert_eq!(&segment[segment.len() - 4..], &[0x00, 0x00, 0xff, 0xff]);
            }
            stream.extend_from_slice(&segment);
            segments.push(segment);
        }
        let unpacked = inflate_bytes(&stream, mixed.len()).unwrap();
        assert_eq!(unpacked, mixed, "level {level:?}");

        // Segment production is independently repeatable — the parallel
        // path composes the identical bytes.
        for (w, segment) in bounds.windows(2).zip(&segments) {
            let again = deflate_segment(
                &mixed[..w[0]],
                &mixed[w[0]..w[1]],
                level,
                w[1] == mixed.len(),
            );
            assert_eq!(&again, segment);
        }
    }
}

#[test]
fn zlib_round_trip_and_header() {
    for data in corpora() {
        let packed = zlib_compress(&data, CompressionLevel::Default);
        assert_eq!(packed[0], 0x78);
        assert_eq!(((u16::from(packed[0]) << 8) | u16::from(packed[1])) % 31, 0);
        let unpacked = zlib_decompress(&packed, data.len().max(1)).unwrap();
        assert_eq!(unpacked, data);
    }
}

#[test]
fn zlib_detects_corruption() {
    let packed = zlib_compress(b"integrity matters", CompressionLevel::Default);
    // Flip a bit in the adler trailer.
    let mut bad = packed.clone();
    let last = bad.len() - 1;
    bad[last] ^= 1;
    assert_eq!(
        zlib_decompress(&bad, 1024),
        Err(InflateError::AdlerMismatch)
    );
    // A preset-dictionary header is refused.
    let mut fdict = packed;
    fdict[1] |= 0x20;
    assert_eq!(
        zlib_decompress(&fdict, 1024),
        Err(InflateError::InvalidZlibHeader)
    );
}

#[test]
fn inflate_cross_validates_against_cpython_zlib() {
    // Fixtures generated by CPython's zlib (an independent compressor):
    // corpus.zlib is zlib.compress(data, 6); corpus.deflate is a raw
    // wbits=-15 stream at level 9.
    let data = include_bytes!("fixtures/corpus.bin").to_vec();
    let z = include_bytes!("fixtures/corpus.zlib");
    let raw = include_bytes!("fixtures/corpus.deflate");
    assert_eq!(zlib_decompress(z, data.len()).unwrap(), data);
    assert_eq!(inflate_bytes(raw, data.len()).unwrap(), data);
    // An exact budget passes; one byte less is the bomb refusal.
    assert_eq!(
        inflate_bytes(raw, data.len() - 1),
        Err(InflateError::OutputLimit {
            limit: data.len() - 1
        })
    );
}

#[test]
fn bomb_is_refused_within_budget() {
    // ~5 MiB of zeros compresses to a few KiB; the budget must refuse
    // during decompression, long before 5 MiB materializes.
    let bomb = deflate_bytes(&vec![0u8; 5 << 20], CompressionLevel::Best);
    assert!(bomb.len() < 32_768);
    let limit = 1 << 20;
    assert_eq!(
        inflate_bytes(&bomb, limit),
        Err(InflateError::OutputLimit { limit })
    );
}

#[test]
fn malformed_streams_are_typed_refusals() {
    // Reserved block type (bfinal=1, btype=11).
    assert_eq!(
        inflate_bytes(&[0x07], 64),
        Err(InflateError::InvalidBlockType)
    );
    // Stored block with a broken LEN/NLEN complement.
    assert_eq!(
        inflate_bytes(&[0x01, 0x05, 0x00, 0x00, 0x00], 64),
        Err(InflateError::InvalidStoredLength)
    );
    // Truncated mid-structure.
    assert_eq!(inflate_bytes(&[], 64), Err(InflateError::UnexpectedEof));
    let good = deflate_bytes(b"truncate me please, thanks", CompressionLevel::Default);
    for cut in 1..good.len().min(8) {
        assert!(
            inflate_bytes(&good[..cut], 64).is_err(),
            "prefix {cut} accepted"
        );
    }
    // Garbage never hangs: arbitrary byte soup must return promptly.
    let soup = lcg_bytes(4096, 7);
    let _ = inflate_bytes(&soup, 1 << 16);
}
