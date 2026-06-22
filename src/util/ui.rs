use super::{
  super::state::terminal::Terminal,
  ShResult, Shed, match_loop, sherr,
  state::terminal::{ColorMode, calc_str_width},
};
use std::fmt::Write;
use yansi::{Paint, Painted, Style};

pub const BOT_LEFT: &str = "\x1b[90m╰\x1b[0m";
pub const BOT_RIGHT: &str = "\x1b[90m╯\x1b[0m";
pub const TOP_LEFT: &str = "\x1b[90m╭\x1b[0m";
pub const TOP_RIGHT: &str = "\x1b[90m╮\x1b[0m";
pub const HOR_LINE: &str = "\x1b[90m─\x1b[0m";
pub const VERT_LINE: &str = "\x1b[90m│\x1b[0m";

fn rgb_to_xterm256(r: u8, g: u8, b: u8) -> u8 {
  let r = (u16::from(r) * 5 / 255) as u8;
  let g = (u16::from(g) * 5 / 255) as u8;
  let b = (u16::from(b) * 5 / 255) as u8;

  16 + r * 36 + g * 6 + b
}

fn rgb_to_xterm16(r: u8, g: u8, b: u8) -> u8 {
  let r = u8::from(r > 128);
  let g = u8::from(g > 128);
  let b = u8::from(b > 128);

  (b << 2) | (g << 1) | r
}

fn apply_fg_rgb(style: Style, r: u8, g: u8, b: u8) -> Style {
  match Shed::term(Terminal::color_mode) {
    None => style,
    Some(ColorMode::Truecolor) => style.rgb(r, g, b),
    Some(ColorMode::Palette256) => style.fixed(rgb_to_xterm256(r, g, b)),
    Some(ColorMode::Palette16) => style.fixed(rgb_to_xterm16(r, g, b)),
  }
}

fn apply_fg_rgb_raw(style: Painted<&str>, r: u8, g: u8, b: u8) -> Painted<&str> {
  match Shed::term(Terminal::color_mode) {
    None => style,
    Some(ColorMode::Truecolor) => style.rgb(r, g, b),
    Some(ColorMode::Palette256) => style.fixed(rgb_to_xterm256(r, g, b)),
    Some(ColorMode::Palette16) => style.fixed(rgb_to_xterm16(r, g, b)),
  }
}

fn apply_bg_rgb(style: Style, r: u8, g: u8, b: u8) -> Style {
  match Shed::term(Terminal::color_mode) {
    None => style,
    Some(ColorMode::Truecolor) => style.on_rgb(r, g, b),
    Some(ColorMode::Palette256) => style.on_fixed(rgb_to_xterm256(r, g, b)),
    Some(ColorMode::Palette16) => style.on_fixed(rgb_to_xterm16(r, g, b)),
  }
}

fn apply_bg_rgb_raw(style: Painted<&str>, r: u8, g: u8, b: u8) -> Painted<&str> {
  match Shed::term(Terminal::color_mode) {
    None => style,
    Some(ColorMode::Truecolor) => style.on_rgb(r, g, b),
    Some(ColorMode::Palette256) => style.on_fixed(rgb_to_xterm256(r, g, b)),
    Some(ColorMode::Palette16) => style.on_fixed(rgb_to_xterm16(r, g, b)),
  }
}

/// A wrapper around `yansi::Style`. Defers application of text attributes like bold/italic.
#[derive(Clone, Debug, Default, Copy)]
pub struct PaletteEntry {
  style: Style,
  decorations: Decorations,
}

