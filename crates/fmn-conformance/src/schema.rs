//! The one API schema — the single source of API truth (§16.2, fm-vn6).
//!
//! One machine-readable description of the surface (classes, methods,
//! parameters and their defaults, CLI flags, config keys) from which the
//! generated artifacts are produced: `fmn-config`'s typed extraction, the
//! Parity Ledger's rows (§16.1), the CLI flag table, and the docs. **Drift
//! between the front doors is a build error** — the generators are pure
//! functions of the schema, and `tests/api_schema.rs` regenerates every
//! artifact in-memory and fails on any byte of difference.
//!
//! # Two layers, deliberately kept apart
//!
//! * **Extracted** (`API_SCHEMA.tsv`) — mechanically derived from the pinned
//!   Reference by `scripts/gen_api_schema.py`; regenerable, never
//!   hand-edited. Regenerating it must never silently discard a ruling.
//! * **Authored** (`API_OVERLAY.tsv`) — C-9's canonical names, the §16.1
//!   semantic tiers, and the Rust bindings the generators need. Maintained by
//!   hand. Authoring a ruling must never require re-deriving 2276 rows of
//!   Reference surface.
//!
//! [`Schema::parse`] merges them into the *effective* schema, which is the
//! only thing generators ever see.
//!
//! # Why a section-tagged TSV
//!
//! The same reason `SUITE.lock` is one: the auditor of the closure cannot
//! itself expand the closure. A schema that needed a YAML or JSON crate to
//! read would put a dependency underneath the artifact that governs
//! dependencies. Sections are `[name]` on their own line; rows are
//! tab-separated; `#` comments and blank lines are skipped everywhere.

use std::collections::BTreeMap;
use std::fmt;
use std::fmt::Write as _;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// A schema file that could not be read as one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaError {
    /// A row carried the wrong number of tab-separated fields.
    Arity {
        /// The file the row came from.
        file: &'static str,
        /// 1-based line number.
        line: usize,
        /// The section the row was in.
        section: String,
        /// Fields the section requires.
        expected: usize,
        /// Fields the row actually had.
        found: usize,
    },
    /// A row appeared before any `[section]` header.
    Sectionless {
        /// The file the row came from.
        file: &'static str,
        /// 1-based line number.
        line: usize,
    },
    /// A field that must parse as a number or an enumerated word did not.
    Field {
        /// The file the row came from.
        file: &'static str,
        /// 1-based line number.
        line: usize,
        /// Which column.
        column: &'static str,
        /// What was there.
        found: String,
    },
    /// The overlay named a symbol, parameter, or config key the extracted
    /// layer does not have — the overlay has gone stale against the pin.
    DanglingOverlay {
        /// Which overlay section.
        section: &'static str,
        /// The key that resolved to nothing.
        key: String,
    },
    /// A config key exists with no binding, or a binding with no key.
    ConfigCoverage {
        /// Human-readable statement of the mismatch.
        detail: String,
    },
}

impl fmt::Display for SchemaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Arity {
                file,
                line,
                section,
                expected,
                found,
            } => write!(
                f,
                "{file}:{line}: [{section}] row has {found} fields, expected {expected}"
            ),
            Self::Sectionless { file, line } => {
                write!(f, "{file}:{line}: row before any [section] header")
            }
            Self::Field {
                file,
                line,
                column,
                found,
            } => write!(f, "{file}:{line}: bad `{column}` field: {found:?}"),
            Self::DanglingOverlay { section, key } => write!(
                f,
                "API_OVERLAY.tsv [{section}] names `{key}`, which API_SCHEMA.tsv does not \
                 contain — the overlay is stale against the Reference pin; \
                 rerun scripts/gen_api_schema.py and reconcile"
            ),
            Self::ConfigCoverage { detail } => write!(f, "config-key coverage: {detail}"),
        }
    }
}

impl std::error::Error for SchemaError {}

// ---------------------------------------------------------------------------
// The sectioned-TSV reader
// ---------------------------------------------------------------------------

/// One parsed row, kept with its provenance so every later failure can name
/// the line it came from.
#[derive(Debug, Clone)]
struct Row {
    line: usize,
    fields: Vec<String>,
}

/// Sections in file order, each a list of rows.
#[derive(Debug, Default)]
struct Sections {
    file: &'static str,
    map: BTreeMap<String, Vec<Row>>,
}

impl Sections {
    fn parse(file: &'static str, text: &str) -> Result<Self, SchemaError> {
        let mut out = Self {
            file,
            map: BTreeMap::new(),
        };
        let mut current: Option<String> = None;
        for (index, raw) in text.lines().enumerate() {
            let line = index + 1;
            let trimmed = raw.trim_end();
            if trimmed.trim().is_empty() || trimmed.trim_start().starts_with('#') {
                continue;
            }
            if let Some(name) = trimmed
                .trim()
                .strip_prefix('[')
                .and_then(|s| s.strip_suffix(']'))
            {
                current = Some(name.to_owned());
                out.map.entry(name.to_owned()).or_default();
                continue;
            }
            let Some(section) = current.clone() else {
                return Err(SchemaError::Sectionless { file, line });
            };
            out.map.entry(section).or_default().push(Row {
                line,
                fields: trimmed.split('\t').map(str::to_owned).collect(),
            });
        }
        Ok(out)
    }

