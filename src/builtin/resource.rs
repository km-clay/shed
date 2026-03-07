use ariadne::Fmt;
use nix::sys::resource::{Resource, getrlimit, setrlimit};
use yansi::Color;

use crate::{
  getopt::{Opt, OptSpec, get_opts_from_tokens, get_opts_from_tokens_strict}, libsh::error::{ShErr, ShErrKind, ShResult, ShResultExt, next_color}, parse::{NdRule, Node, execute::prepare_argv}, prelude::*, state::{self}
};

fn ulimit_opt_spec() -> [OptSpec;5] {
	[
		OptSpec {
			opt: Opt::Short('n'), // file descriptors
			takes_arg: true,
		},
		OptSpec {
			opt: Opt::Short('u'), // max user processes
			takes_arg: true,
		},
		OptSpec {
			opt: Opt::Short('s'), // stack size
			takes_arg: true,
		},
		OptSpec {
			opt: Opt::Short('c'), // core dump file size
			takes_arg: true,
		},
		OptSpec {
			opt: Opt::Short('v'), // virtual memory
			takes_arg: true,
		}
	]
}

struct UlimitOpts {
	fds: Option<u64>,
	procs: Option<u64>,
	stack: Option<u64>,
	core: Option<u64>,
	vmem: Option<u64>,
}

fn get_ulimit_opts(opt: &[Opt]) -> ShResult<UlimitOpts> {
	let mut opts = UlimitOpts {
		fds: None,
		procs: None,
		stack: None,
		core: None,
		vmem: None,
	};

	for o in opt {
		match o {
			Opt::ShortWithArg('n', arg) => {
				opts.fds = Some(arg.parse().map_err(|_| ShErr::simple(
					ShErrKind::ParseErr,
					format!("invalid argument for -n: {}", arg.fg(next_color())),
				))?);
			},
			Opt::ShortWithArg('u', arg) => {
				opts.procs = Some(arg.parse().map_err(|_| ShErr::simple(
					ShErrKind::ParseErr,
					format!("invalid argument for -u: {}", arg.fg(next_color())),
				))?);
			},
			Opt::ShortWithArg('s', arg) => {
				opts.stack = Some(arg.parse().map_err(|_| ShErr::simple(
					ShErrKind::ParseErr,
					format!("invalid argument for -s: {}", arg.fg(next_color())),
				))?);
			},
			Opt::ShortWithArg('c', arg) => {
				opts.core = Some(arg.parse().map_err(|_| ShErr::simple(
					ShErrKind::ParseErr,
					format!("invalid argument for -c: {}", arg.fg(next_color())),
				))?);
			},
			Opt::ShortWithArg('v', arg) => {
				opts.vmem = Some(arg.parse().map_err(|_| ShErr::simple(
					ShErrKind::ParseErr,
					format!("invalid argument for -v: {}", arg.fg(next_color())),
				))?);
			},
			o => return Err(ShErr::simple(
				ShErrKind::ParseErr,
				format!("invalid option: {}", o.fg(next_color())),
			)),
		}
	}

	Ok(opts)
}

