use nix::unistd::Pid;

use crate::{jobs::{ChildProc, JobBldr}, libsh::error::ShResult, parse::{execute::prepare_argv, lex::{Span, Tk}, Redir}, procio::{IoFrame, IoStack}};

pub mod echo;
pub mod cd;
pub mod export;
pub mod pwd;
pub mod source;
pub mod shift;
pub mod jobctl;
pub mod alias;
pub mod flowctl;
pub mod zoltraak;

pub const BUILTINS: [&str;15] = [
	"echo",
	"cd",
	"export",
	"pwd",
	"source",
	"shift",
	"jobs",
	"fg",
	"bg",
	"alias",
	"return",
	"break",
	"continue",
	"exit",
	"zoltraak"
];

/// Sets up a builtin command
///
/// Prepares a builtin for execution by processing arguments, setting up redirections, and registering the command as a child process in the given `JobBldr`
///
/// # Parameters
/// * argv - The vector of raw argument tokens
/// * job - A mutable reference to a `JobBldr`
/// * io_mode - An optional 2-tuple consisting of a mutable reference to an `IoStack` and a vector of `Redirs`
///
/// # Behavior
/// * Cleans, expands, and word splits the arg vector
/// * Adds a new `ChildProc` to the job builder
/// * Performs redirections, if any.
///
/// # Returns
/// * The processed arg vector
/// * The popped `IoFrame`, if any
///
/// # Notes
/// * If redirections are given to this function, the caller must call `IoFrame.restore()` on the returned `IoFrame`
/// * If redirections are given, the second field of the resulting tuple will *always* be `Some()`
/// * If no redirections are given, the second field will *always* be `None`
pub fn setup_builtin(
	argv: Vec<Tk>,
	job: &mut JobBldr,
	io_mode: Option<(&mut IoStack,Vec<Redir>)>,
) -> ShResult<(Vec<(String,Span)>, Option<IoFrame>)> {
	let mut argv: Vec<(String,Span)> = prepare_argv(argv)?;

	let child_pgid = if let Some(pgid) = job.pgid() {
		pgid
	} else {
		job.set_pgid(Pid::this());
		Pid::this()
	};
	let cmd_name = argv.remove(0).0;
	let child = ChildProc::new(Pid::this(), Some(&cmd_name), Some(child_pgid))?;
	job.push_child(child);

	let io_frame = if let Some((io_stack,redirs)) = io_mode {
		io_stack.append_to_frame(redirs);
		let mut io_frame = io_stack.pop_frame();
		io_frame.redirect()?;
		Some(io_frame)
	} else {
		None
	};

	// We return the io_frame because the caller needs to also call io_frame.restore()
	Ok((argv,io_frame))
}