    fn rows(&self, section: &str) -> &[Row] {
        self.map.get(section).map_or(&[], Vec::as_slice)
    }

    /// Rows of `section`, each checked to have exactly `arity` fields.
    fn typed(&self, section: &str, arity: usize) -> Result<Vec<&Row>, SchemaError> {
        let mut out = Vec::new();
        for row in self.rows(section) {
            if row.fields.len() != arity {
                return Err(SchemaError::Arity {
                    file: self.file,
                    line: row.line,
                    section: section.to_owned(),
                    expected: arity,
                    found: row.fields.len(),
                });
            }
            out.push(row);
        }
        Ok(out)
    }

    /// `key\tvalue` rows of a `[meta]`-shaped section.
    fn meta(&self, section: &str) -> BTreeMap<String, String> {
        self.rows(section)
            .iter()
            .filter(|r| r.fields.len() == 2)
            .map(|r| (r.fields[0].clone(), r.fields[1].clone()))
            .collect()
    }
}

/// The TSV placeholder for "no value" — chosen over an empty field so a row's
/// arity is visible to the eye and a trailing tab cannot be lost by an editor.
const NONE: &str = "-";

fn opt(field: &str) -> Option<&str> {
    if field == NONE { None } else { Some(field) }
}

// ---------------------------------------------------------------------------
// The model
// ---------------------------------------------------------------------------

/// What a symbol is on the Reference's public surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SymbolKind {
    /// A class.
    Class,
    /// A method on a class (including `__init__`).
    Method,
    /// A `@property` on a class.
    Property,
    /// A class-level attribute — public surface, since the Reference
    /// configures whole lineages by overriding them.
    Attribute,
    /// A module-level function.
    Function,
    /// A module-level constant.
    Constant,
    /// A name the wildcard surface binds that the package never defines —
    /// `np`, `math`, `moderngl`. Real exports, whether or not anyone wanted
    /// them (§1.6).
    LeakedImport,
}

impl SymbolKind {
    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "class" => Self::Class,
            "method" => Self::Method,
            "property" => Self::Property,
            "attribute" => Self::Attribute,
            "function" => Self::Function,
            "constant" => Self::Constant,
            "leaked_import" => Self::LeakedImport,
            _ => return None,
        })
    }

    /// The word used in the schema file and in generated artifacts.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Class => "class",
            Self::Method => "method",
            Self::Property => "property",
            Self::Attribute => "attribute",
            Self::Function => "function",
            Self::Constant => "constant",
            Self::LeakedImport => "leaked_import",
        }
    }
}

/// How a parameter may be passed. Reproduced exactly by the Python front
/// door: a Reference parameter that is positional-or-keyword must not become
/// keyword-only in `fmn-python`, or source-unedited scenes break.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamKind {
    /// Before a `/`.
    PositionalOnly,
    /// The ordinary kind.
    PositionalOrKeyword,
    /// `*args`.
    VarPositional,
    /// After a `*`.
    KeywordOnly,
    /// `**kwargs`.
    VarKeyword,
}

impl ParamKind {
    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "positional_only" => Self::PositionalOnly,
            "positional_or_keyword" => Self::PositionalOrKeyword,
            "var_positional" => Self::VarPositional,
            "keyword_only" => Self::KeywordOnly,
            "var_keyword" => Self::VarKeyword,
            _ => return None,
        })
    }

    /// The word used in the schema file.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PositionalOnly => "positional_only",
            Self::PositionalOrKeyword => "positional_or_keyword",
            Self::VarPositional => "var_positional",
            Self::KeywordOnly => "keyword_only",
            Self::VarKeyword => "var_keyword",
        }
    }
}

/// One symbol of the Reference's surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    /// Defining module, dotted.
    pub module: String,
    /// `Name`, or `Class.member` for class members.
    pub name: String,
    /// What it is.
    pub kind: SymbolKind,
    /// `defined`, or the module a leaked import came from.
    pub origin: String,
    /// Whether `from manimlib import *` binds this name.
    pub exported: bool,
    /// Bases for a class, default expression for a constant or attribute.
    pub detail: Option<String>,
}

impl Symbol {
    /// `module:name` — the key the overlay addresses symbols by.
    #[must_use]
    pub fn key(&self) -> String {
        format!("{}:{}", self.module, self.name)
    }
}

/// One parameter of one callable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    /// `module:Class.method` or `module:function`.
    pub owner: String,
    /// Position in the declaration.
    pub ordinal: u32,
    /// Parameter name as the Reference spells it.
    pub name: String,
    /// How it may be passed.
    pub kind: ParamKind,
    /// Type annotation source text, if any.
    pub annotation: Option<String>,
    /// Default *expression* source text, if any. Never an evaluated value:
    /// most Reference defaults name module constants (`ORIGIN`, `TAU / 4`)
    /// that only exist once the package is imported.
    pub default: Option<String>,
}

