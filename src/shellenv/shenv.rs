use crate::prelude::*;

#[derive(Clone,Debug)]
pub struct ShEnv {
	vars: shellenv::vars::VarTab,
	logic: shellenv::logic::LogTab,
	meta: shellenv::meta::MetaTab,
	input_man: shellenv::input::InputMan,
	ctx: shellenv::exec_ctx::ExecCtx
}

impl ShEnv {
	pub fn new() -> Self {
		Self {
			vars: shellenv::vars::VarTab::new(),
			logic: shellenv::logic::LogTab::new(),
			meta: shellenv::meta::MetaTab::new(),
			input_man: shellenv::input::InputMan::new(),
			ctx: shellenv::exec_ctx::ExecCtx::new(),
		}
	}
	pub fn vars(&self) -> &shellenv::vars::VarTab {
		&self.vars
	}
	pub fn vars_mut(&mut self) -> &mut shellenv::vars::VarTab {
		&mut self.vars
	}
	pub fn meta(&self) -> &shellenv::meta::MetaTab {
		&self.meta
	}
	pub fn input_slice(&self, span: Rc<RefCell<Span>>) -> &str {
		&self.input_man.get_slice(span).unwrap_or_default()
	}
	pub fn expand_input(&mut self, new: &str, repl_span: Rc<RefCell<Span>>) -> Vec<Token> {
		log!(DEBUG,repl_span);
		if repl_span.borrow().expanded {
			return vec![];
		}
		repl_span.borrow_mut().expanded = true;
		let saved_spans = self.input_man.spans_mut().clone();
		let mut new_tokens = Lexer::new(new.to_string(), self).lex();
		*self.input_man.spans_mut() = saved_spans;

		let offset = repl_span.borrow().start();
		for token in new_tokens.iter_mut() {
			token.span().borrow_mut().shift(offset as isize);
		}

		let repl_start = repl_span.borrow().start();
		let repl_end = repl_span.borrow().end();
		let range = repl_start..repl_end;

		if let Some(input) = self.input_man.get_input_mut() {
			let old = &input[range.clone()];
			let delta: isize = new.len() as isize - old.len() as isize;
			log!(DEBUG, input);
			input.replace_range(range, new);
			let expanded = input.clone();
			log!(DEBUG, expanded);

			for span in self.input_man.spans_mut() {
				let mut span_mut = span.borrow_mut();
				if span_mut.start() > repl_start {
					span_mut.shift(delta);
				}
			}
			for token in &new_tokens {
				self.input_man.spans_mut().push(token.span());
			}
		}
		self.input_man.clamp_all();
		log!(DEBUG, new_tokens);
		if new_tokens.is_empty() {
			let empty = Token::new(
				TkRule::Ident,
				self.inputman_mut().new_span(repl_start, repl_start)
			);
			vec![empty]
		} else {
			new_tokens
		}
	}
	/// Executes a group of command lists, and only uses redirections that operate on input
	/// For instance:
	/// `if cat; then echo foo; fi < file.txt > otherfile.txt`
	/// `cat` will be executed as a condition, meaning the input from file.txt will be the only
	/// redirection used.
	pub fn exec_as_cond(&mut self, nodes: Vec<Node>) -> ShResult<i32> {
		let saved = self.ctx().clone();
		self.ctx = self.ctx().as_cond();
		let ast = SynTree::from_vec(nodes);
		Executor::new(ast, self).walk()?;
		self.ctx = saved;
		Ok(self.get_code())
	}
	/// Executes a group of command lists, and only uses redirections that operate on output
	/// For instance:
	/// `if cat; then echo foo; fi < file.txt > otherfile.txt`
	/// `echo foo` will be executed as a body, meaning the output to otherfile.txt will be the only
	/// redirection used.
	pub fn exec_as_body(&mut self, nodes: Vec<Node>) -> ShResult<i32> {
		let saved = self.ctx().clone();
		self.ctx = self.ctx().as_body();
		let ast = SynTree::from_vec(nodes);
		Executor::new(ast, self).walk()?;
		self.ctx = saved;
		Ok(self.get_code())
	}
	pub fn new_input(&mut self, input: &str) {
		self.input_man.clear();
		self.input_man.new_input(input);
	}
	pub fn get_input(&self) -> String {
		let input = self.input_man.get_input().map(|s| s.to_string()).unwrap_or_default();
		log!(TRACE, input);
		input
	}
	pub fn inputman(&self) -> &shellenv::input::InputMan {
		&self.input_man
	}
	pub fn inputman_mut(&mut self) -> &mut shellenv::input::InputMan {
		&mut self.input_man
	}
	pub fn meta_mut(&mut self) -> &mut shellenv::meta::MetaTab {
		&mut self.meta
	}
	pub fn logic(&self) -> &shellenv::logic::LogTab {
		&self.logic
	}
	pub fn logic_mut(&mut self) -> &mut shellenv::logic::LogTab {
		&mut self.logic
	}
	pub fn save_io(&mut self) -> ShResult<()> {
		let ctx = self.ctx_mut();
		let stdin = ctx.masks().stdin().get_fd();
		let stdout = ctx.masks().stdout().get_fd();
		let stderr = ctx.masks().stderr().get_fd();

		let saved_in = dup(stdin)?;
		let saved_out = dup(stdout)?;
		let saved_err = dup(stderr)?;

		let saved_io = shellenv::exec_ctx::SavedIo::save(saved_in, saved_out, saved_err);
		*ctx.saved_io() = Some(saved_io);
		Ok(())
	}
	pub fn reset_io(&mut self) -> ShResult<()> {
		let ctx = self.ctx_mut();
		if let Some(saved) = ctx.saved_io().take() {
			let saved_in = saved.stdin;
			let saved_out = saved.stdout;
			let saved_err = saved.stderr;
			dup2(0,saved_in)?;
			close(saved_in)?;
			dup2(1,saved_out)?;
			close(saved_out)?;
			dup2(2,saved_err)?;
			close(saved_err)?;
		}
		Ok(())
	}
	pub fn collect_redirs(&mut self, mut redirs: Vec<Redir>) {
		let ctx = self.ctx_mut();
		while let Some(redir) = redirs.pop() {
			ctx.push_rdr(redir);
		}
	}
	pub fn set_code(&mut self, code: i32) {
		self.vars_mut().set_param("?", &code.to_string());
	}
	pub fn get_code(&self) -> i32 {
		self.vars().get_param("?").parse::<i32>().unwrap_or(0)
	}
	pub fn ctx(&self) -> &shellenv::exec_ctx::ExecCtx {
		&self.ctx
	}
	pub fn ctx_mut(&mut self) -> &mut shellenv::exec_ctx::ExecCtx {
		&mut self.ctx
	}
}
