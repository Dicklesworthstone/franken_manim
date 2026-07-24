//! The font book: the bundled sovereign faces plus user faces, under the
//! D-08 policy — the bundled default renders identically on every machine,
//! user TTFs load by path, family-name lookup is a convenience that
//! **never** silently substitutes (a missing family is a named
//! [`TextError::FontUnavailable`]).

use crate::error::TextError;

/// A face variant within a family.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FaceKey {
    /// Bold?
    pub bold: bool,
    /// Italic?
    pub italic: bool,
}

/// One face: the parsed font plus its GPOS kerning, parsed once (CM
/// Unicode kerns through GPOS; the legacy `kern` table is consulted too).
pub struct Face {
    /// The parsed font.
    pub font: fmd_font::Font,
    gpos: fmd_font::Kerning,
}

impl Face {
    fn new(font: fmd_font::Font) -> Self {
        let gpos = font.gpos_kerning();
        Self { font, gpos }
    }

    /// Kerning between two glyphs, font units: legacy `kern` table plus
    /// the GPOS `kern` feature.
    #[must_use]
    pub fn kern_units(&self, left: u16, right: u16) -> i32 {
        i32::from(self.font.kerning_between_glyphs(left, right))
            + i32::from(self.gpos.pair(left, right))
    }
}

/// One loaded family: up to four variant faces (regular required).
pub struct Family {
    /// Canonical family name.
    pub name: String,
    regular: Face,
    bold: Option<Face>,
    italic: Option<Face>,
    bold_italic: Option<Face>,
}

impl Family {
    /// The face for a variant, falling back to regular within the family
    /// (a bold request on a single-face family renders regular — the
    /// family the user named is still the family used).
    #[must_use]
    pub fn face(&self, key: FaceKey) -> &Face {
        match (key.bold, key.italic) {
            (true, true) => self
                .bold_italic
                .as_ref()
                .or(self.bold.as_ref())
                .or(self.italic.as_ref())
                .unwrap_or(&self.regular),
            (true, false) => self.bold.as_ref().unwrap_or(&self.regular),
            (false, true) => self.italic.as_ref().unwrap_or(&self.regular),
            (false, false) => &self.regular,
        }
    }
}

/// The book: families by canonical name, with the bundled set as the
/// sovereign default.
pub struct FontBook {
    families: Vec<Family>,
    /// Index of the default family.
    default_ix: usize,
    /// Index of the monospace family (`<tt>`).
    mono_ix: usize,
}

/// The bundled default family's canonical name.
pub const DEFAULT_FAMILY: &str = "Computer Modern";
/// The bundled monospace family's canonical name.
pub const MONO_FAMILY: &str = "CM Typewriter";
/// The bundled sans family's canonical name.
pub const SANS_FAMILY: &str = "IBM Plex Sans";

impl FontBook {
    /// The bundled sovereign book: Computer Modern (four variants), CM
    /// Typewriter, IBM Plex Sans (regular + bold + italics).
    ///
    /// # Errors
    ///
    /// Propagates a bundled-face parse failure (build corruption, not a
    /// runtime condition).
    pub fn bundled() -> Result<Self, TextError> {
        let parse = |bytes: &[u8], what: &str| {
            fmd_font::Font::parse(bytes.to_vec())
                .map(Face::new)
                .map_err(|e| TextError::FontParse {
                    path: format!("bundled:{what}"),
                    what: e.to_string(),
                })
        };
        let cm = Family {
            name: DEFAULT_FAMILY.to_owned(),
            regular: parse(fmd_font::bundled::CM_REGULAR, "cm-regular")?,
            bold: Some(parse(fmd_font::bundled::CM_BOLD, "cm-bold")?),
            italic: Some(parse(fmd_font::bundled::CM_ITALIC, "cm-italic")?),
            bold_italic: Some(parse(fmd_font::bundled::CM_BOLD_ITALIC, "cm-bold-italic")?),
        };
        let tt = Family {
            name: MONO_FAMILY.to_owned(),
            regular: parse(fmd_font::bundled::CM_TYPEWRITER, "cm-typewriter")?,
            bold: None,
            italic: None,
            bold_italic: None,
        };
        let sans = Family {
            name: SANS_FAMILY.to_owned(),
            regular: parse(fmd_font::bundled::PLEX_REGULAR, "plex-regular")?,
            bold: Some(parse(fmd_font::bundled::PLEX_BOLD, "plex-bold")?),
            italic: Some(parse(fmd_font::bundled::PLEX_ITALIC, "plex-italic")?),
            bold_italic: Some(parse(
                fmd_font::bundled::PLEX_BOLD_ITALIC,
                "plex-bold-italic",
            )?),
        };
        Ok(Self {
            families: vec![cm, tt, sans],
            default_ix: 0,
            mono_ix: 1,
        })
    }