#[expect(dead_code)]
impl PaletteEntry {
  pub fn new() -> Self {
    Self {
      style: Style::new().primary().on_primary(),
      decorations: Decorations::default(),
    }
  }
  /*
  "green" => style = style.green(),
  "red" => style = style.red(),
  "yellow" => style = style.yellow(),
  "blue" => style = style.blue(),
  "magenta" => style = style.magenta(),
  "cyan" => style = style.cyan(),
  "white" => style = style.white(),
  "black" => style = style.black(),
  "bold" => decor = decor.bold(),
  "dim" => decor = decor.dim(),
  "italic" => decor = decor.italic(),
  "underline" => decor = decor.underline(),
  "strikethrough" => decor = decor.strike(),
  "hidden" => decor = decor.hidden(),
  "blink" => decor = decor.blink(),
  "inverted" => decor = decor.inverted(),
  "reset" => style = style.resetting(),
  */
  pub fn style(&self) -> Style {
    self.decorations.apply(self.style)
  }
  pub fn decor(&self) -> Decorations {
    self.decorations
  }
  pub fn set_decor(&mut self, decor: Decorations) {
    self.decorations = decor;
  }
  pub fn green(mut self) -> Self {
    self.style = self.style.green();
    self
  }
  pub fn red(mut self) -> Self {
    self.style = self.style.red();
    self
  }
  pub fn yellow(mut self) -> Self {
    self.style = self.style.yellow();
    self
  }
  pub fn blue(mut self) -> Self {
    self.style = self.style.blue();
    self
  }
  pub fn magenta(mut self) -> Self {
    self.style = self.style.magenta();
    self
  }
  pub fn cyan(mut self) -> Self {
    self.style = self.style.cyan();
    self
  }
  pub fn white(mut self) -> Self {
    self.style = self.style.white();
    self
  }
  pub fn black(mut self) -> Self {
    self.style = self.style.black();
    self
  }
  pub fn bright_green(mut self) -> Self {
    self.style = self.style.bright_green();
    self
  }
  pub fn bright_red(mut self) -> Self {
    self.style = self.style.bright_red();
    self
  }
  pub fn bright_yellow(mut self) -> Self {
    self.style = self.style.bright_yellow();
    self
  }
  pub fn bright_blue(mut self) -> Self {
    self.style = self.style.bright_blue();
    self
  }
  pub fn bright_magenta(mut self) -> Self {
    self.style = self.style.bright_magenta();
    self
  }
  pub fn bright_cyan(mut self) -> Self {
    self.style = self.style.bright_cyan();
    self
  }
  pub fn bright_white(mut self) -> Self {
    self.style = self.style.bright_white();
    self
  }
  pub fn bright_black(mut self) -> Self {
    self.style = self.style.bright_black();
    self
  }
  pub fn on_green(mut self) -> Self {
    self.style = self.style.on_green();
    self
  }
  pub fn on_red(mut self) -> Self {
    self.style = self.style.on_red();
    self
  }
  pub fn on_yellow(mut self) -> Self {
    self.style = self.style.on_yellow();
    self
  }
  pub fn on_blue(mut self) -> Self {
    self.style = self.style.on_blue();
    self
  }
  pub fn on_magenta(mut self) -> Self {
    self.style = self.style.on_magenta();
    self
  }
  pub fn on_cyan(mut self) -> Self {
    self.style = self.style.on_cyan();
    self
  }
  pub fn on_white(mut self) -> Self {
    self.style = self.style.on_white();
    self
  }
  pub fn on_black(mut self) -> Self {
    self.style = self.style.on_black();
    self
  }
  pub fn on_bright(mut self) -> Self {
    self.style = self.style.on_bright();
    self
  }
  pub fn bold(mut self) -> Self {
    self.decorations = self.decorations.bold();
    self
  }
  pub fn italic(mut self) -> Self {
    self.decorations = self.decorations.italic();
    self
  }
  pub fn strike(mut self) -> Self {
    self.decorations = self.decorations.strike();
    self
  }
  pub fn underline(mut self) -> Self {
    self.decorations = self.decorations.underline();
    self
  }
  pub fn dim(mut self) -> Self {
    self.decorations = self.decorations.dim();
    self
  }
  pub fn blink(mut self) -> Self {
    self.decorations = self.decorations.blink();
    self
  }
  pub fn hidden(mut self) -> Self {
    self.decorations = self.decorations.hidden();
    self
  }
  pub fn inverted(mut self) -> Self {
    self.decorations = self.decorations.inverted();
    self
  }
}

