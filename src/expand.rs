use std::collections::HashSet;
use std::str::FromStr;

use glob::Pattern;
use regex::Regex;

use crate::state::{read_vars, write_meta, write_vars, LogTab};
use crate::procio::{IoBuf, IoFrame, IoMode};
use crate::prelude::*;
use crate::parse::{Redir, RedirType};
use crate::parse::execute::exec_input;
use crate::parse::lex::{is_field_sep, is_hard_sep, LexFlags, LexStream, Tk, TkFlags, TkRule};
use crate::libsh::error::{ShErr, ShErrKind, ShResult};

/// Variable substitution marker
pub const VAR_SUB: char = '\u{fdd0}';
/// Double quote '"' marker
pub const DUB_QUOTE: char = '\u{fdd1}';
/// Single quote '\\'' marker
pub const SNG_QUOTE: char = '\u{fdd2}';
/// Tilde sub marker
pub const TILDE_SUB: char = '\u{fdd3}';
/// Subshell marker
pub const SUBSH: char = '\u{fdd4}';

impl Tk {
	/// Create a new expanded token
	///
	/// params
	/// tokens: A vector of raw tokens lexed from the expansion result
	/// span: The span of the original token that is being expanded
	/// flags: some TkFlags
	pub fn expand(self) -> ShResult<Self> {
		let flags = self.flags;
		let span = self.span.clone();
		let exp = Expander::new(self).expand()?;
		let class = TkRule::Expanded { exp };
		Ok(Self { class, span, flags, })
	}
	/// Perform word splitting
	pub fn get_words(&self) -> Vec<String> {
		match &self.class {
			TkRule::Expanded { exp } => exp.clone(),
			_ => vec![self.to_string()]
		}
	}
}

pub struct Expander {
	raw: String,
}

impl Expander {
	pub fn new(raw: Tk) -> Self {
		let unescaped = unescape_str(raw.span.as_str());
		Self { raw: unescaped }
	}
	pub fn expand(&mut self) -> ShResult<Vec<String>> {
		self.raw = self.expand_raw()?;
		if let Ok(glob_exp) = expand_glob(&self.raw) {
			if !glob_exp.is_empty() {
				self.raw = glob_exp;
			}
		}
		Ok(self.split_words())
	}
	pub fn split_words(&mut self) -> Vec<String> {
		let mut words = vec![];
		let mut chars = self.raw.chars();
		let mut cur_word = String::new();

		'outer: while let Some(ch) = chars.next() {
			match ch {
				DUB_QUOTE |
				SNG_QUOTE |
				SUBSH => {
					while let Some(q_ch) = chars.next() {
						match q_ch {
							_ if q_ch == ch => continue 'outer, // Isn't rust cool
							_ => cur_word.push(q_ch)
						}
					}
				}
				_ if is_field_sep(ch) => {
					words.push(mem::take(&mut cur_word));
				}
				_ => cur_word.push(ch)
			}
		}
		if !cur_word.is_empty() {
			words.push(cur_word);
		}
		words
	}
	pub fn expand_raw(&self) -> ShResult<String> {
		let mut chars = self.raw.chars().peekable();
		let mut result = String::new();
		let mut var_name = String::new();
		let mut in_brace = false;
		flog!(INFO, self.raw);

		while let Some(ch) = chars.next() {
			match ch {
				TILDE_SUB => {
					let home = env::var("HOME").unwrap_or_default();
					result.push_str(&home);
				}
				VAR_SUB => {
					while let Some(ch) = chars.next() {
						flog!(INFO,ch);
						flog!(INFO,var_name);
						match ch {
							SUBSH if var_name.is_empty() => {
								let mut subsh_body = String::new();
								while let Some(ch) = chars.next() {
									match ch {
										SUBSH => {
											break
										}
										_ => subsh_body.push(ch)
									}
								}
								let expanded = expand_cmd_sub(&subsh_body)?;
								flog!(INFO, expanded);
								result.push_str(&expanded);
							}
							'{' if var_name.is_empty() => in_brace = true,
							'}' if in_brace => {
								flog!(DEBUG, var_name);
								let var_val = perform_param_expansion(&var_name)?;
								result.push_str(&var_val);
								var_name.clear();
								break
							}
							_ if in_brace => {
								var_name.push(ch)
							}
							_ if is_hard_sep(ch) || ch == DUB_QUOTE || ch == SUBSH || ch == '/' => {
								let var_val = read_vars(|v| v.get_var(&var_name));
								result.push_str(&var_val);
								result.push(ch);
								var_name.clear();
								break
							}
							_ => var_name.push(ch),
						}
					}
					if !var_name.is_empty() {
						let var_val = read_vars(|v| v.get_var(&var_name));
						result.push_str(&var_val);
						var_name.clear();
					}
				}
				_ => result.push(ch)
			}
		}
		Ok(result)
	}
}

