use std::iter::Peekable;
use std::path::PathBuf;
use std::str::Chars;

use itertools::Itertools;

use crate::expand::Expander;
use crate::libsh::error::ShResult;
use crate::parse::lex::TkFlags;
use crate::readline::editcmd::{
  Anchor, CmdFlags, EditCmd, LineAddr, Motion, MotionCmd, ReadSrc, RegisterName, To, Verb, VerbCmd, WriteDest
};
use crate::readline::editmode::{EditMode, ModeReport, ViInsert};
use crate::readline::history::History;
use crate::readline::keys::KeyEvent;
use crate::readline::linebuf::LineBuf;
use crate::{motion, sherr};
use crate::state::write_meta;
use crate::{bitflags, match_loop};
use crate::verb;

bitflags! {
  #[derive(Debug,Clone,Copy,PartialEq,Eq)]
  pub struct SubFlags: u16 {
    const GLOBAL           = 1 << 0; // g
    const CONFIRM          = 1 << 1; // c (probably not implemented)
    const IGNORE_CASE      = 1 << 2; // i
    const NO_IGNORE_CASE   = 1 << 3; // I
    const SHOW_COUNT       = 1 << 4; // n
    const PRINT_RESULT     = 1 << 5; // p
    const PRINT_NUMBERED   = 1 << 6; // #
    const PRINT_LEFT_ALIGN = 1 << 7; // l
  }
}

#[derive(Clone, Debug)]
struct ExEditor {
  buf: LineBuf,
  mode: ViInsert,
  history: History,
}

impl Default for ExEditor {
  fn default() -> Self {
    Self {
      buf: LineBuf::default(),
      mode: ViInsert::default(),
      history: History::new("ex_history").unwrap_or_else(|_| History::empty("ex_history")),
    }
  }
}

impl ExEditor {
  pub fn new(history: History, has_select: bool) -> Self {
		let mut buf = LineBuf::default();
		if has_select {
			buf = buf.with_initial("'<,'>", 6);
		}
    Self {
      history,
			buf,
      mode: ViInsert::default(),
    }
  }
  pub fn clear(&mut self) {
    *self = Self::default()
  }
  pub fn should_grab_history(&mut self, cmd: &EditCmd) -> bool {
    cmd.verb().is_none()
      && (cmd
        .motion()
        .is_some_and(|m| matches!(m, MotionCmd(_, Motion::LineUp)))
        && self.buf.start_of_line() == 0)
      || (cmd
        .motion()
        .is_some_and(|m| matches!(m, MotionCmd(_, Motion::LineDown)))
        && self.buf.on_last_line())
  }
  pub fn scroll_history(&mut self, cmd: EditCmd) {
    let count = &cmd.motion().unwrap().0;
    let motion = &cmd.motion().unwrap().1;
    let count = match motion {
      Motion::LineUp => -(*count as isize),
      Motion::LineDown => *count as isize,
      _ => unreachable!(),
    };
    let entry = self.history.scroll(count);
    if let Some(entry) = entry {
      let buf = std::mem::take(&mut self.buf);
      self.buf.set_buffer(entry.command().to_string());
      if self.history.pending.is_none() {
        self.history.pending = Some(buf);
      }
      self.buf.set_hint(None);
      self.buf.move_cursor_to_end();
    } else if let Some(pending) = self.history.pending.take() {
      self.buf = pending;
    }
  }
  pub fn handle_key(&mut self, key: KeyEvent) -> ShResult<()> {
    let Some(cmd) = self.mode.handle_key(key) else {
      return Ok(());
    };
    log::debug!("ExEditor got cmd: {:?}", cmd);
    if self.should_grab_history(&cmd) {
      log::debug!("Grabbing history for cmd: {:?}", cmd);
      self.scroll_history(cmd);
      return Ok(());
    }
    self.buf.exec_cmd(cmd)
  }
}

#[derive(Default, Clone, Debug)]
pub struct ViEx {
  pending_cmd: ExEditor,
}

impl ViEx {
  pub fn new(history: History, has_select: bool) -> Self {
    Self {
      pending_cmd: ExEditor::new(history, has_select),
    }
  }
}

