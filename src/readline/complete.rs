use std::{collections::HashSet, env, fmt::Debug, os::unix::fs::PermissionsExt, path::PathBuf, sync::Arc};

use crate::{
  builtin::{BUILTINS, complete::{CompFlags, CompOpts}},
  libsh::{error::{ShErr, ShErrKind, ShResult}, utils::TkVecUtils},
  parse::{execute::{VarCtxGuard, exec_input}, lex::{self, LexFlags, Tk, TkFlags, TkRule}},
  readline::{
    Marker, annotate_input, annotate_input_recursive, get_insertions,
    markers::{self, is_marker},
  },
  state::{VarFlags, VarKind, read_logic, read_meta, read_vars, write_vars},
};

pub fn complete_users(start: &str) -> Vec<String> {
	let Ok(passwd) = std::fs::read_to_string("/etc/passwd") else {
		return vec![];
	};
	passwd
		.lines()
		.filter_map(|line| line.split(':').next())
		.filter(|username| username.starts_with(start))
		.map(|s| s.to_string())
		.collect()
}

pub fn complete_vars(start: &str) -> Vec<String> {
	let Some((var_name, start, end)) = extract_var_name(start) else {
		return vec![]
	};
	if !read_vars(|v| v.get_var(&var_name)).is_empty() {
		return vec![]
	}
	// if we are here, we have a variable substitution that isn't complete
	// so let's try to complete it
	read_vars(|v| {
		v.flatten_vars()
			.keys()
			.filter(|k| k.starts_with(&var_name) && *k != &var_name)
			.map(|k| k.to_string())
			.collect::<Vec<_>>()

	})
}

pub fn extract_var_name(text: &str) -> Option<(String, usize, usize)> {
	let mut chars = text.chars().peekable();
	let mut name = String::new();
	let mut reading_name = false;
	let mut pos = 0;
	let mut name_start = 0;
	let mut name_end = 0;

	while let Some(ch) = chars.next() {
		match ch {
			'$' => {
				if chars.peek() == Some(&'{') {
					continue;
				}

				reading_name = true;
				name_start = pos + 1; // Start after the '$'
				}
			'{' if !reading_name => {
				reading_name = true;
				name_start = pos + 1;
			}
			ch if ch.is_alphanumeric() || ch == '_' => {
				if reading_name {
					name.push(ch);
				}
			}
			_ => {
				if reading_name {
					name_end = pos; // End before the non-alphanumeric character
					break;
				}
			}
		}
		pos += 1;
	}

	if !reading_name {
		return None;
	}

	if name_end == 0 {
		name_end = pos;
	}

	Some((name, name_start, name_end))
}

fn complete_commands(start: &str) -> Vec<String> {
	let mut candidates: Vec<String> = read_meta(|m| {
		m.cached_cmds()
			.iter()
			.filter(|c| c.starts_with(start))
			.cloned()
			.collect()
	});

	candidates.sort();
	candidates
}

fn complete_dirs(start: &str) -> Vec<String> {
	let filenames = complete_filename(start);
	filenames.into_iter().filter(|f| std::fs::metadata(f).map(|m| m.is_dir()).unwrap_or(false)).collect()
}

fn complete_filename(start: &str) -> Vec<String> {
	let mut candidates = vec![];
	let has_dotslash = start.starts_with("./");

	// Split path into directory and filename parts
	// Use "." if start is empty (e.g., after "foo=")
	let path = PathBuf::from(if start.is_empty() { "." } else { start });
	let (dir, prefix) = if start.ends_with('/') || start.is_empty() {
		// Completing inside a directory: "src/" → dir="src/", prefix=""
		(path, "")
	} else if let Some(parent) = path.parent()
		&& !parent.as_os_str().is_empty()
	{
		// Has directory component: "src/ma" → dir="src", prefix="ma"
		(
			parent.to_path_buf(),
			path.file_name().unwrap().to_str().unwrap_or(""),
		)
	} else {
		// No directory: "fil" → dir=".", prefix="fil"
		(PathBuf::from("."), start)
	};

	let Ok(entries) = std::fs::read_dir(&dir) else {
		return candidates;
	};

	for entry in entries.flatten() {
		let file_name = entry.file_name();
		let file_str = file_name.to_string_lossy();

		// Skip hidden files unless explicitly requested
		if !prefix.starts_with('.') && file_str.starts_with('.') {
			continue;
		}

		if file_str.starts_with(prefix) {
			// Reconstruct full path
			let mut full_path = dir.join(&file_name);

			// Add trailing slash for directories
			if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
				full_path.push(""); // adds trailing /
			}

			let mut path_raw = full_path.to_string_lossy().to_string();
			if path_raw.starts_with("./") && !has_dotslash {
				path_raw = path_raw.trim_start_matches("./").to_string();
			}

			candidates.push(path_raw);
		}
	}

	candidates.sort();
	candidates
}

