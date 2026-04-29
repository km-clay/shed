use crate::{match_loop, sherr, util::error::ShResult};
use crate::{readline::term::calc_str_width, write_term};
use std::fmt::Write;
use yansi::{Paint, Painted, Style};

pub const BOT_LEFT: &str = "\x1b[90m╰\x1b[0m";
pub const BOT_RIGHT: &str = "\x1b[90m╯\x1b[0m";
pub const TOP_LEFT: &str = "\x1b[90m╭\x1b[0m";
pub const TOP_RIGHT: &str = "\x1b[90m╮\x1b[0m";
pub const HOR_LINE: &str = "\x1b[90m─\x1b[0m";
pub const VERT_LINE: &str = "\x1b[90m│\x1b[0m";
pub const TREE_LEFT: &str = "\x1b[90m├\x1b[0m";
pub const TREE_RIGHT: &str = "\x1b[90m┤\x1b[0m";

/// A wrapper around yansi::Style. Defers application of text attributes like bold/italic.
#[derive(Clone, Debug, Default, Copy)]
pub struct PaletteEntry {
  style: Style,
  decorations: Decorations
}

impl PaletteEntry {
  pub fn new() -> Self {
    Self { style: Style::new().primary().on_primary(), decorations: Decorations::default() }
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
      s = s.strike()
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
pub fn pad_line(content: &str, fill: &str, right_border: &str, cols: usize) {
  let used = calc_str_width(content);
  let padding = cols.saturating_sub(used + 1);
  write_term!("{content}").ok();
  for _ in 0..padding {
    write_term!("{fill}").ok();
  }
  write_term!("{right_border}").ok();
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
          style = style.on_rgb(r,g,b);
        }
        _ => return Err(sherr!(ParseErr, "Unknown background color '{}' in color description", word)),
      }
    }

    hex if word.starts_with('#') => {
      let (r,g,b) = hex_to_rgb(hex)?;
      style = style.rgb(r,g,b);
    }

    _ => return Err(sherr!(ParseErr, "Unknown style '{}' in color description", word)),
  });

  Ok(PaletteEntry { style, decorations: decor })
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
          style = style.on_rgb(r,g,b);
        }
        _ => return Err(sherr!(ParseErr, "Unknown background color '{}' in color description", word)),
      }
    }

    hex if word.starts_with('#') => {
      let (r,g,b) = hex_to_rgb(hex)?;
      style = style.rgb(r,g,b);
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
