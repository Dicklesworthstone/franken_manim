//! Rich text authoring over Scribe's existing font, span and layout authorities.
//! No font discovery, markup rewriting, second shaper or external process.
#[path = "portal_markdown.rs"]
mod markdown;
#[path = "portal_svg_ingress.rs"]
mod svg_ingress;
#[path = "portal_table.rs"]
mod table;
#[path = "portal_typesetting.rs"]
mod typesetting;

use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyModule};

use super::{
    BridgeMobject, hoist_descendant_records, install_native_tree, native_error, srgb_from_py,
    with_font_book,
};
use fmn_core::color::Srgb;

const MAX_BYTES: usize = 262_144;
const MAX_ITEMS: usize = 4096;

/// Reuse the library's compact span map over the current live metadata.
///
/// Python's ordinary copy/become and numeric-part replacement already own
/// these fields. Reading them at the boundary avoids a second cached index.
fn string_span_map(
    source: &str,
    spans: Vec<(usize, usize)>,
) -> PyResult<fmn_library::SpanMapData> {
    if spans
        .iter()
        .any(|&(start, end)| start > end || source.get(start..end).is_none())
    {
        return Err(super::tex_error(
            "native string spans must be valid UTF-8 byte intervals",
        ));
    }
    Ok(fmn_library::SpanMapData {
        source: std::sync::Arc::from(source),
        entries: spans
            .into_iter()
            .map(|(start, end)| fmn_library::SpanMapEntry {
                start,
                end,
                // Kind does not participate in selection; live objects
                // retain their original native glyph/rule/path classes.
                kind: fmn_library::SpanKindU8::TextGlyph,
            })
            .collect(),
    })
}

/// Literal matching uses Scribe, while Python callers see character offsets.
#[pyfunction]
fn _string_find_spans(source: &str, needle: &str) -> Vec<(usize, usize)> {
    if needle.is_empty() {
        // Preserve the Reference's re.finditer("", source) boundary matches.
        return (0..=source.chars().count())
            .map(|index| (index, index))
            .collect();
    }
    let mut byte_offset = 0;
    let mut char_offset = 0;
    fmn_library::SpanMapData::find_occurrences(source, needle)
        .into_iter()
        .map(|(start, end)| {
            char_offset += source[byte_offset..start].chars().count();
            let first = char_offset;
            char_offset += source[start..end].chars().count();
            byte_offset = end;
            (first, char_offset)
        })
        .collect()
}

/// Literal, regex and explicit-range selectors share whole-primitive selection.
#[pyfunction]
fn _string_span_selections(
    source: &str,
    spans: Vec<(usize, usize)>,
    selections: Vec<(usize, usize)>,
) -> PyResult<Vec<Vec<usize>>> {
    let data = string_span_map(source, spans)?;
    selections
        .into_iter()
        .map(|range @ (start, end)| {
            if start > end || source.get(start..end).is_none() {
                return Err(super::tex_error(
                    "string selections must be valid UTF-8 byte intervals",
                ));
            }
            Ok(data.select(range))
        })
        .collect()
}

/// Match complete source occurrences without reclaiming already-owned parts.
#[pyfunction]
fn _string_matching_occurrences(
    source: &str,
    spans: Vec<(usize, usize)>,
    needle: &str,
    unavailable: Vec<bool>,
) -> PyResult<Vec<Vec<usize>>> {
    let data = string_span_map(source, spans)?;
    if unavailable.len() != data.entries.len() {
        return Err(super::tex_error(
            "matching availability must have one entry per native source-span part",
        ));
    }
    let mut result = Vec::new();
    for range @ (start, end) in fmn_library::SpanMapData::find_occurrences(source, needle) {
        let overlaps = |entry: &fmn_library::SpanMapEntry| entry.start < end && entry.end > start;
        if data
            .entries
            .iter()
            .zip(&unavailable)
            .any(|(entry, taken)| overlaps(entry) && *taken)
        {
            continue;
        }
        if data
            .entries
            .iter()
            .any(|entry| overlaps(entry) && !(start <= entry.start && entry.end <= end))
        {
            return Err(super::tex_error(format!(
                "Matching key {needle:?} splits a native source-span part"
            )));
        }
        let selected = data.select(range);
        if !selected.is_empty() {
            result.push(selected);
        }
    }
    Ok(result)
}

