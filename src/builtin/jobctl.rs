use crate::{jobs::{ChildProc, JobBldr, JobCmdFlags, JobID}, libsh::error::{ErrSpan, ShErr, ShErrKind, ShResult}, parse::{execute::prepare_argv, lex::Span, NdRule, Node}, prelude::*, procio::{borrow_fd, IoStack}, state::{self, read_jobs, write_jobs}};

use super::setup_builtin;

pub enum JobBehavior {
	Foregound,
	Background
}

pub fn continue_job(node: Node, job: &mut JobBldr, behavior: JobBehavior) -> ShResult<()> {
	let blame = ErrSpan::from(node.get_span());
	let cmd = match behavior {
		JobBehavior::Foregound => "fg",
		JobBehavior::Background => "bg"
	};
	let NdRule::Command { assignments: _, argv } = node.class else {
		unreachable!()
	};

	let (argv,_) = setup_builtin(argv, job, None)?;
	let mut argv = argv.into_iter();

	if read_jobs(|j| j.get_fg().is_some()) {
		return Err(
			ShErr::full(
				ShErrKind::InternalErr,
				format!("Somehow called '{}' with an existing foreground job",cmd),
				blame
			)
		)
	}

	let curr_job_id = if let Some(id) = read_jobs(|j| j.curr_job()) {
		id
	} else {
		return Err(
			ShErr::full(
				ShErrKind::ExecFail,
				"No jobs found",
				blame
			)
		)
	};

	let tabid = match argv.next() {
		Some((arg,blame)) => parse_job_id(&arg, blame)?,
		None => curr_job_id
	};

	let mut job = write_jobs(|j| {
		let id = JobID::TableID(tabid);
		let query_result = j.query(id.clone());
		if query_result.is_some() {
			Ok(j.remove_job(id.clone()).unwrap())
		} else {
			Err(
				ShErr::full(
					ShErrKind::ExecFail,
					format!("Job id `{}' not found", tabid),
					blame
				)
			)
		}
	})?;

	job.killpg(Signal::SIGCONT)?;

	match behavior {
		JobBehavior::Foregound => {
			write_jobs(|j| j.new_fg(job))?;
		}
		JobBehavior::Background => {
			let job_order = read_jobs(|j| j.order().to_vec());
			write(borrow_fd(1), job.display(&job_order, JobCmdFlags::PIDS).as_bytes())?;
			write_jobs(|j| j.insert_job(job, true))?;
		}
	}
	state::set_status(0);
	Ok(())
}

fn parse_job_id(arg: &str, blame: Span) -> ShResult<usize> {
	if arg.starts_with('%') {
		let arg = arg.strip_prefix('%').unwrap();
		if arg.chars().all(|ch| ch.is_ascii_digit()) {
			Ok(arg.parse::<usize>().unwrap())
		} else {
			let result = write_jobs(|j| {
				let query_result = j.query(JobID::Command(arg.into()));
				query_result.map(|job| job.tabid().unwrap())
			});
			match result {
				Some(id) => Ok(id),
				None => Err(
					ShErr::full(
						ShErrKind::InternalErr,
						"Found a job but no table id in parse_job_id()",
						blame.into()
					)
				)
			}
		}
	} else if arg.chars().all(|ch| ch.is_ascii_digit()) {
		let result = write_jobs(|j| {
			let pgid_query_result = j.query(JobID::Pgid(Pid::from_raw(arg.parse::<i32>().unwrap())));
			if let Some(job) = pgid_query_result {
				return Some(job.tabid().unwrap())
			}

			if arg.parse::<i32>().unwrap() > 0 {
				let table_id_query_result = j.query(JobID::TableID(arg.parse::<usize>().unwrap()));
				return table_id_query_result.map(|job| job.tabid().unwrap());
			}

			None
		});

		match result {
			Some(id) => Ok(id),
			None => Err(
				ShErr::full(
					ShErrKind::InternalErr,
					"Found a job but no table id in parse_job_id()",
					blame.into()
				)
			)
		}
	} else {
		Err(
			ShErr::full(
				ShErrKind::SyntaxErr,
				format!("Invalid fd arg: {}", arg),
				blame.into()
			)
		)
	}
}

pub fn jobs(node: Node, io_stack: &mut IoStack, job: &mut JobBldr) -> ShResult<()> {
	let NdRule::Command { assignments: _, argv } = node.class else {
		unreachable!()
	};

	let (argv,io_frame) = setup_builtin(argv, job, Some((io_stack,node.redirs)))?;

	let mut flags = JobCmdFlags::empty();
	for (arg,span) in argv {
		let mut chars = arg.chars().peekable();
		if chars.peek().is_none_or(|ch| *ch != '-') {
			return Err(
				ShErr::full(
					ShErrKind::SyntaxErr,
					"Invalid flag in jobs call",
					span.into()
				)
			)
		}
		chars.next();
		while let Some(ch) = chars.next() {
			let flag = match ch {
				'l' => JobCmdFlags::LONG,
				'p' => JobCmdFlags::PIDS,
				'n' => JobCmdFlags::NEW_ONLY,
				'r' => JobCmdFlags::RUNNING,
				's' => JobCmdFlags::STOPPED,
				_ => return Err(
					ShErr::full(
						ShErrKind::SyntaxErr,
						"Invalid flag in jobs call",
						span.into()
					)
				)

			};
			flags |= flag
		}
	}
	write_jobs(|j| j.print_jobs(flags))?;
	io_frame.unwrap().restore()?;
	state::set_status(0);

	Ok(())
}
