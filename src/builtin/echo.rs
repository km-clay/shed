use std::sync::LazyLock;

use crate::{builtin::setup_builtin, getopt::{get_opts_from_tokens, Opt, OptSet}, jobs::JobBldr, libsh::error::{ShErr, ShErrKind, ShResult, ShResultExt}, parse::{NdRule, Node}, prelude::*, procio::{borrow_fd, IoStack}, state};

pub static ECHO_OPTS: LazyLock<OptSet> = LazyLock::new(|| {[
	Opt::Short('n'),
	Opt::Short('E'),
	Opt::Short('e'),
	Opt::Short('r'),
].into()});

bitflags! {
	pub struct EchoFlags: u32 {
		const NO_NEWLINE = 0b000001;
		const USE_STDERR = 0b000010;
		const USE_ESCAPE = 0b000100;
	}
}

pub fn echo(node: Node, io_stack: &mut IoStack, job: &mut JobBldr) -> ShResult<()> {
	let blame = node.get_span().clone();
	let NdRule::Command { assignments: _, argv } = node.class else {
		unreachable!()
	};
	assert!(!argv.is_empty());
	let (argv,opts) = get_opts_from_tokens(argv);
	let flags = get_echo_flags(opts).blame(blame)?;
	let (argv,io_frame) = setup_builtin(argv, job, Some((io_stack,node.redirs)))?;

	let output_channel = if flags.contains(EchoFlags::USE_STDERR) {
		borrow_fd(STDERR_FILENO)
	} else {
		borrow_fd(STDOUT_FILENO)
	};

	let mut echo_output = argv.into_iter()
		.map(|a| a.0) // Extract the String from the tuple of (String,Span)
		.collect::<Vec<_>>()
		.join(" ");

	if !flags.contains(EchoFlags::NO_NEWLINE) {
		echo_output.push('\n')
	}

	write(output_channel, echo_output.as_bytes())?;

	io_frame.unwrap().restore()?;
	state::set_status(0);
	Ok(())
}

pub fn get_echo_flags(mut opts: Vec<Opt>) -> ShResult<EchoFlags> {
	let mut flags = EchoFlags::empty();

	while let Some(opt) = opts.pop() {
		if !ECHO_OPTS.contains(&opt) {
			return Err(
				ShErr::simple(
					ShErrKind::ExecFail,
					format!("echo: Unexpected flag '{opt}'"),
				)
			)
		}
		let Opt::Short(opt) = opt else {
			unreachable!()
		};

		match opt {
			'n' => flags |= EchoFlags::NO_NEWLINE,
			'r' => flags |= EchoFlags::USE_STDERR,
			'e' => flags |= EchoFlags::USE_ESCAPE,
			_ => unreachable!()
		}
	}

	Ok(flags)
}