impl EditMode for ViEx {
  // Ex mode can return errors, so we use this fallible method instead of the normal one
  fn handle_key_fallible(&mut self, key: KeyEvent) -> ShResult<Option<EditCmd>> {
    use crate::readline::keys::{KeyCode as C, KeyEvent as E, ModKeys as M};
    match key {
      E(C::Char('\r'), M::NONE) | E(C::Enter, M::NONE) => {
        let input = self.pending_cmd.buf.joined();
        let res = match parse_ex_input(&input) {
          Ok(cmd) => Ok(cmd),
          Err(e) => {
            let msg = e.unwrap_or(format!("Not an editor command: {}", &input));
            write_meta(|m| m.post_status_message(msg.clone()));
            Err(sherr!(ParseErr, "{msg}"))
          }
        };

				if let Some(hist) = self.history()
				&& let Err(e) = hist.push(input) {
					write_meta(|m| m.post_status_message(format!("Failed to save ex command to history: {e}")));
				}

				res
      }
      E(C::Char('C'), M::CTRL) => {
        self.pending_cmd.clear();
        Ok(None)
      }
      E(C::Esc, M::NONE) => Ok(Some(EditCmd {
        register: RegisterName::default(),
        verb: Some(verb!(Verb::NormalMode)),
        motion: None,
        flags: CmdFlags::empty(),
        raw_seq: "".into(),
      })),
      _ => self.pending_cmd.handle_key(key).map(|_| None),
    }
  }
  fn handle_key(&mut self, key: KeyEvent) -> Option<EditCmd> {
    let result = self.handle_key_fallible(key);
    result.ok().flatten()
  }
  fn is_repeatable(&self) -> bool {
    false
  }

  fn as_replay(&self) -> Option<super::CmdReplay> {
    None
  }

  fn editor(&mut self) -> Option<&mut LineBuf> {
    Some(&mut self.pending_cmd.buf)
  }

  fn history(&mut self) -> Option<&mut History> {
    Some(&mut self.pending_cmd.history)
  }

  fn cursor_style(&self) -> String {
    "\x1b[3 q".to_string()
  }

  fn pending_seq(&self) -> Option<String> {
    Some(self.pending_cmd.buf.joined())
  }

  fn pending_cursor(&self) -> Option<usize> {
    Some(self.pending_cmd.buf.cursor_to_flat())
  }

  fn move_cursor_on_undo(&self) -> bool {
		self.pending_cmd.mode.move_cursor_on_undo()
  }

  fn clamp_cursor(&self) -> bool {
		self.pending_cmd.mode.clamp_cursor()
  }

  fn hist_scroll_start_pos(&self) -> Option<To> {
    None
  }

  fn report_mode(&self) -> super::ModeReport {
    ModeReport::Ex
  }
}

#[derive(Debug,Clone)]
pub struct CharTracker<'a> {
	chars: Peekable<Chars<'a>>,
	pos: usize
}

impl<'a> CharTracker<'a> {
	pub fn new(s: &'a str) -> Self {
		Self { chars: s.chars().peekable(), pos: 0 }
	}
	pub fn peek(&mut self) -> Option<&char> {
		self.chars.peek()
	}
	pub fn pos(&self) -> usize {
		self.pos
	}
}

impl Iterator for CharTracker<'_> {
	type Item = char;

	fn next(&mut self) -> Option<Self::Item> {
		let ch = self.chars.next()?;
		self.pos += ch.len_utf8();
		Some(ch)
	}
}

impl<'a> itertools::PeekingNext for CharTracker<'a> {
	fn peeking_next<F>(&mut self, accept: F) -> Option<Self::Item>
	where
		Self: Sized,
		F: FnOnce(&Self::Item) -> bool
	{
		let ch = self.chars.peek().copied()?;
		accept(&ch).then(|| self.next()).flatten()
	}
}