#[derive(Default,Debug,Clone)]
pub struct BashCompSpec {
	/// -F: The name of a function to generate the possible completions.
	pub function: Option<String>,
	/// -W: The list of words
	pub wordlist: Option<Vec<String>>,
	/// -f: complete file names
	pub files: bool,
	/// -d: complete directory names
	pub dirs: bool,
	/// -c: complete command names
	pub commands: bool,
	/// -u: complete user names
	pub users: bool,
	/// -v complete variable names
	pub vars: bool,
	/// -A signal: complete signal names
	pub signals: bool,

	/// The original command
	pub source: String
}

impl BashCompSpec {
	pub fn new() -> Self {
		Self::default()
	}
	pub fn with_func(mut self, func: String) -> Self {
		self.function = Some(func);
		self
	}
	pub fn with_wordlist(mut self, wordlist: Vec<String>) -> Self {
		self.wordlist = Some(wordlist);
		self
	}
	pub fn with_source(mut self, source: String) -> Self {
		self.source = source;
		self
	}
	pub fn files(mut self, enable: bool) -> Self {
		self.files = enable;
		self
	}
	pub fn dirs(mut self, enable: bool) -> Self {
		self.dirs = enable;
		self
	}
	pub fn commands(mut self, enable: bool) -> Self {
		self.commands = enable;
		self
	}
	pub fn users(mut self, enable: bool) -> Self {
		self.users = enable;
		self
	}
	pub fn vars(mut self, enable: bool) -> Self {
		self.vars = enable;
		self
	}
	pub fn signals(mut self, enable: bool) -> Self {
		self.signals = enable;
		self
	}
	pub fn from_comp_opts(opts: CompOpts) -> Self {
		let CompOpts { func, wordlist, action: _, flags } = opts;
		Self {
			function: func,
			wordlist,
			files: flags.contains(CompFlags::FILES),
			dirs: flags.contains(CompFlags::DIRS),
			commands: flags.contains(CompFlags::CMDS),
			users: flags.contains(CompFlags::USERS),
			vars: flags.contains(CompFlags::VARS),
			signals: false, // TODO: implement signal completion
			source: String::new()
		}
	}
	pub fn exec_comp_func(&self, ctx: &CompContext) -> ShResult<Vec<String>> {
		let mut vars_to_unset = HashSet::new();
		for var in [ "COMP_WORDS", "COMP_CWORD", "COMP_LINE", "COMP_POINT", "COMPREPLY" ] {
			vars_to_unset.insert(var.to_string());
		}
		let _guard = VarCtxGuard::new(vars_to_unset);

		let CompContext { words, cword, line, cursor_pos } = ctx;

		let raw_words = words.to_vec().into_iter().map(|tk| tk.to_string()).collect();
		write_vars(|v| v.set_var("COMP_WORDS", VarKind::arr_from_vec(raw_words), VarFlags::NONE))?;
		write_vars(|v| v.set_var("COMP_CWORD", VarKind::Str(cword.to_string()), VarFlags::NONE))?;
		write_vars(|v| v.set_var("COMP_LINE", VarKind::Str(line.to_string()), VarFlags::NONE))?;
		write_vars(|v| v.set_var("COMP_POINT", VarKind::Str(cursor_pos.to_string()), VarFlags::NONE))?;

		let cmd_name = words
			.first()
			.map(|s| s.to_string())
			.unwrap_or_default();

		let cword_str = words.get(*cword)
			.map(|s| s.to_string())
			.unwrap_or_default();

		let pword_str = if *cword > 0 {
			words.get(cword - 1).map(|s| s.to_string()).unwrap_or_default()
		} else {
			String::new()
		};

		let input = format!("{} {cmd_name} {cword_str} {pword_str}", self.function.as_ref().unwrap());
		exec_input(input, None, false)?;

		Ok(read_vars(|v| v.get_arr_elems("COMPREPLY")).unwrap_or_default())
	}
}