/// A struct containing various ansi style attributes
///
/// This is made as a workaround for the fact that yansi's `Style` struct does not offer any kind of introspection.
#[derive(Clone, Default, Debug, Copy)]
#[expect(clippy::struct_excessive_bools)]
pub struct Decorations {
  underline: bool,
  bold: bool,
  italic: bool,
  strike: bool,
  dimmed: bool,
  blink: bool,
  hidden: bool,
  inverted: bool,
}

impl Decorations {
  pub fn apply(self, mut s: Style) -> Style {
    if self.underline {
      s = s.underline();
    }
    if self.bold {
      s = s.bold();
    }
    if self.italic {
      s = s.italic();
    }
    if self.strike {
      s = s.strike();
    }
    if self.dimmed {
      s = s.dim();
    }
    if self.blink {
      s = s.attr(yansi::Attribute::Blink);
    }
    if self.hidden {
      s = s.attr(yansi::Attribute::Conceal);
    }
    if self.inverted {
      s = s.attr(yansi::Attribute::Invert);
    }
    s
  }

  pub fn union(self, other: Decorations) -> Self {
    Self {
      underline: self.underline | other.underline,
      bold: self.bold | other.bold,
      italic: self.italic | other.italic,
      strike: self.strike | other.strike,
      dimmed: self.dimmed | other.dimmed,
      blink: self.blink | other.blink,
      hidden: self.hidden | other.hidden,
      inverted: self.inverted | other.inverted,
    }
  }

  pub fn bold(mut self) -> Self {
    self.bold = true;
    self
  }
  pub fn italic(mut self) -> Self {
    self.italic = true;
    self
  }
  pub fn underline(mut self) -> Self {
    self.underline = true;
    self
  }
  pub fn strike(mut self) -> Self {
    self.strike = true;
    self
  }
  pub fn dim(mut self) -> Self {
    self.dimmed = true;
    self
  }
  pub fn blink(mut self) -> Self {
    self.blink = true;
    self
  }
  pub fn hidden(mut self) -> Self {
    self.hidden = true;
    self
  }
  pub fn inverted(mut self) -> Self {
    self.inverted = true;
    self
  }
}

/// Pad `content` with `fill` to `cols` width, appending `right_border` at the end.
pub fn pad_line_into(buf: &mut String, content: &str, fill: &str, right_border: &str, cols: usize) {
  let used = calc_str_width(content);
  let padding = cols.saturating_sub(used + 1);
  write!(buf, "{content}").ok();
  for _ in 0..padding {
    write!(buf, "{fill}").ok();
  }
  write!(buf, "{right_border}").ok();
}

/// Build an ansi color escape sequence from a plain english description
pub fn style_from_description(desc: &str) -> ShResult<PaletteEntry> {
  let mut style = Style::new().primary().on_primary();
  let mut decor = Decorations::default();
  let mut words = desc.split_whitespace();

  match_loop!(words.next() => word, {
    "green" => style = style.green(),
    "red" => style = style.red(),
    "yellow" => style = style.yellow(),
    "blue" => style = style.blue(),
    "magenta" => style = style.magenta(),
    "cyan" => style = style.cyan(),
    "white" => style = style.white(),
    "black" => style = style.black(),
    "bold" => decor = decor.bold(),
    "dim" => decor = decor.dim(),
    "italic" => decor = decor.italic(),
    "underline" => decor = decor.underline(),
    "strikethrough" => decor = decor.strike(),
    "hidden" => decor = decor.hidden(),
    "blink" => decor = decor.blink(),
    "inverted" => decor = decor.inverted(),
    "reset" => style = style.resetting(),

    "bright" => style = style.bright(),
    "on" => {
      let Some(mut word) = words.next() else {
        return Err(sherr!(ParseErr, "Expected background color after 'on' in color description"));
      };
      if word == "bright" {
        style = style.on_bright();
        let Some(w) = words.next() else {
          return Err(sherr!(ParseErr, "Expected background color after 'on bright' in color description"));
        };
        word = w;
      }
      match word {
        "green" => style = style.on_green(),
        "red" => style = style.on_red(),
        "yellow" => style = style.on_yellow(),
        "blue" => style = style.on_blue(),
        "magenta" => style = style.on_magenta(),
        "cyan" => style = style.on_cyan(),
        "white" => style = style.on_white(),
        "black" => style = style.on_black(),
        hex if word.starts_with('#') => {
          let (r,g,b) = hex_to_rgb(hex)?;
          style = apply_bg_rgb(style, r, g, b);
        }
        _ => return Err(sherr!(ParseErr, "Unknown background color '{}' in color description", word)),
      }
    }

    hex if word.starts_with('#') => {
      let (r,g,b) = hex_to_rgb(hex)?;
      style = apply_fg_rgb(style, r, g, b);
    }

    _ => return Err(sherr!(ParseErr, "Unknown style '{}' in color description", word)),
  });

  Ok(PaletteEntry {
    style,
    decorations: decor,
  })
}

