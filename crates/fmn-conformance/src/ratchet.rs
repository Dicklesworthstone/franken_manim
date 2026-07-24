//! The public coverage ratchet (§11.5, §18; R1's public mitigation).
//!
//! With no LaTeX fallback, coverage *discipline* replaces fallback
//! discipline — public, precise, monotone. The four published numbers
//! (occurrence-weighted and unique-string coverage, parse vs layout split
//! out) are computed against G0-4's **frozen** corpus denominator and
//! recorded in `docs/ratchet/baseline.tsv`; the human-facing dashboard is
//! `docs/ratchet/dashboard.md`.
//!
//! **The pin-coupling insight that gives CI teeth without the private
//! corpus:** coverage is a pure function of (frozen corpus, fmd-math
//! pin). Between `SUITE.lock` bumps of `franken_markdown` the numbers
//! *cannot move*, so an always-on CI test simply asserts the baseline
//! names the current pin — any pin bump without a ratchet re-run fails —
//! while the corpus-bearing environments (dev boxes, the pin-bump ritual)
//! recompute, enforce monotonicity, and bless with
//! `RATCHET_UPDATE=1`. The 3b1b-authored strings never leave the private
//! fixture (§15.3): the committed artifacts carry numbers, construct
//! names, and hashes only.

use std::fmt::Write as _;

/// The exact counts behind the four published numbers, plus identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Baseline {
    /// The harvest lexer/extraction rules version (G0-4).
    pub rules_version: u32,
    /// The corpus identity hash (G0-4's denominator).
    pub corpus_hash: String,
    /// The franken_markdown commit the numbers were computed against.
    pub franken_markdown_rev: String,
    /// Distinct (mode, string) pairs in the denominator.
    pub unique_total: u64,
    /// Occurrence-weighted denominator.
    pub occurrence_total: u64,
    /// Distinct strings that parse end-to-end.
    pub parse_unique: u64,
    /// Occurrences that parse end-to-end.
    pub parse_occurrences: u64,
    /// Distinct strings that parse and lay out.
    pub layout_unique: u64,
    /// Occurrences that parse and lay out.
    pub layout_occurrences: u64,
}

impl Baseline {
    /// Serialize as the committed TSV (std-parseable, no YAML/TOML).
    #[must_use]
    pub fn to_tsv(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "# The coverage-ratchet baseline (fm-mol). Counts are exact; the\n\
             # four public percentages derive from them. Regenerate with\n\
             # RATCHET_UPDATE=1 (needs the private corpus); the always-on CI\n\
             # test pins this file to the SUITE.lock franken_markdown rev."
        );
        let _ = writeln!(out, "rules_version\t{}", self.rules_version);
        let _ = writeln!(out, "corpus_hash\t{}", self.corpus_hash);
        let _ = writeln!(out, "franken_markdown_rev\t{}", self.franken_markdown_rev);
        let _ = writeln!(out, "unique_total\t{}", self.unique_total);
        let _ = writeln!(out, "occurrence_total\t{}", self.occurrence_total);
        let _ = writeln!(out, "parse_unique\t{}", self.parse_unique);
        let _ = writeln!(out, "parse_occurrences\t{}", self.parse_occurrences);
        let _ = writeln!(out, "layout_unique\t{}", self.layout_unique);
        let _ = writeln!(out, "layout_occurrences\t{}", self.layout_occurrences);
        out
    }

    /// Parse the committed TSV.
    #[must_use]
    pub fn from_tsv(text: &str) -> Option<Self> {
        let mut map = std::collections::BTreeMap::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (k, v) = line.split_once('\t')?;
            map.insert(k.to_owned(), v.to_owned());
        }
        Some(Self {
            rules_version: map.get("rules_version")?.parse().ok()?,
            corpus_hash: map.get("corpus_hash")?.clone(),
            franken_markdown_rev: map.get("franken_markdown_rev")?.clone(),
            unique_total: map.get("unique_total")?.parse().ok()?,
            occurrence_total: map.get("occurrence_total")?.parse().ok()?,
            parse_unique: map.get("parse_unique")?.parse().ok()?,
            parse_occurrences: map.get("parse_occurrences")?.parse().ok()?,
            layout_unique: map.get("layout_unique")?.parse().ok()?,
            layout_occurrences: map.get("layout_occurrences")?.parse().ok()?,
        })
    }

    /// The four public percentages.
    #[must_use]
    pub fn percentages(&self) -> [f64; 4] {
        let pct = |n: u64, d: u64| {
            if d == 0 {
                0.0
            } else {
                100.0 * n as f64 / d as f64
            }
        };
        [
            pct(self.parse_occurrences, self.occurrence_total),
            pct(self.parse_unique, self.unique_total),
            pct(self.layout_occurrences, self.occurrence_total),
            pct(self.layout_unique, self.unique_total),
        ]
    }
}

