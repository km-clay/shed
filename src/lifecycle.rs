//! This module contains functions for managing the lifecycle of the program.
//! These functions do stuff like setting up the logger, parsing the command line arguments, hanging up child processes on exit, etc.

use std::{
  io::Write,
  os::unix::ffi::{OsStrExt, OsStringExt},
  path::PathBuf,
  process::ExitCode,
  sync::atomic::Ordering,
};

use nix::sys::signal::{
  SaFlags, SigAction, SigHandler, SigSet, SigmaskHow, Signal, raise, sigaction, sigprocmask,
};

use crate::eval::execute;

use super::{
  ShResult, Shed, autocmd,
  builtin::set::{Role, SetFlags, scan_options},
  eval::{execute::exec_nonint, lex::Span},
  outln,
  procio::{self, RedirType},
  sherr, signal,
  state::{
    jobs::JobTab,
    logic::{LogTab, TrapTarget},
    meta::MetaTab,
    terminal::Terminal,
    util::{self, generate_default_rc, source_env},
    vars::{VarFlags, VarKind, VarStr},
  },
  status_msg,
  util::flog,
};

#[expect(clippy::struct_excessive_bools)]
#[derive(Debug, Default)]
pub(super) struct ShedArgs {
  /// Evaluate the given string as a command and exit (`-c`)
  pub(super) command: Option<String>,
  /// Script path and positional arguments
  pub(super) script_args: Vec<String>,
  /// Print version info (`--version`)
  pub(super) version: bool,
  /// Start the shell in interactive mode (`-i`)
  pub(super) interactive: bool,
  /// Read input from stdin (`-s`)
  pub(super) stdin: bool,
  /// Start the shell as a login shell (sources .`shed_profile`)
  pub(super) login_shell: bool,
  /// Print the welcome message after arriving at the prompt (`-w`)
  pub(super) welcome: bool,
  /// Skip sourcing runtime command files (`--no-rc`)
  pub(super) no_rc: bool,
  /// Provide the path to the runtime commands file (`--rc-path`)
  pub(super) rc_path: Option<String>,
  /// Input is read as a keymap for the line editor (`--edit-script`)
  pub(super) edit_script: bool,
}

/// Print a short usage synopsis for `--help`.
fn print_usage() {
  outln!(
    "\x1b[1;4mshed\x1b[0m - an experimental POSIX shell\n\n\
     \x1b[1;4mUsage\x1b[0m: shed [OPTIONS] [SCRIPT [ARGS...]]\n\n\
     \x1b[1;4mArguments\x1b[0m: [SCRIPT [ARGS...]] - script path and positional arguments\n\n\
     \x1b[1;4mOptions\x1b[0m:\n  \
     \x1b[1m-c/--command\x1b[0m <COMMAND>  evaluate COMMAND and exit\n  \
     \x1b[1m-s\x1b[0m                      read commands from stdin\n  \
     \x1b[1m-i/--interactive\x1b[0m        force interactive mode\n  \
     \x1b[1m-l/--login\x1b[0m              run as a login shell\n  \
     \x1b[1m-w\x1b[0m                      print the welcome message\n  \
     \x1b[1m-o\x1b[0m NAME / +o NAME       enable/disable a `set` option (e.g. -o errexit)\n  \
     \x1b[1m-e\x1b[0m -x -u ...            any short `set` option (see `help set`)\n  \
     \x1b[1m--no-rc\x1b[0m                 skip runtime command files\n  \
     \x1b[1m--rc-path\x1b[0m PATH          use PATH as the runtime commands file\n  \
     \x1b[1m--version\x1b[0m               print version info\n  \
     \x1b[1m--about\x1b[0m                  print program info\n  \
     \x1b[1m--help\x1b[0m                  print this message"
  );
}

