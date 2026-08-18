pub(crate) type Marker = char;

#[cfg(test)]
pub(crate) fn is_marker(c: Marker) -> bool {
  ('\u{fdd0}'..='\u{fdef}').contains(&c)
}

/// Value-layer separator joining `$@`/`${arr[@]}` elements so each becomes its
/// own field after word splitting. Arrays and positional params store their
/// joined value as a `VarStr`, so the separator lives as this byte sequence and
/// is translated to `Marker::ArgSep` when the value enters a `SegStream`.
pub(crate) const ARG_SEP: Marker = '\u{fdd6}';
/// Value-layer null-field marker for empty `"$@"`/`"${arr[@]}"` (zero fields).
/// Translated to `Marker::NullExpand` at the `SegStream` boundary.
pub(crate) const NULL_EXPAND: Marker = '\u{fdd5}';

// Display markers (help/syntax highlighting and format placeholders). These
// live in `String`s on the display path, not in the byte-native expansion
// pipeline (which uses `stream::Marker`).
/// Escape/placeholder sentinel used by display formatters (e.g. flog's `%`).
pub(crate) const ESCAPE: Marker = '\u{fdd9}';
/// Reset to default styling for help/syntax highlighting.
pub(crate) const RESET: Marker = '\u{fdda}';

// Help command formatting markers
pub(crate) const TAG: Marker = '\u{fddb}';
pub(crate) const REFERENCE: Marker = '\u{fddc}';
pub(crate) const HEADER: Marker = '\u{fddd}';
pub(crate) const CODE: Marker = '\u{fdde}';
/// angle brackets
pub(crate) const KEYWORD_1: Marker = '\u{fddf}';
/// square brackets
pub(crate) const KEYWORD_2: Marker = '\u{fde0}';

#[cfg(test)]
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
}
