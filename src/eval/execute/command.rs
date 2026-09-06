//! Execution of regular external commands
//!
//! This module handles the execution of regular external commands, including:
//! * setting up the environment
//! * handling redirections
//! * job table bookkeeping
//! * managing the resulting child processes.
//!
//! It also includes error handling for common execution errors such as command not found, permission denied, and exec format errors.

use std::{ffi::CString, sync::Arc, thread};

use itertools::Itertools;
use nix::{
  errno::Errno,
  unistd::{self, ForkResult, Pid},
};

use crate::{
  HashSet, autocmd,
  builtin::{self, Builtin},
  eval::parse::NdFlags,
  lifecycle,
  procio::Sinks,
  sherr, signal, socket,
  state::{
    Shed, cmd,
    jobs::{ChildProc, StageThread},
    meta::MetaTab,
    params, shopt,
    terminal::Terminal,
    thread::StageResult,
    vars::VarStr,
  },
  util::{
    error::{ShErr, ShResult},
    guards, posix, with_status,
  },
  varstr,
};

use super::{AssignBehavior, Ast, NdRule, NodeId};
impl super::Dispatcher {
  pub(super) fn exec_cmd(&mut self, tree: &Ast, cmd_id: NodeId) -> ShResult<()> {
    let cmd = &tree[cmd_id];
    let context = &cmd.context;
    let NdRule::Command { assignments, argv } = &cmd.class else {
      unreachable!(
        "found node class '{:?}' in exec_cmd",
        cmd.class.as_nd_kind()
      )
    };
    let assign_behavior = if argv.is_empty() {
      AssignBehavior::Set
    } else {
      AssignBehavior::Export
    };

    if let AssignBehavior::Set = assign_behavior {
      if Shed::meta_mut(MetaTab::take_fork) {
        let child = tree.break_off(cmd_id);
        let Some(root) = child.get_root() else {
          unreachable!()
        };
        return self.run_fork(b"", move |s| {
          super::catch_exit(|| s.exec_cmd(&child, root), super::exit_with);
        });
      }

      // argv is empty: a command with no command word. Perform any assignments
      // in the current shell, then apply any redirections.
      if !assignments.is_empty() {
        Shed::meta_mut(MetaTab::take_last_cmdsub_status);
        if let Err(e) = Self::set_assignments(tree, &tree[*assignments], assign_behavior) {
          Shed::set_status(1);
          e.print_error();
          return Ok(());
        }
      }
      match Sinks::try_apply_set(&tree[cmd.redirs].into(), false) {
        Ok(Some(_)) => {
          // command has only redirections: status 0 if it succeeded
          // then throw the guard away and return here

          if assignments.is_empty() {
            return with_status(0);
          }
          // keep status
          return Ok(());
        }
        Ok(None) => return Ok(()),
        Err(e) => return Err(e), // fatal error, propagate
      }
    }
    // argv is not empty. let's set this stuff here.
    let cmd_tk = &tree[argv.get(0)];

    let cmd_name = &cmd_tk.slice();
    let cmd_name = &cmd_name.to_str_lossy();

    let exec_path = cmd::lookup_cmd(cmd_name);

    let no_fork = cmd.flags.contains(NdFlags::NO_FORK);

    // POSIX 2.8.1: a redirection failure on an ordinary command is non-fatal
    let fatal = !Shed::term(Terminal::interactive)
      && builtin::lookup_builtin(cmd_name.as_bytes()).is_some_and(Builtin::is_special);
    let _guard = match Sinks::try_apply_set(&tree[cmd.redirs].into(), fatal) {
      Ok(Some(g)) => g,
      Ok(None) => return Ok(()),
      Err(e) => return Err(e),
    };
    let existing_pgid = self.job_stack.curr_job_mut().unwrap().pgid();

    let fg_job = self.fg_job;
    let interactive = Shed::term(Terminal::interactive);

    let expanded = super::prepare_argv(&tree[*argv])?;
    if expanded.is_empty() {
      Shed::set_status(0);
      return Ok(());
    }

    let mut resolved_env = vec![];

    // Resolve prefix assignments in the parent. We set them in the child later.
    if !assignments.is_empty() {
      let assignments = &tree[*assignments];
      let _guard = guards::prefix_assign_guard(tree, assignments);
      if let Err(e) = Self::set_assignments(tree, assignments, assign_behavior) {
        Shed::set_status(1);
        e.print_error();
        return Ok(());
      }
      let mut names: HashSet<VarStr> = HashSet::default();
      for id in assignments {
        let a = &tree[*id];
        if let NdRule::Assignment { var, .. } = &a.class {
          let name = tree[*var].span.slice();
          let raw = name.as_bytes();

          let name: VarStr =
            params::parse_arr_bracket(raw).map_or_else(|| raw.into(), |(base, _)| base);

          if names.insert(name.clone()) {
            let Some(var) = Shed::vars(|v| v.try_get_var_meta(&name.to_str_lossy())) else {
              continue;
            };
            resolved_env.push((name, var));
          }
        }
      }
    }

    if !cmd.flags.contains(NdFlags::NO_TRACE) {
      shopt::xtrace_print(&expanded);
    }

    let child_logic = |pgid: Option<Pid>| -> ! {
      lifecycle::setup_child();

      if let Some(pgid) = pgid {
        let _ = unistd::setpgid(Pid::from_raw(0), pgid);
      }
      // Apply the values resolved in the parent above (already `EXPORT`-flagged),
      // so `get_envp` picks them up for `execve`. No expansion happens here.
      for (name, var) in &resolved_env {
        let _ =
          Shed::vars_mut(|v| v.set_var(&name.to_str_lossy(), var.kind().clone(), var.flags()));
      }
      let exec_args = super::ExecArgs::from_expanded(expanded.clone());

      if interactive || !no_fork {
        signal::reset_signals(fg_job);
      }

      // Materialize the virtual fd table onto real fds before exec, so the
      // command inherits the redirs/pipe ends the parent wired up instead of
      // stale fds. Internal fds are CLOEXEC and drop out at exec.
      if let Err(e) = Shed::sinks(|s| s.commit_redirects()) {
        ShErr::from(e).print_error();
        unsafe { nix::libc::_exit(1) };
      }

      let cmd = &exec_args.cmd.0;
      let span = exec_args.cmd.1;
      let cmd_raw = cmd.to_str().unwrap_or_default();

      let Err(e) = if let Some(path) = exec_path {
        let path_bytes = path.as_os_str().to_str().unwrap_or_default().as_bytes();
        let c_path = CString::new(path_bytes).unwrap_or_default();
        let mut envp = exec_args.envp.to_vec();
        envp.retain(|e| !e.as_bytes().starts_with(b"_="));
        envp.push(unsafe { CString::from_vec_unchecked([b"_=", path_bytes].concat()) });

        unistd::execve(&c_path, &exec_args.argv, &envp)
      } else {
        log::warn!("command not found in cache: {cmd_raw}");
        posix::execvpe(cmd, &exec_args.argv, &exec_args.envp)
      };

      // execvpe only returns on error
      let print_error = |err: ShErr| {
        if interactive {
          // try reporting the error to the parent shell
          // if we are interactive, there should be a socket to post this to
          let request = socket::authorize(format_args!("post-error {err}"));
          if socket::send_to_socket(&request).is_ok() {
            return;
          }
        }

        // if that fails, or we are not in an interactive shell, just print it here
        err.print_error();
      };
      match e {
        Errno::ENOENT => {
          let suggestions = cmd::check_typo(cmd.as_bytes());
          let note = match suggestions.as_slice() {
            [] => None,
            [one] => Some(varstr!("did you mean '{one}'?")),
            many => {
              let list = many.iter().map(|s| format!("'{s}'")).join(", ");

              Some(varstr!("did you mean one of: {list}?"))
            }
          };

          let mut err =
            sherr!(NotFound @ span, "command not found").with_context(tree[*context].iter());
          if let Some(note) = note {
            err = err.with_note(note);
          }

          print_error(err);

          params::with_vars([("CMD".into(), cmd.to_str().unwrap_or_default())], || {
            autocmd!(OnCommandNotFound);
          });

          unsafe { nix::libc::_exit(127) };
        }
        Errno::EACCES => {
          let err =
            sherr!(BadPermission @ span, "permission denied").with_context(tree[*context].iter());
          print_error(err);

          unsafe { nix::libc::_exit(126) };
        }
        Errno::EISDIR => {
          let err = sherr!(ExecFail @ span, "is a directory").with_context(tree[*context].iter());
          print_error(err);
          unsafe { nix::libc::_exit(126) };
        }
        Errno::ENOEXEC => {
          let err =
            sherr!(ExecFail @ span, "exec format error").with_context(tree[*context].iter());
          print_error(err);
          unsafe { nix::libc::_exit(126) };
        }
        _ => {
          let err = sherr!(Errno(e) @ span, "{e}").with_context(tree[*context].iter());
          print_error(err);
          unsafe { nix::libc::_exit(e as i32) }
        }
      }
    };

    if no_fork {
      child_logic(existing_pgid);
    }

    match unsafe { unistd::fork()? } {
      ForkResult::Child => child_logic(existing_pgid),
      ForkResult::Parent { child } => {
        let timer = self.take_timer();
        let job = self.job_stack.curr_job_mut().unwrap();

        let child_pgid = if let Some(pgid) = existing_pgid {
          pgid
        } else if interactive {
          job.set_pgid(child);
          child
        } else {
          let pgrp = unistd::getpgrp();
          job.set_pgid(pgrp);
          pgrp
        };
        let child_proc = ChildProc::new(child, Some(cmd_name.as_bytes()), Some(child_pgid), timer);
        job.push_child(child_proc);
      }
    }

    Ok(())
  }

  pub(super) fn spawn_stage(&self, tree: &Ast, node: NodeId, sinks: Sinks) -> StageThread {
    let ast = Arc::new(tree.break_off(node));
    let spec = Shed::fork_spec(sinks, ast);
    let source = self.source_name.clone();
    let handle = thread::spawn(move || {
      let ast = Shed::install(spec);
      let root = ast.get_root().unwrap();
      let mut d = super::Dispatcher::new(source);
      d.job_stack.new_job();
      if let Err(e) = d.dispatch_node(&ast, root)
        && !e.is_flow_control()
      {
        e.print_error();
      }
      if let Some(job) = d.job_stack.finalize_job() {
        crate::state::jobs::dispatch_job(job, false, false).ok();
      }
      StageResult::new(Shed::get_status())
    });

    let cmd_name = tree.command_for(node).map(|tk| tk.slice());

    StageThread::new(handle).with_name(cmd_name)
  }
}