/// Count native primitives for a snippet absent from the live formula.
#[pyfunction]
fn _tex_substring_path_count(
    source: &str,
    text_mode: bool,
    template: &str,
    preamble: &str,
) -> PyResult<usize> {
    if source.len() > MAX_BYTES {
        return Err(super::tex_error(format!(
            "TeX substring exceeds the {MAX_BYTES}-byte source limit"
        )));
    }
    with_tex_template(template, |engine| {
        if text_mode {
            fmn_library::TexText::new(source)
                .preamble(preamble)
                .build(engine)
        } else {
            fmn_library::Tex::new(source)
                .preamble(preamble)
                .build(engine)
        }
        .map(|tex| tex.len())
        .map_err(super::tex_error)
    })
}

thread_local! {
    static BASIC_TEX_ENGINE: std::cell::OnceCell<fmn_library::TexEngine> =
        const { std::cell::OnceCell::new() };
    static EMPTY_TEX_ENGINE: std::cell::OnceCell<fmn_library::TexEngine> =
        const { std::cell::OnceCell::new() };
}

/// Reuse a bounded set of native engines; never share mutable macro state.
pub(crate) fn with_tex_template<T>(
    template: &str,
    operation: impl FnOnce(&fmn_library::TexEngine) -> PyResult<T>,
) -> PyResult<T> {
    if template.len() > 1024 {
        return Err(PyValueError::new_err(
            "Tex template exceeds 1024 UTF-8 bytes",
        ));
    }
    let registry = fmn_config::PackRegistry::builtin();
    let pack = registry
        .resolve_template(if template.is_empty() {
            "default"
        } else {
            template
        })
        .map_err(super::tex_error)?;
    let cell = match pack.name {
        "default" => return super::with_tex_engine(operation),
        "basic" => &BASIC_TEX_ENGINE,
        "empty" => &EMPTY_TEX_ENGINE,
        _ => return Err(super::tex_error("native Tex pack has no engine slot")),
    };
    cell.with(|cell| {
        if cell.get().is_none() {
            let engine =
                fmn_library::TexEngine::new(pack.content_id, None).map_err(super::tex_error)?;
            let _ = cell.set(engine);
        }
        operation(cell.get().expect("initialized above"))
    })
}

/// Admission happens before a Python constructor installs its live state.
#[pyfunction]
fn _validate_tex_options(template: &str, preamble: &str, text_mode: bool) -> PyResult<()> {
    with_tex_template(template, |engine| {
        if preamble.is_empty() {
            return Ok(());
        }
        if text_mode {
            fmn_library::TexText::new("")
                .preamble(preamble)
                .build(engine)
        } else {
            fmn_library::Tex::new("").preamble(preamble).build(engine)
        }
        .map(|_| ())
        .map_err(super::tex_error)
    })
}

fn number(options: &Bound<'_, PyDict>, name: &str, default: f64, positive: bool) -> PyResult<f64> {
    let value = match options.get_item(name)? {
        Some(v) => v.extract::<f64>()?,
        None => default,
    };
    if !value.is_finite() || (positive && value <= 0.0) || value.abs() > f64::from(f32::MAX) {
        return Err(PyValueError::new_err(format!(
            "Text {name} must be finite, f32-representable{}",
            if positive { " and positive" } else { "" }
        )));
    }
    Ok(value)
}

fn string(options: &Bound<'_, PyDict>, name: &str, default: &str) -> PyResult<String> {
    match options.get_item(name)? {
        Some(v) => v.extract(),
        None => Ok(default.to_owned()),
    }
}

fn flag(options: &Bound<'_, PyDict>, name: &str, default: bool) -> PyResult<bool> {
    match options.get_item(name)? {
        Some(v) => v.extract(),
        None => Ok(default),
    }
}