pub fn expand_glob(raw: &str) -> ShResult<String> {
	let mut words = vec![];

	for entry in glob::glob(raw)
		.map_err(|_| ShErr::simple(ShErrKind::SyntaxErr, "Invalid glob pattern"))? {
		let entry = entry
			.map_err(|_| ShErr::simple(ShErrKind::SyntaxErr, "Invalid filename found in glob"))?;

		words.push(entry.to_str().unwrap().to_string())
	}
	Ok(words.join(" "))
}

/// Get the command output of a given command input as a String
pub fn expand_cmd_sub(raw: &str) -> ShResult<String> {
	flog!(DEBUG, "in expand_cmd_sub");
	flog!(DEBUG, raw);
	let (rpipe,wpipe) = IoMode::get_pipes();
	let cmd_sub_redir = Redir::new(wpipe, RedirType::Output);
	let mut cmd_sub_io_frame = IoFrame::from_redir(cmd_sub_redir);
	let mut io_buf = IoBuf::new(rpipe);

	match unsafe { fork()? } {
		ForkResult::Child => {
			if let Err(e) = cmd_sub_io_frame.redirect() {
				eprintln!("{e}");
				exit(1);
			}

			if let Err(e) = exec_input(raw.to_string()) {
				eprintln!("{e}");
				exit(1);
			}
			exit(0);
		}
		ForkResult::Parent { child } => {
			std::mem::drop(cmd_sub_io_frame); // Closes the write pipe
			let status = waitpid(child, Some(WtFlag::WSTOPPED))?;
			match status {
				WtStat::Exited(_, _) => {
					flog!(DEBUG, "filling buffer");
					io_buf.fill_buffer()?;
					flog!(DEBUG, "done");
					Ok(io_buf.as_str()?.trim().to_string())
				}
				_ => Err(ShErr::simple(ShErrKind::InternalErr, "Command sub failed"))
			}
		}
	}
}

/// Processes strings into intermediate representations that are more readable by the program
///
/// Clean up a single layer of escape characters, and then replace control characters like '$' with a non-character unicode representation that is unmistakable by the rest of the code
pub fn unescape_str(raw: &str) -> String {
	let mut chars = raw.chars().peekable();
	let mut result = String::new();
	let mut first_char = true;


	while let Some(ch) = chars.next() {
		flog!(DEBUG,result);
		match ch {
			'~' if first_char => {
				result.push(TILDE_SUB)
			}
			'\\' => {
				if let Some(next_ch) = chars.next() {
					result.push(next_ch)
				}
			}
			'(' => {
				result.push(SUBSH);
				let mut paren_count = 1;
				while let Some(subsh_ch) = chars.next() {
					match subsh_ch {
						'\\' => {
							result.push(subsh_ch);
							if let Some(next_ch) = chars.next() {
								result.push(next_ch)
							}
						}
						'$' if chars.peek() != Some(&'(') => result.push(VAR_SUB),
						'(' => {
							paren_count += 1;
							result.push(subsh_ch)
						}
						')' => {
							paren_count -= 1;
							if paren_count == 0 {
								result.push(SUBSH);
								break
							} else {
								result.push(subsh_ch)
							}
						}
						_ => result.push(subsh_ch)
					}
				}
			}
			'"' => {
				result.push(DUB_QUOTE);
				while let Some(q_ch) = chars.next() {
					match q_ch {
						'\\' => {
							result.push(q_ch);
							if let Some(next_ch) = chars.next() {
								result.push(next_ch)
							}
						}
						'$' => {
							result.push(VAR_SUB);
							if chars.peek() == Some(&'(') {
								chars.next();
								let mut cmdsub_count = 1;
								result.push(SUBSH);
								while let Some(subsh_ch) = chars.next() {
									flog!(DEBUG, subsh_ch);
									match subsh_ch {
										'\\' => {
											result.push(subsh_ch);
											if let Some(next_ch) = chars.next() {
												result.push(next_ch)
											}
										}
										'$' if chars.peek() == Some(&'(') => {
											result.push(subsh_ch);
											cmdsub_count += 1;
										}
										')' => {
											cmdsub_count -= 1;
											if cmdsub_count <= 0 {
												result.push(SUBSH);
												break
											} else {
												result.push(subsh_ch);
											}
										}
										_ => result.push(subsh_ch),
									}
								}
							}
						}
						'"' => {
							result.push(DUB_QUOTE);
							break
						}
						_ => result.push(q_ch)
					}
				}
			}
			'\'' => {
				result.push(SNG_QUOTE);
				while let Some(q_ch) = chars.next() {
					match q_ch {
						'\'' => {
							result.push(SNG_QUOTE);
							break
						}
						_ => result.push(q_ch)
					}
				}
			}
			'$' => result.push(VAR_SUB),
			_ => result.push(ch)
		}
		first_char = false;
	}
	result
}

