//! The governed-closure audit (D1, fm-g2c): parse the workspace's
//! `Cargo.lock` and the committed `SUITE_ALLOWLIST.tsv`, and refuse any
//! package the allowlist does not admit — the CI teeth behind "no new
//! unreviewed direct runtime dependencies".
//!
//! Both parsers are hand-rolled over line formats (no TOML/serde — the
//! auditor of the closure cannot itself expand the closure). `Cargo.lock`
//! is stable, line-oriented TOML the subset parser below covers fully:
//! `[[package]]` blocks of `key = "value"` lines.

/// One package as recorded in `Cargo.lock`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockedPackage {
    pub name: String,
    pub version: String,
    /// `None` for workspace path members (Cargo omits `source`).
    pub source: Option<String>,
    /// `None` for path members (Cargo omits `checksum`).
    pub checksum: Option<String>,
}

/// Parse `Cargo.lock` text into its package list.
#[must_use]
pub fn parse_cargo_lock(text: &str) -> Vec<LockedPackage> {
    let mut packages = Vec::new();
    let mut current: Option<LockedPackage> = None;
    for line in text.lines() {
        let line = line.trim();
        if line == "[[package]]" {
            if let Some(package) = current.take() {
                packages.push(package);
            }
            current = Some(LockedPackage {
                name: String::new(),
                version: String::new(),
                source: None,
                checksum: None,
            });
            continue;
        }
        let Some(package) = current.as_mut() else {
            continue;
        };
        let Some((key, value)) = line.split_once(" = ") else {
            continue;
        };
        let value = value.trim_matches('"').to_string();
        match key {
            "name" => package.name = value,
            "version" => package.version = value,
            "source" => package.source = Some(value),
            "checksum" => package.checksum = Some(value),
            _ => {}
        }
    }
    if let Some(package) = current.take() {
        packages.push(package);
    }
    packages
}

/// One row of the allowlist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowRow {
    pub name: String,
    pub version: String,
    pub source: String,
    pub checksum: String,
    pub class: String,
}

/// Parse `SUITE_ALLOWLIST.tsv` (comments and blanks skipped).
#[must_use]
pub fn parse_allowlist(text: &str) -> Vec<AllowRow> {
    text.lines()
        .filter(|line| !line.trim_start().starts_with('#') && !line.trim().is_empty())
        .filter_map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            if fields.len() < 12 {
                return None;
            }
            Some(AllowRow {
                name: fields[0].to_string(),
                version: fields[1].to_string(),
                source: fields[2].to_string(),
                checksum: fields[3].to_string(),
                class: fields[11].to_string(),
            })
        })
        .collect()
}

/// A closure violation, ready for a CI failure message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Violation {
    /// A locked package with no allowlist row.
    Unlisted { name: String, version: String },
    /// A locked package whose recorded checksum differs from the
    /// allowlist's.
    ChecksumMismatch {
        name: String,
        locked: String,
        allowed: String,
    },
    /// An allowlist row marked as consumed (non-pending, non-workspace)
    /// that no locked package matches — a stale row is an audit smell.
    StaleRow { name: String },
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unlisted { name, version } => write!(
                f,
                "package `{name} {version}` is in Cargo.lock but NOT in SUITE_ALLOWLIST.tsv — \
                 the governed closure (D1) admits nothing unlisted"
            ),
            Self::ChecksumMismatch {
                name,
                locked,
                allowed,
            } => write!(
                f,
                "package `{name}` checksum drift: lock has {locked}, allowlist has {allowed}"
            ),
            Self::StaleRow { name } => write!(
                f,
                "allowlist row `{name}` is marked consumed but absent from Cargo.lock — \
                 remove or re-class the row deliberately"
            ),
        }
    }
}

/// Audit the lock against the allowlist. Empty result = the closure is
/// exactly the governed universe.
#[must_use]
pub fn audit(lock: &[LockedPackage], allowlist: &[AllowRow]) -> Vec<Violation> {
    let mut violations = Vec::new();
    for package in lock {
        match allowlist.iter().find(|row| row.name == package.name) {
            None => violations.push(Violation::Unlisted {
                name: package.name.clone(),
                version: package.version.clone(),
            }),
            Some(row) => {
                let locked_sum = package.checksum.as_deref().unwrap_or("-");
                if row.checksum != "TBD" && row.checksum != locked_sum {
                    violations.push(Violation::ChecksumMismatch {
                        name: package.name.clone(),
                        locked: locked_sum.to_string(),
                        allowed: row.checksum.clone(),
                    });
                }
            }
        }
    }
    for row in allowlist {
        let consumed_class = row.class != "pending";
        if consumed_class && !lock.iter().any(|p| p.name == row.name) {
            violations.push(Violation::StaleRow {
                name: row.name.clone(),
            });
        }
    }
    violations
}
