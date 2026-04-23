use ariadne::Fmt;

use crate::{
  builtin::BuiltinArgs,
  getopt::{Opt, OptSpec},
  jobs::{JobCmdFlags, JobID, wait_bg},
  out, outln,
  parse::lex::Span,
  prelude::*,
  sherr,
  signal::parse_signal,
  state::{self, read_jobs, write_jobs},
  util::{
    error::{ShResult, ShResultExt, next_color},
    with_status,
  },
};

fn parse_job_id(arg: &str, blame: Span) -> ShResult<usize> {
  if arg.starts_with('%') {
    let arg = arg.strip_prefix('%').unwrap();
    if arg.chars().all(|ch| ch.is_ascii_digit()) {
      let num = arg.parse::<usize>().unwrap_or_default();
      if num == 0 {
        Err(sherr!(
          SyntaxErr @ blame,
          "Invalid job id: {}", arg.fg(next_color()),
        ))
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
        None => Err(sherr!(
          InternalErr @ blame,
          "Found a job but no table id in parse_job_id()",
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
      None => Err(sherr!(
        InternalErr @ blame,
        "Found a job but no table id in parse_job_id()",
      )),
    }
  } else {
    Err(sherr!(
      SyntaxErr @ blame,
      "Invalid arg: {}", arg.fg(next_color()),
    ))
  }
}

pub enum JobBehavior {
  Foregound,
  Background,
}

pub(super) struct Fg;
impl super::Builtin for Fg {
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    continue_job(args, JobBehavior::Foregound)
  }
}

pub(super) struct Bg;
impl super::Builtin for Bg {
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    continue_job(args, JobBehavior::Background)
  }
}

pub fn continue_job(args: BuiltinArgs, behavior: JobBehavior) -> ShResult<()> {
  let span = args.span();
  let mut argv = args.argv.into_iter();

  let curr_job_id = if let Some(id) = read_jobs(|j| j.curr_job()) {
    id
  } else {
    return Err(sherr!(ExecFail @ span, "No jobs found"));
  };

  let tabid = match argv.next() {
    Some((arg, blame)) => parse_job_id(&arg, blame)?,
    None => curr_job_id,
  };

  let Some(mut job) = write_jobs(|j| j.remove_job(JobID::TableID(tabid))) else {
    return Err(sherr!(
      ExecFail @ span,
      "Job id `{tabid}' not found"
    ));
  };

  job.killpg(Signal::SIGCONT)?;

  match behavior {
    JobBehavior::Foregound => {
      write_jobs(|j| j.new_fg(job))?;
    }
    JobBehavior::Background => {
      let job_order = read_jobs(|j| j.order().to_vec());
      out!("{}", job.display(&job_order, JobCmdFlags::PIDS))?;
      write_jobs(|j| j.insert_job(job, true))?;
    }
  }

  with_status(0)
}

pub(super) struct Jobs;
impl super::Builtin for Jobs {
  fn opts(&self) -> Vec<crate::getopt::OptSpec> {
    vec![
      OptSpec::flag('l'),
      OptSpec::flag('p'),
      OptSpec::flag('n'),
      OptSpec::flag('r'),
      OptSpec::flag('s'),
    ]
  }
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    let mut flags = JobCmdFlags::empty();
    for opt in &args.opts {
      match opt {
        Opt::Short('l') => flags |= JobCmdFlags::LONG,
        Opt::Short('p') => flags |= JobCmdFlags::PIDS,
        Opt::Short('n') => flags |= JobCmdFlags::NEW_ONLY,
        Opt::Short('r') => flags |= JobCmdFlags::RUNNING,
        Opt::Short('s') => flags |= JobCmdFlags::STOPPED,
        _ => {
          return Err(sherr!(
            SyntaxErr @ args.span(),
            "Invalid flag in jobs call",
          ));
        }
      }
    }

    write_jobs(|j| j.print_jobs(flags))?;
    with_status(0)
  }
}

pub(super) struct Wait;
impl super::Builtin for Wait {
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    let span = args.span();
    if read_jobs(|j| j.curr_job().is_none()) {
      return Err(sherr!(ExecFail @ span, "wait: No jobs found"));
    }
    let argv = args
      .argv
      .into_iter()
      .map(|arg| {
        if arg.0.as_str().chars().all(|ch| ch.is_ascii_digit()) {
          Ok(JobID::Pid(Pid::from_raw(arg.0.parse::<i32>().unwrap())))
        } else {
          Ok(JobID::TableID(parse_job_id(&arg.0, arg.1)?))
        }
      })
      .collect::<ShResult<Vec<JobID>>>()
      .promote_err(span.clone())?;

    if argv.is_empty() {
      write_jobs(|j| j.wait_all_bg()).promote_err(span)?;
    } else {
      for arg in argv {
        wait_bg(arg).promote_err(span.clone())?;
      }
    }

    // we dont set the status here
    // the status of the waited-on job should be the status of the wait builtin
    Ok(())
  }
}

pub(super) struct Disown;
impl super::Builtin for Disown {
  fn opts(&self) -> Vec<OptSpec> {
    vec![OptSpec::flag('h'), OptSpec::flag('a')]
  }
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    let span = args.span();
    let mut nohup = false;
    let mut disown_all = false;

