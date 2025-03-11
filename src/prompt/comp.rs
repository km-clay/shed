use rustyline::completion::{Candidate, Completer};

use crate::{expand::cmdsub::expand_cmdsub_string, parse::lex::KEYWORDS, prelude::*};

use super::readline::SynHelper;

impl<'a> Completer for SynHelper<'a> {
	type Candidate = String;
	fn complete( &self, line: &str, pos: usize, ctx: &rustyline::Context<'_>,) -> rustyline::Result<(usize, Vec<Self::Candidate>)> {
		let mut shenv = self.shenv.clone();
		let mut comps = vec![];
		shenv.new_input(line);
		let mut token_stream = Lexer::new(line.to_string(), &mut shenv).lex();
		if let Some(comp_token) = token_stream.pop() {
			let raw = comp_token.as_raw(&mut shenv);
			let is_cmd = if let Some(token) = token_stream.pop() {
				match token.rule() {
					TkRule::Sep => true,
					_ if KEYWORDS.contains(&token.rule()) => true,
					_ => false
				}
			} else {
				true
			};
			if let TkRule::Ident | TkRule::Whitespace = comp_token.rule() {
				if is_cmd {
					let cmds = shenv.meta().path_cmds();
					comps.extend(cmds.iter().map(|cmd| cmd.to_string()));
					comps.retain(|cmd| cmd.starts_with(&raw));
					if !comps.is_empty() && comps.len() > 1 {
						if get_bin_path("fzf", &self.shenv).is_some() {
							if let Some(mut selection) = fzf_comp(&comps, &mut shenv) {
								while selection.starts_with(&raw) {
									selection = selection.strip_prefix(&raw).unwrap().to_string();
								}
								comps = vec![selection];
							}
						}
					} else if let Some(mut comp) = comps.pop() {
						while comp.starts_with(&raw) {
							comp = comp.strip_prefix(&raw).unwrap().to_string();
						}
						comps = vec![comp];
					}
					return Ok((pos,comps))
				} else {
					let (start, matches) = self.file_comp.complete(line, pos, ctx)?;
					comps.extend(matches.iter().map(|c| c.display().to_string()));

					if !comps.is_empty() && comps.len() > 1 {
						if get_bin_path("fzf", &self.shenv).is_some() {
							if let Some(selection) = fzf_comp(&comps, &mut shenv) {
								return Ok((start, vec![selection]))
							} else {
								return Ok((start, vec![]))
							}
						} else {
							return Ok((start, comps))
						}
					} else if let Some(comp) = comps.pop() {
						// Slice off the already typed bit
						return Ok((start, vec![comp]))
					}
				}
			}
		}
		Ok((pos,comps))
	}
}

pub fn fzf_comp(comps: &[String], shenv: &mut ShEnv) -> Option<String> {
	// All of the fzf wrapper libraries suck
	// So we gotta do this now
	let echo_args = comps.join("\n");
	let echo = format!("echo \"{echo_args}\"");
	let fzf = "fzf --height=~30% --layout=reverse --border --border-label=completion";
	let command = format!("{echo} | {fzf}");

	shenv.ctx_mut().set_flag(ExecFlags::NO_EXPAND); // Prevent any pesky shell injections with filenames like '$(rm -rf /)'
	let selection = expand_cmdsub_string(&command, shenv).ok()?;
	if selection.is_empty() {
		None
	} else {
		Some(selection.trim().to_string())
	}
}