pub fn parse_ex_input(raw: &str) -> Result<Option<EditCmd>, Option<String>> {
  let raw = raw.trim();
  if raw.is_empty() {
    return Ok(None);
  }
  let mut chars = CharTracker::new(raw);
	let mut motion = parse_ex_address(&mut chars)?.map(|m| motion!(m));
	log::debug!("Parsed motion: {:?}", motion);
	let verb = {
		if chars.peek() == Some(&'g') {
			let mut cmd_name = String::new();
			while let Some(ch) = chars.peek() {
				if ch.is_alphanumeric() {
					cmd_name.push(*ch);
					chars.next();
				} else {
					break
				}
			}
			if !"global".starts_with(&cmd_name) {
				return Err(None)
			}
			let Some(result) = parse_global(&mut chars,motion.as_ref().map(|mcmd| &mcmd.1))? else { return Ok(None) };
			motion = Some(motion!(result.0));
			Some(VerbCmd(1,result.1))
		} else {
			parse_ex_command(&mut chars)?.map(|v| verb!(v))
		}
	};
	if motion.is_none() && !matches!(verb, Some(VerbCmd(_,Verb::Write(_)))) {
		motion = Some(motion!(Motion::Line(LineAddr::Current)))
	}

  Ok(Some(EditCmd {
    register: RegisterName::default(),
    verb,
    motion,
    raw_seq: raw.to_string(),
    flags: CmdFlags::EXIT_CUR_MODE | CmdFlags::IS_EX_CMD,
  }))
}

pub fn parse_ex_address(chars: &mut CharTracker<'_>) -> Result<Option<Motion>,Option<String>> {
	if chars.peek() == Some(&'%') {
		chars.next();
		return Ok(Some(Motion::LineRange(LineAddr::Number(1), LineAddr::Last)))
	}

	let mut chars_clone = chars.clone();
	let Some(start) = parse_one_addr(&mut chars_clone)? else { return Ok(None) };
	*chars = chars_clone.clone();

	if let Some(&',') = chars.peek()
	&& let Some(end) = { chars_clone.next(); parse_one_addr(&mut chars_clone)? } {
		*chars = chars_clone;
		Ok(Some(Motion::LineRange(start, end)))
	} else {
		*chars = chars_clone;
		Ok(Some(Motion::Line(start)))
	}
}

pub fn parse_one_addr(chars: &mut CharTracker<'_>) -> Result<Option<LineAddr>,Option<String>> {
	let Some(first) = chars.next() else { return Ok(None) };
	match first {
		'0'..='9' => {
			let mut digits = String::new();
			digits.push(first);
			digits.extend(chars.peeking_take_while(|c| c.is_ascii_digit()));

			let number = digits.parse::<usize>()
				.map_err(|_| None)?;

			Ok(Some(LineAddr::Number(number)))
		}
		'\'' => {
			let Some(ch) = chars.next() else { return Err(Some("Expected mark name after ' in ex address".into())) };
			if !ch.is_ascii_lowercase() && !"<>[]^.'`".contains(ch) {
				return Err(Some(format!("Invalid mark name in ex address: {ch}")));
			}
			Ok(Some(LineAddr::Mark(ch)))
		}
		'+' | '-' => {
			let mut digits = String::new();
			digits.push(first);
			digits.extend(chars.peeking_take_while(|c| c.is_ascii_digit()));

			let number = digits.parse::<isize>()
				.map_err(|_| None)?;

			Ok(Some(LineAddr::Offset(number)))
		}
		'/' | '?' => {
			let mut pattern = String::new();
			while let Some(ch) = chars.next() {
				match ch {
					'\\' => {
						pattern.push('\\');
						if let Some(esc_ch) = chars.next() {
							pattern.push(esc_ch)
						}
					}
					_ if ch == first => break,
					_ => pattern.push(ch)
				}
			}
			match first {
				'/' => Ok(Some(LineAddr::Pattern(pattern))),
				'?' => Ok(Some(LineAddr::PatternRev(pattern))),
				_ => unreachable!()
			}
		}
		'.' => Ok(Some(LineAddr::Current)),
		'$' => Ok(Some(LineAddr::Last)),
		_ => Ok(None)
	}

}

/// Unescape shell command arguments
fn unescape_shell_cmd(cmd: &str) -> String {
  let mut result = String::new();
  let mut chars = cmd.chars().peekable();

  match_loop!(chars.next() => ch, {
    '\\' => {
      if let Some(&'"') = chars.peek() {
        chars.next();
        result.push('"');
      } else {
        result.push(ch);
      }
    }
    _ => result.push(ch),
  });

  result
}