impl CompSpec for BashCompSpec {
	fn complete(&self, ctx: &CompContext) -> ShResult<Vec<String>> {
		let mut candidates = vec![];
		let prefix = &ctx.words[ctx.cword];

		let expanded = prefix.clone().expand()?.get_words().join(" ");
		if self.files {
			candidates.extend(complete_filename(&expanded));
		}
		if self.dirs {
			candidates.extend(complete_dirs(&expanded));
		}
		if self.commands {
			candidates.extend(complete_commands(&expanded));
		}
		if self.vars {
			candidates.extend(complete_vars(&expanded));
		}
		if self.users {
			candidates.extend(complete_users(&expanded));
		}
		if let Some(words) = &self.wordlist {
			candidates.extend(
				words
					.iter()
					.filter(|w| w.starts_with(&expanded))
					.cloned(),
			);
		}
		if self.function.is_some() {
			candidates.extend(self.exec_comp_func(ctx)?);
		}

		Ok(candidates)
	}

	fn source(&self) -> &str {
	  &self.source
	}
}

pub trait CompSpec: Debug + CloneCompSpec {
	fn complete(&self, ctx: &CompContext) -> ShResult<Vec<String>>;
	fn source(&self) -> &str;
}

pub trait CloneCompSpec {
	fn clone_box(&self) -> Box<dyn CompSpec>;
}

impl<T: CompSpec + Clone + 'static> CloneCompSpec for T {
	fn clone_box(&self) -> Box<dyn CompSpec> {
		Box::new(self.clone())
	}
}

impl Clone for Box<dyn CompSpec> {
	fn clone(&self) -> Self {
		self.clone_box()
	}
}

pub struct CompContext {
	pub words: Vec<Tk>,
	pub cword: usize,
	pub line: String,
	pub cursor_pos: usize
}

impl CompContext {
	pub fn cmd(&self) -> Option<&str> {
		self.words.first().map(|s| s.as_str())
	}
}

pub enum CompResult {
  NoMatch,
  Single { result: String },
  Many { candidates: Vec<String> },
}

impl CompResult {
  pub fn from_candidates(candidates: Vec<String>) -> Self {
    if candidates.is_empty() {
      Self::NoMatch
    } else if candidates.len() == 1 {
      Self::Single {
        result: candidates[0].clone(),
      }
    } else {
      Self::Many { candidates }
    }
  }
}

pub struct Completer {
  pub candidates: Vec<String>,
  pub selected_idx: usize,
  pub original_input: String,
  pub token_span: (usize, usize),
  pub active: bool,
}

impl Completer {
  pub fn new() -> Self {
    Self {
      candidates: vec![],
      selected_idx: 0,
      original_input: String::new(),
      token_span: (0, 0),
      active: false,
    }
  }

  pub fn slice_line(line: &str, cursor_pos: usize) -> (&str, &str) {
    let (before_cursor, after_cursor) = line.split_at(cursor_pos);
    (before_cursor, after_cursor)
  }

  pub fn get_completion_context(&self, line: &str, cursor_pos: usize) -> (Vec<Marker>, usize) {
    let annotated = annotate_input_recursive(line);
    let mut ctx = vec![markers::NULL];
    let mut last_priority = 0;
    let mut ctx_start = 0;
    let mut pos = 0;

    for ch in annotated.chars() {
      match ch {
        _ if is_marker(ch) => match ch {
          markers::COMMAND | markers::BUILTIN => {
            if last_priority < 2 {
              if last_priority > 0 {
                ctx.pop();
              }
              ctx_start = pos;
              last_priority = 2;
              ctx.push(markers::COMMAND);
            }
          }
          markers::VAR_SUB => {
            if last_priority < 3 {
              if last_priority > 0 {
                ctx.pop();
              }
              ctx_start = pos;
              last_priority = 3;
              ctx.push(markers::VAR_SUB);
            }
          }
          markers::ARG | markers::ASSIGNMENT => {
            if last_priority < 1 {
              ctx_start = pos;
              ctx.push(markers::ARG);
            }
          }
          _ => {}
        },
        _ => {
          last_priority = 0; // reset priority on normal characters
          pos += 1; // we hit a normal character, advance our position
          if pos >= cursor_pos {
            break;
          }
        }
      }
    }

    (ctx, ctx_start)
  }

