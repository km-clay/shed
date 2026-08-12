//! This module contains functions for managing the lifecycle of the program.
//! These functions do stuff like setting up the logger, parsing the command line arguments, hanging up child processes on exit, etc.

use std::{io::Write, path::PathBuf, process::ExitCode, sync::atomic::Ordering};

use clap::Parser;

use crate::eval::execute;

use super::{
  ShResult, Shed, autocmd,
  eval::execute::{Dispatcher, exec_nonint},
  outln,
  procio::{self, RedirType},
  signal,
  state::{
    jobs::JobTab,
    logic::TrapTarget,
    meta::MetaTab,
    terminal::Terminal,
    util::{self, generate_default_rc, source_env},
    vars::{VarFlags, VarKind},
  },
  status_msg, try_var,
  util::flog,
};

#[expect(clippy::struct_excessive_bools)]
#[derive(Parser, Debug)]
#[command(
  author = "Kyler Clay",
  about = "An experimental POSIX shell",
  long_about = "shed is an experimental POSIX shell focused on interative user experience, extensibility, and powerful line editing."
)]
pub(super) struct ShedArgs {
  /// Evaluate the given string as a command and exit
  #[arg(short, long, conflicts_with_all = ["interactive", "stdin"])]
  pub(super) command: Option<String>,

  /// Script path and arguments
  #[arg(trailing_var_arg = true)]
  pub(super) script_args: Vec<String>,

  /// Print version info
  #[arg(long)]
  pub(super) version: bool,

  /// Start the shell in interactive mode
  #[arg(short, long)]
  pub(super) interactive: bool,

  /// Read input from stdin
  #[arg(short)]
  pub(super) stdin: bool,

  /// Start the shell as a login shell (sources .`shed_profile`)
  #[arg(long, short)]
  pub(super) login_shell: bool,

  /// Print the welcome message after arriving at the prompt
  #[arg(long, short)]
  pub(super) welcome: bool,

  /// Skip sourcing runtime command files
  #[arg(long)]
  pub(super) no_rc: bool,

  /// Provide the path to the runtime commands file
  #[arg(long)]
  pub(super) rc_path: Option<String>,

  /// List of POSIX 'set' options to enable
  #[arg(short = 'o', value_name = "OPTION", value_parser = Self::SET_OPTS)]
  pub(super) set: Vec<String>,

  /// Read and parse commands but do not execute them. Equivalent to `-o noexec`.
  #[arg(short = 'n')]
  pub(super) noexec: bool,

  /// Input is read as a keymap for the line editor to execute
  /// instead of raw shell commands. Used to script the line editor
  #[arg(long)]
  pub(super) edit_script: bool,
}

impl ShedArgs {
  const SET_OPTS: [&str; 15] = [
    "errexit",
    "allexport",
    "ignoreeof",
    "monitor",
    "noclobber",
    "noglob",
    "noexec",
    "nolog",
    "notify",
    "nounset",
    "verbose",
    "vi",
    "emacs",
    "xtrace",
    "hashall",
  ];
}

/// Internal set up for `shed`.
///
/// Does the following:
/// - enables `yansi`
/// - sets up `shed`'s panic handler
/// - initializes the `flog` logger
/// - sets the `$SH_LVL`, `$SHED_VERSION`, and `$SHED_VER_INFO` variables
/// - installs OS signal handlers so `trap` works in every mode
pub(super) fn setup() -> Option<ShedArgs> {
  yansi::enable();
  setup_panic_handler();
  flog::init().ok();
  util::set_ver_info().ok();
  util::set_sh_lvl().ok();

  let mut args = ShedArgs::parse();
  if std::env::args().next().is_some_and(|a| a.starts_with('-')) {
    // first arg is '-shed'
    // meaning we are in a login shell
    args.login_shell = true;
  }
  if args.version {
    outln!(
      "shed {} ({} {})",
      env!("CARGO_PKG_VERSION"),
      std::env::consts::ARCH,
      std::env::consts::OS
    );
    return None;
  }

  if !args.no_rc {
    if let Some(ref path) = args.rc_path {
      Shed::vars_mut(|v| v.set_var("SHED_RC", VarKind::string(path), VarFlags::EXPORT)).ok();
    }
    if let Err(e) = source_env() {
      e.print_error();
    }
  }

  if args.noexec {
    args.set.push("noexec".to_string());
  }

  for set_opt in &args.set {
    if set_opt == "emacs" {
      Shed::shopts_mut(|o| o.query("set.vi=false")).ok();
      continue;
    }
    Shed::shopts_mut(|o| o.query(&format!("set.{set_opt}=true"))).ok();
  }

  // Install trap handlers for every mode. The interactive loop calls
  // `sig_setup` later (adding tty/job-control setup on top); non-interactive
  // shells rely on this so `trap` works in `-c` and scripts.
  signal::install_signal_handlers();
  util::register_fork_marker();

  Some(args)
}

