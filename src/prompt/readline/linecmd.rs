// Credit to Rustyline for enumerating these editor commands
// https://github.com/kkawakam/rustyline

use crate::libsh::error::{ShErr, ShErrKind, ShResult};
use crate::prelude::*;

pub type RepeatCount = u16;

#[derive(Default, Debug, Clone, Eq, PartialEq)]
pub struct ViCmdBuilder {
	verb_count: Option<u16>,
	verb: Option<Verb>,
	move_count: Option<u16>,
	movement: Option<Movement>,
}

impl ViCmdBuilder {
	pub fn new() -> Self {
		Self::default()
	}
	pub fn with_verb_count(self, verb_count: u16) -> Self {
		let Self { verb_count: _, verb, move_count, movement } = self;
		Self { verb_count: Some(verb_count), verb, move_count, movement }
	}
	pub fn with_verb(self, verb: Verb) -> Self {
		let Self { verb_count, verb: _, move_count, movement } = self;
		Self { verb_count, verb: Some(verb), move_count, movement }
	}
	pub fn with_move_count(self, move_count: u16) -> Self {
		let Self { verb_count, verb, move_count: _, movement } = self;
		Self { verb_count, verb, move_count: Some(move_count), movement }
	}
	pub fn with_movement(self, movement: Movement) -> Self {
		let Self { verb_count, verb, move_count, movement: _ } = self;
		Self { verb_count, verb, move_count, movement: Some(movement) }
	}
	pub fn append_digit(&mut self, digit: char) {
		// Convert char digit to a number (assuming ASCII '0'..'9')
		let digit_val = digit.to_digit(10).expect("digit must be 0-9") as u16;

		if self.verb.is_none() {
			// Append to verb_count
			self.verb_count = Some(match self.verb_count {
				Some(count) => count * 10 + digit_val,
				None => digit_val,
			});
		} else {
			// Append to move_count
			self.move_count = Some(match self.move_count {
				Some(count) => count * 10 + digit_val,
				None => digit_val,
			});
		}
	}
	pub fn is_unfinished(&self) -> bool {
		(self.verb.is_none() && self.movement.is_none()) ||
		(self.verb.is_none() && self.movement.as_ref().is_some_and(|m| m.needs_verb())) ||
		(self.movement.is_none() && self.verb.as_ref().is_some_and(|v| v.needs_movement()))
	}
	pub fn build(self) -> ShResult<ViCmd> {
		if self.is_unfinished() {
			flog!(ERROR, "Unfinished Builder: {:?}", self);
			return Err(
				ShErr::simple(ShErrKind::ReadlineErr, "called ViCmdBuilder::build() with an unfinished builder")
			)
		}
		let Self { verb_count, verb, move_count, movement } = self;
		let verb_count = verb_count.unwrap_or(1);
		let move_count = move_count.unwrap_or(if verb.is_none() { verb_count } else { 1 });
		let verb = verb.map(|v| VerbCmd { verb_count, verb: v });
		let movement = movement.map(|m| MoveCmd { move_count, movement: m });
		Ok(match (verb, movement) {
			(Some(v), Some(m)) => ViCmd::MoveVerb(v, m),
			(Some(v), None) => ViCmd::Verb(v),
			(None, Some(m)) => ViCmd::Move(m),
			(None, None) => unreachable!(),
		})
	}
}


