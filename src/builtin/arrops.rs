use crate::{
  getopt::{Opt, OptSpec, get_opts_from_tokens}, jobs::JobBldr, libsh::error::{ShErr, ShErrKind, ShResult}, parse::{NdRule, Node}, prelude::*, procio::{IoStack, borrow_fd}, state::{self, VarFlags, VarKind, write_vars}
};

use super::setup_builtin;

fn arr_op_optspec() -> Vec<OptSpec> {
	vec![
		OptSpec {
			opt: Opt::Short('c'),
			takes_arg: true
		},
		OptSpec {
			opt: Opt::Short('r'),
			takes_arg: false
		},
		OptSpec {
			opt: Opt::Short('v'),
			takes_arg: true
		}
	]
}

pub struct ArrOpOpts {
	count: usize,
	reverse: bool,
	var: Option<String>,
}

impl Default for ArrOpOpts {
	fn default() -> Self {
		Self {
			count: 1,
			reverse: false,
			var: None,
		}
	}
}

#[derive(Clone, Copy)]
enum End { Front, Back }

fn arr_pop_inner(node: Node, io_stack: &mut IoStack, job: &mut JobBldr, end: End) -> ShResult<()> {
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

	let (argv, opts) = get_opts_from_tokens(argv, &arr_op_optspec())?;
	let arr_op_opts = get_arr_op_opts(opts)?;
  let (argv, _guard) = setup_builtin(argv, job, Some((io_stack, node.redirs)))?;
  let stdout = borrow_fd(STDOUT_FILENO);
	let mut status = 0;

	for (arg,_) in argv {
		for _ in 0..arr_op_opts.count {
			let pop = |arr: &mut std::collections::VecDeque<String>| match end {
				End::Front => arr.pop_front(),
				End::Back => arr.pop_back(),
			};
			let Some(popped) = write_vars(|v| v.get_arr_mut(&arg).ok().and_then(pop)) else {
				status = 1;
				break;
			};
			status = 0;

			if let Some(ref var) = arr_op_opts.var {
				write_vars(|v| v.set_var(var, VarKind::Str(popped), VarFlags::NONE))?;
			} else {
				write(stdout, popped.as_bytes())?;
				write(stdout, b"\n")?;
			}
		}
	}

  state::set_status(status);
  Ok(())
}

fn arr_push_inner(node: Node, io_stack: &mut IoStack, job: &mut JobBldr, end: End) -> ShResult<()> {
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

	let (argv, opts) = get_opts_from_tokens(argv, &arr_op_optspec())?;
	let _arr_op_opts = get_arr_op_opts(opts)?;
  let (argv, _guard) = setup_builtin(argv, job, Some((io_stack, node.redirs)))?;

	let mut argv = argv.into_iter();
	let Some((name, _)) = argv.next() else {
		return Err(ShErr::simple(ShErrKind::ExecFail, "push: missing array name".to_string()));
	};

	for (val, _) in argv {
		let push_val = val.clone();
		if let Err(e) = write_vars(|v| v.get_arr_mut(&name).map(|arr| match end {
			End::Front => arr.push_front(push_val),
			End::Back => arr.push_back(push_val),
		})) {
			state::set_status(1);
			return Err(e);
		};
	}

  state::set_status(0);
  Ok(())
}

pub fn arr_pop(node: Node, io_stack: &mut IoStack, job: &mut JobBldr) -> ShResult<()> {
	arr_pop_inner(node, io_stack, job, End::Back)
}

pub fn arr_fpop(node: Node, io_stack: &mut IoStack, job: &mut JobBldr) -> ShResult<()> {
	arr_pop_inner(node, io_stack, job, End::Front)
}

pub fn arr_push(node: Node, io_stack: &mut IoStack, job: &mut JobBldr) -> ShResult<()> {
	arr_push_inner(node, io_stack, job, End::Back)
}

pub fn arr_fpush(node: Node, io_stack: &mut IoStack, job: &mut JobBldr) -> ShResult<()> {
	arr_push_inner(node, io_stack, job, End::Front)
}

pub fn arr_rotate(node: Node, io_stack: &mut IoStack, job: &mut JobBldr) -> ShResult<()> {
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

	let (argv, opts) = get_opts_from_tokens(argv, &arr_op_optspec())?;
	let arr_op_opts = get_arr_op_opts(opts)?;
  let (argv, _guard) = setup_builtin(argv, job, Some((io_stack, node.redirs)))?;

	for (arg, _) in argv {
		write_vars(|v| -> ShResult<()> {
			let arr = v.get_arr_mut(&arg)?;
			if arr_op_opts.reverse {
				arr.rotate_right(arr_op_opts.count.min(arr.len()));
			} else {
				arr.rotate_left(arr_op_opts.count.min(arr.len()));
			}
			Ok(())
		})?;
	}

  state::set_status(0);
  Ok(())
}

pub fn get_arr_op_opts(opts: Vec<Opt>) -> ShResult<ArrOpOpts> {
	let mut arr_op_opts = ArrOpOpts::default();
	for opt in opts {
		match opt {
			Opt::ShortWithArg('c', count) => {
				arr_op_opts.count = count.parse::<usize>().map_err(|_| {
					ShErr::simple(ShErrKind::ParseErr, format!("invalid count: {}", count))
				})?;
			}
			Opt::Short('c') => {
				return Err(ShErr::simple(ShErrKind::ParseErr, "missing count for -c".to_string()));
			}
			Opt::Short('r') => {
				arr_op_opts.reverse = true;
			}
			Opt::ShortWithArg('v', var) => {
				arr_op_opts.var = Some(var);
			}
			Opt::Short('v') => {
				return Err(ShErr::simple(ShErrKind::ParseErr, "missing variable name for -v".to_string()));
			}
			_ => {
				return Err(ShErr::simple(ShErrKind::ParseErr, format!("invalid option: {}", opt)));
			}
		}
	}
	Ok(arr_op_opts)
}