/// Run first time setup of `shed`.
///
/// Generates a default runtime commands file, and displays a status message announcing its path.
pub(super) fn first_run_setup() -> ShResult<()> {
  let rc_path = generate_default_rc()?;

  if let Some(rc_path) = rc_path {
    status_msg!("Generated default rc file at '{}'", rc_path.display());
  }

  Ok(())
}

/// We need to make sure that even if we panic, our child processes get sighup
///
/// This basically just wraps the default panic handler with our job control stuff
fn setup_panic_handler() {
  // take the default hook
  let default_panic_hook = std::panic::take_hook();

  // set our hook
  std::panic::set_hook(Box::new(move |info| {
    // hang up jobs
    Shed::jobs_mut(JobTab::hang_up);

    // log panic
    let data_dir = dirs::data_dir().unwrap_or_else(|| {
      let home = try_var!("HOME").unwrap();
      PathBuf::from(format!("{home}/.local/share"))
    });
    let log_dir = data_dir.join("shed").join("log");
    std::fs::create_dir_all(&log_dir).unwrap();
    let log_file_path = log_dir.join("panic.log");
    let mut log_file = procio::get_redir_file(RedirType::Output, log_file_path).unwrap();

    let panic_info_raw = info.to_string();
    log_file.write_all(panic_info_raw.as_bytes()).unwrap();
    log_file.write_all(b"\n\n").unwrap();

    let backtrace = std::backtrace::Backtrace::force_capture();
    log_file
      .write_all(format!("\nBacktrace:\n{backtrace:#?}").as_bytes())
      .unwrap();

    // call the default panic hook
    default_panic_hook(info);
  }));
}

/// Tear down `shed`'s execution environment.
///
/// When this program closes, we also need to send `SIGHUP` to any remaining child processes.
/// We also execute the `on-exit` autocmd group here.
///
/// The return is an `ExitCode` constructed from the current value of [`signal::QUIT_CODE`]
#[expect(clippy::cast_sign_loss)]
pub(super) fn tear_down() -> ExitCode {
  signal::clear_quit_latch();

  if let Some(trap) = Shed::logic(|l| l.get_trap(TrapTarget::Exit)) {
    execute::catch_exit(
      || exec_nonint(trap.clone(), Some("trap".into())),
      |code| signal::QUIT_CODE.store(code, Ordering::SeqCst),
    );
  }

  let mut deferred = Shed::vars_mut(|v| v.cur_scope_mut().take_deferred_cmds());

  while let Some(cmd) = deferred.pop() {
    let mut dispatcher = Dispatcher::new(vec![cmd], "defer".into());
    if let Err(e) = dispatcher.begin_dispatch() {
      e.print_error();
    }
  }

  if Shed::meta(MetaTab::interactive_shell) {
    autocmd!(OnExit);
    crate::write_term!("\n").ok();
  }

  Shed::jobs_mut(JobTab::hang_up);
  Shed::term_mut(Terminal::reset_for_exit);

  ExitCode::from(signal::QUIT_CODE.load(Ordering::SeqCst) as u8)
}

pub(super) fn exit_shed(run_trap: bool, code: i32) -> ! {
  signal::clear_quit_latch();

  let mut code = code;
  if run_trap && let Some(trap) = Shed::logic(|l| l.get_trap(TrapTarget::Exit)) {
    execute::catch_exit(
      || exec_nonint(trap.clone(), Some("trap".into())),
      |status| code = status,
    );
  }

  let mut deferred = Shed::vars_mut(|v| v.cur_scope_mut().take_deferred_cmds());

  while let Some(cmd) = deferred.pop() {
    let mut dispatcher = Dispatcher::new(vec![cmd], "defer".into());
    if let Err(e) = dispatcher.begin_dispatch() {
      e.print_error();
    }
  }

  std::process::exit(code)
}

/// Code for forked children to execute
///
/// Ideally this should be executed at the top of any `ForkResult::Child` block in the codebase
pub(super) fn setup_child() {
  if !util::FORKED_CHILD.load(Ordering::SeqCst) {
    return;
  }

  // remove the inherited exit trap
  Shed::logic_mut(|l| l.remove_trap(TrapTarget::Exit));
}
