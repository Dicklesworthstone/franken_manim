//! The `.npy` fixture-interchange loop, end to end (§16.3 plane 1, fm-xb3):
//! `scripts/gen_npy_fixtures.py` drives the pinned Reference's own
//! `utils/bezier.py` and saves its outputs with `np.save`; this test verifies
//! manifest integrity (sha256 via fmn-hash), decodes with the owned reader,
//! recomputes each case with fmn-geom, and compares at the doctrine's loose
//! cross-engine tolerance (§16.4).
//!
//! The re-encode check also locks byte-compatibility with numpy's writer:
//! `write_npy(read_npy(bytes)) == bytes`, so fixtures round-trip through
//! Python tooling without churn.

use fmn_conformance::npy::read_npy;
use fmn_conformance::npy::write_npy;
use fmn_conformance::tolerance::{NanPolicy, check_points_abs};
use fmn_core::constants::TAU;
use fmn_core::types::Vec3;
use fmn_geom::bezier::{partial_quadratic, quadratic_points_for_arc};
use fmn_hash::sha256;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Loose cross-engine tolerance: both sides compute these formulas in f64,
/// but op order differs; 1e-6 is far looser than the observed drift and far
/// tighter than any geometric significance.
const TOL: f64 = 1e-6;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/npy")
}

/// Manifest rows: file name → (dtype, shape, sha256-hex).
fn load_manifest() -> BTreeMap<String, (String, String, String)> {
    let text = std::fs::read_to_string(fixture_dir().join("MANIFEST.tsv"))
        .expect("fixtures/npy/MANIFEST.tsv present; regenerate with scripts/gen_npy_fixtures.py");
    let mut rows = BTreeMap::new();
    for line in text.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        assert!(fields.len() >= 4, "manifest row too short: {line:?}");
        rows.insert(
            fields[0].to_string(),
            (
                fields[1].to_string(),
                fields[2].to_string(),
                fields[3].to_string(),
            ),
        );
    }
    rows
}

/// Read one fixture, verifying its manifest hash first.
fn load_points(name: &str, manifest: &BTreeMap<String, (String, String, String)>) -> Vec<Vec3> {
    let (dtype, _shape, hex) = manifest
        .get(name)
        .unwrap_or_else(|| panic!("{name} missing from MANIFEST.tsv"));
    assert_eq!(dtype, "<f8", "{name}: manifest dtype");
    let bytes = std::fs::read(fixture_dir().join(name)).expect("fixture file present");
    assert_eq!(
        &sha256(&bytes).to_hex(),
        hex,
        "{name}: fixture bytes do not match MANIFEST.tsv — regenerate or investigate"
    );
    let array = read_npy(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
    // Byte-compatibility with np.save: our writer must reproduce the file.
    assert_eq!(
        write_npy(&array),
        bytes,
        "{name}: owned writer is not byte-compatible with np.save"
    );
    array.to_points().unwrap_or_else(|e| panic!("{name}: {e}"))
}

#[test]
fn arc_fixtures_match_fmn_geom() {
    let manifest = load_manifest();
    for (name, angle, n) in [
        ("arc_quarter_n4.npy", TAU / 4.0, 4usize),
        ("arc_full_n8.npy", TAU, 8),
        ("arc_neg_third_n2.npy", -TAU / 3.0, 2),
    ] {
        let reference = load_points(name, &manifest);
        let ours = quadratic_points_for_arc(angle, n);
        check_points_abs(&reference, &ours, TOL, NanPolicy::Reject)
            .unwrap_or_else(|m| panic!("{name}: {m}"));
    }
}

#[test]
fn partial_quad_fixture_matches_fmn_geom() {
    let manifest = load_manifest();
    let reference = load_points("partial_quad.npy", &manifest);
    // The same asymmetric off-axis quadratic the generator hardcodes.
    let quad: [Vec3; 3] = [[-1.0, 0.5, 0.25], [0.75, 2.0, -0.5], [2.0, -1.0, 1.0]];
    let ours = partial_quadratic(&quad, 0.25, 0.75);
    check_points_abs(&reference, &ours, TOL, NanPolicy::Reject)
        .unwrap_or_else(|m| panic!("partial_quad: {m}"));
}

#[test]
fn every_manifest_row_has_its_file_and_hash() {
    let manifest = load_manifest();
    assert!(manifest.len() >= 4, "expected the full fixture set");
    for (name, (_dtype, shape, hex)) in &manifest {
        let bytes = std::fs::read(fixture_dir().join(name)).expect("fixture listed but missing");
        assert_eq!(&sha256(&bytes).to_hex(), hex, "{name}: integrity");
        let array = read_npy(&bytes).unwrap_or_else(|e| panic!("{name}: {e}"));
        let dims: Vec<String> = array.shape.iter().map(ToString::to_string).collect();
        assert_eq!(&dims.join("x"), shape, "{name}: manifest shape");
    }
}

// ------------------------------------------- per-field snapshot export

/// §8.7's fixture-interchange hook (fm-879): any snapshot record column
/// exports as a `.npy` NumPy can read — here locked by round-tripping
/// through the owned writer/reader (whose byte-compatibility with numpy's
/// writer the re-encode test above already pins).
#[test]
fn snapshot_field_exports_as_npy() {
    use fmn_conformance::npy::{NpyArray, NpyData};
    use fmn_mobject::record::{RecordBuffer, RecordSchema};
    use fmn_mobject::{Mobject, Stage};

    let mut stage = Stage::new();
    let mob = stage.add(Mobject::new());
    let entry = stage.get_mut(mob).unwrap();
    entry.buffer = RecordBuffer::new(RecordSchema::vmobject(), 3);
    let flat: Vec<f32> = vec![0.0, 0.0, 0.0, 1.0, 2.0, 0.0, 2.0, 0.0, 0.0];
    entry.buffer.write_range("point", 0, &flat);

    let column = stage.get(mob).unwrap().buffer.read_column("point").unwrap();
    let rows = column.len() / 3;
    let array = NpyArray::new(vec![rows, 3], NpyData::F32(column.clone())).unwrap();
    let bytes = write_npy(&array);
    let back = read_npy(&bytes).unwrap();
    assert_eq!(back.as_f32().unwrap(), column.as_slice());
    assert_eq!(back.shape, vec![3, 3]);
}