/// The ratchet rule: within a `rules_version` and denominator, none of the
/// four counts may decrease. Returns the violations (empty = pass).
#[must_use]
pub fn ratchet_violations(baseline: &Baseline, current: &Baseline) -> Vec<String> {
    let mut violations = Vec::new();
    if baseline.rules_version != current.rules_version
        || baseline.corpus_hash != current.corpus_hash
    {
        // A rules/denominator change restates the corpus (G0-4 rule 5);
        // comparability resets and the boundary is annotated on the chart.
        return violations;
    }
    let mut check = |name: &str, before: u64, now: u64| {
        if now < before {
            violations.push(format!("{name} regressed: {before} → {now}"));
        }
    };
    check(
        "parse_occurrences",
        baseline.parse_occurrences,
        current.parse_occurrences,
    );
    check("parse_unique", baseline.parse_unique, current.parse_unique);
    check(
        "layout_occurrences",
        baseline.layout_occurrences,
        current.layout_occurrences,
    );
    check(
        "layout_unique",
        baseline.layout_unique,
        current.layout_unique,
    );
    violations
}

/// One pending construct for the dashboard: a named blocker with its
/// occurrence mass and where it is tracked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pending {
    /// Construct name, table scheme.
    pub construct: String,
    /// Occurrences blocked on it.
    pub occurrences: u64,
    /// Where it is tracked.
    pub tracked: String,
}