pub fn parse_ex_command_name(chars: &mut CharTracker<'_>) -> String {
	log::debug!("Parsing ex command from: {}", chars.clone().collect::<String>());
  let mut cmd_name = String::new();

  match_loop!(chars.peek() => ch, {
    '!' if cmd_name.is_empty() || cmd_name == "normal" => {
      cmd_name.push(*ch);
      chars.next();
			break
    }
    _ if ch.is_alphanumeric() => {
      cmd_name.push(*ch);
      chars.next();
    }
    _ => break,
  });

	cmd_name
}

pub fn ex_command_name_is_valid(name: &str) -> bool {
	name == "!" ||
	"help".starts_with(name) ||
	name.starts_with("normal!") ||
	"delete".starts_with(name) ||
	"yank".starts_with(name) ||
	"put".starts_with(name) ||
	"quit".starts_with(name) ||
	"read".starts_with(name) ||
	"write".starts_with(name) ||
	"edit".starts_with(name) ||
	"substitute".starts_with(name) ||
	"global".starts_with(name)
}

pub fn parse_ex_command(chars: &mut CharTracker<'_>) -> Result<Option<Verb>, Option<String>> {
	log::debug!("Parsing ex command from: {}", chars.clone().collect::<String>());
  let cmd_name = parse_ex_command_name(chars);

	if cmd_name.is_empty() { return Ok(None) }
  match cmd_name.as_str() {
    "!" => {
      let cmd = chars.collect::<String>();
      let cmd = unescape_shell_cmd(&cmd);
      Ok(Some(Verb::ShellCmd(cmd)))
    }
    _ if "help".starts_with(&cmd_name) => {
      let cmd = "help ".to_string() + chars.collect::<String>().trim();
      log::debug!("Parsed help command: {}", cmd);
      Ok(Some(Verb::ShellCmd(cmd)))
    }
    _ if cmd_name.starts_with("normal!") => parse_normal(chars),
    _ if "delete".starts_with(&cmd_name) => Ok(Some(Verb::Delete)),
    _ if "yank".starts_with(&cmd_name) => Ok(Some(Verb::Yank)),
    _ if "put".starts_with(&cmd_name) => Ok(Some(Verb::Put(Anchor::After))),
    _ if "quit".starts_with(&cmd_name) => Ok(Some(Verb::Quit)),
    _ if "read".starts_with(&cmd_name) => parse_read(chars),
    _ if "write".starts_with(&cmd_name) => parse_write(chars),
    _ if "edit".starts_with(&cmd_name) => parse_edit(chars),
    _ if "substitute".starts_with(&cmd_name) => parse_substitute(chars),
    _ => Err(None),
  }
}

pub fn parse_normal(chars: &mut CharTracker<'_>) -> Result<Option<Verb>, Option<String>> {
	chars
		.peeking_take_while(|c| c.is_whitespace())
		.for_each(drop);

  let seq: String = chars.collect();
  Ok(Some(Verb::Normal(seq)))
}

pub fn parse_edit(chars: &mut CharTracker<'_>) -> Result<Option<Verb>, Option<String>> {
  chars
    .peeking_take_while(|c| c.is_whitespace())
    .for_each(drop);

  let arg: String = chars.collect();
  if arg.trim().is_empty() {
    return Err(Some("Expected file path after ':edit'".into()));
  }
  let arg_path = get_path(arg.trim())?;
  Ok(Some(Verb::Edit(arg_path)))
}

pub fn parse_read(chars: &mut CharTracker<'_>) -> Result<Option<Verb>, Option<String>> {
  chars
    .peeking_take_while(|c| c.is_whitespace())
    .for_each(drop);

  let is_shell_read = if chars.peek() == Some(&'!') {
    chars.next();
    true
  } else {
    false
  };
  let arg: String = chars.collect();

  if arg.trim().is_empty() {
    return Err(Some(
      "Expected file path or shell command after ':r'".into(),
    ));
  }

  if is_shell_read {
    Ok(Some(Verb::Read(ReadSrc::Cmd(arg))))
  } else {
    let arg_path = get_path(arg.trim())?;
    Ok(Some(Verb::Read(ReadSrc::File(arg_path))))
  }
}