fn pairs<T>(options: &Bound<'_, PyDict>, name: &str) -> PyResult<Vec<(String, T)>>
where
    for<'a, 'py> T: FromPyObject<'a, 'py>,
{
    let Some(value) = options.get_item(name)? else {
        return Ok(Vec::new());
    };
    if value.len()? > MAX_ITEMS {
        return Err(PyValueError::new_err("Text style map exceeds 4096 entries"));
    }
    value.extract().map_err(|_| {
        PyTypeError::new_err(format!("Text {name} must be ordered string/value pairs"))
    })
}

fn colors(options: &Bound<'_, PyDict>) -> PyResult<Vec<(String, Vec<Srgb>)>> {
    let Some(value) = options.get_item("t2g")? else {
        return Ok(Vec::new());
    };
    if value.len()? > MAX_ITEMS {
        return Err(PyValueError::new_err(
            "Text gradient map exceeds 4096 entries",
        ));
    }
    let mut result = Vec::new();
    let mut total = 0usize;
    for pair in value.try_iter()? {
        let pair = pair?;
        if pair.len()? != 2 {
            return Err(PyTypeError::new_err("Text gradient entries must be pairs"));
        }
        let key = pair.get_item(0)?.extract::<String>()?;
        let stops = pair.get_item(1)?;
        total = total.saturating_add(stops.len()?);
        if total > MAX_ITEMS {
            return Err(PyValueError::new_err(
                "Text gradients exceed 4096 total stops",
            ));
        }
        let mut converted = Vec::new();
        for stop in stops.try_iter()? {
            let color = srgb_from_py(&stop?)?;
            if [color.r, color.g, color.b]
                .iter()
                .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
            {
                return Err(PyValueError::new_err(
                    "Text colors must be finite RGB values in [0, 1]",
                ));
            }
            converted.push(color);
        }
        result.push((key, converted));
    }
    Ok(result)
}

#[pyfunction]
fn _build_styled_text<'py>(
    slf: &Bound<'py, BridgeMobject>,
    factory: &Bound<'py, PyAny>,
    text: &str,
    markup: bool,
    hoist_records: bool,
    options: &Bound<'py, PyDict>,
) -> PyResult<Bound<'py, PyList>> {
    if text.len() > MAX_BYTES {
        return Err(PyValueError::new_err("Text source exceeds 262144 bytes"));
    }
    for key in options.keys().iter() {
        let key = key.extract::<String>()?;
        if ![
            "font_size",
            "font",
            "bold",
            "italic",
            "alignment",
            "line_spacing",
            "line_width",
            "justify",
            "indent",
            "disable_ligatures",
            "t2f",
            "t2s",
            "t2w",
            "t2g",
            "code_language",
            "code_style",
        ]
        .contains(&key.as_str())
        {
            return Err(PyTypeError::new_err(format!(
                "unknown native Text option: {key}"
            )));
        }
    }
    let font = string(options, "font", "")?;
    let align = match string(options, "alignment", "LEFT")?.as_str() {
        "LEFT" => fmn_library::text::Align::Left,
        "CENTER" => fmn_library::text::Align::Center,
        "RIGHT" => fmn_library::text::Align::Right,
        _ => {
            return Err(PyValueError::new_err(
                "Text alignment must be LEFT, CENTER or RIGHT",
            ));
        }
    };
    let families = pairs::<String>(options, "t2f")?;
    let slants = pairs::<bool>(options, "t2s")?;
    let weights = pairs::<bool>(options, "t2w")?;
    let gradients = colors(options)?;
    let count = families.len() + slants.len() + weights.len() + gradients.len();
    if text.len().saturating_mul(count.max(1)) > 16_777_216 {
        return Err(PyValueError::new_err(
            "Text style matching exceeds the 16777216-byte-work budget",
        ));
    }
    let family_refs: Vec<(&str, &str)> = families
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let slant_refs: Vec<(&str, bool)> = slants.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    let weight_refs: Vec<(&str, bool)> = weights.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    let gradient_refs: Vec<(&str, &[Srgb])> = gradients
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_slice()))
        .collect();
    // Highlight by original byte position, never substring/color replacement.
    // The public Code class selects its default bundled family separately so
    // explicit fonts and local face maps retain the ordinary text semantics.
    let code_styles = if options.contains("code_language")? {
        if markup {
            return Err(PyValueError::new_err(
                "Code source must be literal, not markup",
            ));
        }
        let language = string(options, "code_language", "")?;
        let style = string(options, "code_style", "monokai")?;
        if language.len() > 1024 || style.len() > 1024 {
            return Err(PyValueError::new_err(
                "Code language/style names exceed 1024 bytes",
            ));
        }
        let theme = fmn_library::CodeTheme::from_pygments_name(&style).ok_or_else(|| {
            PyValueError::new_err(format!("unsupported native Code theme: {style}"))
        })?;
        let mut overrides = fmn_library::Code::new(text)
            .language(&language)
            .theme(theme)
            .character_overrides();
        for entry in &mut overrides {
            entry.family = None;
        }
        overrides
    } else {
        if options.contains("code_style")? {
            return Err(PyValueError::new_err(
                "Code theme requires a language declaration",
            ));
        }
        Vec::new()
    };
    let mut builder = if markup {
        fmn_library::Text::markup(text)
    } else {
        fmn_library::Text::new(text)
    }
    .font_size(number(options, "font_size", 48., true)?)
    .font(&font)
    .bold(flag(options, "bold", false)?)
    .italic(flag(options, "italic", false)?)
    .align(align)
    .line_spacing(number(options, "line_spacing", 1., true)?)
    .justify(flag(options, "justify", false)?)
    .indent(number(options, "indent", 0., false)?)
    .ligatures(!flag(options, "disable_ligatures", true)?)
    .t2f(&family_refs)
    .t2s(&slant_refs)
    .t2w(&weight_refs)
    .t2g(&gradient_refs)
    .char_overrides(&code_styles);
    if options.contains("line_width")? {
        builder = builder.width(number(options, "line_width", 1., true)?);
    }
    let built = with_font_book(|book| {
        for (_, family) in &families {
            book.family(family).map_err(native_error)?;
        }
        builder.build(book).map_err(native_error)
    })?;
    let spans: Vec<(usize, usize)> = built.layout.glyphs.iter().map(|glyph| glyph.span).collect();
    let paths: Vec<Vec<usize>> = (0..spans.len()).map(|index| vec![index]).collect();
    let mut tree = fmn_mobject::Mobject::from(built.vmob);
    if hoist_records {
        hoist_descendant_records(&mut tree, true)?;
    }
    let specs = install_native_tree(slf, factory, tree)?;
    slf.as_any().setattr("_string_sub_spans", spans)?;
    slf.as_any().setattr("_string_sub_paths", paths)?;
    Ok(specs)
}