#[derive(Debug)]
pub enum ParamExp {
	Len, // #var_name
	DefaultUnsetOrNull(String), // :-
	DefaultUnset(String), // -
	SetDefaultUnsetOrNull(String), // :=
	SetDefaultUnset(String), // =
	AltSetNotNull(String), // :+
	AltNotNull(String), // +
	ErrUnsetOrNull(String), // :?
	ErrUnset(String), // ?
	Substr(usize), // :pos
	SubstrLen(usize,usize), // :pos:len
	RemShortestPrefix(String), // #pattern
	RemLongestPrefix(String), // ##pattern
	RemShortestSuffix(String), // %pattern
	RemLongestSuffix(String), // %%pattern
	ReplaceFirstMatch(String,String), // /search/replace
	ReplaceAllMatches(String,String), // //search/replace
	ReplacePrefix(String,String), // #search/replace
	ReplaceSuffix(String,String), // %search/replace
	VarNamesWithPrefix(String), // !prefix@ || !prefix*
	ExpandInnerVar(String), // !var
}

impl FromStr for ParamExp {
	type Err = ShErr;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		use ParamExp::*;

		let parse_err = || Err(ShErr::Simple {
			kind: ShErrKind::SyntaxErr,
			msg: "Invalid parameter expansion".into(),
			notes: vec![],
		});

		// Handle indirect var expansion: ${!var}
		if let Some(var) = s.strip_prefix('!') {
			if var.ends_with('*') || var.ends_with('@') {
				return Ok(VarNamesWithPrefix(var.to_string()));
			}
			return Ok(ExpandInnerVar(var.to_string()));
		}

		// Pattern removals
		if let Some(rest) = s.strip_prefix("##") {
			return Ok(RemLongestPrefix(rest.to_string()));
		} else if let Some(rest) = s.strip_prefix('#') {
			return Ok(RemShortestPrefix(rest.to_string()));
		}
		if let Some(rest) = s.strip_prefix("%%") {
			return Ok(RemLongestSuffix(rest.to_string()));
		} else if let Some(rest) = s.strip_prefix('%') {
			return Ok(RemShortestSuffix(rest.to_string()));
		}

		// Replacements
		if let Some(rest) = s.strip_prefix("//") {
			let mut parts = rest.splitn(2, '/');
			let pattern = parts.next().unwrap_or("");
			let repl = parts.next().unwrap_or("");
			return Ok(ReplaceAllMatches(pattern.to_string(), repl.to_string()));
		}
		if let Some(rest) = s.strip_prefix('/') {
			if let Some(rest) = rest.strip_prefix('%') {
				let mut parts = rest.splitn(2, '/');
				let pattern = parts.next().unwrap_or("");
				let repl = parts.next().unwrap_or("");
				return Ok(ReplaceSuffix(pattern.to_string(), repl.to_string()));
			} else if let Some(rest) = rest.strip_prefix('#') {
				let mut parts = rest.splitn(2, '/');
				let pattern = parts.next().unwrap_or("");
				let repl = parts.next().unwrap_or("");
				return Ok(ReplacePrefix(pattern.to_string(), repl.to_string()));
			} else {
				let mut parts = rest.splitn(2, '/');
				let pattern = parts.next().unwrap_or("");
				let repl = parts.next().unwrap_or("");
				return Ok(ReplaceFirstMatch(pattern.to_string(), repl.to_string()));
			}
		}