/// Build an ansi color escape sequence from a plain english description
pub fn ansi_from_description(desc: &str) -> ShResult<String> {
  let mut style: Painted<&str> = "".primary().on_primary().linger();
  let mut words = desc.split_whitespace();

  match_loop!(words.next() => word, {
    "green" => style = style.green(),
    "red" => style = style.red(),
    "yellow" => style = style.yellow(),
    "blue" => style = style.blue(),
    "magenta" => style = style.magenta(),
    "cyan" => style = style.cyan(),
    "white" => style = style.white(),
    "black" => style = style.black(),
    "bold" => style = style.bold(),
    "dim" => style = style.dim(),
    "italic" => style = style.italic(),
    "underline" => style = style.underline(),
    "strikethrough" => style = style.strike(),
    "hidden" => style = style.attr(yansi::Attribute::Conceal),
    "blink" => style = style.attr(yansi::Attribute::Blink),
    "inverted" => style = style.attr(yansi::Attribute::Invert),
    "reset" => style = style.resetting(),

    "bright" => style = style.bright(),
    "on" => {
      let Some(mut word) = words.next() else {
        return Err(sherr!(ParseErr, "Expected background color after 'on' in color description"));
      };
      if word == "bright" {
        style = style.on_bright();
        let Some(w) = words.next() else {
          return Err(sherr!(ParseErr, "Expected background color after 'on bright' in color description"));
        };
        word = w;
      }
      match word {
        "green" => style = style.on_green(),
        "red" => style = style.on_red(),
        "yellow" => style = style.on_yellow(),
        "blue" => style = style.on_blue(),
        "magenta" => style = style.on_magenta(),
        "cyan" => style = style.on_cyan(),
        "white" => style = style.on_white(),
        "black" => style = style.on_black(),
        hex if word.starts_with('#') => {
          let (r,g,b) = hex_to_rgb(hex)?;
          style = apply_bg_rgb_raw(style, r, g, b);
        }
        _ => return Err(sherr!(ParseErr, "Unknown background color '{}' in color description", word)),
      }
    }

    hex if word.starts_with('#') => {
      let (r,g,b) = hex_to_rgb(hex)?;
      style = apply_fg_rgb_raw(style, r, g, b);
    }

    _ => return Err(sherr!(ParseErr, "Unknown style '{}' in color description", word)),
  });

  Ok(style.to_string())
}

pub fn hex_to_rgb(hex: &str) -> ShResult<(u8, u8, u8)> {
  let hex = &hex[1..];
  if hex.len() != 6
    || !hex
      .chars()
      .all(|ch: char| ('a'..='f').contains(&ch) || ch.is_ascii_digit())
  {
    return Err(sherr!(
      ParseErr,
      "Invalid hex color '{}' in color description",
      hex
    ));
  }
  let r = u8::from_str_radix(&hex[..2], 16).unwrap();
  let g = u8::from_str_radix(&hex[2..4], 16).unwrap();
  let b = u8::from_str_radix(&hex[4..6], 16).unwrap();

  Ok((r, g, b))
}