/// Render the public dashboard (numbers, construct names, hashes — never
/// the corpus strings; §15.3) plus the trend log and the R1 escalation
/// path.
#[must_use]
pub fn render_dashboard(
    baseline: &Baseline,
    parse_pending: &[Pending],
    layout_pending: &[Pending],
    trend: &[Baseline],
) -> String {
    let [po, pu, lo, lu] = baseline.percentages();
    let mut out = String::new();
    let _ = writeln!(out, "# The fmd-math coverage ratchet\n");
    let _ = writeln!(
        out,
        "The headline metric of the no-LaTeX pivot (§11.5): what fraction of the\n\
         real 3b1b formula corpus typesets natively. The denominator is **frozen**\n\
         (G0-4: `{}` distinct strings, `{}` occurrences, corpus hash\n\
         `{}`, rules_version {}); the numbers may only rise.\n",
        baseline.unique_total,
        baseline.occurrence_total,
        baseline.corpus_hash,
        baseline.rules_version
    );
    let _ = writeln!(out, "**Computed against franken_markdown `{}`.**\n", {
        &baseline.franken_markdown_rev[..12.min(baseline.franken_markdown_rev.len())]
    });
    let _ = writeln!(out, "| Plane | Occurrence-weighted | Unique-string |");
    let _ = writeln!(out, "|---|---|---|");
    let _ = writeln!(out, "| **Parse** | {po:.3} % | {pu:.3} % |");
    let _ = writeln!(out, "| **Parse + layout** | {lo:.3} % | {lu:.3} % |");
    let _ = writeln!(out, "\n## Pending constructs (parse plane)\n");
    let _ = writeln!(out, "| Construct | Occurrences blocked | Tracked at |");
    let _ = writeln!(out, "|---|---|---|");
    for p in parse_pending {
        let _ = writeln!(
            out,
            "| `{}` | {} | {} |",
            p.construct, p.occurrences, p.tracked
        );
    }
    let _ = writeln!(out, "\n## Pending at layout (parse succeeds)\n");
    let _ = writeln!(out, "| Construct | Occurrences blocked | Tracked at |");
    let _ = writeln!(out, "|---|---|---|");
    for p in layout_pending {
        let _ = writeln!(
            out,
            "| `{}` | {} | {} |",
            p.construct, p.occurrences, p.tracked
        );
    }
    let _ = writeln!(out, "\n## Trend (by franken_markdown rev)\n");
    let _ = writeln!(
        out,
        "| Rev | Parse occ. % | Parse uniq. % | Layout occ. % | Layout uniq. % |"
    );
    let _ = writeln!(out, "|---|---|---|---|---|");
    for b in trend {
        let [tpo, tpu, tlo, tlu] = b.percentages();
        let _ = writeln!(
            out,
            "| `{}` | {tpo:.3} | {tpu:.3} | {tlo:.3} | {tlu:.3} |",
            &b.franken_markdown_rev[..12.min(b.franken_markdown_rev.len())]
        );
    }
    let _ = writeln!(
        out,
        "\n## How this is enforced\n\n\
         - Coverage is a pure function of (frozen corpus, fmd-math pin), so the\n\
           numbers can only move when `SUITE.lock`'s `franken_markdown` row moves.\n\
           An always-on CI test requires `baseline.tsv` to name the current pin:\n\
           **a pin bump without a ratchet re-run fails CI.**\n\
         - On corpus-bearing machines the ratchet test recomputes all four\n\
           counts and fails on any decrease; `RATCHET_UPDATE=1` blesses a\n\
           deliberate advance (with this dashboard regenerated in the same\n\
           commit).\n\
         - Every non-tier-1 construct must fail with its precise, named,\n\
           tier-tagged error — audited construct-by-construct against the G0-4\n\
           table in always-on CI. Nothing ever fails silently.\n\n\
         ## The escalation path (R1, the G2 checkpoint)\n\n\
         If coverage misses a gate's criteria, the response is a **public\n\
         amendment with a construct-sprint plan** — never a silent slip: the\n\
         gap is named construct-by-construct above, each with its tracked\n\
         bead, and the gate review adjudicates the sprint scope in the open.\n\n\
         ## Licensing (§15.3)\n\n\
         The corpus strings are 3b1b-authored course material and stay in the\n\
         private fixture; this dashboard publishes **numbers, construct names,\n\
         and hashes only**. Anyone with the pinned trees can reproduce the\n\
         denominator byte-for-byte via `scripts/harvest_tex_corpus.py`."
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Baseline {
        Baseline {
            rules_version: 1,
            corpus_hash: "abc".to_owned(),
            franken_markdown_rev: "deadbeef".to_owned(),
            unique_total: 100,
            occurrence_total: 1000,
            parse_unique: 90,
            parse_occurrences: 950,
            layout_unique: 85,
            layout_occurrences: 920,
        }
    }

    #[test]
    fn tsv_round_trips() {
        let b = base();
        assert_eq!(Baseline::from_tsv(&b.to_tsv()), Some(b));
    }

    #[test]
    fn the_ratchet_fails_on_any_decrease() {
        // The deliberate-regression negative test: removing a construct
        // (coverage drops) must fail.
        let b = base();
        let mut worse = base();
        worse.parse_occurrences -= 1;
        let v = ratchet_violations(&b, &worse);
        assert_eq!(v.len(), 1);
        assert!(v[0].contains("parse_occurrences regressed"));
        let mut worse = base();
        worse.layout_unique -= 5;
        assert_eq!(ratchet_violations(&b, &worse).len(), 1);
    }

    #[test]
    fn the_ratchet_passes_on_advance_or_equality() {
        let b = base();
        assert!(ratchet_violations(&b, &b).is_empty());
        let mut better = base();
        better.layout_occurrences += 10;
        assert!(ratchet_violations(&b, &better).is_empty());
    }

    #[test]
    fn a_rules_version_change_resets_comparability() {
        let b = base();
        let mut restated = base();
        restated.rules_version = 2;
        restated.parse_occurrences = 0;
        assert!(
            ratchet_violations(&b, &restated).is_empty(),
            "a restated denominator annotates the chart instead of failing"
        );
    }

    #[test]
    fn percentages_derive_from_counts() {
        let [po, pu, lo, lu] = base().percentages();
        assert!((po - 95.0).abs() < 1e-9);
        assert!((pu - 90.0).abs() < 1e-9);
        assert!((lo - 92.0).abs() < 1e-9);
        assert!((lu - 85.0).abs() < 1e-9);
    }

    #[test]
    fn dashboard_renders_numbers_and_never_strings() {
        let d = render_dashboard(
            &base(),
            &[Pending {
                construct: "\\substack".to_owned(),
                occurrences: 5,
                tracked: "fm-j5t".to_owned(),
            }],
            &[],
            &[base()],
        );
        assert!(d.contains("95.000 %"));
        assert!(d.contains("\\substack"));
        assert!(d.contains("escalation path"));
    }
}