		// Fallback / assignment / alt
		if let Some(rest) = s.strip_prefix(":-") {
			return Ok(DefaultUnsetOrNull(rest.to_string()));
		} else if let Some(rest) = s.strip_prefix('-') {
			return Ok(DefaultUnset(rest.to_string()));
		} else if let Some(rest) = s.strip_prefix(":+") {
			return Ok(AltSetNotNull(rest.to_string()));
		} else if let Some(rest) = s.strip_prefix('+') {
			return Ok(AltNotNull(rest.to_string()));
		} else if let Some(rest) = s.strip_prefix(":=") {
			return Ok(SetDefaultUnsetOrNull(rest.to_string()));
		} else if let Some(rest) = s.strip_prefix('=') {
			return Ok(SetDefaultUnset(rest.to_string()));
		} else if let Some(rest) = s.strip_prefix(":?") {
			return Ok(ErrUnsetOrNull(rest.to_string()));
		} else if let Some(rest) = s.strip_prefix('?') {
			return Ok(ErrUnset(rest.to_string()));
		}

		// Substring
		if let Some((pos, len)) = parse_pos_len(s) {
			return Ok(match len {
				Some(l) => SubstrLen(pos, l),
				None => Substr(pos),
			});
		}

		parse_err()
	}
}

pub fn parse_pos_len(s: &str) -> Option<(usize, Option<usize>)> {
	let raw = s.strip_prefix(':')?;
	if let Some((start,len)) = raw.split_once(':') {
		Some((
			start.parse::<usize>().ok()?,
			len.parse::<usize>().ok(),
		))
	} else {
		Some((
			raw.parse::<usize>().ok()?,
			None,
		))
	}
}