pub(crate) fn stylize_loglevel(level: log::Level) -> String {
  let style = match level {
    log::Level::Error => style_from_description("red bold").unwrap(),
    log::Level::Warn => style_from_description("yellow bold").unwrap(),
    log::Level::Info => style_from_description("green bold").unwrap(),
    log::Level::Debug => style_from_description("blue bold").unwrap(),
    log::Level::Trace => style_from_description("magenta bold").unwrap(),
  };
  format!("{level}").paint(style.style()).to_string()
}

#[cfg(test)]
mod stylize_loglevel_tests {
  use super::*;

  /// Every level should include the level name verbatim inside the
  /// ANSI-wrapped string.
  #[test]
  fn output_contains_level_name() {
    for level in [
      log::Level::Error,
      log::Level::Warn,
      log::Level::Info,
      log::Level::Debug,
      log::Level::Trace,
    ] {
      let out = stylize_loglevel(level);
      let name = format!("{level}");
      assert!(out.contains(&name), "level {level}: {out:?}");
    }
  }

  /// Output should be ANSI-styled, i.e. longer than the bare level name
  /// and contain at least one ESC byte.
  #[test]
  fn output_is_ansi_styled() {
    let out = stylize_loglevel(log::Level::Error);
    assert!(out.contains('\x1b'));
    assert!(out.len() > "ERROR".len());
  }

  /// Different levels should produce different ANSI sequences (different
  /// foreground colors). We just sanity-check pairwise distinctness for
  /// the five variants — any collision would mean the match arm is wrong.
  #[test]
  fn each_level_uses_distinct_styling() {
    let levels = [
      log::Level::Error,
      log::Level::Warn,
      log::Level::Info,
      log::Level::Debug,
      log::Level::Trace,
    ];
    let outputs: Vec<String> = levels.iter().copied().map(stylize_loglevel).collect();
    for (i, a) in outputs.iter().enumerate() {
      for (j, b) in outputs.iter().enumerate() {
        if i != j {
          assert_ne!(
            a, b,
            "levels {:?} and {:?} produced identical styling",
            levels[i], levels[j]
          );
        }
      }
    }
  }
}

#[cfg(test)]
mod color_application_tests {
  //! Tests for the `apply_fg_rgb` / `apply_bg_rgb` family + the
  //! `rgb_to_xterm{256,16}` lookup tables they depend on.
  //!
  //! The functions branch on `Shed::term(|t| t.color_mode())`, which
  //! consults env vars (`NO_COLOR`, `SHED_COLOR_MODE`, TERM, plus
  //! terminfo). We drive the dispatch by setting `SHED_COLOR_MODE` in
  //! the test vars.

  use super::*;
  use crate::state::Shed;
  use crate::state::vars::{VarFlags, VarKind};
  use crate::tests::testutil::TestGuard;
  use yansi::Paint;

  fn set_var(name: &str, val: &str) {
    Shed::vars_mut(|v| {
      v.set_var(name, VarKind::Str(val.into()), VarFlags::empty())
        .unwrap();
    });
  }

  fn reset_color_env() {
    Shed::vars_mut(|v| {
      v.unset_var("NO_COLOR").ok();
      v.unset_var("SHED_COLOR_MODE").ok();
      v.unset_var("TERM").ok();
    });
  }

  /// Render `style` over a single-character target and stringify it,
  /// so we can assert against the resulting ANSI escape sequence.
  fn render(style: Style) -> String {
    format!("{}", "x".paint(style))
  }

  // ===================== rgb_to_xterm256 =====================

  #[test]
  fn xterm256_black_is_16() {
    assert_eq!(rgb_to_xterm256(0, 0, 0), 16);
  }

  #[test]
  fn xterm256_white_is_231() {
    // 16 + 5*36 + 5*6 + 5 = 231
    assert_eq!(rgb_to_xterm256(255, 255, 255), 231);
  }

