use crate::prelude::*;

pub fn expand_cmdsub_token(token: Token, shenv: &mut ShEnv) -> ShResult<Vec<Token>> {
	let cmdsub_raw = token.as_raw(shenv);
	let output = expand_cmdsub_string(&cmdsub_raw, shenv)?;
	let new_tokens = shenv.expand_input(&output, token.span());

	Ok(new_tokens)
}

pub fn expand_cmdsub_string(mut s: &str, shenv: &mut ShEnv) -> ShResult<String> {
	if s.starts_with("$(") && s.ends_with(')') {
		s = &s[2..s.len() - 1]; // From '$(this)' to 'this'
	}

	let (r_pipe,w_pipe) = c_pipe()?;
	let pipe_redir = Redir::output(1, w_pipe);
	let mut sub_shenv = shenv.clone();
	sub_shenv.ctx_mut().set_flag(ExecFlags::NO_FORK);
	sub_shenv.collect_redirs(vec![pipe_redir]);

	match unsafe { fork()? } {
		Child => {
			close(r_pipe).ok();
			exec_input(s, &mut sub_shenv).abort_if_err();
			exit(0);
		}
		Parent { child: _ } => {
			close(w_pipe).ok();
		}
	}
	Ok(read_to_string(r_pipe)?)
}
