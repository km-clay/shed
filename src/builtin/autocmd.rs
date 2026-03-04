use regex::Regex;

use crate::{
  getopt::{Opt, OptSpec, get_opts_from_tokens}, libsh::error::{ShErr, ShErrKind, ShResult, ShResultExt}, parse::{NdRule, Node, execute::prepare_argv}, state::{self, AutoCmd, AutoCmdKind, write_logic}
};

pub struct AutoCmdOpts {
	pattern: Option<Regex>,
	clear: bool
}
fn autocmd_optspec() -> [OptSpec;2] {
	[
		OptSpec {
			opt: Opt::Short('p'),
			takes_arg: true
		},
		OptSpec {
			opt: Opt::Short('c'),
			takes_arg: false
		}
	]
}

pub fn get_autocmd_opts(opts: &[Opt]) -> ShResult<AutoCmdOpts> {
	let mut autocmd_opts = AutoCmdOpts {
		pattern: None,
		clear: false
	};

	let mut opts = opts.iter();
	while let Some(arg) = opts.next() {
		match arg {
			Opt::ShortWithArg('p', arg) => {
				autocmd_opts.pattern = Some(Regex::new(arg).map_err(|e| ShErr::simple(ShErrKind::ExecFail, format!("invalid regex for -p: {}", e)))?);
			}
			Opt::Short('c') => {
				autocmd_opts.clear = true;
			}
			_ => {
				return Err(ShErr::simple(ShErrKind::ExecFail, format!("unexpected option: {}", arg)));
			}
		}
	}

	Ok(autocmd_opts)
}

pub fn autocmd(node: Node) -> ShResult<()> {
  let span = node.get_span();
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

	let (argv,opts) = get_opts_from_tokens(argv, &autocmd_optspec()).promote_err(span.clone())?;
	let autocmd_opts = get_autocmd_opts(&opts).promote_err(span.clone())?;
  let mut argv = prepare_argv(argv)?;
  if !argv.is_empty() { argv.remove(0); }
	let mut args = argv.iter();

	let Some(autocmd_kind) = args.next() else {
		return Err(ShErr::at(ShErrKind::ExecFail, span, "expected an autocmd kind".to_string()));
	};

	let Ok(autocmd_kind) = autocmd_kind.0.parse::<AutoCmdKind>() else {
		return Err(ShErr::at(ShErrKind::ExecFail, autocmd_kind.1.clone(), format!("invalid autocmd kind: {}", autocmd_kind.0)));
	};

	if autocmd_opts.clear {
		write_logic(|l| l.clear_autocmds(autocmd_kind));
		state::set_status(0);
		return Ok(());
	}

	let Some(autocmd_cmd) = args.next() else {
		return Err(ShErr::at(ShErrKind::ExecFail, span, "expected an autocmd command".to_string()));
	};

	let autocmd = AutoCmd {
		pattern: autocmd_opts.pattern,
		command: autocmd_cmd.0.clone(),
	};

	write_logic(|l| l.insert_autocmd(autocmd_kind, autocmd));

  state::set_status(0);
  Ok(())
}