#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ViCmd {
	MoveVerb(VerbCmd, MoveCmd),
	Verb(VerbCmd),
	Move(MoveCmd)
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct VerbCmd {
	pub verb_count: u16,
	pub verb: Verb
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct MoveCmd {
	pub move_count: u16,
	pub movement: Movement
}

#[derive(Debug, Clone, Eq, PartialEq)]
#[non_exhaustive]
pub enum Verb {
	/// `d`, `D` — delete motion or line
	Delete,
	/// `x`, `X` — delete one char, forward or back
	DeleteOne(Anchor),
	/// `c`, `C` — change (delete + insert)
	Change,
	/// `y`, `Y` — yank (copy)
	Yank,
	/// `r` — replace a single character
	ReplaceChar(char),
	/// `s` or `S` — substitute (change + single char or line)
	Substitute,
	/// `~` — swap case
	ToggleCase,
	/// `u` — undo
	Undo,
	/// `.` — repeat last edit
	RepeatLast,
	/// `p`, `P` — paste
	Put(Anchor),
	/// `R` — overwrite characters
	OverwriteMode,
	/// `i`, `a`, `I`, `A`, `o`, `O` — insert/append text
	InsertMode,
	/// `J` — join lines
	JoinLines,
	InsertChar(char),
	Indent,
	Dedent
}

impl Verb {
	pub fn needs_movement(&self) -> bool {
		match self {
			Verb::DeleteOne(_) |
			Verb::InsertMode |
			Verb::JoinLines |
			Verb::ToggleCase |
			Verb::OverwriteMode |
			Verb::Substitute |
			Verb::Put(_) |
			Verb::Undo |
			Verb::RepeatLast |
			Verb::Dedent |
			Verb::Indent |
			Verb::InsertChar(_) |
			Verb::ReplaceChar(_) => false,
			Verb::Delete |
			Verb::Change |
			Verb::Yank => true
		}
	}
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum Movement {
	/// Whole current line (not really a movement but a range)
	WholeLine,
	TextObj(TextObj, Bound),
	BeginningOfFirstWord,
	/// beginning-of-line
	BeginningOfLine,
	/// end-of-line
	EndOfLine,
	/// backward-word, vi-prev-word
	BackwardWord(Word), // Backward until start of word
																	 /// forward-word, vi-end-word, vi-next-word
	ForwardWord(At, Word), // Forward until start/end of word
																			/// character-search, character-search-backward, vi-char-search
	CharSearch(CharSearch),
	/// vi-first-print
	ViFirstPrint,
	/// backward-char
	BackwardChar,
	/// forward-char
	ForwardChar,
	/// move to the same column on the previous line
	LineUp,
	/// move to the same column on the next line
	LineDown,
	/// Whole user input (not really a movement but a range)
	WholeBuffer,
	/// beginning-of-buffer
	BeginningOfBuffer,
	/// end-of-buffer
	EndOfBuffer,
	Null
}

impl Movement {
	pub fn needs_verb(&self) -> bool {
		match self {
			Self::WholeLine |
			Self::BeginningOfLine |
			Self::BeginningOfFirstWord |
			Self::EndOfLine |
			Self::BackwardWord(_) |
			Self::ForwardWord(_, _) |
			Self::CharSearch(_) |
			Self::ViFirstPrint |
			Self::BackwardChar |
			Self::ForwardChar |
			Self::LineUp |
			Self::LineDown |
			Self::WholeBuffer |
			Self::BeginningOfBuffer |
			Self::EndOfBuffer => false,
			Self::Null |
			Self::TextObj(_, _) => true
		}
	}
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum TextObj {
	/// `iw`, `aw` — inner word, around word
	Word,

	/// `is`, `as` — inner sentence, around sentence
	Sentence,

	/// `ip`, `ap` — inner paragraph, around paragraph
	Paragraph,

	/// `i"`, `a"` — inner/around double quotes
	DoubleQuote,
	/// `i'`, `a'`
	SingleQuote,
	/// `i\``, `a\``
	BacktickQuote,

	/// `i)`, `a)` — round parens
	Paren,
	/// `i]`, `a]`
	Bracket,
	/// `i}`, `a}`
	Brace,
	/// `i<`, `a<`
	Angle,

	/// `it`, `at` — HTML/XML tags (if you support it)
	Tag,

	/// Custom user-defined objects maybe?
	Custom(char),
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum Bound {
	Inside,
	Around
}

#[derive(Debug, Clone, Eq, PartialEq)]
#[non_exhaustive]
pub enum LineCmd {
    Abort,
    AcceptLine,
    BeginningOfHistory,
    CapitalizeWord,
    ClearScreen,
    Complete,
    CompleteBackward,
    CompleteHint,
    DowncaseWord,
    EndOfFile,
    EndOfHistory,
    ForwardSearchHistory,
    HistorySearchBackward,
    HistorySearchForward,
    Insert(String),
    Interrupt,
    Move(Movement),
    NextHistory,
    Noop,
    Overwrite(char),
    PreviousHistory,
    QuotedInsert,
    Repaint,
    ReverseSearchHistory,
    Suspend,
    TransposeChars,
    TransposeWords,
    YankPop,
    LineUpOrPreviousHistory,
    LineDownOrNextHistory,
    Newline,
    AcceptOrInsertLine { accept_in_the_middle: bool },
    /// 🧵 New: vi-style editing command
    ViCmd(ViCmd),
    /// unknown/unmapped key
    Unknown,
		Null,
}
impl LineCmd {
	pub fn backspace() -> Self {
		let cmd = ViCmdBuilder::new()
			.with_verb(Verb::DeleteOne(Anchor::Before))
			.build()
			.unwrap();
		Self::ViCmd(cmd)
	}
	const fn is_repeatable_change(&self) -> bool {
		matches!(
			*self,
			Self::Insert(..)
			| Self::ViCmd(..)
		)
	}

	const fn is_repeatable(&self) -> bool {
		match *self {
			Self::Move(_) => true,
			_ => self.is_repeatable_change(),
		}
	}
}

#[derive(Debug, Clone, Eq, PartialEq, Copy)]
pub enum At {
	Start,
	BeforeEnd,
	AfterEnd
}

#[derive(Debug, Clone, Eq, PartialEq, Copy)]
pub enum Anchor {
	After,
	Before
}

#[derive(Debug, Clone, Eq, PartialEq, Copy)]
pub enum CharSearch {
	FindFwd(char),
	FwdTo(char),
	FindBkwd(char),
	BkwdTo(char)
}

#[derive(Debug, Clone, Eq, PartialEq, Copy)]
pub enum Word {
	Big,
	Normal
}


const fn repeat_count(previous: RepeatCount, new: Option<RepeatCount>) -> RepeatCount {
	match new {
		Some(n) => n,
		None => previous,
	}
}

#[derive(Default,Debug,Clone,Copy,PartialEq,Eq,PartialOrd,Ord)]
pub enum InputMode {
	Normal,
	#[default]
	Insert,
	Visual,
	Replace
}