  pub fn reset(&mut self) {
    self.candidates.clear();
    self.selected_idx = 0;
    self.original_input.clear();
    self.token_span = (0, 0);
    self.active = false;
  }

  pub fn complete(
    &mut self,
    line: String,
    cursor_pos: usize,
    direction: i32,
  ) -> ShResult<Option<String>> {
    if self.active {
      Ok(Some(self.cycle_completion(direction)))
    } else {
      self.start_completion(line, cursor_pos)
    }
  }

  pub fn selected_candidate(&self) -> Option<String> {
    self.candidates.get(self.selected_idx).cloned()
  }

  pub fn cycle_completion(&mut self, direction: i32) -> String {
    if self.candidates.is_empty() {
      return self.original_input.clone();
    }

    let len = self.candidates.len();
    self.selected_idx = (self.selected_idx as i32 + direction).rem_euclid(len as i32) as usize;

    self.get_completed_line()
  }

  pub fn start_completion(&mut self, line: String, cursor_pos: usize) -> ShResult<Option<String>> {
    let result = self.get_candidates(line.clone(), cursor_pos)?;
    match result {
      CompResult::Many { candidates } => {
        self.candidates = candidates.clone();
        self.selected_idx = 0;
        self.original_input = line;
        self.active = true;

        Ok(Some(self.get_completed_line()))
      }
      CompResult::Single { result } => {
        self.candidates = vec![result.clone()];
        self.selected_idx = 0;
        self.original_input = line;
        self.active = false;

        Ok(Some(self.get_completed_line()))
      }
      CompResult::NoMatch => Ok(None),
    }
  }


  pub fn get_completed_line(&self) -> String {
    if self.candidates.is_empty() {
      return self.original_input.clone();
    }

    let selected = &self.candidates[self.selected_idx];
    let (start, end) = self.token_span;
    format!(
      "{}{}{}",
      &self.original_input[..start],
      selected,
      &self.original_input[end..]
    )
  }

	pub fn build_comp_ctx(&self, tks: &[Tk], line: &str, cursor_pos: usize) -> ShResult<CompContext> {
		log::debug!("build_comp_ctx: cursor_pos={}, tokens={}", cursor_pos, tks.len());
		let mut ctx = CompContext {
			words: vec![],
			cword: 0,
			line: line.to_string(),
			cursor_pos,
		};

		let segments = tks
			.iter()
			.filter(|&tk| !matches!(tk.class, TkRule::SOI | TkRule::EOI))
			.cloned()
			.collect::<Vec<_>>()
			.split_at_separators();
		log::debug!("build_comp_ctx: {} segments after split", segments.len());

		if segments.is_empty() {
			log::debug!("build_comp_ctx: no segments found");
			return Ok(ctx);
		}

		let relevant_pos = segments
		.iter()
		.position(|tks| tks.iter().next().is_some_and(|tk|{ log::debug!("checking span: {}", tk.span.start); tk.span.start > cursor_pos }))
		.map(|i| i.saturating_sub(1)) // take the pos before it
		.unwrap_or(segments.len().saturating_sub(1));

		let mut relevant = segments[relevant_pos].to_vec();

		log::debug!("build_comp_ctx: relevant segment has {} tokens: {:?}",
			relevant.len(),
			relevant.iter().map(|tk| tk.as_str()).collect::<Vec<_>>()
		);

		let cword = if let Some(pos) = relevant.iter().position(|tk| {
			cursor_pos >= tk.span.start && cursor_pos <= tk.span.end
		}) {
			// Cursor is inside or at the end of an existing token
			pos
		} else {
			// Cursor is in whitespace — find where to insert an empty token
			let insert_pos = relevant.iter()
				.position(|tk| tk.span.start > cursor_pos)
				.unwrap_or(relevant.len());

			let mut new_tk = Tk::default();
			if let Some(tk) = relevant.last() {
				let mut span = tk.span.clone();
				span.set_range(cursor_pos..cursor_pos);
				new_tk.span = span;
			}
			relevant.insert(insert_pos, new_tk);
			insert_pos
		};

		log::debug!("build_comp_ctx: cword={} ('{}')", cword, relevant[cword].as_str());

		ctx.words = relevant;
		ctx.cword = cword;

		Ok(ctx)
	}

