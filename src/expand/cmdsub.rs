use crate::prelude::*;

pub fn expand_cmdsub(token: Token, shenv: &mut ShEnv) -> ShResult<Vec<Token>> {
	let mut new_tokens = vec![];
	let cmdsub_raw = token.as_raw(shenv);
	let body = &cmdsub_raw[2..cmdsub_raw.len() - 1].to_string(); // From '$(this)' to 'this'

	let (r_pipe,w_pipe) = c_pipe()?;
	let pipe_redir = Redir::output(1, w_pipe);
	let mut sub_shenv = shenv.clone();
	sub_shenv.ctx_mut().set_flag(ExecFlags::NO_FORK);
	sub_shenv.collect_redirs(vec![pipe_redir]);

	match unsafe { fork()? } {
		Child => {
			close(r_pipe).ok();
			exec_input(body, shenv).abort_if_err();
			sh_quit(0);
		}
		Parent { child } => {
			close(w_pipe).ok();
		}
	}
	let output = read_to_string(r_pipe)?;
	if !output.is_empty() {
		let lex_input = Rc::new(output);
	}

	Ok(new_tokens)
}
