use super::register::{append_register, read_register, write_register};

//TODO: write tests that take edit results and cursor positions from actual neovim edits and test them against the behavior of this editor

#[derive(Clone,Copy,Debug)]
pub struct RegisterName {
	name: Option<char>,
	count: usize,
	append: bool
}

impl RegisterName {
	pub fn new(name: Option<char>, count: Option<usize>) -> Self {
		let Some(ch) = name else {
			return Self::default()
		};

		let append = ch.is_uppercase();
		let name = ch.to_ascii_lowercase();
		Self {
			name: Some(name),
			count: count.unwrap_or(1),
			append
		}
	}
	pub fn name(&self) -> Option<char> {
		self.name
	}
	pub fn is_append(&self) -> bool {
		self.append
	}
	pub fn count(&self) -> usize {
		self.count
	}
	pub fn write_to_register(&self, buf: String) {
		if self.append {
			append_register(self.name, buf);
		} else {
			write_register(self.name, buf);
		}
	}
	pub fn read_from_register(&self) -> Option<String> {
		read_register(self.name)
	}
}

impl Default for RegisterName {
	fn default() -> Self {
		Self {
			name: None,
			count: 1,
			append: false
		}
	}
}

#[derive(Clone,Default,Debug)]
pub struct ViCmd {
	pub register: RegisterName,
	pub verb: Option<VerbCmd>,
	pub motion: Option<MotionCmd>,
	pub raw_seq: String, 
}

impl ViCmd {
	pub fn new() -> Self {
		Self::default()
	}
	pub fn set_motion(&mut self, motion: MotionCmd) {
		self.motion = Some(motion)
	}
	pub fn set_verb(&mut self, verb: VerbCmd) {
		self.verb = Some(verb)
	}
	pub fn verb(&self) -> Option<&VerbCmd> {
		self.verb.as_ref()
	}
	pub fn motion(&self) -> Option<&MotionCmd> {
		self.motion.as_ref()
	}
	pub fn verb_count(&self) -> usize {
		self.verb.as_ref().map(|v| v.0).unwrap_or(1)
	}
	pub fn motion_count(&self) -> usize {
		self.motion.as_ref().map(|m| m.0).unwrap_or(1)
	}
	pub fn is_repeatable(&self) -> bool {
		self.verb.as_ref().is_some_and(|v| v.1.is_repeatable())
	}
	pub fn is_cmd_repeat(&self) -> bool {
		self.verb.as_ref().is_some_and(|v| matches!(v.1,Verb::RepeatLast))
	}
	pub fn is_motion_repeat(&self) -> bool {
		self.motion.as_ref().is_some_and(|m| matches!(m.1,Motion::RepeatMotion | Motion::RepeatMotionRev))
	}
	pub fn is_char_search(&self) -> bool {
		self.motion.as_ref().is_some_and(|m| matches!(m.1, Motion::CharSearch(..)))
	}
	pub fn should_submit(&self) -> bool {
		self.verb.as_ref().is_some_and(|v| matches!(v.1, Verb::AcceptLineOrNewline))
	}
	pub fn is_undo_op(&self) -> bool {
		self.verb.as_ref().is_some_and(|v| matches!(v.1, Verb::Undo | Verb::Redo))
	}
	pub fn is_line_motion(&self) -> bool {
		self.motion.as_ref().is_some_and(|m| {
			matches!(m.1, 
				Motion::LineUp |
				Motion::LineDown |
				Motion::LineUpCharwise |
				Motion::LineDownCharwise
			)
		})
	}
	/// If a ViCmd has a linewise motion, but no verb, we change it to charwise
	pub fn alter_line_motion_if_no_verb(&mut self) {
		if self.is_line_motion() && self.verb.is_none() {
			if let Some(motion) = self.motion.as_mut() {
				match motion.1 {
					Motion::LineUp => motion.1 = Motion::LineUpCharwise,
					Motion::LineDown => motion.1 = Motion::LineDownCharwise,
					_ => unreachable!()
				}
			}
		}
	}
	pub fn is_mode_transition(&self) -> bool {
		self.verb.as_ref().is_some_and(|v| {
			matches!(v.1, 
				Verb::Change |
				Verb::InsertMode |
				Verb::InsertModeLineBreak(_) |
				Verb::NormalMode |
				Verb::VisualModeSelectLast |
				Verb::VisualMode |
				Verb::ReplaceMode
			)
		})
	}
}

#[derive(Clone,Debug)]
pub struct VerbCmd(pub usize,pub Verb);
#[derive(Clone,Debug)]
pub struct MotionCmd(pub usize,pub Motion);

impl MotionCmd {
	pub fn invert_char_motion(self) -> Self {
		let MotionCmd(count,Motion::CharSearch(dir, dest, ch)) = self else {
			unreachable!()
		};
		let new_dir = match dir {
			Direction::Forward => Direction::Backward,
			Direction::Backward => Direction::Forward,
		};
		MotionCmd(count,Motion::CharSearch(new_dir, dest, ch))
	}
}

