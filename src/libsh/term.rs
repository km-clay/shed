use std::{fmt::Display, ops::BitOr};

/// Enum representing a single ANSI style
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
	Reset,
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
	Bold,
	Italic,
	Underline,
	Reversed,
}

impl Style {
	pub fn as_str(&self) -> &'static str {
		match self {
			Style::Reset => "\x1b[0m",
			Style::Black => "\x1b[30m",
			Style::Red => "\x1b[31m",
			Style::Green => "\x1b[32m",
			Style::Yellow => "\x1b[33m",
			Style::Blue => "\x1b[34m",
			Style::Magenta => "\x1b[35m",
			Style::Cyan => "\x1b[36m",
			Style::White => "\x1b[37m",
			Style::BrightBlack => "\x1b[90m",
			Style::BrightRed => "\x1b[91m",
			Style::BrightGreen => "\x1b[92m",
			Style::BrightYellow => "\x1b[93m",
			Style::BrightBlue => "\x1b[94m",
			Style::BrightMagenta => "\x1b[95m",
			Style::BrightCyan => "\x1b[96m",
			Style::BrightWhite => "\x1b[97m",
			Style::Bold => "\x1b[1m",
			Style::Italic => "\x1b[3m",
			Style::Underline => "\x1b[4m",
			Style::Reversed => "\x1b[7m",
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
		Self { styles: Vec::new() }
	}

	pub fn add(mut self, style: Style) -> Self {
		if !self.styles.contains(&style) {
			self.styles.push(style);
		}
		self
	}

	pub fn as_str(&self) -> String {
		self.styles.iter().map(|s| s.as_str()).collect::<String>()
	}
}

/// Allow OR (`|`) operator to combine multiple `Style` values into a `StyleSet`
impl BitOr for Style {
	type Output = StyleSet;

	fn bitor(self, rhs: Self) -> StyleSet {
		StyleSet::new().add(self).add(rhs)
	}
}

/// Allow OR (`|`) operator to combine `StyleSet` with `Style`
impl BitOr<Style> for StyleSet {
	type Output = StyleSet;

	fn bitor(self, rhs: Style) -> StyleSet {
		self.add(rhs)
	}
}

impl From<Style> for StyleSet {
	fn from(style: Style) -> Self {
		StyleSet::new().add(style)
	}
}

/// Apply styles to a string
pub fn style_text<Str: Display, Sty: Into<StyleSet>>(text: Str, styles: Sty) -> String {
	let styles = styles.into();
	format!("{}{}{}", styles.as_str(), text, Style::Reset.as_str())
}