pub fn perform_param_expansion(raw: &str) -> ShResult<String> {
	let vars = read_vars(|v| v.clone());
	let mut chars = raw.chars();
	let mut var_name = String::new();
	let mut rest = String::new();
	if raw.starts_with('#') {
		return Ok(vars.get_var(raw.strip_prefix('#').unwrap()).len().to_string())
	}

	while let Some(ch) = chars.next() {
		match ch {
			'!' |
			'#' |
			'%' |
			':' |
			'-' |
			'+' |
			'=' |
			'/' |
			'?' => {
				rest.push(ch);
				rest.push_str(&chars.collect::<String>());
				break
			}
			_ => var_name.push(ch)
		}
	}

	flog!(DEBUG,rest);
	if let Ok(expansion) = rest.parse::<ParamExp>() {
		flog!(DEBUG,expansion);
		match expansion {
			ParamExp::Len => unreachable!(),
			ParamExp::DefaultUnsetOrNull(default) => {
				if !vars.var_exists(&var_name) || vars.get_var(&var_name).is_empty() {
					Ok(default)
				} else {
					Ok(vars.get_var(&var_name))
				}
			}
			ParamExp::DefaultUnset(default) => {
				if !vars.var_exists(&var_name) {
					Ok(default)
				} else {
					Ok(vars.get_var(&var_name))
				}
			}
			ParamExp::SetDefaultUnsetOrNull(default) => {
				if !vars.var_exists(&var_name) || vars.get_var(&var_name).is_empty() {
					write_vars(|v| v.set_var(&var_name, &default, false));
					Ok(default)
				} else {
					Ok(vars.get_var(&var_name))
				}
			}
			ParamExp::SetDefaultUnset(default) => {
				if !vars.var_exists(&var_name) {
					write_vars(|v| v.set_var(&var_name, &default, false));
					Ok(default)
				} else {
					Ok(vars.get_var(&var_name))
				}
			}
			ParamExp::AltSetNotNull(alt) => {
				if vars.var_exists(&var_name) && !vars.get_var(&var_name).is_empty() {
					Ok(alt)
				} else {
					Ok("".into())
				}
			}
			ParamExp::AltNotNull(alt) => {
				if vars.var_exists(&var_name) {
					Ok(alt)
				} else {
					Ok("".into())
				}
			}
			ParamExp::ErrUnsetOrNull(err) => {
				if !vars.var_exists(&var_name) || vars.get_var(&var_name).is_empty() {
					Err(
						ShErr::Simple { kind: ShErrKind::ExecFail, msg: err, notes: vec![] }
					)
				} else {
					Ok(vars.get_var(&var_name))
				}
			}
			ParamExp::ErrUnset(err) => {
				if !vars.var_exists(&var_name) {
					Err(
						ShErr::Simple { kind: ShErrKind::ExecFail, msg: err, notes: vec![] }
					)
				} else {
					Ok(vars.get_var(&var_name))
				}
			}
			ParamExp::Substr(pos) => {
				let value = vars.get_var(&var_name);
				if let Some(substr) = value.get(pos..) {
					Ok(substr.to_string())
				} else {
					Ok(value)
				}
			}
			ParamExp::SubstrLen(pos, len) => {
				let value = vars.get_var(&var_name);
				let end = pos.saturating_add(len);
				if let Some(substr) = value.get(pos..end) {
					Ok(substr.to_string())
				} else {
					Ok(value)
				}
			}
			ParamExp::RemShortestPrefix(prefix) => {
				let value = vars.get_var(&var_name);
				let pattern = Pattern::new(&prefix).unwrap();
				for i in 0..=value.len() {
					let sliced = &value[..i];
					if pattern.matches(sliced) {
						return Ok(value[i..].to_string())
					}
				}
				Ok(value)
			}
			ParamExp::RemLongestPrefix(prefix) => {
				let value = vars.get_var(&var_name);
				let pattern = Pattern::new(&prefix).unwrap();
				for i in (0..=value.len()).rev() {
					let sliced = &value[..i];
					if pattern.matches(sliced) {
						return Ok(value[i..].to_string());
					}
				}
				Ok(value) // no match
			}
			ParamExp::RemShortestSuffix(suffix) => {
				let value = vars.get_var(&var_name);
				let pattern = Pattern::new(&suffix).unwrap();
				for i in (0..=value.len()).rev() {
					let sliced = &value[i..];
					if pattern.matches(sliced) {
						return Ok(value[..i].to_string());
					}
				}
				Ok(value)
			}
			ParamExp::RemLongestSuffix(suffix) => {
				let value = vars.get_var(&var_name);
				let pattern = Pattern::new(&suffix).unwrap();
				for i in 0..=value.len() {
					let sliced = &value[i..];
					if pattern.matches(sliced) {
						return Ok(value[..i].to_string());
					}
				}
				Ok(value)
			}
			ParamExp::ReplaceFirstMatch(search, replace) => {
				let value = vars.get_var(&var_name);
				let regex = glob_to_regex(&search, false); // unanchored pattern

				if let Some(mat) = regex.find(&value) {
					let before = &value[..mat.start()];
					let after = &value[mat.end()..];
					let result = format!("{}{}{}", before, replace, after);
					Ok(result)
				} else {
					Ok(value)
				}
			}
			ParamExp::ReplaceAllMatches(search, replace) => {
				let value = vars.get_var(&var_name);
				let regex = glob_to_regex(&search, false);
				let mut result = String::new();
				let mut last_match_end = 0;

				for mat in regex.find_iter(&value) {
					result.push_str(&value[last_match_end..mat.start()]);
					result.push_str(&replace);
					last_match_end = mat.end();
				}

				// Append the rest of the string
				result.push_str(&value[last_match_end..]);
				Ok(result)
			}
			ParamExp::ReplacePrefix(search, replace) => {
				let value = vars.get_var(&var_name);
				let pattern = Pattern::new(&search).unwrap();
				for i in (0..=value.len()).rev() {
					let sliced = &value[..i];
					if pattern.matches(sliced) {
						return Ok(format!("{}{}",replace,&value[i..]))
					}
				}
				Ok(value)
			}
			ParamExp::ReplaceSuffix(search, replace) => {
				let value = vars.get_var(&var_name);
				let pattern = Pattern::new(&search).unwrap();
				for i in (0..=value.len()).rev() {
					let sliced = &value[i..];
					if pattern.matches(sliced) {
						return Ok(format!("{}{}",&value[..i],replace))
					}
				}
				Ok(value)
			}
			ParamExp::VarNamesWithPrefix(prefix) => {
				let mut match_vars = vec![];
				for var in vars.vars().keys() {
					if var.starts_with(&prefix) {
						match_vars.push(var.clone())
					}
				}
				Ok(match_vars.join(" "))
			}
			ParamExp::ExpandInnerVar(var_name) => {
				let value = vars.get_var(&var_name);
				Ok(vars.get_var(&value))
			}
		}
	} else {
		Ok(vars.get_var(&var_name))
	}
}