#[derive(Debug, Clone, Eq, PartialEq)]
#[non_exhaustive]
pub enum Verb {
	Delete,
	Change,
	Yank,
	Rot13, // lol
	ReplaceChar(char),
	ToggleCase,
	ToLower,
	ToUpper,
	Complete,
	CompleteBackward,
	Undo,
	Redo,
	RepeatLast,
	Put(Anchor),
	ReplaceMode,
	InsertMode,
	InsertModeLineBreak(Anchor),
	NormalMode,
	VisualMode,
	VisualModeLine,
	VisualModeBlock, // dont even know if im going to implement this
	VisualModeSelectLast,
	SwapVisualAnchor,
	JoinLines,
	InsertChar(char),
	Insert(String),
	Breakline(Anchor),
	Indent,
	Dedent,
	Equalize,
	AcceptLineOrNewline,
	EndOfFile
}


impl Verb {
	pub fn is_repeatable(&self) -> bool {
		matches!(self,
			Self::Delete |
			Self::Change |
			Self::ReplaceChar(_) |
			Self::ToLower |
			Self::ToUpper |
			Self::ToggleCase |
			Self::Put(_) |
			Self::ReplaceMode |
			Self::InsertModeLineBreak(_) |
			Self::JoinLines |
			Self::InsertChar(_) |
			Self::Insert(_) |
			Self::Breakline(_) |
			Self::Indent |
			Self::Dedent |
			Self::Equalize
		)
	}
	pub fn is_edit(&self) -> bool {
		matches!(self,
			Self::Delete |
			Self::Change |
			Self::ReplaceChar(_) |
			Self::ToggleCase |
			Self::ToLower |
			Self::ToUpper |
			Self::RepeatLast |
			Self::Put(_) |
			Self::ReplaceMode |
			Self::InsertModeLineBreak(_) |
			Self::JoinLines |
			Self::InsertChar(_) |
			Self::Insert(_) |
			Self::Breakline(_) |
			Self::Rot13 |
			Self::EndOfFile
		)
	}
	pub fn is_char_insert(&self) -> bool {
		matches!(self, 
			Self::Change |
			Self::InsertChar(_) |
			Self::ReplaceChar(_)
		)
	}
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum Motion {
	WholeLine,
	TextObj(TextObj, Bound),
	EndOfLastWord,
	BeginningOfFirstWord,
	BeginningOfLine,
	EndOfLine,
	WordMotion(To,Word,Direction),
	CharSearch(Direction,Dest,char),
	BackwardChar,
	ForwardChar,
	LineUp,
	LineUpCharwise,
	ScreenLineUp,
	ScreenLineUpCharwise,
	LineDown,
	LineDownCharwise,
	ScreenLineDown, 
	ScreenLineDownCharwise, 
	BeginningOfScreenLine,
	FirstGraphicalOnScreenLine,
	HalfOfScreen,
	HalfOfScreenLineText,
	WholeBuffer,
	BeginningOfBuffer,
	EndOfBuffer,
	ToColumn(usize),
	ToDelimMatch,
	ToBrace(Direction),
	ToBracket(Direction),
	ToParen(Direction),
	Range(usize,usize),
	RepeatMotion,
	RepeatMotionRev,
	Null
}

#[derive(Clone,Copy,PartialEq,Eq,Debug)]
pub enum MotionBehavior {
	Exclusive,
	Inclusive,
	Linewise
}

impl Motion {
	pub fn behavior(&self) -> MotionBehavior {
		if self.is_linewise() {
			MotionBehavior::Linewise
		} else if self.is_exclusive() {
			MotionBehavior::Exclusive
		} else {
			MotionBehavior::Inclusive
		}
	}
	pub fn is_exclusive(&self) -> bool {
		matches!(&self,
			Self::BeginningOfLine |
			Self::BeginningOfFirstWord |
			Self::BeginningOfScreenLine |
			Self::FirstGraphicalOnScreenLine |
			Self::LineDownCharwise |
			Self::LineUpCharwise |
			Self::ScreenLineUpCharwise |
			Self::ScreenLineDownCharwise |
			Self::ToColumn(_) |
			Self::TextObj(TextObj::ForwardSentence,_) |
			Self::TextObj(TextObj::BackwardSentence,_) |
			Self::TextObj(TextObj::ForwardParagraph,_) |
			Self::TextObj(TextObj::BackwardParagraph,_) |
			Self::CharSearch(Direction::Backward, _, _) |
			Self::WordMotion(To::Start,_,_) |
			Self::ToBrace(_) |
			Self::ToBracket(_) |
			Self::ToParen(_) |
			Self::ScreenLineDown |
			Self::ScreenLineUp |
			Self::Range(_, _)
		)
	}
	pub fn is_linewise(&self) -> bool {
		matches!(self,
			Self::WholeLine |
			Self::LineUp |
			Self::LineDown |
			Self::ScreenLineDown |
			Self::ScreenLineUp
		)
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
	ForwardSentence,
	BackwardSentence,

	/// `ip`, `ap` — inner paragraph, around paragraph
	ForwardParagraph,
	BackwardParagraph,

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

	/// `it`, `at` — HTML/XML tags 
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