fn get_path(path: &str) -> Result<PathBuf, Option<String>> {
  log::debug!("Expanding path: {}", path);
  let expanded = Expander::from_raw(path, TkFlags::empty())
    .map_err(|e| Some(format!("Error expanding path: {}", e)))?
    .expand()
    .map_err(|e| Some(format!("Error expanding path: {}", e)))?
    .join(" ");
  log::debug!("Expanded path: {}", expanded);
  Ok(PathBuf::from(&expanded))
}

pub fn parse_write(chars: &mut CharTracker<'_>) -> Result<Option<Verb>, Option<String>> {
  chars
    .peeking_take_while(|c| c.is_whitespace())
    .for_each(drop);

  let is_shell_write = chars.peek() == Some(&'!');
  if is_shell_write {
    chars.next(); // consume '!'
    let arg: String = chars.collect();
    return Ok(Some(Verb::Write(WriteDest::Cmd(arg))));
  }

  // Check for >>
  let mut append_check = chars.clone();
  let is_file_append = append_check.next() == Some('>') && append_check.next() == Some('>');
  if is_file_append {
    *chars = append_check;
  }

  let arg: String = chars.collect();
  let arg_path = get_path(arg.trim())?;

  let dest = if is_file_append {
    WriteDest::FileAppend(arg_path)
  } else {
    WriteDest::File(arg_path)
  };

  Ok(Some(Verb::Write(dest)))
}

pub fn parse_global(chars: &mut CharTracker<'_>, constraint: Option<&Motion>) -> Result<Option<(Motion,Verb)>,Option<String>> {
	let is_negated = if chars.peek() == Some(&'!') { chars.next(); true } else { false };

	chars.peeking_take_while(|c| c.is_whitespace()).for_each(drop); // Ignore whitespace

	let Some(delimiter) = chars.next() else {
		return Ok(Some((Motion::Null,Verb::RepeatGlobal)))
	};
	if delimiter.is_alphanumeric() {
		return Err(None)
	}
	let global_pat = parse_pattern(chars, delimiter)?;
	let Some(command) = parse_ex_command(chars)? else {
		return Err(Some("Expected a command after global pattern".into()))
	};
	let constraint = Box::new(constraint.cloned().unwrap_or(Motion::LineRange(LineAddr::Number(1),LineAddr::Last)));
	if is_negated {
		Ok(Some((Motion::NotGlobal(constraint,global_pat), command)))
	} else {
		Ok(Some((Motion::Global(constraint,global_pat), command)))
	}
}

pub fn parse_substitute(chars: &mut CharTracker<'_>) -> Result<Option<Verb>, Option<String>> {
  while chars.peek().is_some_and(|c| c.is_whitespace()) {
    chars.next();
  } // Ignore whitespace

  let Some(delimiter) = chars.next() else {
    return Ok(Some(Verb::RepeatSubstitute));
  };
  if delimiter.is_alphanumeric() {
    return Err(None);
  }
  let old_pat = parse_pattern(chars, delimiter)?;
  let new_pat = parse_pattern(chars, delimiter)?;
  let mut flags = SubFlags::empty();
  match_loop!(chars.next() => ch, {
    'g' => flags |= SubFlags::GLOBAL,
    'i' => flags |= SubFlags::IGNORE_CASE,
    'I' => flags |= SubFlags::NO_IGNORE_CASE,
    'n' => flags |= SubFlags::SHOW_COUNT,
    _ => return Err(None),
  });
  Ok(Some(Verb::Substitute(old_pat, new_pat, flags)))
}

pub fn parse_pattern(
  chars: &mut CharTracker<'_>,
  delimiter: char,
) -> Result<String, Option<String>> {
  let mut pat = String::new();
  let mut closed = false;
  match_loop!(chars.next() => ch, {
    '\\' => {
      if chars.peek().is_some_and(|c| *c == delimiter) {
        // We escaped the delimiter, so we consume the escape char and continue
        pat.push(chars.next().unwrap());
        continue;
      } else {
        // The escape char is probably for the regex in the pattern
        pat.push(ch);
        if let Some(esc_ch) = chars.next() {
          pat.push(esc_ch)
        }
      }
    }
    _ if ch == delimiter => {
      closed = true;
      break;
    }
    _ => pat.push(ch),
  });
  if !closed {
    Err(Some("Unclosed pattern in ex command".into()))
  } else {
    Ok(pat)
  }
}
