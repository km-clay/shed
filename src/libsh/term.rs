use std::{fmt::Display, ops::BitOr};

pub trait Styled: Sized + Display {
  fn styled<S: Into<StyleSet>>(self, style: S) -> String {
    let styles: StyleSet = style.into();
    let reset = Style::Reset;
    format!("{styles}{self}{reset}")
  }
}

impl<T: Display> Styled for T {}

/// Enum representing a single ANSI style
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
  // Undoes all styles
  Reset,
  ResetFg,
  ResetBg,
  // Foreground Colors
  Black,
  Red,
  Green,
  Yellow,
  Blue,
  Magenta,
  Cyan,
  White,
  BrightBlack,
  BrightRed,
  BrightGreen,
  BrightYellow,
  BrightBlue,
  BrightMagenta,
  BrightCyan,
  BrightWhite,
  RGB(u8, u8, u8), // Custom foreground color

  // Background Colors
  BgBlack,
  BgRed,
  BgGreen,
  BgYellow,
  BgBlue,
  BgMagenta,
  BgCyan,
  BgWhite,
  BgBrightBlack,
  BgBrightRed,
  BgBrightGreen,
  BgBrightYellow,
  BgBrightBlue,
  BgBrightMagenta,
  BgBrightCyan,
  BgBrightWhite,
  BgRGB(u8, u8, u8), // Custom background color

  // Text Attributes
  Bold,
  Dim,
  Italic,
  Underline,
  Strikethrough,
  Reversed,
}

impl Display for Style {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Style::Reset => write!(f, "\x1b[0m"),
      Style::ResetFg => write!(f, "\x1b[39m"),
      Style::ResetBg => write!(f, "\x1b[49m"),

      // Foreground colors
      Style::Black => write!(f, "\x1b[30m"),
      Style::Red => write!(f, "\x1b[31m"),
      Style::Green => write!(f, "\x1b[32m"),
      Style::Yellow => write!(f, "\x1b[33m"),
      Style::Blue => write!(f, "\x1b[34m"),
      Style::Magenta => write!(f, "\x1b[35m"),
      Style::Cyan => write!(f, "\x1b[36m"),
      Style::White => write!(f, "\x1b[37m"),
      Style::BrightBlack => write!(f, "\x1b[90m"),
      Style::BrightRed => write!(f, "\x1b[91m"),
      Style::BrightGreen => write!(f, "\x1b[92m"),
      Style::BrightYellow => write!(f, "\x1b[93m"),
      Style::BrightBlue => write!(f, "\x1b[94m"),
      Style::BrightMagenta => write!(f, "\x1b[95m"),
      Style::BrightCyan => write!(f, "\x1b[96m"),
      Style::BrightWhite => write!(f, "\x1b[97m"),
      Style::RGB(r, g, b) => write!(f, "\x1b[38;2;{r};{g};{b}m"),

      // Background colors
      Style::BgBlack => write!(f, "\x1b[40m"),
      Style::BgRed => write!(f, "\x1b[41m"),
      Style::BgGreen => write!(f, "\x1b[42m"),
      Style::BgYellow => write!(f, "\x1b[43m"),
      Style::BgBlue => write!(f, "\x1b[44m"),
      Style::BgMagenta => write!(f, "\x1b[45m"),
      Style::BgCyan => write!(f, "\x1b[46m"),
      Style::BgWhite => write!(f, "\x1b[47m"),
      Style::BgBrightBlack => write!(f, "\x1b[100m"),
      Style::BgBrightRed => write!(f, "\x1b[101m"),
      Style::BgBrightGreen => write!(f, "\x1b[102m"),
      Style::BgBrightYellow => write!(f, "\x1b[103m"),
      Style::BgBrightBlue => write!(f, "\x1b[104m"),
      Style::BgBrightMagenta => write!(f, "\x1b[105m"),
      Style::BgBrightCyan => write!(f, "\x1b[106m"),
      Style::BgBrightWhite => write!(f, "\x1b[107m"),
      Style::BgRGB(r, g, b) => write!(f, "\x1b[48;2;{r};{g};{b}m"),

      // Text attributes
      Style::Bold => write!(f, "\x1b[1m"),
      Style::Dim => write!(f, "\x1b[2m"), // New
      Style::Italic => write!(f, "\x1b[3m"),
      Style::Underline => write!(f, "\x1b[4m"),
      Style::Strikethrough => write!(f, "\x1b[9m"), // New
      Style::Reversed => write!(f, "\x1b[7m"),
    }
  }
}

/// Struct representing a **set** of styles
#[derive(Debug, Default, Clone)]
pub struct StyleSet {
  styles: Vec<Style>,
}

impl StyleSet {
  pub fn new() -> Self {
    Self { styles: vec![] }
  }

  pub fn styles(&self) -> &[Style] {
    &self.styles
  }

  pub fn styles_mut(&mut self) -> &mut Vec<Style> {
    &mut self.styles
  }

  pub fn add_style(mut self, style: Style) -> Self {
    if !self.styles.contains(&style) {
      self.styles.push(style);
    }
    self
  }
}

impl Display for StyleSet {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    for style in &self.styles {
      style.fmt(f)?
    }
    Ok(())
  }
}

/// Allow OR (`|`) operator to combine multiple `Style` values into a `StyleSet`
impl BitOr for Style {
  type Output = StyleSet;

  fn bitor(self, rhs: Self) -> Self::Output {
    StyleSet::new().add_style(self).add_style(rhs)
  }
}

/// Allow OR (`|`) operator to combine `StyleSet` with `Style`
impl BitOr<Style> for StyleSet {
  type Output = StyleSet;

  fn bitor(self, rhs: Style) -> Self::Output {
    self.add_style(rhs)
  }
}

impl From<Style> for StyleSet {
  fn from(style: Style) -> Self {
    StyleSet::new().add_style(style)
  }
}
