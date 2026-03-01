use ariadne::Fmt;

use crate::{
  jobs::{JobCmdFlags, JobID, wait_bg},
  libsh::error::{ShErr, ShErrKind, ShResult, next_color},
  parse::{NdRule, Node, execute::prepare_argv, lex::Span},
  prelude::*,
  procio::borrow_fd,
  state::{self, read_jobs, write_jobs},
};

pub enum JobBehavior {
  Foregound,
  Background,
}

pub fn continue_job(node: Node, behavior: JobBehavior) -> ShResult<()> {
  let blame = node.get_span().clone();
	let cmd_tk = node.get_command();
	let cmd_span = cmd_tk.unwrap().span.clone();
  let cmd = match behavior {
    JobBehavior::Foregound => "fg",
    JobBehavior::Background => "bg",
  };
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  let mut argv = prepare_argv(argv)?;
  if !argv.is_empty() { argv.remove(0); }
  let mut argv = argv.into_iter();

  if read_jobs(|j| j.get_fg().is_some()) {
    return Err(ShErr::at(ShErrKind::InternalErr, cmd_span, format!("Somehow called '{}' with an existing foreground job", cmd)));
  }

  let curr_job_id = if let Some(id) = read_jobs(|j| j.curr_job()) {
    id
  } else {
    return Err(ShErr::at(ShErrKind::ExecFail, cmd_span, "No jobs found"));
  };

  let tabid = match argv.next() {
    Some((arg, blame)) => parse_job_id(&arg, blame)?,
    None => curr_job_id,
  };

  let mut job = write_jobs(|j| {
    let id = JobID::TableID(tabid);
    let query_result = j.query(id.clone());
    if query_result.is_some() {
      Ok(j.remove_job(id.clone()).unwrap())
    } else {
      Err(ShErr::at(ShErrKind::ExecFail, blame.clone(), format!("Job id `{}' not found", tabid)))
    }
  })?;

  job.killpg(Signal::SIGCONT)?;

  match behavior {
    JobBehavior::Foregound => {
      write_jobs(|j| j.new_fg(job))?;
    }
    JobBehavior::Background => {
      let job_order = read_jobs(|j| j.order().to_vec());
      write(
        borrow_fd(1),
        job.display(&job_order, JobCmdFlags::PIDS).as_bytes(),
      )?;
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
			let num = arg.parse::<usize>().unwrap_or_default();
			if num == 0 {
				Err(ShErr::at(ShErrKind::SyntaxErr, blame, format!("Invalid job id: {}", arg.fg(next_color()))))
			} else {
				Ok(num.saturating_sub(1))
			}
    } else {
      let result = write_jobs(|j| {
        let query_result = j.query(JobID::Command(arg.into()));
        query_result.map(|job| job.tabid().unwrap())
      });
      match result {
        Some(id) => Ok(id),
        None => Err(ShErr::at(ShErrKind::InternalErr, blame, "Found a job but no table id in parse_job_id()")),
      }
    }
  } else if arg.chars().all(|ch| ch.is_ascii_digit()) {
    let result = write_jobs(|j| {
      let pgid_query_result = j.query(JobID::Pgid(Pid::from_raw(arg.parse::<i32>().unwrap())));
      if let Some(job) = pgid_query_result {
        return Some(job.tabid().unwrap());
      }

      if arg.parse::<i32>().unwrap() > 0 {
        let table_id_query_result = j.query(JobID::TableID(arg.parse::<usize>().unwrap()));
        return table_id_query_result.map(|job| job.tabid().unwrap());
      }

      None
    });

    match result {
      Some(id) => Ok(id),
      None => Err(ShErr::at(ShErrKind::InternalErr, blame, "Found a job but no table id in parse_job_id()")),
    }
  } else {
    Err(ShErr::at(ShErrKind::SyntaxErr, blame, format!("Invalid arg: {}", arg.fg(next_color()))))
  }
}

pub fn jobs(node: Node) -> ShResult<()> {
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  let mut argv = prepare_argv(argv)?;
  if !argv.is_empty() { argv.remove(0); }

  let mut flags = JobCmdFlags::empty();
  for (arg, span) in argv {
    let mut chars = arg.chars().peekable();
    if chars.peek().is_none_or(|ch| *ch != '-') {
      return Err(ShErr::at(ShErrKind::SyntaxErr, span, "Invalid flag in jobs call"));
    }
    chars.next();
    for ch in chars {
      let flag = match ch {
        'l' => JobCmdFlags::LONG,
        'p' => JobCmdFlags::PIDS,
        'n' => JobCmdFlags::NEW_ONLY,
        'r' => JobCmdFlags::RUNNING,
        's' => JobCmdFlags::STOPPED,
        _ => {
          return Err(ShErr::at(ShErrKind::SyntaxErr, span, "Invalid flag in jobs call"));
        }
      };
      flags |= flag
    }
  }
  write_jobs(|j| j.print_jobs(flags))?;
  state::set_status(0);

  Ok(())
}

pub fn wait(node: Node) -> ShResult<()> {
	let blame = node.get_span().clone();
	let NdRule::Command {
		assignments: _,
		argv,
	} = node.class
	else {
		unreachable!()
	};

	let mut argv = prepare_argv(argv)?;
	if !argv.is_empty() { argv.remove(0); }
	if read_jobs(|j| j.curr_job().is_none()) {
		state::set_status(0);
		return Err(ShErr::at(ShErrKind::ExecFail, blame, "wait: No jobs found"));
	}
	let argv = argv.into_iter()
		.map(|arg| {
			if arg.0.as_str().chars().all(|ch| ch.is_ascii_digit()) {
				Ok(JobID::Pid(Pid::from_raw(arg.0.parse::<i32>().unwrap())))
			} else {
				Ok(JobID::TableID(parse_job_id(&arg.0, arg.1)?))
			}
		})
		.collect::<ShResult<Vec<JobID>>>()?;

	if argv.is_empty() {
		write_jobs(|j| j.wait_all_bg())?;
	} else {
		for arg in argv {
			wait_bg(arg)?;
		}
	}

	// don't set status here, the status of the waited-on job should be the status of the wait builtin
	Ok(())
}

pub fn disown(node: Node) -> ShResult<()> {
  let blame = node.get_span().clone();
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  let mut argv = prepare_argv(argv)?;
  if !argv.is_empty() { argv.remove(0); }
  let mut argv = argv.into_iter();

  let curr_job_id = if let Some(id) = read_jobs(|j| j.curr_job()) {
    id
  } else {
    return Err(ShErr::at(ShErrKind::ExecFail, blame, "disown: No jobs to disown"));
  };

  let mut tabid = curr_job_id;
  let mut nohup = false;
  let mut disown_all = false;

  while let Some((arg, span)) = argv.next() {
    match arg.as_str() {
      "-h" => nohup = true,
      "-a" => disown_all = true,
      _ => {
        tabid = parse_job_id(&arg, span.clone())?;
      }
    }
  }

  if disown_all {
    write_jobs(|j| j.disown_all(nohup))?;
  } else {
    write_jobs(|j| j.disown(JobID::TableID(tabid), nohup))?;
  }

  state::set_status(0);
  Ok(())
}
