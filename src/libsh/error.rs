use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::fmt::Display;
use ariadne::Color;
use ariadne::{Report, ReportKind};
use rand::{RngExt, TryRng};

use crate::{
  libsh::term::{Style, Styled},
  parse::lex::{Span, SpanSource},
  prelude::*,
};

pub type ShResult<T> = Result<T, ShErr>;

pub struct ColorRng {
	last_color: Option<Color>,
}

impl ColorRng {
	fn get_colors() -> &'static [Color] {
		&[
			Color::Red,
			Color::Cyan,
			Color::Blue,
			Color::Green,
			Color::Yellow,
			Color::Magenta,
			Color::Fixed(208), // orange
			Color::Fixed(39),  // deep sky blue
			Color::Fixed(170), // orchid / magenta-pink
			Color::Fixed(76),  // chartreuse
			Color::Fixed(51),  // aqua
			Color::Fixed(226), // bright yellow
			Color::Fixed(99),  // slate blue
			Color::Fixed(214), // light orange
			Color::Fixed(48),  // spring green
			Color::Fixed(201), // hot pink
			Color::Fixed(81),  // steel blue
			Color::Fixed(220), // gold
			Color::Fixed(105), // medium purple
		]
	}
}

impl Iterator for ColorRng {
	type Item = Color;
	fn next(&mut self) -> Option<Self::Item> {
		let colors = Self::get_colors();
		let idx = rand::rngs::SysRng.try_next_u32().ok()? as usize % colors.len();
		Some(colors[idx])
	}
}

thread_local! {
	static COLOR_RNG: RefCell<ColorRng> = const { RefCell::new(ColorRng { last_color: None }) };
}

pub fn next_color() -> Color {
	COLOR_RNG.with(|rng| rng.borrow_mut().next().unwrap())
}

pub trait ShResultExt {
  fn blame(self, span: Span) -> Self;
  fn try_blame(self, span: Span) -> Self;
}

impl<T> ShResultExt for Result<T, ShErr> {
  /// Blame a span for an error
  fn blame(self, new_span: Span) -> Self {
		self.map_err(|e| e.blame(new_span))
  }
  /// Blame a span if no blame has been assigned yet
  fn try_blame(self, new_span: Span) -> Self {
		self.map_err(|e| e.try_blame(new_span))
  }
}

#[derive(Clone, Debug)]
pub struct Note {
  main: String,
  sub_notes: Vec<Note>,
  depth: usize,
}

impl Note {
  pub fn new(main: impl Into<String>) -> Self {
    Self {
      main: main.into(),
      sub_notes: vec![],
      depth: 0,
    }
  }

  pub fn with_sub_notes(self, new_sub_notes: Vec<impl Into<String>>) -> Self {
    let Self {
      main,
      mut sub_notes,
      depth,
    } = self;
    for raw_note in new_sub_notes {
      let mut note = Note::new(raw_note);
      note.depth = self.depth + 1;
      sub_notes.push(note);
    }
    Self {
      main,
      sub_notes,
      depth,
    }
  }
}

impl Display for Note {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let note = "note".styled(Style::Green);
    let main = &self.main;
    if self.depth == 0 {
      writeln!(f, "{note}: {main}")?;
    } else {
      let bar_break = "-".styled(Style::Cyan | Style::Bold);
      let indent = "  ".repeat(self.depth);
      writeln!(f, "  {indent}{bar_break} {main}")?;
    }

    for sub_note in &self.sub_notes {
      write!(f, "{sub_note}")?;
    }
    Ok(())
  }
}

#[derive(Debug)]
pub struct ShErr {
	kind: ShErrKind,
	src_span: Option<Span>,
	labels: Vec<ariadne::Label<Span>>,
	sources: Vec<SpanSource>,
	notes: Vec<String>
}