/// One `parser.add_argument` of the Reference's CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flag {
    /// Comma-joined option strings, e.g. `-w,--write_file`.
    pub options: String,
    /// argparse `dest`, if given explicitly.
    pub dest: Option<String>,
    /// argparse action (`store`, `store_true`, ...).
    pub action: String,
    /// argparse `nargs`, if given.
    pub nargs: Option<String>,
    /// Default expression, if given.
    pub default: Option<String>,
    /// `type=` callable, if given.
    pub ty: Option<String>,
    /// Help text.
    pub help: Option<String>,
}

/// The YAML shape a config value takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
    /// `True` / `False`.
    Bool,
    /// An integer scalar.
    Int,
    /// A floating scalar.
    Float,
    /// A quoted or bare string.
    Str,
    /// A tuple-string like `(1920, 1080)`.
    Tuple,
    /// A parent key. Some parents ARE the key — `colors`, `key_bindings`,
    /// and `directories.subdirs` are open, user-extensible maps bound as a
    /// whole rather than child by child.
    Map,
}

impl ValueKind {
    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "bool" => Self::Bool,
            "int" => Self::Int,
            "float" => Self::Float,
            "string" => Self::Str,
            "tuple" => Self::Tuple,
            "map" => Self::Map,
            _ => return None,
        })
    }
}

/// One config key of the shipped defaults document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigKey {
    /// Dotted path into the document.
    pub path: String,
    /// The value's YAML shape.
    pub kind: ValueKind,
    /// The default literal as written. `None` for an optional key, which has
    /// no default by definition, and for a parent.
    pub default: Option<String>,
    /// Whether the Reference's own `default_config.yml` also has this key.
    pub in_reference: bool,
}

/// A C-9 canonical-name ruling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rename {
    /// `module:Name` of the Reference symbol.
    pub symbol: String,
    /// The name the Rust front door and the docs use.
    pub canonical: String,
    /// The Appendix C row this ruling comes from.
    pub ruling: String,
    /// Free-text note.
    pub note: Option<String>,
}

impl Rename {
    /// The Reference's own spelling — the alias `fmn-python` must also bind.
    #[must_use]
    pub fn reference_name(&self) -> &str {
        self.symbol
            .rsplit_once(':')
            .map_or(self.symbol.as_str(), |(_, name)| name)
            .rsplit_once('.')
            .map_or_else(
                || self.symbol.rsplit_once(':').map_or("", |(_, n)| n),
                |(_, leaf)| leaf,
            )
    }
}

/// A parameter-level canonical-name ruling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamRename {
    /// The callable the parameter belongs to.
    pub owner: String,
    /// The Reference's spelling.
    pub reference_name: String,
    /// The canonical spelling.
    pub canonical: String,
    /// The Appendix C row.
    pub ruling: String,
}

/// One config key's binding into the Rust config types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    /// Dotted config path.
    pub path: String,
    /// The Rust struct that carries it.
    pub struct_name: String,
    /// The field within that struct.
    pub field: String,
    /// The typed accessor on `Cx` that extracts it.
    pub accessor: String,
}

impl Binding {
    /// The `Config` field holding this key's struct: the path's first
    /// segment for a nested key, the path itself for a top-level one.
    /// Derived rather than stored, so it cannot disagree with the path.
    #[must_use]
    pub fn outer_field(&self) -> &str {
        self.path.split('.').next().unwrap_or(&self.path)
    }

    /// Whether this key lives directly on `Config`.
    #[must_use]
    pub fn is_top_level(&self) -> bool {
        self.struct_name == "Config"
    }
}

/// The §16.1 semantic tier of one symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Same behavior as the Reference.
    Same,
    /// Deliberately better; carries a Behavior Note.
    Improved,
    /// Reduced scope, with a recorded reason.
    Tiered,
    /// Not offered, with a recorded reason.
    Excluded,
    /// Nobody has adjudicated this symbol yet. The honest default, and the
    /// number the Parity Ledger ratchets down.
    Unreviewed,
}

impl Status {
    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "same" => Self::Same,
            "improved" => Self::Improved,
            "tiered" => Self::Tiered,
            "excluded" => Self::Excluded,
            "unreviewed" => Self::Unreviewed,
            _ => return None,
        })
    }

    /// The word used in the schema file and the Ledger.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Same => "same",
            Self::Improved => "improved",
            Self::Tiered => "tiered",
            Self::Excluded => "excluded",
            Self::Unreviewed => "unreviewed",
        }
    }
}

