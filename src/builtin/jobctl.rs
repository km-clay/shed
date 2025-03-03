use shellenv::jobs::JobCmdFlags;

use crate::prelude::*;

pub fn continue_job(node: Node, shenv: &mut ShEnv, fg: bool) -> ShResult<()> {
	let blame = node.span();
	let cmd = if fg { "fg" } else { "bg" };
	let rule = node.into_rule();
	if let NdRule::Command { argv, redirs } = rule {
		let mut argv_s = argv.drop_first().as_strings(shenv).into_iter();

		if read_jobs(|j| j.get_fg().is_some()) {
			return Err(
				ShErr::full(
					ShErrKind::InternalErr,
					format!("Somehow called {} with an existing foreground job",cmd),
					blame
				)
			)
		}

		let curr_job_id = if let Some(id) = read_jobs(|j| j.curr_job()) {
			id
		} else {
			return Err(ShErr::full(ShErrKind::ExecFail, "No jobs found", blame))
		};

		let tabid = match argv_s.next() {
			Some(arg) => parse_job_id(&arg, blame.clone())?,
			None => curr_job_id
		};

		let mut job = write_jobs(|j| {
			let id = JobID::TableID(tabid);
			let query_result = j.query(id.clone());
			if query_result.is_some() {
				Ok(j.remove_job(id.clone()).unwrap())
			} else {
				Err(ShErr::full(ShErrKind::ExecFail, format!("Job id `{}' not found", tabid), blame))
			}
		})?;

		job.killpg(Signal::SIGCONT)?;

		if fg {
			write_jobs(|j| j.new_fg(job))?;
		} else {
			let job_order = read_jobs(|j| j.order().to_vec());
			write(borrow_fd(1), job.display(&job_order, JobCmdFlags::PIDS).as_bytes())?;
			write_jobs(|j| j.insert_job(job, true))?;
		}
		shenv.set_code(0);
	} else { unreachable!() }
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
				None => Err(ShErr::full(ShErrKind::InternalErr,"Found a job but no table id in parse_job_id()",blame))
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
			None => Err(ShErr::full(ShErrKind::InternalErr,"Found a job but no table id in parse_job_id()",blame))
		}
	} else {
		Err(ShErr::full(ShErrKind::SyntaxErr,format!("Invalid fd arg: {}", arg),blame))
	}
}

pub fn jobs(node: Node, shenv: &mut ShEnv) -> ShResult<()> {
	let rule = node.into_rule();
	if let NdRule::Command { argv, redirs } = rule {
		let mut argv = argv.drop_first().into_iter();

		let mut flags = JobCmdFlags::empty();
		while let Some(arg) = argv.next() {
			let arg_s = arg.to_string();
			let mut chars = arg_s.chars().peekable();
			if chars.peek().is_none_or(|ch| *ch != '-') {
				return Err(ShErr::full(ShErrKind::SyntaxErr, "Invalid flag in jobs call", arg.span().clone()))
			}
			chars.next();
			while let Some(ch) = chars.next() {
				let flag = match ch {
					'l' => JobCmdFlags::LONG,
					'p' => JobCmdFlags::PIDS,
					'n' => JobCmdFlags::NEW_ONLY,
					'r' => JobCmdFlags::RUNNING,
					's' => JobCmdFlags::STOPPED,
					_ => return Err(ShErr::full(ShErrKind::SyntaxErr, "Invalid flag in jobs call", arg.span().clone()))

				};
				flags |= flag
			}
		}
		read_jobs(|j| j.print_jobs(flags))?;
		shenv.set_code(0);
	} else { unreachable!() }

	Ok(())
}