fn glob_to_regex(glob: &str, anchored: bool) -> Regex {
	let mut regex = String::new();
	if anchored {
		regex.push('^');
	}
	for ch in glob.chars() {
		match ch {
			'*' => regex.push_str(".*"),
			'?' => regex.push('.'),
			'.' | '+' | '(' | ')' | '|' | '^' | '$' | '[' | ']' | '{' | '}' | '\\' => {
				regex.push('\\');
				regex.push(ch);
			}
			_ => regex.push(ch),
		}
	}
	if anchored {
		regex.push('$');
	}
	flog!(DEBUG, regex);
	Regex::new(&regex).unwrap()
}

#[derive(Debug)]
pub enum PromptTk {
	AsciiOct(i32),
	Text(String),
	AnsiSeq(String),
	VisGrp,
	UserSeq,
	Runtime,
	Weekday,
	Dquote,
	Squote,
	Return,
	Newline,
	Pwd,
	PwdShort,
	Hostname,
	HostnameShort,
	ShellName,
	Username,
	PromptSymbol,
	ExitCode,
	SuccessSymbol,
	FailureSymbol,
	JobCount
}

pub fn format_cmd_runtime(dur: std::time::Duration) -> String {
	const ETERNITY: u128 = f32::INFINITY as u128;
	let mut micros     = dur.as_micros();
	let mut millis     = 0;
	let mut seconds    = 0;
	let mut minutes    = 0;
	let mut hours      = 0;
	let mut days       = 0;
	let mut weeks      = 0;
	let mut months     = 0;
	let mut years      = 0;
	let mut decades    = 0;
	let mut centuries  = 0;
	let mut millennia  = 0;
	let mut epochs     = 0;
	let mut aeons      = 0;
	let mut eternities = 0;

	if micros >= 1000 {
		millis = micros / 1000;
		micros %= 1000;
	}
	if millis >= 1000 {
		seconds = millis / 1000;
		millis %= 1000;
	}
	if seconds >= 60 {
		minutes = seconds / 60;
		seconds %= 60;
	}
	if minutes >= 60 {
		hours = minutes / 60;
		minutes %= 60;
	}
	if hours >= 24 {
		days = hours / 24;
		hours %= 24;
	}
	if days >= 7 {
		weeks = days / 7;
		days %= 7;
	}
	if weeks >= 4 {
		months = weeks / 4;
		weeks %= 4;
	}
	if months >= 12 {
		years = months / 12;
		weeks %= 12;
	}
	if years >= 10 {
		decades = years / 10;
		years %= 10;
	}
	if decades >= 10 {
		centuries = decades / 10;
		decades %= 10;
	}
	if centuries >= 10 {
		millennia = centuries / 10;
		centuries %= 10;
	}
	if millennia >= 1000 {
		epochs = millennia / 1000;
		millennia %= 1000;
	}
	if epochs >= 1000 {
		aeons = epochs / 1000;
		epochs %= aeons;
	}
	if aeons == ETERNITY {
		eternities = aeons / ETERNITY;
		aeons %= ETERNITY;
	}

	// Format the result
	let mut result = Vec::new();
	if eternities > 0 {
		let mut string = format!("{} eternit", eternities);
		if eternities > 1 {
			string.push_str("ies");
		} else {
			string.push('y');
		}
		result.push(string)
	}
	if aeons > 0 {
		let mut string = format!("{} aeon", aeons);
		if aeons > 1 {
			string.push('s')
		}
		result.push(string)
	}
	if epochs > 0 {
		let mut string = format!("{} epoch", epochs);
		if epochs > 1 {
			string.push('s')
		}
		result.push(string)
	}
	if millennia > 0 {
		let mut string = format!("{} millenni", millennia);
		if millennia > 1 {
			string.push_str("um")
		} else {
			string.push('a')
		}
		result.push(string)
	}
	if centuries > 0 {
		let mut string = format!("{} centur", centuries);
		if centuries > 1 {
			string.push_str("ies")
		} else {
			string.push('y')
		}
		result.push(string)
	}
	if decades > 0 {
		let mut string = format!("{} decade", decades);
		if decades > 1 {
			string.push('s')
		}
		result.push(string)
	}
	if years > 0 {
		let mut string = format!("{} year", years);
		if years > 1 {
			string.push('s')
		}
		result.push(string)
	}
	if months > 0 {
		let mut string = format!("{} month", months);
		if months > 1 {
			string.push('s')
		}
		result.push(string)
	}
	if weeks > 0 {
		let mut string = format!("{} week", weeks);
		if weeks > 1 {
			string.push('s')
		}
		result.push(string)
	}
	if days > 0 {
		let mut string = format!("{} day", days);
		if days > 1 {
			string.push('s')
		}
		result.push(string)
	}
	if hours > 0 {
		let string = format!("{}h", hours);
		result.push(string);
	}
	if minutes > 0 {
		let string = format!("{}m", minutes);
		result.push(string);
	}
	if seconds > 0 {
		let string = format!("{}s", seconds);
		result.push(string);
	}
	if millis > 0 {
		let string = format!("{}ms",millis);
		result.push(string);
	}
	if result.is_empty() && micros > 0 {
		let string = format!("{}µs",micros);
		result.push(string);
	}

	result.join(" ")
}