/// The effective schema: the extracted layer with the authored layer applied.
#[derive(Debug, Clone, Default)]
pub struct Schema {
    /// `[meta]` of the extracted layer (schema version, Reference commit).
    pub meta: BTreeMap<String, String>,
    /// Every symbol of the Reference's surface.
    pub symbols: Vec<Symbol>,
    /// Every parameter of every callable.
    pub params: Vec<Param>,
    /// The Reference's CLI flag surface.
    pub flags: Vec<Flag>,
    /// The shipped config-key surface.
    pub config: Vec<ConfigKey>,
    /// C-9 canonical-name rulings, by `module:Name`.
    pub renames: BTreeMap<String, Rename>,
    /// C-9 parameter renames.
    pub param_renames: Vec<ParamRename>,
    /// Config-key bindings, in emission order.
    pub bindings: Vec<Binding>,
    /// Adjudicated semantic tiers, by `module:Name`.
    pub statuses: BTreeMap<String, (Status, String)>,
}

impl Schema {
    /// Parse and merge the two layers.
    ///
    /// # Errors
    /// [`SchemaError`] naming the file and line for a malformed row, or the
    /// dangling key for an overlay that has gone stale against the pin.
    pub fn parse(extracted: &str, overlay: &str) -> Result<Self, SchemaError> {
        let ex = Sections::parse("API_SCHEMA.tsv", extracted)?;
        let ov = Sections::parse("API_OVERLAY.tsv", overlay)?;
        let mut schema = Self {
            meta: ex.meta("meta"),
            ..Self::default()
        };

        for row in ex.typed("symbols", 6)? {
            let f = &row.fields;
            schema.symbols.push(Symbol {
                module: f[0].clone(),
                name: f[1].clone(),
                kind: SymbolKind::parse(&f[2]).ok_or_else(|| SchemaError::Field {
                    file: ex.file,
                    line: row.line,
                    column: "kind",
                    found: f[2].clone(),
                })?,
                origin: f[3].clone(),
                exported: f[4] == "1",
                detail: opt(&f[5]).map(str::to_owned),
            });
        }

        for row in ex.typed("params", 6)? {
            let f = &row.fields;
            schema.params.push(Param {
                owner: f[0].clone(),
                ordinal: f[1].parse().map_err(|_| SchemaError::Field {
                    file: ex.file,
                    line: row.line,
                    column: "ordinal",
                    found: f[1].clone(),
                })?,
                name: f[2].clone(),
                kind: ParamKind::parse(&f[3]).ok_or_else(|| SchemaError::Field {
                    file: ex.file,
                    line: row.line,
                    column: "kind",
                    found: f[3].clone(),
                })?,
                annotation: opt(&f[4]).map(str::to_owned),
                default: opt(&f[5]).map(str::to_owned),
            });
        }

        for row in ex.typed("flags", 7)? {
            let f = &row.fields;
            schema.flags.push(Flag {
                options: f[0].clone(),
                dest: opt(&f[1]).map(str::to_owned),
                action: f[2].clone(),
                nargs: opt(&f[3]).map(str::to_owned),
                default: opt(&f[4]).map(str::to_owned),
                ty: opt(&f[5]).map(str::to_owned),
                help: opt(&f[6]).map(str::to_owned),
            });
        }

        for row in ex.typed("config", 4)? {
            let f = &row.fields;
            schema.config.push(ConfigKey {
                path: f[0].clone(),
                kind: ValueKind::parse(&f[1]).ok_or_else(|| SchemaError::Field {
                    file: ex.file,
                    line: row.line,
                    column: "kind",
                    found: f[1].clone(),
                })?,
                default: opt(&f[2]).map(str::to_owned),
                in_reference: f[3] == "1",
            });
        }

        let symbol_keys: std::collections::BTreeSet<String> =
            schema.symbols.iter().map(Symbol::key).collect();

        for row in ov.typed("canonical", 4)? {
            let f = &row.fields;
            if !symbol_keys.contains(&f[0]) {
                return Err(SchemaError::DanglingOverlay {
                    section: "canonical",
                    key: f[0].clone(),
                });
            }
            schema.renames.insert(
                f[0].clone(),
                Rename {
                    symbol: f[0].clone(),
                    canonical: f[1].clone(),
                    ruling: f[2].clone(),
                    note: opt(&f[3]).map(str::to_owned),
                },
            );
        }

        for row in ov.typed("param_canonical", 4)? {
            let f = &row.fields;
            let known = schema
                .params
                .iter()
                .any(|p| p.owner == f[0] && p.name == f[1]);
            if !known {
                return Err(SchemaError::DanglingOverlay {
                    section: "param_canonical",
                    key: format!("{}#{}", f[0], f[1]),
                });
            }
            schema.param_renames.push(ParamRename {
                owner: f[0].clone(),
                reference_name: f[1].clone(),
                canonical: f[2].clone(),
                ruling: f[3].clone(),
            });
        }

        for row in ov.typed("optional_config", 3)? {
            let f = &row.fields;
            schema.config.push(ConfigKey {
                path: f[0].clone(),
                kind: ValueKind::parse(&f[1]).ok_or_else(|| SchemaError::Field {
                    file: ov.file,
                    line: row.line,
                    column: "kind",
                    found: f[1].clone(),
                })?,
                default: None,
                in_reference: f[2] == "1",
            });
        }

        for row in ov.typed("config_binding", 4)? {
            let f = &row.fields;
            schema.bindings.push(Binding {
                path: f[0].clone(),
                struct_name: f[1].clone(),
                field: f[2].clone(),
                accessor: f[3].clone(),
            });
        }

        for row in ov.typed("status", 3)? {
            let f = &row.fields;
            if !symbol_keys.contains(&f[0]) {
                return Err(SchemaError::DanglingOverlay {
                    section: "status",
                    key: f[0].clone(),
                });
            }
            let status = Status::parse(&f[1]).ok_or_else(|| SchemaError::Field {
                file: ov.file,
                line: row.line,
                column: "status",
                found: f[1].clone(),
            })?;
            schema.statuses.insert(f[0].clone(), (status, f[2].clone()));
        }

        Ok(schema)
    }

