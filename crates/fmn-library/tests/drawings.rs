//! fm-3kr tranche 1: the geometry-native drawings — Clock, DieFace,
//! Dartboard — structural fixtures plus one bit-locked point digest per
//! class. The asset-backed census classes wait on fm-7lx1's ruling and are
//! deliberately absent.

use fmn_core::constants::TAU;
use fmn_hash::sha256;
use fmn_library::drawings::{Clock, dartboard, die_face, piano, piano_3d, speedometer};
use fmn_library::vmobject::VMobject;
use fmn_text::FontBook;

fn digest(vmob: &VMobject) -> String {
    // Depth-first pre-order over the whole family: group roots carry no
    // own points — the ink lives in the children.
    let mut bytes = Vec::new();
    let mut stack = vec![vmob];
    while let Some(current) = stack.pop() {
        for point in current.points() {
            for component in point {
                bytes.extend_from_slice(&component.to_le_bytes());
            }
        }
        for child in current.children().iter().rev() {
            stack.push(child);
        }
    }
    sha256(&bytes).to_hex()
}

#[allow(dead_code)] // tranche-reserved: used by the upcoming family tests
fn count_family(vmob: &VMobject) -> usize {
    let mut count = 1;
    for child in vmob.children() {
        count += count_family(child);
    }
    count
}

#[test]
fn clock_matches_the_reference_structure() {
    let clock = Clock::new().build();
    // [circle, hour_hand, minute_hand, ticks]
    assert_eq!(clock.children().len(), 4);
    // The ticks child holds twelve tick lines.
    assert_eq!(clock.children()[3].children().len(), 12);
    // Cardinals double: 8 short + 4 long.
    let tick_points: Vec<usize> = clock.children()[3]
        .children()
        .iter()
        .map(|tick| tick.points().len())
        .collect();
    assert!(tick_points.iter().all(|count| *count >= 2));
}

#[test]
fn die_face_rejects_out_of_range_values() {
    assert!(die_face(0, 1.0, 0.5).is_none());
    assert!(die_face(7, 1.0, 0.5).is_none());
}

#[test]
fn die_face_pip_counts_follow_the_reference_layouts() {
    let expected: [usize; 6] = [1, 2, 3, 4, 5, 6];
    for value in 1u8..=6 {
        let face = die_face(value, 1.0, 0.5).expect("in range");
        // [square, pip group]
        assert_eq!(face.children().len(), 2);
        assert_eq!(
            face.children()[1].children().len(),
            expected[value as usize - 1],
            "value {value} pip count"
        );
    }
}

#[test]
fn dartboard_carries_the_full_ring_stack() {
    let board = dartboard().expect("pure geometry");
    // [segments group, bullseyes group]
    assert_eq!(board.children().len(), 2);
    assert_eq!(
        board.children()[0].children().len(),
        20 * 4,
        "twenty sectors across four rings"
    );
    assert_eq!(board.children()[1].children().len(), 2, "two bullseyes");
    // The class radius 3, seen through the hull: an 18-degree arc segment's
    // quadratic handle overshoots the true radius by 1/cos(9°), and the
    // bbox counts handles — the same hull behavior as the Reference's.
    let hull = 3.0 / f64::cos(TAU / 40.0);
    let (min, max) = board.extent().expect("a drawn board has an extent");
    assert!(
        ((min[0] + hull).abs() < 1e-9) && ((max[0] - hull).abs() < 1e-9),
        "board extent x = [{}, {}], hull bound {}",
        min[0],
        max[0],
        hull
    );
    let _ = TAU;
}

#[test]
fn self_goldens_lock_each_class_s_canonical_output() {
    let book = FontBook::bundled().expect("bundled book");
    let clock = Clock::new().build();
    let die = die_face(3, 1.0, 0.5).expect("in range");
    let board = dartboard().expect("pure geometry");
    let speedo = speedometer(&book).expect("labels typeset");
    let piano = piano().expect("pure geometry + booleans");
    let piano_3d = piano_3d().expect("pure geometry + extrusions");
    let cases: [(&str, String); 6] = [
        ("clock", digest(&clock)),
        ("die_face", digest(&die)),
        ("dartboard", digest(&board)),
        ("speedometer", digest(&speedo)),
        ("piano", digest(&piano)),
        ("piano_3d", digest(&piano_3d)),
    ];
    if std::env::var("PRINT_DRAWINGS_GOLDENS").is_ok() {
        for (name, digest) in &cases {
            println!("DRAWINGS_GOLDEN {name} {digest}");
        }
    }
    let expected: [(&str, &str); 6] = [
        (
            "clock",
            "8ef9fe99145e3b8b371cf5af95af4d526615b50faf108de35b34e03123bb400f",
        ),
        (
            "die_face",
            "a0cbb3350a222e71ccb269782e0c4cbf5600a7725cc01f0050ed696596a05477",
        ),
        (
            "dartboard",
            "6e7aebb6d059b3fe28cbab2028714a701766cffb553b377b0246d1539beded64",
        ),
        (
            "speedometer",
            "51f1ca809a55c122dd69be9844df9c0071e1ae6d099e9db9eb6caffd154eee04",
        ),
        (
            "piano",
            "2e17730728ece6b4b7a1412a968eab6b8f714ab05a9c3a742ef848344cbe8a41",
        ),
        (
            "piano_3d",
            "8e62f98b0fcf17256c8e988c1135c37a1b8d056eb2c2ecf5c95647f6a9da7fdd",
        ),
    ];
    for ((name, actual), (expected_name, expected_hash)) in cases.iter().zip(expected.iter()) {
        assert_eq!(expected_name, name);
        assert_eq!(
            actual, expected_hash,
            "self-golden drift for {name}: rerun with PRINT_DRAWINGS_GOLDENS=1 to review"
        );
    }
}
