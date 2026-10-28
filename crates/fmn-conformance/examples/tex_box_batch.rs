//! fmd-math's boxes for a batch of formulas, laid out as the TeX box census
//! lays its strings out (display style, the default macro pack): the fmd side
//! of `scripts/tex_grammar_differential.py`
//! (fm-tex-grammar-differential-r12-i0nh).
//!
//! Reads `id<TAB>formula` lines and writes `id<TAB>width<TAB>height<TAB>depth`
//! in ems, `id<TAB>ERR<TAB>message` for a refused formula, or
//! `id<TAB>PANIC<TAB>message` for one that panicked, which fmd-math must never
//! do.
//!
//! ```sh
//! cargo run -p fmn-conformance --example tex_box_batch -- cases.tsv boxes.tsv
//! ```
#![forbid(unsafe_code)]

use std::fmt::Write as _;

fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let (Some(input), Some(output)) = (args.next(), args.next()) else {
        return Err("usage: tex_box_batch <cases.tsv> <boxes.tsv>".to_owned());
    };
    let cases = std::fs::read_to_string(&input).map_err(|error| format!("{input}: {error}"))?;
    let engine = fmd_math::Engine::bundled().map_err(|error| format!("bundled faces: {error}"))?;
    let pack = fmd_math::MacroSet::pack("fmd-math/pack/default")
        .ok_or("the default macro pack is missing")?;
    let mut boxes = String::new();
    for line in cases
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        let (id, formula) = line
            .split_once('\t')
            .ok_or_else(|| format!("malformed case line {line:?}"))?;
        let laid = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            engine.typeset_with_macros(formula, fmd_math::Style::Display, &pack)
        }));
        match laid {
            Ok(Ok(layout)) => writeln!(
                boxes,
                "{id}\t{:.6}\t{:.6}\t{:.6}",
                layout.width, layout.height, layout.depth
            ),
            Ok(Err(error)) => writeln!(boxes, "{id}\tERR\t{}", one_line(&error.to_string())),
            Err(payload) => {
                let message = payload
                    .downcast_ref::<&str>()
                    .map(|message| (*message).to_owned())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_default();
                writeln!(boxes, "{id}\tPANIC\t{}", one_line(&message))
            }
        }
        .map_err(|error| error.to_string())?;
    }
    std::fs::write(&output, boxes).map_err(|error| format!("{output}: {error}"))
}

fn one_line(text: &str) -> String {
    text.replace(['\t', '\n', '\r'], " ")
}