    /// Every config key is bound exactly once, and every binding names a key.
    ///
    /// This is what makes the generated extraction trustworthy: a key added
    /// to `default_config.yml` without a binding, or a binding left behind by
    /// a removed key, fails here rather than producing code that silently
    /// ignores a knob.
    ///
    /// # Errors
    /// [`SchemaError::ConfigCoverage`] naming every offending path.
    pub fn check_config_coverage(&self) -> Result<(), SchemaError> {
        let keys: std::collections::BTreeSet<&str> =
            self.config.iter().map(|c| c.path.as_str()).collect();
        let mut bound = std::collections::BTreeSet::new();
        let mut problems = Vec::new();

        for binding in &self.bindings {
            if !bound.insert(binding.path.as_str()) {
                problems.push(format!("`{}` is bound twice", binding.path));
            }
        }
        // A map-valued key (`colors`, `key_bindings`, `directories.subdirs`)
        // is bound as a whole, so everything beneath it is covered by it. A
        // parent that is merely structural (`camera`) needs no binding of its
        // own — its children carry the values.
        for key in self.config.iter().filter(|c| c.kind != ValueKind::Map) {
            let path = key.path.as_str();
            let covered =
                bound.contains(path) || bound.iter().any(|b| path.starts_with(&format!("{b}.")));
            if !covered {
                problems.push(format!("config key `{path}` has no binding"));
            }
        }
        for binding in &bound {
            if !keys.contains(binding) {
                problems.push(format!("binding `{binding}` names no config key"));
            }
        }

        if problems.is_empty() {
            Ok(())
        } else {
            Err(SchemaError::ConfigCoverage {
                detail: problems.join("; "),
            })
        }
    }

    /// Symbols the wildcard surface binds — the import-surface inventory
    /// §1.6 says the Ledger has to enumerate, because `manimlib` has no
    /// `__all__` to trust.
    #[must_use]
    pub fn exported(&self) -> Vec<&Symbol> {
        self.symbols.iter().filter(|s| s.exported).collect()
    }

    /// Symbols of one kind.
    #[must_use]
    pub fn of_kind(&self, kind: SymbolKind) -> Vec<&Symbol> {
        self.symbols.iter().filter(|s| s.kind == kind).collect()
    }

    /// The name the Rust front door and the docs use for a symbol: its
    /// canonical name where C-9 ruled one, otherwise the Reference's.
    #[must_use]
    pub fn canonical_name(&self, symbol: &Symbol) -> String {
        self.renames.get(&symbol.key()).map_or_else(
            || symbol.name.clone(),
            |rename| match symbol.name.rsplit_once('.') {
                Some((owner, _)) => format!("{owner}.{}", rename.canonical),
                None => rename.canonical.clone(),
            },
        )
    }

    /// The adjudicated tier of a symbol, defaulting to
    /// [`Status::Unreviewed`].
    #[must_use]
    pub fn status(&self, symbol: &Symbol) -> Status {
        self.statuses
            .get(&symbol.key())
            .map_or(Status::Unreviewed, |(status, _)| *status)
    }

    /// The Reference commit the extracted layer was taken from.
    #[must_use]
    pub fn reference_commit(&self) -> &str {
        self.meta
            .get("reference_commit")
            .map_or("unknown", String::as_str)
    }
}

// ---------------------------------------------------------------------------
// Generators
// ---------------------------------------------------------------------------

