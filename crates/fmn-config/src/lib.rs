//! YAML-subset parser, typed configuration, and the preamble-pack registry (§6.4, §13.6).
//!
//! Three layers, strictly ordered:
//!
//! - [`yaml`] — the owned parser for the actual shipped config-file shapes
//!   (see its module docs for the precise subset and the named diagnostics
//!   for everything outside it), plus the Reference-exact recursive merge.
//! - [`config`] — the typed configuration: the Reference's
//!   `default_config.yml` key surface as hand-written structs (migrating to
//!   schema codegen when W10's fm-vn6 lands), resolved through the
//!   Reference's precedence exactly — **built-in defaults → user config
//!   file(s) → CLI overlay** — with tuple-strings typed at this layer, the
//!   way the Reference `literal_eval`s them.
//! - [`packs`] — the preamble-pack registry: the `tex_templates.yml` concept
//!   reborn as named fmd-math preamble packs, with the compatibility mapping
//!   for the common templates. This crate owns the registry *surface*
//!   (naming, lookup, config keys); pack *content* is W6's business
//!   (fm-kg9).
//!
//! # The quality-knob doctrine (§4)
//!
//! [`config::DeterminismConfig`] and [`config::RenderConfig`] carry the
//! determinism-mode and engine/backend keys (`standard | certified`;
//! `cpu | metal | cuda`; AA policy; thread policy) as opaque typed enums.
//! They select engines and schedules — they are **never scene-visible data**:
//! no path exists from these knobs into mobject state, animation timing, or
//! any other semantic surface, so a quality/backend change can change speed
//! but structurally cannot change meaning.
#![forbid(unsafe_code)]

pub mod config;
pub mod packs;
pub mod yaml;

pub use config::{Config, ConfigError};
pub use packs::{Pack, PackError, PackRegistry};
pub use yaml::{ParseError, Value, Warning};