/// Print program information for `--about`: description, author, license, and
/// the source repository. Metadata is sourced from `Cargo.toml` so it stays in
/// sync with the package.
fn print_about() {
  let cargo_ver = env!("CARGO_PKG_VERSION");
  let cargo_authors = env!("CARGO_PKG_AUTHORS");
  let cargo_license = env!("CARGO_PKG_LICENSE");
  let cargo_repo = env!("CARGO_PKG_REPOSITORY");
  outln!(
    "\x1b[1;4mshed\x1b[0m {cargo_ver}\n\n\
     An experimental POSIX shell focused on interactive user experience,\n\
     extensibility, and powerful line editing.\n\n\
     \x1b[1mAuthor:\x1b[0m     {cargo_authors}\n\
     \x1b[1mLicense:\x1b[0m    {cargo_license}\n\
     \x1b[1mSource:\x1b[0m     {cargo_repo}",
  );
}

/// Apply an invocation-only short flag (`-c`/`-s`/`-i`/`-l`/`-w`).
fn apply_invocation_flag<I>(
  ch: char,
  attached: Option<VarStr>,
  words: &mut std::iter::Peekable<I>,
  span: Span,
  cfg: &mut ShedArgs,
) -> ShResult<()>
where
  I: Iterator<Item = (VarStr, Span)>,
{
  match ch {
    'c' => {
      let val = attached
        .or_else(|| words.next().map(|(w, _)| w))
        .ok_or_else(|| sherr!(ParseErr @ span, "shed: -c requires an argument"))?;
      cfg.command = Some(val.to_string());
    }
    's' => cfg.stdin = true,
    'i' => cfg.interactive = true,
    'l' => cfg.login_shell = true,
    'w' => cfg.welcome = true,
    _ => unreachable!("classify only routes c/s/i/l/w to invocation"),
  }
  Ok(())
}

/// Handle a `--long` invocation flag.
///
/// Takes an iterator of [`VarStr`] and [`Span`], and mutates the given [`ShedArgs`] accordingly.
///
/// ### Errors:
/// Returns an error if the flag is unrecognized or missing a required argument.
fn handle_long_flag<I>(words: &mut std::iter::Peekable<I>, cfg: &mut ShedArgs) -> ShResult<()>
where
  I: Iterator<Item = (VarStr, Span)>,
{
  let (word, span) = words.next().unwrap();
  match word.to_str_lossy().as_ref() {
    "--version" => cfg.version = true,
    "--help" => {
      print_usage();
      std::process::exit(0);
    }
    "--about" => {
      print_about();
      std::process::exit(0);
    }
    "--command" => {
      let val = words
        .next()
        .map(|(w, _)| w.to_string())
        .ok_or_else(|| sherr!(ParseErr @ span, "shed: --command requires an argument"))?;
      cfg.command = Some(val);
    }
    "--interactive" => cfg.interactive = true,
    "--login" | "--login-shell" => cfg.login_shell = true,
    "--welcome" => cfg.welcome = true,
    "--no-rc" => cfg.no_rc = true,
    "--rc-path" => {
      let val = words
        .next()
        .map(|(w, _)| w.to_string())
        .ok_or_else(|| sherr!(ParseErr @ span, "shed: --rc-path requires an argument"))?;
      cfg.rc_path = Some(val);
    }
    "--edit-script" | "--script" => cfg.edit_script = true,
    other => return Err(sherr!(ParseErr @ span, "shed: unrecognized option '{other}'")),
  }
  Ok(())
}