/// How a generated artifact spells a comment.
#[derive(Clone, Copy)]
enum Comment {
    /// One marker per line — Rust doc comments and TSV `#` headers.
    PerLine(&'static str),
    /// One `<!--` ... `-->` block, because Markdown has no line comment and
    /// repeating the opener per line produces a broken document that renders
    /// as literal text.
    Block,
}

/// Header stamped on every generated artifact, so a reader who finds one
/// without the schema in hand still knows not to hand-edit it.
fn banner(comment: Comment, artifact: &str, commit: &str) -> String {
    let body = format!(
        "{artifact}\n\
         \n\
         GENERATED from API_SCHEMA.tsv + API_OVERLAY.tsv by\n\
         fmn_conformance::schema — regenerate, never hand-edit.\n\
         Reference pin: {commit}\n\
         \n\
         Regenerate:  UPDATE_API_ARTIFACTS=1 cargo test -p fmn-conformance \\\n\
         \x20                --test api_schema\n"
    );
    match comment {
        Comment::PerLine(marker) => body
            .lines()
            .map(|line| {
                if line.is_empty() {
                    format!("{marker}\n")
                } else {
                    format!("{marker} {line}\n")
                }
            })
            .collect(),
        Comment::Block => format!("<!--\n{body}-->\n"),
    }
}

/// Generate `crates/fmn-config/src/generated.rs`: the typed extraction of the
/// config document, one `cx.<accessor>("<path>")?` per bound key.
///
/// W1CONF hand-wrote this body; generating it means the config surface has
/// exactly one definition (the schema) instead of two that can drift.
#[must_use]
pub fn generate_config_rs(schema: &Schema) -> String {
    let mut out = String::new();
    out.push_str(&banner(
        Comment::PerLine("//!"),
        "The typed config extraction (§16.2, fm-vn6).",
        schema.reference_commit(),
    ));
    out.push_str(
        "//!\n\
         //! The bound keys come from API_OVERLAY.tsv's `[config_binding]`\n\
         //! section; the accessors are `Cx`'s in `config.rs`. A key added to\n\
         //! `default_config.yml` without a binding fails the coverage check\n\
         //! before it can reach this file.\n\n",
    );
    // One `use` per line. A braced list would be repacked by rustfmt's width
    // algorithm, which the generator would then have to reimplement exactly
    // to stay byte-stable; separate statements are left alone.
    let mut structs: Vec<&str> = schema
        .bindings
        .iter()
        .map(|b| b.struct_name.as_str())
        .collect();
    structs.push("ConfigError");
    structs.push("Cx");
    structs.sort_unstable();
    structs.dedup();
    for name in &structs {
        let _ = writeln!(out, "use crate::config::{name};");
    }
    out.push_str("use crate::yaml::Value;\n\n");

    out.push_str(
        "/// Type a fully merged configuration document.\n\
         ///\n\
         /// # Errors\n\
         /// A [`ConfigError`] naming the key path and the expected-vs-found\n\
         /// shapes.\n\
         pub(crate) fn config_from_value(root: Value) -> Result<Config, ConfigError> {\n\
         \x20   let cx = Cx { root: &root };\n\
         \x20   let config = Config {\n",
    );

    let mut open: Option<&str> = None;
    for binding in &schema.bindings {
        if binding.is_top_level() {
            if open.is_some() {
                out.push_str("        },\n");
                open = None;
            }
            let _ = writeln!(
                out,
                "        {}: cx.{}(\"{}\")?,",
                binding.field, binding.accessor, binding.path
            );
            continue;
        }
        if open != Some(binding.struct_name.as_str()) {
            if open.is_some() {
                out.push_str("        },\n");
            }
            let _ = writeln!(
                out,
                "        {}: {} {{",
                binding.outer_field(),
                binding.struct_name
            );
            open = Some(binding.struct_name.as_str());
        }
        let _ = writeln!(
            out,
            "            {}: cx.{}(\"{}\")?,",
            binding.field, binding.accessor, binding.path
        );
    }
    if open.is_some() {
        out.push_str("        },\n");
    }

    out.push_str(
        "        // Placed below, once the borrows of `root` have ended.\n\
         \x20       raw: Value::Null,\n\
         \x20   };\n\
         \x20   Ok(Config {\n\
         \x20       raw: root,\n\
         \x20       ..config\n\
         \x20   })\n\
         }\n",
    );
    out
}

/// Generate the Parity Ledger's rows (§16.1): one row per symbol, carrying
/// the canonical name, the wildcard-export flag, and the semantic tier.
///
/// fm-iz4 owns the Ledger itself; this is the machine-generated substrate it
/// consumes, so the Ledger can never disagree with the schema about what the
/// surface *is*.
#[must_use]
pub fn generate_ledger_tsv(schema: &Schema) -> String {
    let mut out = banner(
        Comment::PerLine("#"),
        "The Parity Ledger (§16.1).",
        schema.reference_commit(),
    );
    out.push_str(
        "#\n\
         # module\tsymbol\tcanonical\tkind\texported\tstatus\tevidence\n",
    );
    let mut rows: Vec<String> = schema
        .symbols
        .iter()
        .map(|symbol| {
            let (status, evidence) = schema
                .statuses
                .get(&symbol.key())
                .map_or((Status::Unreviewed, NONE), |(s, e)| (*s, e.as_str()));
            format!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                symbol.module,
                symbol.name,
                schema.canonical_name(symbol),
                symbol.kind.as_str(),
                u8::from(symbol.exported),
                status.as_str(),
                evidence
            )
        })
        .collect();
    rows.sort_unstable();
    for row in rows {
        out.push_str(&row);
        out.push('\n');
    }
    out
}

