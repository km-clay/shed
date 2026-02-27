use crate::{
  jobs::{JobBldr, JobCmdFlags, JobID},
  libsh::error::{ShErr, ShErrKind, ShResult},
  parse::{NdRule, Node, lex::Span},
  prelude::*,
  procio::{IoStack, borrow_fd},
  state::{self, read_jobs, write_jobs},
};

use super::setup_builtin;

pub enum JobBehavior {
  Foregound,
  Background,
}

pub fn continue_job(node: Node, job: &mut JobBldr, behavior: JobBehavior) -> ShResult<()> {
  let blame = node.get_span().clone();
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

  let (argv, _) = setup_builtin(argv, job, None)?;
  let mut argv = argv.into_iter();

  if read_jobs(|j| j.get_fg().is_some()) {
    return Err(ShErr::full(
      ShErrKind::InternalErr,
      format!("Somehow called '{}' with an existing foreground job", cmd),
      blame,
    ));
  }

  let curr_job_id = if let Some(id) = read_jobs(|j| j.curr_job()) {
    id
  } else {
    return Err(ShErr::full(ShErrKind::ExecFail, "No jobs found", blame));
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
      Err(ShErr::full(
        ShErrKind::ExecFail,
        format!("Job id `{}' not found", tabid),
        blame,
      ))
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
      Ok(arg.parse::<usize>().unwrap())
    } else {
      let result = write_jobs(|j| {
        let query_result = j.query(JobID::Command(arg.into()));
        query_result.map(|job| job.tabid().unwrap())
      });
      match result {
        Some(id) => Ok(id),
        None => Err(ShErr::full(
          ShErrKind::InternalErr,
          "Found a job but no table id in parse_job_id()",
          blame,
        )),
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
      None => Err(ShErr::full(
        ShErrKind::InternalErr,
        "Found a job but no table id in parse_job_id()",
        blame,
      )),
    }
  } else {
    Err(ShErr::full(
      ShErrKind::SyntaxErr,
      format!("Invalid arg: {}", arg),
      blame,
    ))
  }
}

pub fn jobs(node: Node, io_stack: &mut IoStack, job: &mut JobBldr) -> ShResult<()> {
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  let (argv, _guard) = setup_builtin(argv, job, Some((io_stack, node.redirs)))?;

  let mut flags = JobCmdFlags::empty();
  for (arg, span) in argv {
    let mut chars = arg.chars().peekable();
    if chars.peek().is_none_or(|ch| *ch != '-') {
      return Err(ShErr::full(
        ShErrKind::SyntaxErr,
        "Invalid flag in jobs call",
        span,
      ));
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
          return Err(ShErr::full(
            ShErrKind::SyntaxErr,
            "Invalid flag in jobs call",
            span,
          ));
        }
      };
      flags |= flag
    }
  }
  write_jobs(|j| j.print_jobs(flags))?;
  state::set_status(0);

  Ok(())
}

pub fn disown(node: Node, io_stack: &mut IoStack, job: &mut JobBldr) -> ShResult<()> {
  let blame = node.get_span().clone();
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  let (argv, _guard) = setup_builtin(argv, job, Some((io_stack, node.redirs)))?;
  let mut argv = argv.into_iter();

  let curr_job_id = if let Some(id) = read_jobs(|j| j.curr_job()) {
    id
  } else {
    return Err(ShErr::full(
      ShErrKind::ExecFail,
      "disown: No jobs to disown",
      blame,
    ));
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