    let Some(id) = read_jobs(|j| j.curr_job()) else {
      return Err(sherr!(
          ExecFail @ span,
          "disown: No jobs to disown",
      ));
    };

    let mut ids = vec![id];

    for opt in args.opts {
      match opt {
        Opt::Short('h') => nohup = true,
        Opt::Short('a') => disown_all = true,
        _ => {
          return Err(sherr!(
            SyntaxErr @ span,
            "Invalid flag in disown call",
          ));
        }
      }
    }

    for (arg, span) in args.argv {
      let id = parse_job_id(&arg, span)?;
      ids.push(id);
    }

    if disown_all {
      write_jobs(|j| j.disown_all(nohup))?;
    } else {
      for id in ids {
        write_jobs(|j| j.disown(JobID::TableID(id), nohup))?;
      }
    }

    with_status(0)
  }
}

enum KillTarget {
  Pid(Pid),
  Pgid(Pid),
  OurPgrp,
  Broadcast,
  Job(JobID),
}

fn parse_kill_target(arg: &str, blame: Span) -> ShResult<KillTarget> {
  let Ok(n) = arg.parse::<i32>() else {
    let Ok(id) = parse_job_id(arg, blame.clone()) else {
      return Err(sherr!(ParseErr @ blame, "Invalid kill target: {arg}"));
    };
    return Ok(KillTarget::Job(JobID::TableID(id)));
  };

  Ok(match n {
    -1 => KillTarget::Broadcast,
    0 => KillTarget::OurPgrp,
    _ if n < -1 => KillTarget::Pgid(Pid::from_raw(-n)),
    _ => KillTarget::Pid(Pid::from_raw(n)),
  })
}

fn list_all_signals() -> ShResult<()> {
  let signals: String = crate::signal::ALL_SIGNALS
    .iter()
    .map(|sig| {
      let sig = sig.to_string();
      sig.strip_prefix("SIG").unwrap_or(&sig).to_string()
    })
    .collect::<Vec<_>>()
    .join(&state::get_separator());
  outln!("{signals}")?;
  Ok(())
}

fn send_signal(target: &KillTarget, sig: Signal, verbose: bool, blame: &Span) -> ShResult<()> {
  let desc = match target {
    KillTarget::Pid(pid) => {
      kill(*pid, sig)?;
      format!("killing process {pid} with {sig}")
    }
    KillTarget::Pgid(pid) => {
      killpg(*pid, sig)?;
      format!("killing process group {pid} with {sig}")
    }
    KillTarget::OurPgrp => {
      let pgrp = getpgrp();
      killpg(pgrp, sig)?;
      format!("killing shell's process group ({pgrp}) with {sig}")
    }
    KillTarget::Broadcast => {
      kill(Pid::from_raw(-1), sig)?;
      format!("broadcasting {sig} to all processes")
    }
    KillTarget::Job(job_id) => {
      write_jobs(|j| {
        if let Some(job) = j.query_mut(job_id.clone()) {
          job.killpg(sig)
        } else {
          Err(sherr!(ExecFail @ blame.clone(), "Job not found"))
        }
      })?;
      format!("killing job {job_id:?} with {sig}")
    }
  };
  if verbose {
    outln!("kill: {desc}")?;
  }
  Ok(())
}

pub(super) struct Kill;
impl super::Builtin for Kill {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::flag('l'),
      OptSpec::flag('v'),
      OptSpec::single_arg('s'),
    ]
  }

  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let mut signal: Option<Signal> = None;
    let mut list_sig = false;
    let mut verbose = false;

    for opt in &args.opts {
      match opt {
        Opt::Short('v') => verbose = true,
        Opt::Short('l') => list_sig = true,
        Opt::ShortWithArg('s', sig_name) => {
          signal = Some(parse_signal(sig_name).promote_err(args.span.clone())?);
        }
        _ => {}
      }
    }

    if list_sig {
      let Some((arg, span)) = args.argv.first() else {
        // kill -l - list all signals
        return list_all_signals();
      };

      // kill -l <signal> - print the name
      let sig = parse_signal(arg).promote_err(span.clone())?;
      let name = sig.to_string();
      outln!("{}", name.strip_prefix("SIG").unwrap_or(&name))?;

      return with_status(0);
    }

    if args.argv.is_empty() {
      return Err(sherr!(SyntaxErr @ args.span, "usage: kill [-signal] pid ..."));
    }

    let sig = signal.unwrap_or(Signal::SIGTERM);

    for (arg, span) in &args.argv {
      // Check if the arg looks like a signal (e.g. kill -TERM pid)
      if arg.starts_with('-') && !arg.starts_with("--") {
        let stripped = arg.trim_start_matches('-');
        if let Ok(sig_override) = parse_signal(stripped).promote_err(span.clone()) {
          signal.replace(sig_override);
          continue;
        }
      }

      let target = parse_kill_target(arg, span.clone())?;
      send_signal(&target, signal.unwrap_or(sig), verbose, span)?;
    }

    with_status(0)
  }
}