    /// Add a user family from font bytes (the caller reads the file; the
    /// engine owns no filesystem access — capability doctrine). The name
    /// is the family name the user will select by.
    ///
    /// # Errors
    ///
    /// [`TextError::FontParse`] when the bytes are not a usable font.
    pub fn add_family(&mut self, name: &str, bytes: Vec<u8>) -> Result<(), TextError> {
        let font = fmd_font::Font::parse(bytes).map_err(|e| TextError::FontParse {
            path: name.to_owned(),
            what: e.to_string(),
        })?;
        self.families.push(Family {
            name: name.to_owned(),
            regular: Face::new(font),
            bold: None,
            italic: None,
            bold_italic: None,
        });
        Ok(())
    }

    /// The default family.
    #[must_use]
    pub fn default_family(&self) -> &Family {
        &self.families[self.default_ix]
    }

    /// The monospace family (`<tt>`).
    #[must_use]
    pub fn mono_family(&self) -> &Family {
        &self.families[self.mono_ix]
    }

    /// Look a family up by name, case-insensitively, tolerating the
    /// common aliases of the bundled names. A miss is the named
    /// capability-style error — never a substitution.
    ///
    /// # Errors
    ///
    /// [`TextError::FontUnavailable`] naming the request and the roster.
    pub fn family(&self, name: &str) -> Result<&Family, TextError> {
        let want = normalize(name);
        let hit = self.families.iter().find(|f| {
            let have = normalize(&f.name);
            have == want || aliases(&f.name).iter().any(|a| normalize(a) == want)
        });
        hit.ok_or_else(|| TextError::FontUnavailable {
            family: name.to_owned(),
            available: self.families.iter().map(|f| f.name.clone()).collect(),
        })
    }

    /// The family names the book can serve.
    #[must_use]
    pub fn available(&self) -> Vec<String> {
        self.families.iter().map(|f| f.name.clone()).collect()
    }
}

fn normalize(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_whitespace() && *c != '-' && *c != '_')
        .flat_map(char::to_lowercase)
        .collect()
}

fn aliases(canonical: &str) -> &'static [&'static str] {
    match canonical {
        DEFAULT_FAMILY => &["CMU Serif", "Computer Modern Roman", "cmr"],
        MONO_FAMILY => &["Computer Modern Typewriter", "CMU Typewriter Text"],
        SANS_FAMILY => &["Plex Sans", "IBM Plex"],
        _ => &[],
    }
}

/// Per-glyph metrics in ems of the face's design size.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct GlyphMetrics {
    /// Advance width.
    pub advance: f64,
    /// Ink extent above the baseline.
    pub height: f64,
    /// Ink extent below the baseline, positive.
    pub depth: f64,
}

/// Measure a glyph in ems.
#[must_use]
pub fn glyph_metrics(face: &Face, gid: u16) -> GlyphMetrics {
    let font = &face.font;
    let upm = f64::from(font.units_per_em.max(1));
    let advance = f64::from(font.advance_width(gid)) / upm;
    let (height, depth) = font.glyph_bbox(gid).map_or((0.0, 0.0), |bbox| {
        (
            f64::from(bbox[3]).max(0.0) / upm,
            (-f64::from(bbox[1])).max(0.0) / upm,
        )
    });
    GlyphMetrics {
        advance,
        height,
        depth,
    }
}

/// Kerning between two glyphs of one face, in ems (legacy kern table plus
/// the GPOS kern feature).
#[must_use]
pub fn kern_em(face: &Face, left: u16, right: u16) -> f64 {
    let upm = f64::from(face.font.units_per_em.max(1));
    f64::from(face.kern_units(left, right)) / upm
}
