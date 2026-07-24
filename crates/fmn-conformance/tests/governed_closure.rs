//! The governed-closure CI gate (D1, fm-g2c): the real workspace lock
//! must audit clean against the committed allowlist, SUITE.lock must
//! parse and agree with the pinned toolchain, and an injected unlisted
//! package must be caught (the negative test the bead demands).

use fmn_conformance::closure::{
    Violation, audit, audit_with_aux, parse_allowlist, parse_cargo_lock,
};

fn repo_file(name: &str) -> String {
    let path = format!("{}/../../{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {path}: {e}"))
}

/// Committed lockfiles of governed NON-member crates (ADR-0003: the fuzz
/// harness, non-member spikes). Their packages carry class=dev/fuzz rows;
/// the audit walks each lock that exists.
const AUX_LOCKS: &[&str] = &["spikes/g0-5-python-ext/Cargo.lock", "fuzz/Cargo.lock"];

#[test]
fn workspace_closure_is_exactly_the_governed_universe() {
    let lock = parse_cargo_lock(&repo_file("Cargo.lock"));
    assert!(
        lock.len() >= 20,
        "lock parser found only {} packages — parser or lock broken",
        lock.len()
    );
    let allowlist = parse_allowlist(&repo_file("SUITE_ALLOWLIST.tsv"));
    let aux: Vec<_> = AUX_LOCKS
        .iter()
        .filter_map(|name| {
            let path = format!("{}/../../{name}", env!("CARGO_MANIFEST_DIR"));
            std::fs::read_to_string(path).ok()
        })
        .map(|text| parse_cargo_lock(&text))
        .collect();
    assert!(
        !aux.is_empty(),
        "the G0-5 spike lock must exist and be committed (fm-87q)"
    );
    let violations = audit_with_aux(&lock, &aux, &allowlist);
    assert!(
        violations.is_empty(),
        "governed-closure violations:\n{}",
        violations
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn injected_unlisted_package_is_caught() {
    // The negative test: a package smuggled into the lock must fail loud.
    let mut lock_text = repo_file("Cargo.lock");
    lock_text.push_str(
        "\n[[package]]\nname = \"smuggled-dep\"\nversion = \"1.2.3\"\n\
         source = \"registry+https://github.com/rust-lang/crates.io-index\"\n\
         checksum = \"deadbeef\"\n",
    );
    let lock = parse_cargo_lock(&lock_text);
    let allowlist = parse_allowlist(&repo_file("SUITE_ALLOWLIST.tsv"));
    let violations = audit(&lock, &allowlist);
    assert!(
        violations.iter().any(|v| matches!(
            v,
            Violation::Unlisted { name, .. } if name == "smuggled-dep"
        )),
        "the smuggled package was not flagged: {violations:?}"
    );
}

#[test]
fn checksum_drift_is_caught() {
    let lock = parse_cargo_lock(
        "[[package]]\nname = \"pyo3\"\nversion = \"0.23.0\"\n\
         source = \"registry+x\"\nchecksum = \"aaaa\"\n",
    );
    // A consumed row with a pinned checksum that differs must flag.
    let allowlist = parse_allowlist(
        "pyo3\t0.23.0\tcrates-io\tbbbb\tx\tMIT\tyes\tyes\tpython\tno\texpansion\tffi\treason\tfm\tpolicy\n",
    );
    let violations = audit(&lock, &allowlist);
    assert!(
        violations
            .iter()
            .any(|v| matches!(v, Violation::ChecksumMismatch { name, .. } if name == "pyo3")),
        "checksum drift not flagged: {violations:?}"
    );
    // TBD checksums (pending rows) do not flag — consumption fills them.
    let pending = parse_allowlist(
        "pyo3\t0.23.0\tcrates-io\tTBD\tx\tMIT\tyes\tyes\tpython\tno\texpansion\tpending\treason\tfm\tpolicy\n",
    );
    assert!(audit(&lock, &pending).is_empty());
}

#[test]
fn stale_consumed_rows_are_caught() {
    let lock = parse_cargo_lock("[[package]]\nname = \"fmn-core\"\nversion = \"0.1.0\"\n");
    let allowlist = parse_allowlist(
        "fmn-core\t0.1.0\tworkspace\t-\t-\tMIT\tno\tno\tno\tno\tforbid\tworkspace\tsubstrate\tfm\tw\n\
         ghost-crate\t1.0\tcrates-io\tcccc\t-\tMIT\tno\tno\tno\tno\tpending\truntime\tgone\tfm\tw\n",
    );
    let violations = audit(&lock, &allowlist);
    assert!(
        violations
            .iter()
            .any(|v| matches!(v, Violation::StaleRow { name } if name == "ghost-crate")),
        "stale consumed row not flagged: {violations:?}"
    );
}

#[test]
fn git_dependencies_ride_their_suite_lock_pins() {
    // ADR-0004: every foundation crate consumed as a git dependency must
    // resolve to exactly the commit its repo's SUITE.lock row pins — the
    // lock is the single authority, and a drifted rev is a closure
    // violation even if the allowlist row matches.
    let suite_lock = repo_file("SUITE.lock");
    let pin_for = |repo: &str| -> String {
        suite_lock
            .lines()
            .find(|l| l.starts_with(&format!("{repo}\t")))
            .and_then(|l| l.split('\t').nth(1))
            .unwrap_or_else(|| panic!("SUITE.lock must pin {repo}"))
            .to_string()
    };
    let cargo_lock = repo_file("Cargo.lock");
    // (git-dep package name, owning repo in SUITE.lock)
    let git_deps = [("fmd-font", "franken_markdown")];
    for (pkg, repo) in git_deps {
        let pin = pin_for(repo);
        let mut found = false;
        let mut lines = cargo_lock.lines();
        while let Some(line) = lines.next() {
            if line.trim() == format!("name = \"{pkg}\"") {
                // The package's source line follows within its block.
                for follow in lines.by_ref() {
                    if follow.starts_with("[[package]]") {
                        break;
                    }
                    if let Some(source) = follow.trim().strip_prefix("source = \"") {
                        assert!(
                            source.starts_with("git+"),
                            "{pkg}: expected a git source, got {source}"
                        );
                        let resolved = source
                            .split('#')
                            .nth(1)
                            .map(|s| s.trim_end_matches('"'))
                            .unwrap_or("");
                        assert_eq!(
                            resolved, pin,
                            "{pkg}: Cargo.lock resolves {resolved} but SUITE.lock pins {repo} at {pin} — run the §6 upgrade ritual"
                        );
                        found = true;
                        break;
                    }
                }
            }
        }
        assert!(found, "{pkg} not found in Cargo.lock with a source line");
    }
}

#[test]
fn suite_lock_parses_and_matches_the_pinned_toolchain() {
    let suite_lock = repo_file("SUITE.lock");
    // The rustc pin must equal rust-toolchain.toml's channel — one truth.
    let rustc_line = suite_lock
        .lines()
        .find(|l| l.starts_with("rustc\t"))
        .expect("SUITE.lock must pin rustc");
    let pinned = rustc_line.split('\t').nth(1).expect("rustc pin value");
    let toolchain = repo_file("rust-toolchain.toml");
    assert!(
        toolchain.contains(&format!("channel = \"{pinned}\"")),
        "SUITE.lock rustc pin `{pinned}` disagrees with rust-toolchain.toml"
    );
    // Every [repos] row pins a full 40-hex commit.
    let mut in_repos = false;
    let mut repo_rows = 0;
    for line in suite_lock.lines() {
        if line.starts_with('[') {
            in_repos = line == "[repos]";
            continue;
        }
        if in_repos && !line.starts_with('#') && !line.trim().is_empty() {
            let commit = line.split('\t').nth(1).unwrap_or("");
            assert_eq!(commit.len(), 40, "repo pin not a full commit: {line}");
            assert!(
                commit.chars().all(|c| c.is_ascii_hexdigit()),
                "repo pin not hex: {line}"
            );
            repo_rows += 1;
        }
    }
    assert_eq!(repo_rows, 7, "all seven foundation repos must be pinned");
}
