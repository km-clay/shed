use crate::prelude::*;

#[derive(Clone,Debug)]
pub struct ShEnv {
	vars: shellenv::vars::VarTab,
	logic: shellenv::logic::LogTab,
	meta: shellenv::meta::MetaTab,
	ctx: shellenv::exec_ctx::ExecCtx
}

impl ShEnv {
	pub fn new() -> Self {
		Self {
			vars: shellenv::vars::VarTab::new(),
			logic: shellenv::logic::LogTab::new(),
			meta: shellenv::meta::MetaTab::new(),
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