pub(crate) fn install(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(_string_find_spans, module)?)?;
    module.add_function(wrap_pyfunction!(_string_span_selections, module)?)?;
    module.add_function(wrap_pyfunction!(_string_matching_occurrences, module)?)?;
    module.add_function(wrap_pyfunction!(_tex_substring_path_count, module)?)?;
    module.add_function(wrap_pyfunction!(_build_styled_text, module)?)?;
    module.add_function(wrap_pyfunction!(_validate_tex_options, module)?)?;
    typesetting::install(module)?;
    table::install(module)?;
    svg_ingress::install(module)?;
    markdown::install(module)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_span_frontend_converts_unicode_and_preserves_native_ordinals() {
        let source = "é + é";
        assert_eq!(_string_find_spans(source, "é"), [(0, 1), (4, 5)]);
        assert_eq!(
            _string_find_spans(source, ""),
            [(0, 0), (1, 1), (2, 2), (3, 3), (4, 4), (5, 5)]
        );
        // Painter order is deliberately different from source order.
        let spans = vec![(5, 7), (3, 4), (0, 2)];
        assert_eq!(
            _string_span_selections(
                source,
                spans.clone(),
                vec![(0, 2), (5, 7), (0, 7)]
            )
            .unwrap(),
            [vec![2], vec![0], vec![0, 1, 2]]
        );
        assert_eq!(
            _string_matching_occurrences(source, spans, "é", vec![false, false, true])
                .unwrap(),
            [vec![0]]
        );
    }

    #[test]
    fn matching_refuses_partial_native_parts_but_respects_prior_claims() {
        Python::initialize();
        Python::attach(|_| {
            assert!(_string_span_selections("é", vec![(0, 1)], vec![(0, 2)]).is_err());
            assert!(_string_span_selections("é", vec![(0, 2)], vec![(0, 1)]).is_err());
            assert!(
                _string_matching_occurrences("ab", vec![(0, 2)], "a", vec![false])
                    .is_err()
            );
            assert!(
                _string_matching_occurrences("ab", vec![(0, 2)], "a", vec![true])
                    .unwrap()
                    .is_empty()
            );
        });
    }

    #[test]
    fn absent_tex_substrings_are_bounded_before_template_resolution() {
        Python::initialize();
        Python::attach(|_| {
            let oversized = "x".repeat(MAX_BYTES + 1);
            let error = _tex_substring_path_count(
                &oversized,
                false,
                "unresolved-template",
                "",
            )
            .expect_err("oversized source must be refused before typesetting");
            assert!(error.to_string().contains("262144-byte source limit"));
        });
    }

    #[test]
    fn native_derived_curve_authoring() {
        crate::with_python_test_module("native derived curves", |py, _module, globals| {
            let source = std::ffi::CString::new(include_str!("../tests/native_derived_curves.py"))
                .expect("derived curve tests contain no NUL");
            py.run(source.as_c_str(), Some(globals), Some(globals))
                .inspect_err(|error| error.print(py))
                .unwrap();
            globals
                .get_item("run_native_derived_curves")
                .unwrap()
                .unwrap()
                .call0()
                .inspect_err(|error| error.print(py))
                .unwrap();
        });
    }

    #[test]
    fn native_brace_authoring() {
        crate::with_python_test_module("native brace authoring", |py, _module, globals| {
            let source = std::ffi::CString::new(include_str!("../tests/native_brace_lifecycle.py"))
                .expect("brace tests contain no NUL");
            py.run(source.as_c_str(), Some(globals), Some(globals))
                .inspect_err(|error| error.print(py))
                .unwrap();
            globals
                .get_item("run_native_braces")
                .unwrap()
                .unwrap()
                .call0()
                .inspect_err(|error| error.print(py))
                .unwrap();
        });
    }
}