pub fn ulimit(node: Node) -> ShResult<()> {
  let span = node.get_span();
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

	let (_, opts) = get_opts_from_tokens_strict(argv, &ulimit_opt_spec()).promote_err(span.clone())?;
	let ulimit_opts = get_ulimit_opts(&opts).promote_err(span.clone())?;

	if let Some(fds) = ulimit_opts.fds {
		let (_, hard) =  getrlimit(Resource::RLIMIT_NOFILE).map_err(|e| ShErr::at(
			ShErrKind::ExecFail,
			span.clone(),
			format!("failed to get file descriptor limit: {}", e),
		))?;
		setrlimit(Resource::RLIMIT_NOFILE, fds, hard).map_err(|e| ShErr::at(
			ShErrKind::ExecFail,
			span.clone(),
			format!("failed to set file descriptor limit: {}", e),
		))?;
	}
	if let Some(procs) = ulimit_opts.procs {
		let (_, hard) =  getrlimit(Resource::RLIMIT_NPROC).map_err(|e| ShErr::at(
			ShErrKind::ExecFail,
			span.clone(),
			format!("failed to get process limit: {}", e),
		))?;
		setrlimit(Resource::RLIMIT_NPROC, procs, hard).map_err(|e| ShErr::at(
			ShErrKind::ExecFail,
			span.clone(),
			format!("failed to set process limit: {}", e),
		))?;
	}
	if let Some(stack) = ulimit_opts.stack {
		let (_, hard) =  getrlimit(Resource::RLIMIT_STACK).map_err(|e| ShErr::at(
			ShErrKind::ExecFail,
			span.clone(),
			format!("failed to get stack size limit: {}", e),
		))?;
		setrlimit(Resource::RLIMIT_STACK, stack, hard).map_err(|e| ShErr::at(
			ShErrKind::ExecFail,
			span.clone(),
			format!("failed to set stack size limit: {}", e),
		))?;
	}
	if let Some(core) = ulimit_opts.core {
		let (_, hard) =  getrlimit(Resource::RLIMIT_CORE).map_err(|e| ShErr::at(
			ShErrKind::ExecFail,
			span.clone(),
			format!("failed to get core dump size limit: {}", e),
		))?;
		setrlimit(Resource::RLIMIT_CORE, core, hard).map_err(|e| ShErr::at(
			ShErrKind::ExecFail,
			span.clone(),
			format!("failed to set core dump size limit: {}", e),
		))?;
	}
	if let Some(vmem) = ulimit_opts.vmem {
		let (_, hard) =  getrlimit(Resource::RLIMIT_AS).map_err(|e| ShErr::at(
			ShErrKind::ExecFail,
			span.clone(),
			format!("failed to get virtual memory limit: {}", e),
		))?;
		setrlimit(Resource::RLIMIT_AS, vmem, hard).map_err(|e| ShErr::at(
			ShErrKind::ExecFail,
			span.clone(),
			format!("failed to set virtual memory limit: {}", e),
		))?;
	}

  state::set_status(0);
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::get_ulimit_opts;
  use crate::getopt::Opt;
  use crate::state;
  use crate::testutil::{TestGuard, test_input};
  use nix::sys::resource::{Resource, getrlimit};

  // ===================== Pure: option parsing =====================

  #[test]
  fn parse_fds() {
    let opts = get_ulimit_opts(&[Opt::ShortWithArg('n', "1024".into())]).unwrap();
    assert_eq!(opts.fds, Some(1024));
  }

  #[test]
  fn parse_procs() {
    let opts = get_ulimit_opts(&[Opt::ShortWithArg('u', "512".into())]).unwrap();
    assert_eq!(opts.procs, Some(512));
  }

  #[test]
  fn parse_stack() {
    let opts = get_ulimit_opts(&[Opt::ShortWithArg('s', "8192".into())]).unwrap();
    assert_eq!(opts.stack, Some(8192));
  }

  #[test]
  fn parse_core() {
    let opts = get_ulimit_opts(&[Opt::ShortWithArg('c', "0".into())]).unwrap();
    assert_eq!(opts.core, Some(0));
  }

  #[test]
  fn parse_vmem() {
    let opts = get_ulimit_opts(&[Opt::ShortWithArg('v', "100000".into())]).unwrap();
    assert_eq!(opts.vmem, Some(100000));
  }

  #[test]
  fn parse_multiple() {
    let opts = get_ulimit_opts(&[
      Opt::ShortWithArg('n', "256".into()),
      Opt::ShortWithArg('c', "0".into()),
    ]).unwrap();
    assert_eq!(opts.fds, Some(256));
    assert_eq!(opts.core, Some(0));
    assert!(opts.procs.is_none());
  }

  #[test]
  fn parse_non_numeric_fails() {
    let result = get_ulimit_opts(&[Opt::ShortWithArg('n', "abc".into())]);
    assert!(result.is_err());
  }

  #[test]
  fn parse_invalid_option() {
    let result = get_ulimit_opts(&[Opt::Short('z')]);
    assert!(result.is_err());
  }

  // ===================== Integration =====================

  #[test]
  fn ulimit_set_core_zero() {
    let _g = TestGuard::new();
    // Setting core dump size to 0 is always safe
    test_input("ulimit -c 0").unwrap();
    let (soft, _) = getrlimit(Resource::RLIMIT_CORE).unwrap();
    assert_eq!(soft, 0);
  }

  #[test]
  fn ulimit_invalid_flag() {
    let _g = TestGuard::new();
    let result = test_input("ulimit -z 100");
    assert!(result.is_err());
  }

  #[test]
  fn ulimit_non_numeric_value() {
    let _g = TestGuard::new();
    let result = test_input("ulimit -n abc");
    assert!(result.is_err());
  }

  #[test]
  fn ulimit_status_zero() {
    let _g = TestGuard::new();
    test_input("ulimit -c 0").unwrap();
    assert_eq!(state::get_status(), 0);
  }
}