/// Parse `shed`'s command-line arguments.
///
/// Converts arguments provided by [`std::env::args_os`]
fn parse_args() -> ShResult<ShedArgs> {
  let mut cfg = ShedArgs::default();

  // we use a placeholder `Span::default()` here since we re-use the `set` builtin's option parser
  let mut words = std::env::args_os()
    .skip(1)
    .map(|a| (VarStr::from(a.into_vec()), Span::default()))
    .peekable();

  while let Some((word, _)) = words.peek() {
    let word = word.to_str_lossy();
    if word == "--" {
      words.next();
      break;
    }
    if word.starts_with("--") {
      handle_long_flag(&mut words, &mut cfg)?;
      continue;
    }
    if word == "-" {
      break; // a lone `-` means read from stdin; treat as an operand
    }
    if word.starts_with('-') || word.starts_with('+') {
      let outcome = scan_options(
        &mut words,
        |ch| match ch {
          'c' | 's' | 'i' | 'l' | 'w' => Role::Invocation,
          other => SetFlags::try_from(other).map_or(Role::Unknown, Role::Set),
        },
        |ch, attached, rest, span| apply_invocation_flag(ch, attached, rest, span, &mut cfg),
        true,
      )?;
      if outcome.terminated {
        break;
      }
      continue;
    }
    break; // first operand
  }

  cfg.script_args = words.map(|(w, _)| w.to_string()).collect();
  Ok(cfg)
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

  // Parse argv with shed's own option scanner (shared with the `set` builtin),
  // so `-e`/`-x`/`-o pipefail`/`+e` etc. behave identically at invocation and
  // via `set`. Note: this applies set-opts *before* `source_env` below.
  let mut args = match parse_args() {
    Ok(args) => args,
    Err(e) => {
      e.print_error();
      std::process::exit(2);
    }
  };
  if std::env::args_os()
    .next()
    .is_some_and(|a| a.as_bytes().first() == Some(&b'-'))
  {
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
      Shed::vars_mut(|v| v.set_var("SHED_RC", VarKind::string(path.into()), VarFlags::EXPORT)).ok();
    }
    if let Err(e) = source_env() {
      e.print_error();
    }
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
/// Also logs the panic to a file in the shed data directory
fn setup_panic_handler() {
  // take the default hook
  let default_panic_hook = std::panic::take_hook();

  // set our hook
  std::panic::set_hook(Box::new(move |info| {
    // Best-effort job hangup.
    Shed::try_hang_up();

    let time = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC");

    // Best-effort panic log.
    let log_file = util::data_dir()
      .or_else(|| {
        std::env::var("HOME")
          .ok()
          .map(|home| PathBuf::from(format!("{home}/.local/share")))
      })
      .map(|dir| dir.join("shed").join("log"))
      .filter(|dir| std::fs::create_dir_all(dir).is_ok())
      .and_then(|dir| procio::get_redir_file(RedirType::Output, dir.join("panic.log")).ok());

    if let Some(mut log_file) = log_file {
      let backtrace = std::backtrace::Backtrace::force_capture();
      let _ = write!(log_file, "{time} - {info}\n\n\nBacktrace:\n{backtrace:#?}");
    }

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

  if let Some(trap) = Shed::logic_mut(|l| l.remove_trap(TrapTarget::Exit)) {
    execute::catch_exit(
      || exec_nonint(trap.clone(), Some("trap".into())),
      |code| signal::QUIT_CODE.store(code, Ordering::SeqCst),
    );
  }

  let mut deferred = Shed::vars_mut(|v| v.cur_scope_mut().take_deferred_cmds());

  while let Some(cmd) = deferred.pop() {
    execute::dispatch_deferred_cmd(&cmd);
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
  let quit_sig = signal::quit_signal();
  signal::clear_quit_latch();

  let mut code = code;
  if run_trap && let Some(trap) = Shed::logic_mut(|l| l.remove_trap(TrapTarget::Exit)) {
    execute::catch_exit(
      || exec_nonint(trap.clone(), Some("trap".into())),
      |status| code = status,
    );
  }

  let mut deferred = Shed::vars_mut(|v| v.cur_scope_mut().take_deferred_cmds());

  while let Some(cmd) = deferred.pop() {
    execute::dispatch_deferred_cmd(&cmd);
  }

  if let Some(sig) = quit_sig {
    exit_signaled(sig);
  }

  std::process::exit(code)
}

pub fn exit_signaled(sig: Signal) {
  let dfl = SigAction::new(SigHandler::SigDfl, SaFlags::empty(), SigSet::empty());
  unsafe {
    sigaction(sig, &dfl).ok();
  }

  let mut set = SigSet::empty();
  set.add(sig);
  sigprocmask(SigmaskHow::SIG_UNBLOCK, Some(&set), None).ok();

  let _ = raise(sig);
}

/// Code for forked children to execute
///
/// Ideally this should be executed at the top of any `ForkResult::Child` block in the codebase
pub(super) fn setup_child() {
  if !util::FORKED_CHILD.load(Ordering::SeqCst) {
    return;
  }

  Shed::meta_mut(|m| m.set_interactive_shell(false));
  Shed::meta_mut(|m| m.restore_fork(false));
  Shed::logic_mut(LogTab::reset_caught_traps);
}