impl ShErr {
	pub fn new(kind: ShErrKind, span: Span) -> Self {
		Self { kind, src_span: Some(span), labels: vec![], sources: vec![], notes: vec![] }
	}
	pub fn simple(kind: ShErrKind, msg: impl Into<String>) -> Self {
		Self { kind, src_span: None, labels: vec![], sources: vec![], notes: vec![msg.into()] }
	}
	pub fn at(kind: ShErrKind, span: Span, msg: impl Into<String>) -> Self {
		let color = next_color();
		let src = span.span_source().clone();
		let msg: String = msg.into();
		Self::new(kind, span.clone())
			.with_label(src, ariadne::Label::new(span).with_color(color).with_message(msg))
	}
	pub fn labeled(self, span: Span, msg: impl Into<String>) -> Self {
		let color = next_color();
		let src = span.span_source().clone();
		let msg: String = msg.into();
		self.with_label(src, ariadne::Label::new(span).with_color(color).with_message(msg))
	}
	pub fn blame(self, span: Span) -> Self {
		let ShErr { kind, src_span: _, labels, sources, notes } = self;
		Self { kind, src_span: Some(span), labels, sources, notes }
	}
	pub fn try_blame(self, span: Span) -> Self {
		match self {
			ShErr { kind, src_span: None, labels, sources, notes } => Self { kind, src_span: Some(span), labels, sources, notes },
			_ => self
		}
	}
	pub fn kind(&self) -> &ShErrKind {
		&self.kind
	}
	pub fn rename(mut self, name: impl Into<String>) -> Self {
		if let Some(span) = self.src_span.as_mut() {
			span.rename(name.into());
		}
		self
	}
	pub fn with_label(self, source: SpanSource, label: ariadne::Label<Span>) -> Self {
		let ShErr { kind, src_span, mut labels, mut sources, notes } = self;
		sources.push(source);
		labels.push(label);
		Self { kind, src_span, labels, sources, notes }
	}
	pub fn with_context(self, ctx: VecDeque<(SpanSource, ariadne::Label<Span>)>) -> Self {
		let ShErr { kind, src_span, mut labels, mut sources, notes } = self;
		for (src, label) in ctx {
			sources.push(src);
			labels.push(label);
		}
		Self { kind, src_span, labels, sources, notes }
	}
	pub fn with_note(self, note: impl Into<String>) -> Self {
		let ShErr { kind, src_span, labels, sources, mut notes } = self;
		notes.push(note.into());
		Self { kind, src_span, labels, sources, notes }
	}
	pub fn build_report(&self) -> Option<Report<'_, Span>> {
		let span = self.src_span.as_ref()?;
		let mut report = Report::build(ReportKind::Error, span.clone())
			.with_config(ariadne::Config::default().with_color(true));
		let msg = if self.notes.is_empty() {
			self.kind.to_string()
		} else {
			format!("{} - {}", self.kind, self.notes.first().unwrap())
		};
		report = report.with_message(msg);

		for label in self.labels.clone() {
			report = report.with_label(label);
		}
		for note in &self.notes {
			report = report.with_note(note);
		}

		Some(report.finish())
	}
	fn collect_sources(&self) -> HashMap<SpanSource, String> {
		let mut source_map = HashMap::new();
		if let Some(span) = &self.src_span {
			let src = span.span_source().clone();
			source_map.entry(src.clone())
				.or_insert_with(|| src.content().to_string());
		}
		for src in &self.sources {
			source_map.entry(src.clone())
				.or_insert_with(|| src.content().to_string());
		}
		source_map
	}
	pub fn print_error(&self) {
		let default = || {
			eprintln!("{}", self.kind);
			for note in &self.notes {
				eprintln!("note: {note}");
			}
		};
		let Some(report) = self.build_report() else {
			return default();
		};

		let sources = self.collect_sources();
		let cache = ariadne::FnCache::new(move |src: &SpanSource| {
			sources.get(src)
				.cloned()
				.ok_or_else(|| format!("Failed to fetch source '{}'", src.name()))
		});
		if report.eprint(cache).is_err() {
			default();
		}
	}
}

impl Display for ShErr {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		if self.notes.is_empty() {
			write!(f, "{}", self.kind)
		} else {
			write!(f, "{} - {}", self.kind, self.notes.first().unwrap())
		}
	}
}

impl From<std::io::Error> for ShErr {
  fn from(e: std::io::Error) -> Self {
    let msg = std::io::Error::last_os_error();
    ShErr::simple(ShErrKind::IoErr(e.kind()), msg.to_string())
  }
}

impl From<std::env::VarError> for ShErr {
  fn from(value: std::env::VarError) -> Self {
    ShErr::simple(ShErrKind::InternalErr, value.to_string())
  }
}

impl From<Errno> for ShErr {
  fn from(value: Errno) -> Self {
    ShErr::simple(ShErrKind::Errno(value), value.to_string())
  }
}

#[derive(Debug, Clone)]
pub enum ShErrKind {
  IoErr(io::ErrorKind),
  InvalidOpt,
  SyntaxErr,
  ParseErr,
  InternalErr,
  ExecFail,
  HistoryReadErr,
  ResourceLimitExceeded,
  BadPermission,
  Errno(Errno),
  FileNotFound,
  CmdNotFound,
  ReadlineErr,

  // Not really errors, more like internal signals
  CleanExit(i32),
  FuncReturn(i32),
  LoopContinue(i32),
  LoopBreak(i32),
  ClearReadline,
  Null,
}

impl Display for ShErrKind {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let output = match self {
      Self::IoErr(e) => &format!("I/O Error: {e}"),
      Self::InvalidOpt => "Invalid option",
      Self::SyntaxErr => "Syntax Error",
      Self::ParseErr => "Parse Error",
      Self::InternalErr => "Internal Error",
      Self::HistoryReadErr => "History Parse Error",
      Self::ExecFail => "Execution Failed",
      Self::ResourceLimitExceeded => "Resource Limit Exceeded",
      Self::BadPermission => "Bad Permissions",
      Self::Errno(e) => &format!("Errno: {}", e.desc()),
      Self::FileNotFound => "File not found",
      Self::CmdNotFound => "Command not found",
      Self::CleanExit(_) => "",
      Self::FuncReturn(_) => "Syntax Error",
      Self::LoopContinue(_) => "Syntax Error",
      Self::LoopBreak(_) => "Syntax Error",
      Self::ReadlineErr => "Readline Error",
      Self::ClearReadline => "",
      Self::Null => "",
    };
    write!(f, "{output}")
  }
}
