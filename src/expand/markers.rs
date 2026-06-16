pub(crate) type Marker = char;

/*
 * These are invisible Unicode noncharacters used to annotate
 * strings with various contextual metadata.
 *
 * Noncharacters (U+FDD0..=U+FDEF) are codepoints that Unicode
 * guarantees will never be assigned to any character and are
 * explicitly designated for application-internal use. Unlike the
 * Private Use Area (U+E000..=U+F8FF), which is widely used by
 * powerline glyphs, Nerd Fonts, and other icon sets, noncharacters
 * are safe to use as sentinels without colliding with real user text.
 */

/* Highlight Markers */

// token-level (derived from token class)
pub(crate) const SUBSH: Marker = '\u{fdd7}';

// sub-token (needs scanning)
pub(crate) const VAR_SUB: Marker = '\u{fdd8}';
pub(crate) const ESCAPE: Marker = '\u{fdd9}';

pub(crate) const RESET: Marker = '\u{fdda}';

/* Expansion Markers */
/// Double quote '"' marker
pub(crate) const DUB_QUOTE: Marker = '\u{fdd0}';
/// Single quote '\'' marker
pub(crate) const SNG_QUOTE: Marker = '\u{fdd1}';
/// Tilde sub marker
pub(crate) const TILDE_SUB: Marker = '\u{fdd2}';
/// Input process sub marker
pub(crate) const PROC_SUB_IN: Marker = '\u{fdd3}';
/// Output process sub marker
pub(crate) const PROC_SUB_OUT: Marker = '\u{fdd4}';

/// Marker for null expansion
/// This is used for when "$@" or "$*" are used in quotes and there are no
/// arguments Without this marker, it would be handled like an empty string,
/// which breaks some commands
pub(crate) const NULL_EXPAND: Marker = '\u{fdd5}';

/// Explicit marker for argument separation
/// This is used to join the arguments given by "$@", and preserves exact
/// formatting of the original arguments, including quoting
pub(crate) const ARG_SEP: Marker = '\u{fdd6}';

pub(crate) fn is_marker(c: Marker) -> bool {
  ('\u{fdd0}'..='\u{fdef}').contains(&c)
}

// Help command formatting markers
pub(crate) const TAG: Marker = '\u{fddb}';
pub(crate) const REFERENCE: Marker = '\u{fddc}';
pub(crate) const HEADER: Marker = '\u{fddd}';
pub(crate) const CODE: Marker = '\u{fdde}';
/// angle brackets
pub(crate) const KEYWORD_1: Marker = '\u{fddf}';
/// square brackets
pub(crate) const KEYWORD_2: Marker = '\u{fde0}';

pub(crate) fn strip_markers(str: &str) -> String {
  let mut out = str.to_string();
  out.retain(|c| !is_marker(c));
  out
}

#[cfg(test)]
mod tests {
  use super::*;

  // Regression: is_marker used to claim all of U+E000..U+EFFF as
  // markers, which collided with the Private Use Area characters
  // that fonts legitimately assign to powerline glyphs, nerd font
  // icons, and so on. Strip_markers nuked them out of user strings.
  // Now markers live in the Unicode noncharacter range
  // (U+FDD0..U+FDEF), which is guaranteed never to appear in real
  // text.

  #[test]
  fn powerline_glyph_survives_strip() {
    let powerline = "\u{e0b0}";
    let stripped = strip_markers(powerline);
    assert_eq!(stripped, powerline);
  }

  #[test]
  fn nerd_font_icon_survives_strip() {
    let nerd_font = "\u{f0226}";
    let stripped = strip_markers(nerd_font);
    assert_eq!(stripped, nerd_font);
  }

  #[test]
  fn apple_logo_survives_strip() {
    let apple_logo = "\u{f8ff}";
    let stripped = strip_markers(apple_logo);
    assert_eq!(stripped, apple_logo);
  }

  #[test]
  fn actual_markers_still_stripped() {
    let input = format!("hello{}world{}", NULL_EXPAND, ARG_SEP);
    let stripped = strip_markers(&input);
    assert_eq!(stripped, "helloworld");
  }

  #[test]
  fn mixed_markers_and_pua_preserves_pua() {
    let input = format!("a{}b{}c{}d", NULL_EXPAND, '\u{e0b0}', ARG_SEP);
    let stripped = strip_markers(&input);
    assert_eq!(stripped, "ab\u{e0b0}cd");
  }
}
