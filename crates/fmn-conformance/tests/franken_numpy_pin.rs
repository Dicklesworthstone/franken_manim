//! Permanent FrankenNumPy source-identity and RNG-closure checks.

#![forbid(unsafe_code)]

use std::{collections::BTreeMap, fs::File, io::Read, path::PathBuf};

const MAX_AUTHORITY_BYTES: u64 = 8 * 1024 * 1024;
const CONSUMED_FNP_PACKAGES: &[&str] = &[
    "fnp-dtype",
    "fnp-io",
    "fnp-ndarray",
    "fnp-random-core",
];

fn repo_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").join(name)
}

fn read_repo_file(name: &str) -> String {
    let path = repo_path(name);
    let file = File::open(&path)
        .unwrap_or_else(|error| std::panic::panic_any(format!("opening {}: {error}", path.display())));
    let mut bytes = Vec::new();
    file.take(MAX_AUTHORITY_BYTES + 1)
        .read_to_end(&mut bytes)
        .unwrap_or_else(|error| {
            std::panic::panic_any(format!("reading {}: {error}", path.display()))
        });
    assert!(
        u64::try_from(bytes.len()).unwrap_or(u64::MAX) <= MAX_AUTHORITY_BYTES,
        "{} exceeds the {MAX_AUTHORITY_BYTES}-byte authority limit",
        path.display()
    );
    String::from_utf8(bytes).unwrap_or_else(|error| {
        std::panic::panic_any(format!("{} is not UTF-8: {error}", path.display()))
    })
}

fn suite_pin(document: &str, repo: &str) -> String {
    document
        .lines()
        .find(|line| line.starts_with(&format!("{repo}\t")))
        .and_then(|line| line.split('\t').nth(1))
        .filter(|pin| pin.len() == 40 && pin.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .unwrap_or_else(|| std::panic::panic_any(format!("SUITE.lock must pin {repo} at full SHA")))
        .to_owned()
}

fn package_blocks(lock: &str) -> BTreeMap<String, String> {
    let mut packages = BTreeMap::new();
    for block in lock.split("\n[[package]]\n").skip(1) {
        let name = block
            .lines()
            .find_map(|line| line.strip_prefix("name = \"")?.strip_suffix('"'))
            .unwrap_or_else(|| std::panic::panic_any("Cargo.lock package lacks a canonical name"));
        assert!(
            packages.insert(name.to_owned(), block.to_owned()).is_none(),
            "Cargo.lock contains duplicate package {name}"
        );
    }
    packages
}

fn source_from_block<'a>(name: &str, block: &'a str) -> &'a str {
    block
        .lines()
        .find_map(|line| line.strip_prefix("source = \"")?.strip_suffix('"'))
        .unwrap_or_else(|| std::panic::panic_any(format!("{name} lacks a source in Cargo.lock")))
}

fn dependencies_from_block(block: &str) -> Vec<&str> {
    let mut dependencies = Vec::new();
    let mut in_dependencies = false;
    for line in block.lines() {
        if line == "dependencies = [" {
            in_dependencies = true;
            continue;
        }
        if in_dependencies && line == "]" {
            break;
        }
        if in_dependencies {
            let dependency = line
                .trim()
                .strip_prefix('"')
                .and_then(|value| value.strip_suffix("\","))
                .unwrap_or_else(|| {
                    std::panic::panic_any(format!("malformed Cargo.lock dependency line {line:?}"))
                });
            dependencies.push(dependency);
        }
    }
    dependencies
}

#[test]
fn every_consumed_franken_numpy_package_resolves_to_the_suite_pin() {
    let suite = read_repo_file("SUITE.lock");
    let pin = suite_pin(&suite, "franken_numpy");
    let lock = read_repo_file("Cargo.lock");
    let packages = package_blocks(&lock);

    for package in CONSUMED_FNP_PACKAGES {
        let block = packages
            .get(*package)
            .unwrap_or_else(|| std::panic::panic_any(format!("Cargo.lock lacks {package}")));
        let source = source_from_block(package, block);
        assert!(
            source.starts_with("git+https://github.com/Dicklesworthstone/franken_numpy?rev="),
            "{package} is not sourced through the governed FrankenNumPy repository: {source}"
        );
        assert!(
            source.contains(&format!("?rev={pin}#")) && source.ends_with(&pin),
            "{package} resolves outside the FrankenNumPy SUITE.lock pin {pin}: {source}"
        );
    }
}

#[test]
fn fmn_core_consumes_only_the_dependency_free_rng_primitive() {
    let lock = read_repo_file("Cargo.lock");
    let packages = package_blocks(&lock);
    let core = packages
        .get("fmn-core")
        .unwrap_or_else(|| std::panic::panic_any("Cargo.lock lacks fmn-core"));
    let core_dependencies = dependencies_from_block(core);
    assert!(
        core_dependencies.iter().any(|dependency| *dependency == "fnp-random-core"),
        "fmn-core does not depend on fnp-random-core: {core_dependencies:?}"
    );
    for forbidden in ["fnp-random", "fnp-ndarray", "rayon", "getrandom 0.4.3"] {
        assert!(
            core_dependencies.iter().all(|dependency| *dependency != forbidden),
            "fmn-core directly depends on forbidden RNG-adjacent package {forbidden}: {core_dependencies:?}"
        );
    }
    assert!(
        !packages.contains_key("fnp-random"),
        "full fnp-random entered the native Cargo.lock instead of the dependency-free core"
    );

    let random_core = packages
        .get("fnp-random-core")
        .unwrap_or_else(|| std::panic::panic_any("Cargo.lock lacks fnp-random-core"));
    assert!(
        dependencies_from_block(random_core).is_empty(),
        "fnp-random-core is no longer dependency-free"
    );
}