  #[test]
  fn xterm256_pure_red() {
    // 16 + 5*36 = 196
    assert_eq!(rgb_to_xterm256(255, 0, 0), 196);
  }

  #[test]
  fn xterm256_pure_green() {
    // 16 + 5*6 = 46
    assert_eq!(rgb_to_xterm256(0, 255, 0), 46);
  }

  #[test]
  fn xterm256_pure_blue() {
    // 16 + 5 = 21
    assert_eq!(rgb_to_xterm256(0, 0, 255), 21);
  }

  // ===================== rgb_to_xterm16 =====================

  #[test]
  fn xterm16_packs_bits_as_bgr() {
    // Bits are (b<<2) | (g<<1) | r, with each component thresholded
    // at >128.
    assert_eq!(rgb_to_xterm16(0, 0, 0), 0);
    assert_eq!(rgb_to_xterm16(255, 0, 0), 1); // r only
    assert_eq!(rgb_to_xterm16(0, 255, 0), 2); // g only
    assert_eq!(rgb_to_xterm16(0, 0, 255), 4); // b only
    assert_eq!(rgb_to_xterm16(255, 255, 255), 7); // all
    assert_eq!(rgb_to_xterm16(0, 255, 255), 6); // g+b → cyan
  }

  #[test]
  fn xterm16_threshold_is_strictly_greater_than_128() {
    // 128 → 0 (not >128), 129 → 1.
    assert_eq!(rgb_to_xterm16(128, 0, 0), 0);
    assert_eq!(rgb_to_xterm16(129, 0, 0), 1);
  }

  // ===================== apply_fg_rgb across color modes =====================

  #[test]
  fn fg_no_color_mode_returns_style_unchanged() {
    let _g = TestGuard::new();
    reset_color_env();
    set_var("NO_COLOR", "1");
    let styled = apply_fg_rgb(Style::new(), 200, 50, 75);
    let out = render(styled);
    // No SGR escape sequence emitted at all.
    assert!(!out.contains('\x1b'), "got: {out:?}");
  }

  #[test]
  fn fg_truecolor_mode_emits_sgr_38_2_rgb() {
    let _g = TestGuard::new();
    reset_color_env();
    set_var("SHED_COLOR_MODE", "truecolor");
    let styled = apply_fg_rgb(Style::new(), 12, 34, 56);
    let out = render(styled);
    // CSI 38 ; 2 ; R ; G ; B  m  for 24-bit foreground.
    assert!(out.contains("38;2;12;34;56"), "got: {out:?}");
  }

  #[test]
  fn fg_palette256_mode_emits_sgr_38_5_n() {
    let _g = TestGuard::new();
    reset_color_env();
    set_var("SHED_COLOR_MODE", "256");
    let styled = apply_fg_rgb(Style::new(), 255, 0, 0);
    let out = render(styled);
    // Pure red → xterm256 index 196.
    assert!(out.contains("38;5;196"), "got: {out:?}");
  }

  #[test]
  fn fg_palette16_mode_emits_low_index() {
    let _g = TestGuard::new();
    reset_color_env();
    set_var("SHED_COLOR_MODE", "16");
    let styled = apply_fg_rgb(Style::new(), 0, 0, 255);
    let out = render(styled);
    // Pure blue → index 4 (b<<2).
    assert!(out.contains("38;5;4"), "got: {out:?}");
  }

  // ===================== apply_bg_rgb across color modes =====================

  #[test]
  fn bg_no_color_mode_returns_style_unchanged() {
    let _g = TestGuard::new();
    reset_color_env();
    set_var("NO_COLOR", "1");
    let styled = apply_bg_rgb(Style::new(), 10, 20, 30);
    let out = render(styled);
    assert!(!out.contains('\x1b'), "got: {out:?}");
  }

