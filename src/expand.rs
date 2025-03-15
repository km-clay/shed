use crate::{libsh::error::{ShErr, ShErrKind}, parse::lex::{is_hard_sep, LexFlags, LexStream, Tk, Span, TkErr, TkFlags, TkState, TkRule}, prelude::*, state::read_vars};

/// Variable substitution marker
pub const VAR_SUB: char = '\u{fdd0}';

impl<'t> Tk<'t> {
	/// Create a new expanded token
	///
	/// params
	/// tokens: A vector of raw tokens lexed from the expansion result
	/// span: The span of the original token that is being expanded
	/// flags: some TkFlags
	pub fn expand(self, span: Span<'t>, flags: TkFlags) -> Self {
		let exp = Expander::new(self).expand();
		let class = TkRule::Expanded { exp };
		Self { class, span, err_span: None, flags, err: TkErr::Null }
	}
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

impl<'t> Expander {
	pub fn new(raw: Tk<'t>) -> Self {
		let unescaped = unescape_str(raw.span.as_str());
		Self { raw: unescaped }
	}
	pub fn expand(&'t mut self) -> Vec<String> {
		self.raw = self.expand_raw();
		let tokens: Vec<_> = LexStream::new(&self.raw, LexFlags::RAW)
			.filter(|tk| !matches!(tk.class, TkRule::EOI | TkRule::SOI))
			.map(|tk| tk.to_string())
			.collect();
		tokens
	}
	pub fn expand_raw(&self) -> String {
		let mut chars = self.raw.chars();
		let mut result = String::new();
		let mut var_name = String::new();
		let mut in_brace = false;

		// TODO: implement error handling for unclosed braces
		while let Some(ch) = chars.next() {
			match ch {
				VAR_SUB => {
					while let Some(ch) = chars.next() {
						match ch {
							'{' => in_brace = true,
							'}' if in_brace => {
								let var_val = read_vars(|v| v.get_var(&var_name));
								result.push_str(&var_val);
								var_name.clear();
								break
							}
							_ if is_hard_sep(ch) => {
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
		result
	}
}

/// Clean up a single layer of escape characters, and then replace control characters like '$' with a non-character unicode representation that is unmistakable by the rest of the code
pub fn unescape_str(raw: &str) -> String {
	let mut chars = raw.chars();
	let mut result = String::new();

	while let Some(ch) = chars.next() {
		match ch {
			'\\' => {
				if let Some(next_ch) = chars.next() {
					result.push(next_ch)
				}
			}
			'$' => result.push(VAR_SUB),
			_ => result.push(ch)
		}
	}
	result
}