/// Generate the CLI flag table (§13.6) — W9's normative source for the flag
/// surface it keeps "where it still means something".
#[must_use]
pub fn generate_cli_table_md(schema: &Schema) -> String {
    let mut out = banner(
        Comment::Block,
        "The CLI flag table (§13.6).",
        schema.reference_commit(),
    );
    out.push_str("\n# The `fmn` flag surface\n\n");
    let _ = writeln!(
        out,
        "The Reference's `manimlib/config.py` declares {} options. Each is kept, \
         re-specified, or dropped by W9 (fm-c53); this table is the inventory that \
         decision is made against, and it is generated, so the inventory cannot \
         quietly shrink.\n",
        schema.flags.len()
    );
    out.push_str("| Options | Action | Default | Help |\n|---|---|---|---|\n");
    for flag in &schema.flags {
        let _ = writeln!(
            out,
            "| `{}` | {} | {} | {} |",
            flag.options,
            flag.action,
            flag.default.as_deref().unwrap_or("—"),
            flag.help.as_deref().unwrap_or("—")
        );
    }
    out
}

/// Generate the schema's human-facing summary — the docs artifact of §16.2,
/// and the place the import-surface and C-9 numbers are published.
#[must_use]
pub fn generate_docs_md(schema: &Schema) -> String {
    let mut out = banner(
        Comment::Block,
        "The API schema summary (§16.2).",
        schema.reference_commit(),
    );
    out.push_str("\n# The one API schema\n\n");
    let _ = writeln!(
        out,
        "Generated from `API_SCHEMA.tsv` (extracted from the pinned Reference) and \
         `API_OVERLAY.tsv` (authored). Reference pin `{}`.\n",
        schema.reference_commit()
    );

    out.push_str("## Surface inventory\n\n| Kind | Total | Wildcard-exported |\n|---|---|---|\n");
    for kind in [
        SymbolKind::Class,
        SymbolKind::Method,
        SymbolKind::Property,
        SymbolKind::Attribute,
        SymbolKind::Function,
        SymbolKind::Constant,
        SymbolKind::LeakedImport,
    ] {
        let all = schema.of_kind(kind);
        let exported = all.iter().filter(|s| s.exported).count();
        let _ = writeln!(out, "| {} | {} | {} |", kind.as_str(), all.len(), exported);
    }

    let _ = writeln!(
        out,
        "\n`from manimlib import *` binds {} names. The Reference declares no \
         `__all__` (§1.6), so that number is the *computed* wildcard closure, \
         leaked third-party imports included — enumerating it is the only way to \
         know what the surface actually is.\n",
        schema.exported().len()
    );

    let _ = writeln!(
        out,
        "## Semantic tiers (§16.1)\n\n| Status | Symbols |\n|---|---|"
    );
    for status in [
        Status::Same,
        Status::Improved,
        Status::Tiered,
        Status::Excluded,
        Status::Unreviewed,
    ] {
        let n = schema
            .symbols
            .iter()
            .filter(|s| schema.status(s) == status)
            .count();
        let _ = writeln!(out, "| {} | {} |", status.as_str(), n);
    }
    out.push_str(
        "\n`unreviewed` is the honest default for a surface nobody has \
         adjudicated yet; it is the number the Parity Ledger ratchets down.\n",
    );

    let _ = writeln!(
        out,
        "\n## Canonical names (Appendix C, C-9)\n\n{} symbols and {} parameters \
         carry a public-surface typo. The Rust front door and these docs use the \
         canonical name; `fmn-python` binds both, so source-unedited scenes keep \
         working.\n",
        schema.renames.len(),
        schema.param_renames.len()
    );
    out.push_str("| Reference | Canonical |\n|---|---|\n");
    for rename in schema.renames.values() {
        let _ = writeln!(out, "| `{}` | `{}` |", rename.symbol, rename.canonical);
    }

    let _ = writeln!(
        out,
        "\n## Config keys\n\n{} keys, {} of them shared with the Reference's \
         `default_config.yml`. Every one is bound to a Rust field by \
         `API_OVERLAY.tsv`, and the binding generates \
         `crates/fmn-config/src/generated.rs`.\n",
        schema.config.len(),
        schema.config.iter().filter(|c| c.in_reference).count()
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXTRACT: &str = "[meta]\nreference_commit\tabc123\n\n\
        [symbols]\nm\tA\tclass\tdefined\t1\tobject\n\
        m\tA.foo_listner\tmethod\tdefined\t0\t-\n\n\
        [params]\nm:A.foo_listner\t0\tself\tpositional_or_keyword\t-\t-\n\n\
        [flags]\n-w,--write\twrite\tstore_true\t-\tFalse\t-\twrite it\n\n\
        [config]\ncamera.fps\tint\t30\t1\n";

    const OVERLAY: &str = "[meta]\noverlay_version\t1\n\n\
        [canonical]\nm:A.foo_listner\tfoo_listener\tC-9\t-\n\n\
        [param_canonical]\n\n\
        [config_binding]\ncamera.fps\tCameraConfig\tfps\tu32\n\n\
        [status]\nm:A\timproved\tBN-01\n";

    fn schema() -> Schema {
        Schema::parse(EXTRACT, OVERLAY).expect("fixture parses")
    }

    #[test]
    fn the_two_layers_merge_into_one_effective_schema() {
        let s = schema();
        assert_eq!(s.reference_commit(), "abc123");
        assert_eq!(s.symbols.len(), 2);
        assert_eq!(s.flags.len(), 1);
        assert_eq!(s.config.len(), 1);
        assert_eq!(s.bindings.len(), 1);
    }

    #[test]
    fn a_c9_ruling_renames_only_the_member_leaf() {
        let s = schema();
        let method = s
            .symbols
            .iter()
            .find(|x| x.kind == SymbolKind::Method)
            .unwrap();
        assert_eq!(s.canonical_name(method), "A.foo_listener");
        let class = s
            .symbols
            .iter()
            .find(|x| x.kind == SymbolKind::Class)
            .unwrap();
        assert_eq!(
            s.canonical_name(class),
            "A",
            "an unruled symbol keeps its name"
        );
    }

    #[test]
    fn an_unadjudicated_symbol_reads_as_unreviewed() {
        let s = schema();
        let method = s
            .symbols
            .iter()
            .find(|x| x.kind == SymbolKind::Method)
            .unwrap();
        assert_eq!(s.status(method), Status::Unreviewed);
        let class = s
            .symbols
            .iter()
            .find(|x| x.kind == SymbolKind::Class)
            .unwrap();
        assert_eq!(s.status(class), Status::Improved);
    }

    #[test]
    fn an_overlay_naming_an_unknown_symbol_is_stale_not_ignored() {
        let stale = OVERLAY.replace("m:A.foo_listner", "m:Gone.method");
        let err = Schema::parse(EXTRACT, &stale).unwrap_err();
        assert!(
            matches!(
                err,
                SchemaError::DanglingOverlay {
                    section: "canonical",
                    ..
                }
            ),
            "got {err}"
        );
        assert!(err.to_string().contains("gen_api_schema.py"));
    }

    #[test]
    fn a_row_with_the_wrong_arity_names_its_line() {
        let broken = EXTRACT.replace("m\tA\tclass\tdefined\t1\tobject", "m\tA\tclass");
        let err = Schema::parse(&broken, OVERLAY).unwrap_err();
        match err {
            SchemaError::Arity {
                line,
                expected,
                found,
                ..
            } => {
                assert_eq!((expected, found), (6, 3));
                assert!(line > 0);
            }
            other => panic!("expected an arity error, got {other}"),
        }
    }

    #[test]
    fn an_unknown_enumerated_word_is_a_named_field_error() {
        let broken = EXTRACT.replace("\tclass\t", "\tklass\t");
        let err = Schema::parse(&broken, OVERLAY).unwrap_err();
        assert!(
            matches!(err, SchemaError::Field { column: "kind", .. }),
            "got {err}"
        );
    }

    #[test]
    fn a_row_before_any_section_header_is_refused() {
        let err = Schema::parse("orphan\trow\n[meta]\n", OVERLAY).unwrap_err();
        assert!(
            matches!(err, SchemaError::Sectionless { line: 1, .. }),
            "got {err}"
        );
    }

    #[test]
    fn config_coverage_catches_both_directions() {
        let s = schema();
        s.check_config_coverage().expect("the fixture is covered");

        let unbound = OVERLAY.replace("camera.fps\tCameraConfig\tfps\tu32\n", "");
        let s = Schema::parse(EXTRACT, &unbound).unwrap();
        let err = s.check_config_coverage().unwrap_err();
        assert!(err.to_string().contains("has no binding"), "got {err}");

        let extra = OVERLAY.replace(
            "[config_binding]\n",
            "[config_binding]\ncamera.ghost\tCameraConfig\tghost\tu32\n",
        );
        let s = Schema::parse(EXTRACT, &extra).unwrap();
        let err = s.check_config_coverage().unwrap_err();
        assert!(err.to_string().contains("names no config key"), "got {err}");
    }

    #[test]
    fn every_generator_stamps_the_reference_pin() {
        let s = schema();
        for artifact in [
            generate_config_rs(&s),
            generate_ledger_tsv(&s),
            generate_cli_table_md(&s),
            generate_docs_md(&s),
        ] {
            assert!(artifact.contains("abc123"), "artifact lost the pin");
            assert!(
                artifact.contains("never hand-edit"),
                "artifact lost its banner"
            );
        }
    }

    #[test]
    fn generation_is_a_pure_function_of_the_schema() {
        let s = schema();
        assert_eq!(generate_config_rs(&s), generate_config_rs(&schema()));
        assert_eq!(generate_ledger_tsv(&s), generate_ledger_tsv(&schema()));
    }
}
