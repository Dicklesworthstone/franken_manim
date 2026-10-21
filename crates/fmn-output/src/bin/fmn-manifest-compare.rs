//! `fmn-manifest-compare A.fmnp B.fmnp`: the certified-matrix check of
//! docs/INPUT_CLOSURE.md §4. Two FMNP manifests, typically from two
//! certified platforms, are compared by their platform-neutral semantic
//! digests and their certified output identities (kind and digest).
//!
//! Prints one JSON line (`schema` `fmn.manifest-compare`, version 1) and
//! exits 0 when the certified promise holds (same semantic input, both
//! certified, at least one certified output, every certified output equal),
//! 1 on a determinism failure (same semantic input, a certified output
//! differs), 2 when the manifests are not comparable (different input,
//! not both certified, or neither declares a certified output), and 64
//! on a usage, invalid-manifest, or bounded-read error.

#![forbid(unsafe_code)]

use std::process::ExitCode;

use fmn_output::{ManifestError, ProvenanceManifest};
use fmn_platform::fs::{FileSystem, StdFs};

/// No manifest a render publishes comes near this; a larger file is not one.
const MAX_MANIFEST_BYTES: usize = 64 * 1024 * 1024;

fn load(path: &std::ffi::OsStr) -> Result<ProvenanceManifest, String> {
    let path = std::path::Path::new(path);
    let shown = path.display();
    // Enforce the ceiling while reading, not merely against a metadata snapshot
    // before an unbounded read. The shared capability also refuses non-files.
    let bytes = StdFs
        .read_bounded(path, MAX_MANIFEST_BYTES)
        .map_err(|error| format!("{shown}: {error}"))?;
    ProvenanceManifest::from_bytes(&bytes).map_err(|error| format!("{shown}: {error}"))
}

fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            ch if u32::from(ch) < 0x20 => out.push_str(&format!("\\u{:04x}", u32::from(ch))),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let [left, right] = args.as_slice() else {
        eprintln!("usage: fmn-manifest-compare A.fmnp B.fmnp");
        return ExitCode::from(64);
    };
    let (left, right) = match (load(left), load(right)) {
        (Ok(left), Ok(right)) => (left, right),
        (Err(error), _) | (_, Err(error)) => {
            eprintln!("fmn-manifest-compare: {error}");
            return ExitCode::from(64);
        }
    };
    let digests = left
        .semantic_digest()
        .and_then(|l| right.semantic_digest().map(|r| (l, r)));
    let (left_semantic, right_semantic) = match digests {
        Ok(digests) => digests,
        Err(error) => {
            eprintln!("fmn-manifest-compare: {error}");
            return ExitCode::from(64);
        }
    };
    let comparison = match left.compare(&right) {
        Ok(comparison) => comparison,
        Err(ManifestError::NoCertifiedOutputs) => {
            // Valid encoded-only receipts remain readable provenance. They
            // provide no certified artifacts, hence no bit-agreement evidence.
            println!(
                "{{\"schema\":\"fmn.manifest-compare\",\"version\":1,\
                 \"verdict\":\"not-comparable\",\"reason\":\"no-certified-outputs\",\
                 \"semantic_equal\":{},\"closure_equal\":{},\"certified_both\":true,\
                 \"left_semantic_digest\":{},\"right_semantic_digest\":{},\
                 \"differing_outputs\":[]}}",
                left_semantic == right_semantic,
                left.closure_digest == right.closure_digest,
                json_string(&left_semantic.to_string()),
                json_string(&right_semantic.to_string()),
            );
            return ExitCode::from(2);
        }
        Err(error) => {
            eprintln!("fmn-manifest-compare: {error}");
            return ExitCode::from(64);
        }
    };
    let (verdict, code) = if comparison.certified_bits_agree() {
        ("agree", 0)
    } else if comparison.semantic_equal && comparison.certified_both {
        ("determinism-failure", 1)
    } else {
        ("not-comparable", 2)
    };
    let outputs: Vec<String> = comparison
        .differing_outputs
        .iter()
        .map(|path| json_string(path))
        .collect();
    println!(
        "{{\"schema\":\"fmn.manifest-compare\",\"version\":1,\"verdict\":{},\
         \"semantic_equal\":{},\"closure_equal\":{},\"certified_both\":{},\
         \"left_semantic_digest\":{},\"right_semantic_digest\":{},\
         \"differing_outputs\":[{}]}}",
        json_string(verdict),
        comparison.semantic_equal,
        comparison.closure_equal,
        comparison.certified_both,
        json_string(&left_semantic.to_string()),
        json_string(&right_semantic.to_string()),
        outputs.join(",")
    );
    ExitCode::from(code)
}
