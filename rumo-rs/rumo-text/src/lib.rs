// SPDX-License-Identifier: Apache-2.0

//! Minimal font inspection backed by `ttf-parser` (pure Rust, no C).

/// Summary metadata extracted from a font file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontInfo {
    /// Font family name (name ID 1, fallback: typographic family, ID 16).
    pub family: String,
    /// Total number of glyphs in the face.
    pub num_glyphs: u32,
    /// Design units per em.
    pub units_per_em: u16,
    /// Bold flag (`mac_style` / OS/2 selection flags via [`ttf_parser::Face::is_bold`]).
    pub is_bold: bool,
}

/// Parse `bytes` as a font (TrueType / OpenType) and return its metadata.
///
/// Returns `None` for empty or malformed input.
pub fn inspect_font(bytes: &[u8]) -> Option<FontInfo> {
    let face = ttf_parser::Face::parse(bytes, 0).ok()?;
    Some(FontInfo {
        family: family_name(&face),
        num_glyphs: u32::from(face.number_of_glyphs()),
        units_per_em: face.units_per_em(),
        is_bold: face.is_bold(),
    })
}

/// Return the glyph ID for `ch` in the font stored in `bytes`.
///
/// Returns `None` for empty/malformed input or when the character
/// has no glyph in the font (`glyph_index` maps missing glyphs to `None`,
/// not to `.notdef` 0).
pub fn glyph_id_for_char(bytes: &[u8], ch: char) -> Option<u16> {
    let face = ttf_parser::Face::parse(bytes, 0).ok()?;
    face.glyph_index(ch).map(|id| id.0)
}

/// Extract the family name: name ID 1 (Font Family) wins,
/// name ID 16 (Typographic Family) is kept as fallback.
/// Non-Unicode records (`to_string() == None`) are skipped.
fn family_name(face: &ttf_parser::Face<'_>) -> String {
    let mut fallback: Option<String> = None;
    for name in face.names() {
        if name.name_id != 1 && name.name_id != 16 {
            continue;
        }
        let Some(text) = name.to_string() else {
            continue;
        };
        if text.is_empty() {
            continue;
        }
        if name.name_id == 1 {
            return text;
        }
        if fallback.is_none() {
            fallback = Some(text);
        }
    }
    fallback.unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reject_empty() {
        assert!(inspect_font(b"").is_none());
        assert!(glyph_id_for_char(b"", 'A').is_none());
    }

    #[test]
    fn reject_garbage() {
        // 0xABABABAB is not a valid sfnt version / collection tag,
        // so parsing must fail instead of returning a bogus face.
        let garbage = [0xABu8; 64];
        assert!(inspect_font(&garbage).is_none());
        assert!(glyph_id_for_char(&garbage, 'A').is_none());
    }

    #[test]
    fn glyph_missing_char_none() {
        // Empty input cannot map any char, including NUL and non-BMP planes.
        assert!(glyph_id_for_char(b"", '\0').is_none());
        assert!(glyph_id_for_char(b"", '\u{10FFFF}').is_none());
    }
}