fn tokenize_prompt(raw: &str) -> Vec<PromptTk> {
	let mut chars = raw.chars().peekable();
	let mut tk_text = String::new();
	let mut tokens = vec![];

	while let Some(ch) = chars.next() {
		match ch {
			'\\' => {
				// Push any accumulated text as a token
				if !tk_text.is_empty() {
					tokens.push(PromptTk::Text(std::mem::take(&mut tk_text)));
				}

				// Handle the escape sequence
				if let Some(ch) = chars.next() {
					match ch {
						'w' => tokens.push(PromptTk::Pwd),
						'W' => tokens.push(PromptTk::PwdShort),
						'h' => tokens.push(PromptTk::Hostname),
						'H' => tokens.push(PromptTk::HostnameShort),
						's' => tokens.push(PromptTk::ShellName),
						'u' => tokens.push(PromptTk::Username),
						'$' => tokens.push(PromptTk::PromptSymbol),
						'n' => tokens.push(PromptTk::Text("\n".into())),
						'r' => tokens.push(PromptTk::Text("\r".into())),
						'T' => tokens.push(PromptTk::Runtime),
						'\\' => tokens.push(PromptTk::Text("\\".into())),
						'"' => tokens.push(PromptTk::Text("\"".into())),
						'\'' => tokens.push(PromptTk::Text("'".into())),
						'e' => {
							if chars.next() == Some('[') {
								let mut params = String::new();

								// Collect parameters and final character
								while let Some(ch) = chars.next() {
									match ch {
										'0'..='9' | ';' | '?' | ':' => params.push(ch), // Valid parameter characters
										'A'..='Z' | 'a'..='z' => { // Final character (letter)
											params.push(ch);
											break;
										}
										_ => {
											// Invalid character in ANSI sequence
											tokens.push(PromptTk::Text(format!("\x1b[{params}")));
											break;
										}
									}
								}

								tokens.push(PromptTk::AnsiSeq(format!("\x1b[{params}")));
							} else {
								// Handle case where 'e' is not followed by '['
								tokens.push(PromptTk::Text("\\e".into()));
							}
						}
						'0'..='7' => {
							// Handle octal escape
							let mut octal_str = String::new();
							octal_str.push(ch);

							// Collect up to 2 more octal digits
							for _ in 0..2 {
								if let Some(&next_ch) = chars.peek() {
									if ('0'..='7').contains(&next_ch) {
										octal_str.push(chars.next().unwrap());
									} else {
										break;
									}
								} else {
									break;
								}
							}

							// Parse the octal string into an integer
							if let Ok(octal) = i32::from_str_radix(&octal_str, 8) {
								tokens.push(PromptTk::AsciiOct(octal));
							} else {
								// Fallback: treat as raw text
								tokens.push(PromptTk::Text(format!("\\{octal_str}")));
							}
						}
						_ => {
							// Unknown escape sequence: treat as raw text
							tokens.push(PromptTk::Text(format!("\\{ch}")));
						}
					}
				} else {
					// Handle trailing backslash
					tokens.push(PromptTk::Text("\\".into()));
				}
			}
			_ => {
				// Accumulate non-escape characters
				tk_text.push(ch);
			}
		}
	}

	// Push any remaining text as a token
	if !tk_text.is_empty() {
		tokens.push(PromptTk::Text(tk_text));
	}

	tokens
}

