use std::str::FromStr;

use ariadne::Fmt;

use crate::{
  jobs::{JobCmdFlags, JobID, wait_bg},
  libsh::error::{ShResult, next_color},
  parse::{NdRule, Node, execute::prepare_argv, lex::Span},
  prelude::*,
  procio::borrow_fd,
  sherr,
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
  if !argv.is_empty() {
    argv.remove(0);
  }
  let mut argv = argv.into_iter();

  if read_jobs(|j| j.get_fg().is_some()) {
    return Err(sherr!(
      InternalErr @ cmd_span,
      "Somehow called '{cmd}' with an existing foreground job"
    ));
  }

  let curr_job_id = if let Some(id) = read_jobs(|j| j.curr_job()) {
    id
  } else {
    return Err(sherr!(ExecFail @ cmd_span, "No jobs found"));
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
      Err(sherr!(
        ExecFail @ blame.clone(),
        "Job id `{tabid}' not found"
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

pub fn jobs(node: Node) -> ShResult<()> {
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  let mut argv = prepare_argv(argv)?;
  if !argv.is_empty() {
    argv.remove(0);
  }

  let mut flags = JobCmdFlags::empty();
  for (arg, span) in argv {
    let mut chars = arg.chars().peekable();
    if chars.peek().is_none_or(|ch| *ch != '-') {
      return Err(sherr!(
        SyntaxErr @ span,
        "Invalid flag in jobs call",
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
          return Err(sherr!(
            SyntaxErr @ span,
            "Invalid flag in jobs call",
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
  if !argv.is_empty() {
    argv.remove(0);
  }
  if read_jobs(|j| j.curr_job().is_none()) {
    state::set_status(0);
    return Err(sherr!(ExecFail @ blame, "wait: No jobs found"));
  }
  let argv = argv
    .into_iter()
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
  if !argv.is_empty() {
    argv.remove(0);
  }
  let mut argv = argv.into_iter();

  let curr_job_id = if let Some(id) = read_jobs(|j| j.curr_job()) {
    id
  } else {
    return Err(sherr!(
      ExecFail @ blame,
      "disown: No jobs to disown",
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

pub fn kill_builtin(node: Node) -> ShResult<()> {
	let NdRule::Command {
		assignments: _,
		argv,
	} = node.class else {
		unreachable!()
	};

	// TODO: This can probably be refactored to use the getopt framework
	let mut argv = prepare_argv(argv)?.into_iter().skip(1);

	let mut signal: Option<Signal> = None;
	let mut print_sig: Option<Signal> = None;
	let mut targets: Vec<(KillTarget,Span)> = vec![];

	let mut verbose = false;
	let mut rest_are_targets = false;
	while let Some((arg, span)) = argv.next() {
		if arg == "--" {
			rest_are_targets = true;
			continue
		} else if rest_are_targets {
			let target = parse_kill_target(&arg, span.clone())?;
			targets.push((target,span));
			continue
		}

		if !arg.starts_with("-") {
			if let Ok(target) = parse_kill_target(&arg, span.clone()) {
				targets.push((target,span));
			} else {
				return Err(sherr!(SyntaxErr @ span, "Invalid flag or kill target: {arg}"));
			}
		} else {
			let stripped = arg.trim_start_matches('-');

			match stripped {
				"v" | "verbose" => verbose = true,
				"l" => {
					let Some((arg,span)) = argv.next() else {
						let signals: String = crate::signal::ALL_SIGNALS
							.iter()
							.map(|sig| {
								let sig = sig.to_string();
								sig.strip_prefix("SIG").unwrap_or(&sig).to_string()
							})
							.collect::<Vec<_>>()
							.join(&state::get_separator());

						let stdout = borrow_fd(STDOUT_FILENO);
						write(stdout, signals.as_bytes())?;
						write(stdout, b"\n")?;

						state::set_status(0);
						return Ok(())
					};

					let parse_result = arg.parse::<Signal>()
						.or_else(|_| format!("SIG{arg}").parse::<Signal>());
					if let Ok(parsed_signal) = parse_result {
						print_sig = Some(parsed_signal);
						continue
					}

					let Ok(mut n) = arg.parse::<usize>() else {
						return Err(sherr!(SyntaxErr @ span, "Invalid signal name or number: {arg}"));
					};

					if n > 128 {
						n = n.saturating_sub(128);
					}

					let Ok(sig) = Signal::try_from(n as i32) else {
						return Err(sherr!(SyntaxErr @ span, "Invalid signal number: {n}"));
					};

					print_sig = Some(sig);
					break
				}
				"s" | "signal" => {
					let Some((arg,span)) = argv.next() else {
						return Err(sherr!(SyntaxErr @ span, "Expected signal name or number after -s"));
					};

					let parse_result = arg.parse::<Signal>()
						.or_else(|_| format!("SIG{arg}").parse::<Signal>());

					match parse_result {
						Ok(parsed) => {
							signal = Some(parsed);
						}
						Err(_) => {
							return Err(sherr!(SyntaxErr @ span, "Invalid signal name or number: {arg}"));
						}
					}
				}
				_ if stripped.parse::<usize>().is_ok() => {
					let n = stripped.parse::<usize>().unwrap();
					let Ok(sig) = Signal::try_from(n as i32) else {
						return Err(sherr!(SyntaxErr @ span, "Invalid signal number: {n}"));
					};
					signal = Some(sig);
				}
				_ => {
					let parse_result = stripped.parse::<Signal>()
						.or_else(|_| format!("SIG{stripped}").parse::<Signal>());
					if let Ok(parsed_signal) = parse_result {
						signal = Some(parsed_signal);
					} else if let Ok(target) = parse_kill_target(&arg, span.clone()) {
						targets.push((target,span));
					} else {
						return Err(sherr!(SyntaxErr @ span, "Invalid flag or kill target: {arg}"));
					}
				}
			}
		}
	}

	let stdout = borrow_fd(STDOUT_FILENO);

	if let Some(sig) = print_sig {
		let sig = sig.to_string();
		let sig = sig.strip_prefix("SIG").unwrap_or(&sig);

		write(stdout, sig.as_bytes())?;
		write(stdout, b"\n")?;
	} else if let Some(sig) = signal.or(Some(Signal::SIGTERM)) && !targets.is_empty() {
		for (target,blame) in targets {
			match target {
				KillTarget::Pid(pid) => {
					if verbose {
						write(stdout, format!("kill: killing process {pid} with {sig}\n").as_bytes())?;
						write(stdout, b"\n")?;
					}
					kill(pid, sig)?;
				}
				KillTarget::Pgid(pid) => {
					if verbose {
						write(stdout, format!("kill: killing process group {pid} with {sig}\n").as_bytes())?;
						write(stdout, b"\n")?;
					}
					killpg(pid, sig)?;
				}
				KillTarget::OurPgrp => {
					let pgrp = getpgrp();
					if verbose {
						write(stdout, format!("kill: killing shell's process group ({pgrp}) with {sig}\n").as_bytes())?;
						write(stdout, b"\n")?;
					}
					killpg(pgrp, sig)?;
				}
				KillTarget::Broadcast => {
					if verbose {
						write(stdout, format!("kill: broadcasting {sig} to all processes\n").as_bytes())?;
						write(stdout, b"\n")?;
					}
					kill(Pid::from_raw(-1), sig)?;
				}
				KillTarget::Job(job_id) => {
					write_jobs(|j| {
						if let Some(job) = j.query_mut(job_id.clone()) {
							if verbose {
								write(stdout, format!("kill: killing job {} with {sig}\n", job.name().unwrap_or(&format!("{job_id:?}"))).as_bytes())?;
								write(stdout, b"\n")?;
							}
							job.killpg(sig)
						} else {
							Err(sherr!(
								ExecFail @ blame.clone(),
								"Job not found"
							))
						}
					})?;
				}
			}
		}
	} else {
		let usage = "usage: kill [-signal_name] pid ...";
		let stderr = borrow_fd(STDERR_FILENO);
		write(stderr, usage.as_bytes())?;
		write(stderr, b"\n")?;
	}

	state::set_status(0);
	Ok(())
}
