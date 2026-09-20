#!/usr/bin/env python3
"""Keep profile input dependencies when degenerate geometry has no stations."""
from pathlib import Path

pending = {}
for name in ['plan.rs', 'fill_profile.rs', 'stroke_profile.rs']:
    p = 'crates/fmn-render/src/' + name
    pending[p] = Path(p).read_text()

def replace(name, old, new):
    text = pending[name]
    if text.count(old) != 1:
        raise SystemExit('changed profile source anchor: ' + name)
    pending[name] = text.replace(old, new, 1)

for kind, end in [('fill', '\n#[cfg(test)]'), ('stroke', '\n/// Geometry-only station')]:
    name = f'crates/fmn-render/src/{kind}_profile.rs'
    text = pending[name]
    a = text.index('pub(crate) fn from_records(')
    z = text.index(end, a)
    function = text[a:z]
    ty = kind.capitalize() + 'Profile'
    function = function.replace(f'Result<Option<{ty}>,', f'Result<(Option<{ty}>, bool),')
    # No records or flat columns do not need geometry. Variable paint still
    # needs it even when collapse leaves no measurable curve at this instant.
    assert function.count('return Ok(None);') == 4
    function = function.replace('return Ok(None);', 'return Ok((None, false));', 3)
    function = function.replace('return Ok(None);', 'return Ok((None, true));')
    assert function.count('.map(Some)') == 1
    function = function.replace('.map(Some)', '.map(|profile| (Some(profile), true))')
    pending[name] = text[:a] + '/// Return the optional measured profile and whether its authored paint\n/// depends on geometry. A collapsed path can lack stations without becoming\n/// uniform paint; retaining that distinction is essential for recovery.\n' + function + text[z:]

name = 'crates/fmn-render/src/plan.rs'
replace(name, '''            let previous_profiled = previous.is_some_and(|old| {
                self.styles
                    .get(old.style)
                    .or_else(|| {
                        pending_styles
                            .iter()
                            .find(|(i, _)| *i == old.style)
                            .map(|(_, s)| s)
                    })
                    .is_some_and(|style| {
                        style.stroke_profile.is_some() || style.fill_profile.is_some()
                    })
            });
''', '''            // The dependency describes authored paint, not just a currently
            // measurable profile. A zero-length path has no stations but must
            // rebuild its variable paint when it expands again.
            let previous_profiled = previous
                .is_some_and(|old| old.style_dep.axes().contains(&Axis::Geometry));
''')
replace(name, '''            let style = match &previous {
                Some(retained) if !style_unsafe && !retained.style_dep.is_stale(&now) => {
                    retained.style
''', '''            let (style, profiled) = match &previous {
                Some(retained) if !style_unsafe && !retained.style_dep.is_stale(&now) => {
                    (retained.style, previous_profiled)
''')
replace(name, '''                    row.stroke_profile =
                        crate::stroke_profile::from_records(stage, mob, decode_rgba)?
                            .map(std::sync::Arc::new);
                    row.fill_profile = crate::fill_profile::from_records(stage, mob, decode_rgba)?
                        .map(std::sync::Arc::new);
                    let key = row.bits();
                    if let Some(index) = self.styles.index_of(&row) {
''', '''                    let (stroke_profile, stroke_depends_on_geometry) =
                        crate::stroke_profile::from_records(stage, mob, decode_rgba)?;
                    let (fill_profile, fill_depends_on_geometry) =
                        crate::fill_profile::from_records(stage, mob, decode_rgba)?;
                    row.stroke_profile = stroke_profile.map(std::sync::Arc::new);
                    row.fill_profile = fill_profile.map(std::sync::Arc::new);
                    let profiled = stroke_depends_on_geometry || fill_depends_on_geometry;
                    let key = row.bits();
                    let style = if let Some(index) = self.styles.index_of(&row) {
''')
replace(name, '''                        pending_styles.push((index, row));
                        index
                    }
                }
            };
''', '''                        pending_styles.push((index, row));
                        index
                    };
                    (style, profiled)
                }
            };
''')
replace(name, '''            let profiled = self
                .styles
                .get(style)
                .or_else(|| {
                    pending_styles
                        .iter()
                        .find(|(i, _)| *i == style)
                        .map(|(_, s)| s)
                })
                .is_some_and(|row| row.stroke_profile.is_some() || row.fill_profile.is_some());
''', '')

for name, text in pending.items():
    Path(name).write_text(text)
