use super::{linebuf::{TermChar, TermCharBuf}, register::{append_register, read_register, write_register}};

#[derive(Clone,Copy,Default,Debug)]
pub struct RegisterName {
	name: Option<char>,
	append: bool
}

impl RegisterName {
	pub fn name(&self) -> Option<char> {
		self.name
	}
	pub fn is_append(&self) -> bool {
		self.append
	}
	pub fn write_to_register(&self, buf: TermCharBuf) {
		if self.append {
			append_register(self.name, buf);
		} else {
			write_register(self.name, buf);
		}
	}
	pub fn read_from_register(&self) -> Option<TermCharBuf> {
		read_register(self.name)
	}
}

#[derive(Clone,Default,Debug)]
pub struct ViCmd {
	pub wants_register: bool, // Waiting for register character

	/// Register to read from/write to
	pub register_count: Option<u16>,
	pub register: RegisterName,

	/// Verb to perform
	pub verb_count: Option<u16>,
	pub verb: Option<Verb>,

	/// Motion to perform
	pub motion_count: Option<u16>,
	pub motion: Option<Motion>,

	/// Count digits are held here until we know what we are counting
	/// Once a register/verb/motion is set, the count is taken from here
	pub pending_count: Option<u16>,

	/// The actual keys the user typed for this command
	/// Maybe display this somewhere around the prompt later?
	/// Prompt escape sequence maybe?
	pub raw_seq: String, 
}

impl ViCmd {
	pub fn new() -> Self {
		Self::default()
	}
	pub fn set_register(&mut self, register: char) {
		let append = register.is_uppercase();
		let name = Some(register.to_ascii_lowercase());
		let reg_name = RegisterName { name, append };
		self.register = reg_name;
		self.register_count = self.pending_count.take();
		self.wants_register = false;
	}
	pub fn append_seq_char(&mut self, ch: char) {
		self.raw_seq.push(ch)
	}
	pub fn is_empty(&self) -> bool {
		!self.wants_register &&
		self.register.name.is_none() &&
		self.verb_count.is_none() &&
		self.verb.is_none() &&
		self.motion_count.is_none() &&
		self.motion.is_none()
	}
	pub fn set_verb(&mut self, verb: Verb) {
		self.verb = Some(verb);
		self.verb_count = self.pending_count.take();
	}
	pub fn set_motion(&mut self, motion: Motion) {
		self.motion = Some(motion);
		self.motion_count = self.pending_count.take();
	}
	pub fn register(&self) -> RegisterName {
		self.register
	}
	pub fn verb(&self) -> Option<&Verb> {
		self.verb.as_ref()
	}
	pub fn verb_count(&self) -> u16 {
		self.verb_count.unwrap_or(1)
	}
	pub fn motion(&self) -> Option<&Motion> {
		self.motion.as_ref()
	}
	pub fn motion_count(&self) -> u16 {
		self.motion_count.unwrap_or(1)
	}
	pub fn append_digit(&mut self, digit: char) {
		// Convert char digit to a number (assuming ASCII '0'..'9')
		let digit_val = digit.to_digit(10).expect("digit must be 0-9") as u16;
		self.pending_count = Some(match self.pending_count {
			Some(count) => count * 10 + digit_val,
			None => digit_val,
		});
	}
	pub fn is_building(&self) -> bool {
		matches!(self.verb, Some(Verb::Builder(_))) ||
		matches!(self.motion, Some(Motion::Builder(_))) ||
		self.wants_register
	}
	pub fn is_complete(&self) -> bool {
		!(
			(self.verb.is_none() && self.motion.is_none()) ||
			(self.verb.is_none() && self.motion.as_ref().is_some_and(|m| m.needs_verb())) ||
			(self.motion.is_none() && self.verb.as_ref().is_some_and(|v| v.needs_motion())) ||
			self.is_building()
		)
	}
	pub fn should_submit(&self) -> bool {
		self.verb.as_ref().is_some_and(|v| *v == Verb::AcceptLine)
	}
	pub fn is_mode_transition(&self) -> bool {
		self.verb.as_ref().is_some_and(|v| {
			matches!(*v, Verb::InsertMode | Verb::NormalMode | Verb::OverwriteMode | Verb::VisualMode)
		})
	}
}

#[derive(Debug, Clone, Eq, PartialEq)]
#[non_exhaustive]
pub enum Verb {
	Delete,
	DeleteChar(Anchor),
	Change,
	Yank,
	ReplaceChar(char),
	Substitute,
	ToggleCase,
	Complete,
	CompleteBackward,
	Undo,
	RepeatLast,
	Put(Anchor),
	OverwriteMode,
	InsertMode,
	NormalMode,
	VisualMode,
	JoinLines,
	InsertChar(TermChar),
	Insert(String),
	Breakline(Anchor),
	Indent,
	Dedent,
	AcceptLine,
	Builder(VerbBuilder),
	EndOfFile
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum VerbBuilder {
}

impl Verb {
	pub fn needs_motion(&self) -> bool {
		matches!(self, 
			Self::Indent |
			Self::Dedent |
			Self::Delete |
			Self::Change |
			Self::Yank
		)
	}
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum Motion {
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
	ForwardWord(To, Word), // Forward until start/end of word
																			/// character-search, character-search-backward, vi-char-search
	CharSearch(Direction,Dest,TermChar),
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
	/// beginning-of-register
	BeginningOfBuffer,
	/// end-of-register
	EndOfBuffer,
	Builder(MotionBuilder),
	Null
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum MotionBuilder {
	CharSearch(Option<Direction>,Option<Dest>,Option<char>),
	TextObj(Option<TextObj>,Option<Bound>)
}

impl Motion {
	pub fn needs_verb(&self) -> bool {
		matches!(self, Self::TextObj(_, _))
	}
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum Anchor {
	After,
	Before
}
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum TextObj {
	/// `iw`, `aw` — inner word, around word
	Word(Word),

	/// for stuff like 'dd'
	Line,

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

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum Word {
	Big,
	Normal
}
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Bound {
	Inside,
	Around
}

#[derive(Default, Debug, Clone, Copy, Eq, PartialEq)]
pub enum Direction {
	#[default]
	Forward,
	Backward
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Dest {
	On,
	Before,
	After
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum To {
	Start,
	End
}
