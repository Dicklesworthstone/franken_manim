Path: fmn-geom/src/svg.rs (Sovereign SVG parser)
Reproducer: <17 bytes, see crash-*>
Mode: cargo fuzz run svg_document (release profile, ASAN, debug-assertions)
Symptom: exit 77 (sanitizer abort) within 15s of fuzzing.
Status: NOT yet triaged. Reproduction artifact preserved.