  #[test]
  fn bg_truecolor_mode_emits_sgr_48_2_rgb() {
    let _g = TestGuard::new();
    reset_color_env();
    set_var("SHED_COLOR_MODE", "truecolor");
    let styled = apply_bg_rgb(Style::new(), 12, 34, 56);
    let out = render(styled);
    // CSI 48 ; 2 ; R ; G ; B m for 24-bit background.
    assert!(out.contains("48;2;12;34;56"), "got: {out:?}");
  }

  #[test]
  fn bg_palette256_mode_emits_sgr_48_5_n() {
    let _g = TestGuard::new();
    reset_color_env();
    set_var("SHED_COLOR_MODE", "256");
    let styled = apply_bg_rgb(Style::new(), 0, 255, 0);
    let out = render(styled);
    // Pure green → xterm256 index 46.
    assert!(out.contains("48;5;46"), "got: {out:?}");
  }

  #[test]
  fn bg_palette16_mode_emits_low_index() {
    let _g = TestGuard::new();
    reset_color_env();
    set_var("SHED_COLOR_MODE", "16");
    let styled = apply_bg_rgb(Style::new(), 255, 0, 0);
    let out = render(styled);
    // Pure red → index 1.
    assert!(out.contains("48;5;1"), "got: {out:?}");
  }

  // ===================== apply_fg_rgb_raw =====================

  /// Render a Painted<&str> directly; the _raw variants chain the
  /// color onto an already-Painted value.
  fn render_painted(p: Painted<&str>) -> String {
    format!("{p}")
  }

  #[test]
  fn fg_raw_truecolor_emits_sgr_38_2_rgb() {
    let _g = TestGuard::new();
    reset_color_env();
    set_var("SHED_COLOR_MODE", "truecolor");
    let painted = apply_fg_rgb_raw("x".paint(Style::new()), 7, 8, 9);
    let out = render_painted(painted);
    assert!(out.contains("38;2;7;8;9"), "got: {out:?}");
  }

  #[test]
  fn fg_raw_no_color_emits_no_escape() {
    let _g = TestGuard::new();
    reset_color_env();
    set_var("NO_COLOR", "1");
    let painted = apply_fg_rgb_raw("x".paint(Style::new()), 7, 8, 9);
    let out = render_painted(painted);
    assert!(!out.contains('\x1b'), "got: {out:?}");
  }

  // ===================== apply_bg_rgb_raw =====================

  #[test]
  fn bg_raw_truecolor_emits_sgr_48_2_rgb() {
    let _g = TestGuard::new();
    reset_color_env();
    set_var("SHED_COLOR_MODE", "truecolor");
    let painted = apply_bg_rgb_raw("x".paint(Style::new()), 7, 8, 9);
    let out = render_painted(painted);
    assert!(out.contains("48;2;7;8;9"), "got: {out:?}");
  }

  #[test]
  fn bg_raw_palette256_emits_sgr_48_5_n() {
    let _g = TestGuard::new();
    reset_color_env();
    set_var("SHED_COLOR_MODE", "256");
    let painted = apply_bg_rgb_raw("x".paint(Style::new()), 0, 0, 255);
    let out = render_painted(painted);
    // Pure blue → 21.
    assert!(out.contains("48;5;21"), "got: {out:?}");
  }

  // ===================== fg vs bg never confused =====================

  #[test]
  fn fg_emits_38_not_48() {
    let _g = TestGuard::new();
    reset_color_env();
    set_var("SHED_COLOR_MODE", "truecolor");
    let out = render(apply_fg_rgb(Style::new(), 1, 2, 3));
    assert!(out.contains("38;2;"), "got: {out:?}");
    assert!(!out.contains("48;2;"), "got: {out:?}");
  }

  #[test]
  fn bg_emits_48_not_38() {
    let _g = TestGuard::new();
    reset_color_env();
    set_var("SHED_COLOR_MODE", "truecolor");
    let out = render(apply_bg_rgb(Style::new(), 1, 2, 3));
    assert!(out.contains("48;2;"), "got: {out:?}");
    assert!(!out.contains("38;2;"), "got: {out:?}");
  }
}