pub fn expand_prompt(raw: &str) -> ShResult<String> {
	let mut tokens = tokenize_prompt(raw).into_iter();
	let mut result = String::new();

	while let Some(token) = tokens.next() {
		match token {
			PromptTk::AsciiOct(_) => todo!(),
			PromptTk::Text(txt) => result.push_str(&txt),
			PromptTk::AnsiSeq(params) => result.push_str(&params),
			PromptTk::Runtime => {
				if let Some(runtime) = write_meta(|m| m.stop_timer()) {
					let runtime_fmt = format_cmd_runtime(runtime);
					result.push_str(&runtime_fmt);
				}
			}
			PromptTk::Pwd => {
				let mut pwd = std::env::var("PWD")?;
				let home = std::env::var("HOME")?;
				if pwd.starts_with(&home) {
					pwd = pwd.replacen(&home, "~", 1);
				}
				result.push_str(&pwd);
			}
			PromptTk::PwdShort => {
				let mut path = std::env::var("PWD")?;
				let home = std::env::var("HOME")?;
				if path.starts_with(&home) {
					path = path.replacen(&home, "~", 1);
				}
				let pathbuf = PathBuf::from(&path);
				let mut segments = pathbuf.iter().count();
				let mut path_iter = pathbuf.iter();
				while segments > 4 {
					path_iter.next();
					segments -= 1;
				}
				let path_rebuilt: PathBuf = path_iter.collect();
				let mut path_rebuilt = path_rebuilt.to_str().unwrap().to_string();
				if path_rebuilt.starts_with(&home) {
					path_rebuilt = path_rebuilt.replacen(&home, "~", 1);
				}
				result.push_str(&path_rebuilt);
			}
			PromptTk::Hostname => {
				let hostname = std::env::var("HOSTNAME")?;
				result.push_str(&hostname);
			}
			PromptTk::HostnameShort => todo!(),
			PromptTk::ShellName => result.push_str("fern"),
			PromptTk::Username => {
				let username = std::env::var("USER")?;
				result.push_str(&username);
			}
			PromptTk::PromptSymbol => {
				let uid = std::env::var("UID")?;
				let symbol = if &uid == "0" {
					'#'
				} else {
					'$'
				};
				result.push(symbol);
			}
			PromptTk::ExitCode => todo!(),
			PromptTk::SuccessSymbol => todo!(),
			PromptTk::FailureSymbol => todo!(),
			PromptTk::JobCount => todo!(),
			_ => unimplemented!()
		}
	}

	Ok(result)
}

/// Expand aliases in the given input string
///
/// Recursively calls itself until all aliases are expanded
pub fn expand_aliases(input: String, mut already_expanded: HashSet<String>, log_tab: &LogTab) -> String {
	let mut result = input.clone();
	let tokens: Vec<_> = LexStream::new(Arc::new(input), LexFlags::empty()).collect();
	let mut expanded_this_iter: Vec<String> = vec![];

	for token_result in tokens.into_iter().rev() {
		let Ok(tk) = token_result else { continue };

		if !tk.flags.contains(TkFlags::IS_CMD) { continue }
		if tk.flags.contains(TkFlags::KEYWORD) { continue }

		let raw_tk = tk.span.as_str().to_string();

		if already_expanded.contains(&raw_tk) { continue }

		if let Some(alias) = log_tab.get_alias(&raw_tk) {
			result.replace_range(tk.span.range(), &alias);
			expanded_this_iter.push(raw_tk);
		}
	}

	if expanded_this_iter.is_empty() {
		result
	} else {
		already_expanded.extend(expanded_this_iter);
		expand_aliases(result, already_expanded, log_tab)
	}
}