	pub fn try_comp_spec(&self, ctx: &CompContext) -> ShResult<CompResult> {
		let cmd = ctx.cmd().unwrap_or("<empty>");
		log::debug!("try_comp_spec: looking up spec for '{}'", cmd);

		let Some(cmd) = ctx.cmd() else {
			log::debug!("try_comp_spec: no command in context");
			return Ok(CompResult::NoMatch);
		};

		let Some(spec) = read_meta(|m| m.get_comp_spec(cmd)) else {
			log::debug!("try_comp_spec: no spec registered for '{}'", cmd);
			return Ok(CompResult::NoMatch);
		};

		log::debug!("try_comp_spec: found spec for '{}', executing", cmd);
		let candidates = spec.complete(ctx)?;
		log::debug!("try_comp_spec: got {} candidates: {:?}", candidates.len(), candidates);
		if candidates.is_empty() {
			Ok(CompResult::NoMatch)
		} else {
			Ok(CompResult::from_candidates(candidates))
		}
	}

  pub fn get_candidates(&mut self, line: String, cursor_pos: usize) -> ShResult<CompResult> {
    log::debug!("get_candidates: line='{}', cursor_pos={}", line, cursor_pos);
    let source = Arc::new(line.clone());
    let tokens =
      lex::LexStream::new(source, LexFlags::LEX_UNFINISHED).collect::<ShResult<Vec<Tk>>>()?;

    let ctx = self.build_comp_ctx(&tokens, &line, cursor_pos)?;

    // Set token_span from CompContext's current word
    if let Some(cur) = ctx.words.get(ctx.cword) {
      self.token_span = (cur.span.start, cur.span.end);
    } else {
      self.token_span = (cursor_pos, cursor_pos);
    }

    // Try programmable completion first
    let res = self.try_comp_spec(&ctx)?;
    if !matches!(res, CompResult::NoMatch) {
      log::debug!("get_candidates: comp_spec matched, returning");
      return Ok(res);
    }

    // Get the current token from CompContext
    let Some(mut cur_token) = ctx.words.get(ctx.cword).cloned() else {
      log::debug!("get_candidates: no current token, falling back to filename completion");
      let candidates = complete_filename("./");
      let end_pos = line.len();
      self.token_span = (end_pos, end_pos);
      return Ok(CompResult::from_candidates(candidates));
    };

    self.token_span = (cur_token.span.start, cur_token.span.end);

    // If token contains '=', only complete after the '='
    let token_str = cur_token.span.as_str();
    if let Some(eq_pos) = token_str.rfind('=') {
      log::debug!("get_candidates: assignment token, completing after '='");
      self.token_span.0 = cur_token.span.start + eq_pos + 1;
      cur_token
        .span
        .set_range(self.token_span.0..self.token_span.1);
    }

    let raw_tk = cur_token.as_str().to_string();
    let is_cmd = cur_token.flags.contains(TkFlags::IS_CMD)
      || cur_token.flags.contains(TkFlags::BUILTIN)
      || ctx.cword == 0;
    let expanded_tk = cur_token.expand()?;
    let expanded_words = expanded_tk.get_words().into_iter().collect::<Vec<_>>();
    let expanded = expanded_words.join("\\ ");

    log::debug!("get_candidates: is_cmd={}, raw='{}', expanded='{}'", is_cmd, raw_tk, expanded);

    let mut candidates = if is_cmd {
      complete_commands(&expanded)
    } else {
      complete_filename(&expanded)
    };
    log::debug!("get_candidates: {} candidates from default completion", candidates.len());

    // Graft the completed text onto the original token.
    // This prevents something like $SOME_PATH/ from being
    // completed into /path/to/some_path/file.txt
    // and instead returns $SOME_PATH/file.txt
    candidates = candidates
      .into_iter()
      .map(|c| match c.strip_prefix(&expanded) {
        Some(suffix) => format!("{raw_tk}{suffix}"),
        None => c,
      })
      .collect();

    let limit = crate::state::read_shopts(|s| s.prompt.comp_limit);
    candidates.truncate(limit);

    Ok(CompResult::from_candidates(candidates))
  }

}

impl Default for Completer {
  fn default() -> Self {
    Self::new()
  }
}
