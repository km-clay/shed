//! `shed`'s builtin commands.
//!
//! This module contains the implementation of all builtin commands, as well as the [`Builtin`] trait
//! that defines the interface for builtins. Each builtin is implemented as a struct that implements
//! the [`Builtin`] trait. The builtins are registered in the [`BUILTIN_TABLE`] static variable, which
//! is used to look up builtins by name.

use nix::unistd::Pid;
use std::{
  fs,
  io::{self, Read},
};

use crate::{
  defer, errln,
  eval::{
    execute::{self, AssignBehavior, Dispatcher},
    lex::{KEYWORDS, Span, Tk, TkRule},
    parse::{
      NdFlags, NdRule,
      ast::{Ast, NodeId},
    },
  },
  expand::{arithmetic, escape},
  outln,
  procio::{self, RedirResult, RedirSet},
  sherr, signal,
  state::{
    Shed, cmd,
    jobs::ChildProc,
    meta::{MetaTab, UtilKind},
    params, shopt as shopts,
    terminal::Terminal,
    vars::VarStr,
  },
  util::{
    self,
    error::{ShErrKind, ShResult, ShResultExt},
    guards,
  },
};

mod alias;
pub(crate) mod argv;
mod arrops;
mod autocmd;
mod autoload;
mod cd;
mod complete;
mod dirstack;
mod echo;
mod evaluate;
mod exec;
mod fixcmd;
mod flog;
mod flowctl;
mod genrc;
mod getopts;
mod hash;
mod help;
mod hist;
mod intro;
mod jobctl;
mod keymap;
mod msg;
mod opt;
mod printf;
mod pwd;
mod quote;
mod read;
mod resource;
mod scry;
mod seek;
pub(crate) mod set;
mod shift;
mod shopt;
mod sock;
mod source;
mod stash;
mod stat;
mod test; // [[ ]] thing
mod times;
mod trap;
mod varcmds;
mod vice;
mod width;

pub(crate) use argv::{BuiltinArgs, join_raw_args};
pub(crate) use help::HELP_PAGE_INSTALL_DIR;
use opt::{OptSpec, Parsed};

/// A macro to register builtins in the `BUILTIN_TABLE` static variable.
macro_rules! register_builtins {
  ($($name:literal => $ty:expr),* $(,)?) => {
    static BUILTIN_TABLE: &[(&[u8], &dyn Builtin)] = &[
      $(($name, &$ty)),*
    ];

    pub(crate) const BUILTIN_NAMES: &[&[u8]] = &[
      $($name),*
    ];

    // this is in util::macros
    $crate::assert_sorted!(BUILTIN_NAMES);
  };
}

// these have to be in alphabetical order, because of the way lookup_builtin() works
// if the list is unsorted, that is a compile error thanks to the const evaluation above
// if you're using vim, you can visual select the block and filter it through ''<,'>:!LC_ALL=C sort'
// if you're not using vim, idk. you know the alphabet right?
register_builtins! {
  b"."        => source::Source,
  b":"        => Colon,
  b"["        => test::Test,
  b"[["       => test::Test,
  b"accept"   => sock::Accept,
  b"alias"    => alias::Alias,
  b"autocmd"  => autocmd::AutoCmdBuiltin,
  b"autoload" => autoload::Autoload,
  b"bg"       => jobctl::Bg,
  b"break"    => flowctl::Break,
  b"builtin"  => BuiltinBuiltin,
  b"cd"       => cd::Cd,
  b"command"  => CommandBuiltin,
  b"compadd"  => complete::Compadd,
  b"compgen"  => complete::CompGen,
  b"complete" => complete::Complete,
  b"continue" => flowctl::Continue,
  b"declare"  => varcmds::Declare,
  b"dirs"     => dirstack::Dirs,
  b"disown"   => jobctl::Disown,
  b"echo"     => echo::Echo,
  b"eval"     => evaluate::Eval,
  b"excmd"    => alias::ExCmd,
  b"exec"     => exec::Exec,
  b"exit"     => flowctl::Exit,
  b"export"   => varcmds::Export,
  b"false"    => False,
  b"fc"       => fixcmd::FixCmd,
  b"fg"       => jobctl::Fg,
  b"flog"     => flog::Flog,
  b"fpop"     => arrops::FrontPop,
  b"fpush"    => arrops::FrontPush,
  b"genrc"    => genrc::GenRc,
  b"getopts"  => getopts::GetOpts,
  b"hash"     => hash::Hash,
  b"help"     => help::Help,
  b"hist"     => hist::Hist,
  b"jobs"     => jobctl::Jobs,
  b"keymap"   => keymap::KeyMapBuiltin,
  b"kill"     => jobctl::Kill,
  b"let"      => Let,
  b"listen"   => sock::Listen,
  b"local"    => varcmds::Local,
  b"msg"      => msg::Msg,
  b"pop"      => arrops::Pop,
  b"popd"     => dirstack::PopDir,
  b"printf"   => printf::Printf,
  b"push"     => arrops::Push,
  b"pushd"    => dirstack::PushDir,
  b"pwd"      => pwd::Pwd,
  b"quote"    => quote::Quote,
  b"raise"    => flowctl::Raise,
  b"read"     => read::Read,
  b"readkey"  => read::ReadKey,
  b"readonly" => varcmds::Readonly,
  b"return"   => flowctl::Return,
  b"rotate"   => arrops::Rotate,
  b"scry"     => scry::Scry,
  b"seek"     => seek::Seek,
  b"set"      => set::Set,
  b"shift"    => shift::Shift,
  b"shopt"    => shopt::Shopt,
  b"sock"     => sock::Sock,
  b"source"   => source::Source,
  b"stash"    => stash::StashBuiltin,
  b"stat"     => stat::Stat,
  b"test"     => test::Test,
  b"thru"     => Thru,
  b"times"    => times::Times,
  b"trap"     => trap::Trap,
  b"true"     => True,
  b"type"     => intro::Type,
  b"typeset"  => varcmds::Declare,
  b"ulimit"   => resource::ULimit,
  b"umask"    => resource::UMask,
  b"unalias"  => alias::Unalias,
  b"unquote"  => quote::Unquote,
  b"unset"    => varcmds::Unset,
  b"vice"     => vice::Vice,
  b"wait"     => jobctl::Wait,
  b"width"    => width::Width,
  b"zd"       => cd::Zd,
}

/// Lookup a name in the builtin table via binary search
pub(super) fn lookup_builtin(name: &[u8]) -> Option<&'static dyn Builtin> {
  BUILTIN_TABLE
    .binary_search_by_key(&name, |(n, _)| n)
    .ok()
    .map(|idx| BUILTIN_TABLE[idx].1 as &dyn Builtin)
}

/// A trait that provides a common interface for all builtin commands.
///
/// Has exactly one required member: `execute()`, which is called to run the builtin.
/// All other members have default implementations.
pub(super) trait Builtin: Sync {
  /// The actual logic of the builtin. The only required member of Builtin.
  fn execute(&self, args: BuiltinArgs) -> ShResult<()>;

  /// The option specification for the builtin.
  fn opts(&self) -> Vec<OptSpec> {
    vec![]
  }
  /// Whether unrecognized flags should be treated as errors.
  fn strict_opts(&self) -> bool {
    false
  }

  /// Whether `--help` opens the `help` page for a specific builtin.
  ///
  /// Certain builtins like `echo`/`printf` are supposed to read `--help` as
  /// a literal argument.
  fn no_help(&self) -> bool {
    false
  }

  /// Whether `--` is a literal operand rather than an end-of-options separator.
  ///
  /// `echo`/`printf` have no `--` terminator in operand position — POSIX `echo`
  /// prints `--`, and `printf` treats it as data once the format is seen.
  fn double_dash_operand(&self) -> bool {
    false
  }

  /// Whether or not to persist variables assigned via command prefix i.e. `FOO=bar command`
  /// It's a POSIX thing
  fn is_special(&self) -> bool {
    false
  }

  /// Whether or not the builtin forks a new process
  ///
  /// `false` by default, so that builtins are eligible for the in-process fast path by default.
  /// Overridden by commands like `eval` and `command` that result in forking execution as a side effect.
  /// `exit` also overrides this, so it doesn't stop the parent shell in subshells
  fn always_forks(&self) -> bool {
    false
  }

  /// The way that the builtin parses its options. Some of them are weird, like `set`
  fn get_argv_and_opts(&self, cmd_span: Span, argv: &[Tk], _no_split: bool) -> ShResult<Parsed> {
    let opts = self.opts();
    let parsed = opt::parse_opts_with(argv, &opts, self.strict_opts(), self.double_dash_operand())
      .promote_err(cmd_span)?;

    // `$_` is the last expanded word of the command line, options included; the
    // flat trace list preserves it in order.
    execute::record_last_arg(parsed.trace.last().cloned());
    Ok(parsed)
  }

  fn get_input_str(&self, args: &mut BuiltinArgs) -> Option<String> {
    self.get_input(args).map(procio::bytes_to_string)
  }

  /// Default input getter
  ///
  /// Only reads stdin whenn no arguments are given
  fn get_input(&self, args: &mut BuiltinArgs) -> Option<Vec<u8>> {
    self.get_input_with(args, |a| a.argv().is_empty())
  }

  /// Input getter. Takes a predicate that decides whether to slurp stdin or not.
  fn get_input_with(
    &self,
    args: &mut BuiltinArgs,
    should_slurp: fn(&BuiltinArgs) -> bool,
  ) -> Option<Vec<u8>> {
    if !should_slurp(args) {
      return None;
    }
    // Nothing to slurp if stdin is the bare interactive terminal (no piped sink
    if !procio::has_in_sink() && procio::stdin_is_tty() {
      return None;
    }
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
      match Shed::sinks(|s| s.read(&mut chunk)) {
        Ok(0) => break,
        Ok(n) => buf.extend_from_slice(&chunk[..n]),
        Err(e) if e.kind() == io::ErrorKind::Interrupted => {
          if signal::sigint_pending() {
            return None; // abort; the still-pending SIGINT aborts the command
          }
          // benign signal (SIGCHLD/SIGWINCH/…): retry the read
        }
        Err(_) => break,
      }
    }
    Some(buf)
  }

  /// The main entry point for running a builtin.
  /// This is responsible for setting up the environment, handling redirections, and catching control flow errors.
  fn setup_builtin(
    &self,
    tree: &Ast,
    node_id: NodeId,
    dispatcher: &mut Dispatcher,
  ) -> ShResult<()> {
    let node = &tree[node_id];
    let cmd = node.get_command().unwrap();
    let cmd_raw = tree[cmd].as_bytes();

    let context = node.context;
    let NdRule::Command { assignments, argv } = &node.class else {
      unreachable!()
    };
    let assign_behavior = if self.is_special() {
      AssignBehavior::Set
    } else {
      AssignBehavior::Export
    };

    // reverts any variable assignments made by the builtin if it is a special builtin
    let _var_guard = matches!(assign_behavior, AssignBehavior::Export)
      .then(|| guards::prefix_assign_guard(tree, &tree[*assignments]));

    Dispatcher::set_assignments(tree, &tree[*assignments], assign_behavior)?;
    let fork_builtins = node.flags.contains(NdFlags::FORK_BUILTINS);

    if !self.no_help() && argv.len() == 2 && tree[argv.get(1)].as_bytes() == b"--help" {
      // we have been asked for help
      // is this a hack? only the nose knows.
      return execute::exec_nonint(
        [b"help builtin-", cmd_raw].concat().into(),
        Some("<builtin-help>".into()),
      );
    }

    // Set up redirections here so we can attach the guard to propagated errors.
    let redirs: RedirSet = RedirSet::from(&tree[node.redirs]);
    let fatal = self.is_special() && !Shed::term(Terminal::interactive);
    let guard = match redirs.try_apply(fatal) {
      RedirResult::Applied(guard) => Some(guard),
      RedirResult::NoRedirs => None,
      RedirResult::Skipped => return Ok(()),
      RedirResult::Error(e) => return Err(e),
    };

    if fork_builtins {
      // Register ChildProc in current job
      let timer = dispatcher.take_timer();
      let job = dispatcher.job_stack.curr_job_mut().unwrap();
      let child_pgid = if let Some(pgid) = job.pgid() {
        pgid
      } else {
        let pid = Pid::this();
        job.set_pgid(pid);
        pid
      };
      let child = ChildProc::new(
        Pid::this(),
        Some(cmd_raw),
        fork_builtins.then_some(child_pgid),
        timer,
      );
      job.push_child(child);
    }

    // Handle exec specially - persist redirections before dispatch
    if cmd_raw == b"exec"
      && let Some(guard) = guard
    {
      guard.persist();
    }

    let result = self.run_builtin(tree, node_id, dispatcher);

    // Now we inspect the error that we got, if any
    match result {
      Ok(()) => Ok(()),
      Err(mut e) => {
        // if we aren't in the context these are looking for
        // then they will bubble all the way up to main
        // which cancels execution. Let's catch that here
        let kind = e.kind_mut();
        let should_propagate = match kind {
          ShErrKind::CleanExit(_) |     // this one always goes
          ShErrKind::Raised(_, _) |     // raise builtin, propagate
          ShErrKind::Interrupt => true, // Ctrl+C or something?
          ShErrKind::LoopBreak(_) | ShErrKind::LoopContinue(_) => {
            Shed::meta(MetaTab::in_loop)
          }
          ShErrKind::FuncReturn(_) => Shed::meta(MetaTab::in_func),
          _ if crate::shopt!(set.errexit) => {
            // propagate if this is enabled
            *kind = ShErrKind::ErrInterrupt;
            true
          }
          _ => false,
        };

        if should_propagate {
          let status = match e.kind() {
            ShErrKind::Custom(_, code) | ShErrKind::CleanExit(code) => *code,
            _ => 1,
          };
          Shed::set_status(status);
          Err(e.with_context(tree[context].iter()))
        } else {
          let status = if let ShErrKind::Custom(_, code) = e.kind() {
            *code
          } else {
            1
          };

          e.with_context(tree[context].iter()).print_error();
          util::with_status(status)
        }
      }
    }
  }
  /// Parse arguments and options, pack `BuiltinArgs`, run `self.execute()`
  fn run_builtin(&self, tree: &Ast, node_id: NodeId, _dispatcher: &mut Dispatcher) -> ShResult<()> {
    let node = &tree[node_id];
    let span = tree[node.get_span()].clone();
    let no_split = node.flags.contains(NdFlags::NO_SPLIT);
    let NdRule::Command {
      assignments: _,
      argv,
    } = &node.class
    else {
      unreachable!()
    };

    let cmd_span = argv
      .first()
      .map_or_else(|| span.clone(), |tk| tree[tk].span.clone());

    let parsed = self.get_argv_and_opts(cmd_span.clone(), &tree[*argv], no_split)?;

    if !node.flags.contains(NdFlags::NO_TRACE) {
      // Trace the flat, in-order expansion (options + their args intact),
      // exactly as external commands are traced.
      shopts::xtrace_print_raw(&parsed.trace);
    }

    let mut argv = parsed.words;
    if !argv.is_empty() {
      argv.remove(0);
    }

    let builtin_args = BuiltinArgs::new(argv, span, cmd_span);

    self.execute(builtin_args)
  }
}

// The easy ones

/// The POSIX no-op command. It does nothing.
struct Colon;
impl Builtin for Colon {
  fn is_special(&self) -> bool {
    true
  }
  fn no_help(&self) -> bool {
    true
  }
  fn execute(&self, _args: BuiltinArgs) -> ShResult<()> {
    util::with_status(0)
  }
}

/// Sets the shell's status to '0' and then returns
struct True;
impl Builtin for True {
  fn no_help(&self) -> bool {
    true
  }
  fn execute(&self, _args: BuiltinArgs) -> ShResult<()> {
    util::with_status(0)
  }
}

/// Sets the shell's status to '1' and then returns
struct False;
impl Builtin for False {
  fn no_help(&self) -> bool {
    true
  }
  fn execute(&self, _args: BuiltinArgs) -> ShResult<()> {
    util::with_status(1)
  }
}

/// Evaluate arithmetic expressions and set the shell's status based on the result
struct Let;
impl Builtin for Let {
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    if args.arguments().next().is_none() {
      // bash: `let` with no expressions returns 1
      return util::with_status(1);
    }
    let mut last = 0i64;
    for (expr, _) in args.arguments() {
      let result = arithmetic::expand_arithmetic(expr.as_bytes())?;
      last = result.to_str_lossy().trim().parse::<i64>().unwrap_or(0);
    }
    util::with_status(i32::from(last == 0))
  }
}

/// A source of bytes for the `thru` builtin, which can be either a file or stdin.
enum ThruSource {
  File(fs::File),
  Stdin,
}
impl ThruSource {
  /// Read bytes from the source into the provided buffer, returning the number of bytes read.
  fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
    match self {
      ThruSource::File(f) => f.read(buf),
      ThruSource::Stdin => Shed::sinks(|s| s.read(buf)),
    }
  }
}

/// Identity function that reads from stdin or files and writes to stdout, optionally teeing to a file and counting bytes.
///
/// Basically `cat` + `tee`, with no fork involved. Useful for keeping pipelines in-process if speed matters in a script.
struct Thru;
impl Builtin for Thru {
  fn strict_opts(&self) -> bool {
    true
  }
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::new("count").short(b'c').long("count"),
      OptSpec::new("append").short(b'a').long("append"),
      OptSpec::new("tee").short(b't').long("tee").argc(1),
      OptSpec::new("limit").short(b'L').long("limit").argc(1),
    ]
  }
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    let mut count = false;
    let mut append = false;
    let mut tee: Option<VarStr> = None;
    let mut limit = None;

    for opt in args.options() {
      match opt.key() {
        "append" => append = true,
        "count" => count = true,
        "tee" => tee = Some(opt.value()?.into()),
        "limit" => {
          let arg = opt.value()?;
          let Ok(parsed) = arg.parse::<usize>() else {
            return Err(sherr!(InvalidOpt @ opt.span(), "invalid limit: {arg}"));
          };
          limit = Some(parsed);
        }
        _ => {}
      }
    }

    let mut tee_file = tee
      .map(|dest| {
        let file = if append {
          std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&dest)
        } else {
          std::fs::File::create(&dest)
        };
        file.inspect_err(|e| {
          errln!("thru: failed to open {dest} for writing: {e}");
        })
      })
      .transpose()
      .ok()
      .flatten();

    let mut sources: Vec<Option<VarStr>> = args
      .arguments()
      .map(|(a, _)| (a.to_str_lossy() != "-").then(|| a.clone()))
      .collect();
    if sources.is_empty() {
      // no source operands → read stdin
      sources.push(None);
    }

    let mut byte_count = 0;

    for src in sources {
      if limit == Some(0) {
        break;
      }

      let mut reader = match &src {
        Some(path) => match fs::File::open(path) {
          Ok(f) => ThruSource::File(f),
          Err(e) => {
            errln!("thru: {path}: {e}");
            continue;
          }
        },
        None => ThruSource::Stdin,
      };
      let path = src.unwrap_or_else(|| "stdin".into());

      let mut buf = [0u8; 8192];
      loop {
        let cap = limit.map_or(buf.len(), |r| r.min(buf.len()));
        if cap == 0 {
          break;
        }

        let n = match reader.read(&mut buf[..cap]) {
          Ok(0) => break,
          Ok(n) => n,
          Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
          Err(e) => {
            errln!("thru: {path}: error reading input: {e}");
            break;
          }
        };

        let chunk = &buf[..n];
        procio::out_bytes(chunk);

        if let Some(t) = tee_file.as_mut() {
          use std::io::Write;
          t.write_all(chunk).ok();
        }

        byte_count += n;
        if let Some(l) = limit.as_mut() {
          *l -= n;
        }
      }
    }

    if count {
      errln!("thru: {byte_count} bytes");
    }

    util::with_status(0)
  }
}

/// A builtin that runs another builtin, bypassing the normal command lookup and dispatch.
///
/// This is mainly used to bypass symbols that may shadow existing commands.
/// Yes the struct name is unfortunate. No, I'm not changing it.
struct BuiltinBuiltin;
impl Builtin for BuiltinBuiltin {
  // lol
  fn execute(&self, _args: BuiltinArgs) -> ShResult<()> {
    unreachable!("this one operates on the node directly")
  }
  fn setup_builtin(
    &self,
    tree: &Ast,
    node_id: NodeId,
    dispatcher: &mut Dispatcher,
  ) -> ShResult<()> {
    let node = &tree[node_id];
    let span = tree[node.get_span()].clone();
    let NdRule::Command { argv, .. } = &node.class else {
      unreachable!()
    };
    let mut inner_argv = expand_argv(&tree[*argv])?;

    if !node.flags.contains(NdFlags::NO_TRACE) {
      shopts::xtrace_print_tokens(&inner_argv);
    }

    if !inner_argv.is_empty() {
      inner_argv.remove(0);
    }

    let cmd = inner_argv.first().map(Tk::word).unwrap_or_default();
    let Some(builtin) = lookup_builtin(cmd.as_bytes()) else {
      sherr!(NotFound @ span, "builtin not found: {cmd}").print_error();
      return util::with_status(127);
    };

    // copy the wrapped invocation into its own ast, then dispatch
    let mut sub_ast = tree.break_off(node_id);
    let inner_argv = sub_ast.alloc_tokens(inner_argv);

    let fwd_id = sub_ast.get_root().expect("forwarded command has no root");
    let NdRule::Command { assignments, .. } = &sub_ast[fwd_id].class else {
      unreachable!()
    };
    let assignments = *assignments;
    sub_ast[fwd_id].class = NdRule::Command {
      assignments,
      argv: inner_argv,
    };
    sub_ast[fwd_id].flags |= NdFlags::NO_TRACE;

    builtin.setup_builtin(&sub_ast, fwd_id, dispatcher)
  }
}

/// Expand and flatten an argv into single-word `Expanded` tokens.
fn expand_argv(argv: &[Tk]) -> ShResult<Vec<Tk>> {
  let mut out = Vec::with_capacity(argv.len());
  for tk in argv {
    let words = tk.expand_to_words()?;
    for word in words.iter() {
      out.push(Tk {
        class: TkRule::Expanded {
          exp: [word.clone()].into(),
        },
        ..tk.clone()
      });
    }
  }
  Ok(out)
}

/// The `command` builtin, which runs a command while bypassing any shell functions or aliases that may shadow it.
///
/// This is a special builtin that always forks, because it needs to run the command in a new process to avoid shadowing.
pub(crate) struct CommandBuiltin;
impl Builtin for CommandBuiltin {
  fn always_forks(&self) -> bool {
    true
  }

  fn execute(&self, _args: BuiltinArgs) -> ShResult<()> {
    unreachable!("this one operates on the node directly")
  }
  fn run_builtin(&self, tree: &Ast, node_id: NodeId, dispatcher: &mut Dispatcher) -> ShResult<()> {
    let node = &tree[node_id];
    let NdRule::Command { argv, .. } = &node.class else {
      unreachable!()
    };
    // Expand first so a smuggled `command` (`C="command echo hi"`) is split
    // into words before we strip the leading `command`.
    let mut argv = expand_argv(&tree[*argv])?;

    if !node.flags.contains(NdFlags::NO_TRACE) {
      shopts::xtrace_print_tokens(&argv);
    }

    if !argv.is_empty() {
      argv.remove(0);
    }

    let mut use_default_path = false;
    let mut print_path = false;
    let mut print_type = false;
    let mut seen_dd = false;

    let iter = argv.into_iter();
    let mut rest = vec![];

    for tk in iter {
      if !rest.is_empty() || seen_dd {
        rest.push(tk);
        continue;
      }

      match tk.word().to_str_lossy().as_ref() {
        "-p" => use_default_path = true,

        "-v" if !print_type => print_path = true,
        "-V" if !print_path => print_type = true,

        "-v" if print_type => {
          return Err(sherr!(InvalidOpt @ tk.span.clone(), "cannot specify both -v and -V"));
        }
        "-V" if print_path => {
          return Err(sherr!(InvalidOpt @ tk.span.clone(), "cannot specify both -v and -V"));
        }

        "--" => seen_dd = true,
        s if s.starts_with('-') => {
          return Err(sherr!(InvalidOpt @ tk.span.clone(), "invalid option: {s}"));
        }
        _ => rest.push(tk),
      }
    }

    if rest.is_empty() {
      return util::with_status(0);
    }

    argv = rest;

    let mut sub_ast = tree.break_off(node_id);
    let inner_argv = sub_ast.alloc_tokens(argv);
    let root = sub_ast
      .get_root()
      .expect("command: forwarded node has no root");
    let NdRule::Command { assignments, .. } = &sub_ast[root].class else {
      unreachable!()
    };
    let assignments = *assignments;
    sub_ast[root].class = NdRule::Command {
      assignments,
      argv: inner_argv,
    };
    sub_ast[root].flags |= NdFlags::NO_TRACE;

    if use_default_path {
      let Some(default_path) = params::get_default_path() else {
        #[cfg(target_os = "android")]
        return Err(
          sherr!(ExecFail @ sub_ast[root].get_span(), "the -p flag is not supported on Android"),
        );

        #[cfg(not(target_os = "android"))]
        let span = sub_ast[sub_ast[root].get_span()].clone();
        return Err(sherr!(ExecFail @ span, "unable to get default path"));
      };
      // TODO: Find a way to do this that doesn't involve forcing a full PATH rehash twice
      defer! {
        Shed::meta_mut(MetaTab::rehash_path_cache);
      }
      params::with_vars([("PATH".into(), default_path)], || {
        Shed::meta_mut(MetaTab::rehash_path_cache);
        Self::execute_inner(print_path, print_type, &sub_ast, root, dispatcher)
      })
    } else {
      Self::execute_inner(print_path, print_type, &sub_ast, root, dispatcher)
    }
  }
}

impl CommandBuiltin {
  fn execute_inner(
    print_path: bool,
    print_type: bool,
    tree: &Ast,
    node_id: NodeId,
    dispatcher: &mut Dispatcher,
  ) -> ShResult<()> {
    let node = &tree[node_id];
    let NdRule::Command { argv, .. } = &node.class else {
      unreachable!()
    };
    if print_path {
      let Some(name) = argv.first() else {
        return util::with_status(2);
      };
      let name_word = tree[name].word();
      let name_str = name_word.to_str_lossy();
      match cmd::which_util(&name_str) {
        Some(util) => match util.kind() {
          UtilKind::Alias => {
            let Some(alias) = Shed::logic(|l| l.get_alias(&name_str)) else {
              return util::with_status(127);
            };
            outln!(
              "alias {name_str}={}",
              escape::shell_quote(&alias.body().to_str_lossy())
            );
          }
          UtilKind::Function | UtilKind::Builtin => outln!("{name_str}"),
          UtilKind::Command(p) | UtilKind::File(p) => outln!("{}", p.display()),
        },
        None if KEYWORDS.contains(&name_str.as_bytes()) => outln!("{name_str}"),
        None => return util::with_status(127),
      }

      return util::with_status(0);
    }
    if print_type {
      let Some(name) = argv.first() else {
        return util::with_status(2);
      };
      let name_word = tree[name].word();
      let name_str = name_word.to_str_lossy();
      match cmd::which_util(&name_str) {
        Some(util) => match util.kind() {
          UtilKind::Alias => {
            let Some(alias) = Shed::logic(|l| l.get_alias(&name_str)) else {
              return util::with_status(127);
            };
            outln!(
              "{name_str} is an alias for {}",
              escape::shell_quote(&alias.body().to_str_lossy())
            );
          }
          UtilKind::Function => outln!("{name_str} is a function"),
          UtilKind::Builtin => outln!("{name_str} is a shell builtin"),
          UtilKind::Command(p) | UtilKind::File(p) => {
            outln!("{name_str} is {}", p.display());
          }
        },
        None if KEYWORDS.contains(&name_str.as_bytes()) => outln!("{name_str} is a shell keyword"),
        None => {
          errln!("command: {name_str}: not found");
          return util::with_status(127);
        }
      }

      return util::with_status(0);
    }

    // Per POSIX, `command` suppresses alias/function lookup but must still
    // execute shell builtins (and external commands). Route through the same
    // dispatcher logic as `dispatch_cmd`, just with function lookup disabled.
    dispatcher.route_command(tree, node_id, false)
  }
}

#[cfg(test)]
pub(crate) mod tests {
  use std::env;

  use tempfile::TempDir;

  use crate::{
    assert_status_eq,
    eval::execute::exec_nonint,
    state::{self, Shed, cmd, vars::VarFlags},
    tests::testutil::{TestGuard, canon, has_cmd, test_input},
  };

  // ===================== exit status propagation =====================

  #[test]
  fn exit_leaves_status_equal_to_its_code() {
    // Regression: `setup_builtin` blanket-set `$?`=1 on a propagated CleanExit,
    // so interactively `exit N` exited with 1 (main.rs stores get_status() on
    // normal completion). The status must now equal the exit code.
    let _g = TestGuard::new();
    for code in [5, 0, 42, 300] {
      // exit raises CleanExit and propagates out of exec_nonint as an Err.
      let _ = exec_nonint(format!("exit {code}").into(), None);
      // `$?` is byte-masked, so 300 -> 44.
      assert_eq!(Shed::get_status(), code % 256, "exit {code}");
    }
  }

  // ===================== xtrace (set -x) covers builtins =====================

  #[test]
  fn xtrace_traces_builtin_with_opts() {
    // Regression: builtins with opts (echo, cd, read, …) took the
    // get_opts_from_tokens path, which never emitted an xtrace line.
    let g = TestGuard::new();
    test_input("set -x; echo hello_xtrace; set +x").unwrap();
    let out = g.read_output();
    assert!(out.contains("+ echo hello_xtrace"), "got: {out:?}");
  }

  #[test]
  fn xtrace_traces_empty_opt_builtin() {
    let g = TestGuard::new();
    test_input("set -x; :; set +x").unwrap();
    let out = g.read_output();
    assert!(out.contains("+ :"), "got: {out:?}");
  }

  #[test]
  fn xtrace_traces_assignment_builtin() {
    let g = TestGuard::new();
    test_input("set -x; export XTRACE_FOO=bar; set +x").unwrap();
    let out = g.read_output();
    assert!(out.contains("+ export XTRACE_FOO=bar"), "got: {out:?}");
  }

  #[test]
  fn xtrace_traces_bare_assignment() {
    // Regression: an assignment-only command (`x=5`) applied its value without
    // ever emitting an xtrace line.
    let g = TestGuard::new();
    test_input("set -x; XTRACE_BARE=5; set +x").unwrap();
    let out = g.read_output();
    assert!(out.contains("+ XTRACE_BARE=5"), "got: {out:?}");
  }

  #[test]
  fn xtrace_traces_bare_assignment_expanded_rhs() {
    // The traced value is the *expanded* RHS, like bash (`+ x=4`, not `x=$((..)`).
    let g = TestGuard::new();
    test_input("set -x; XTRACE_N=$((2+2)); set +x").unwrap();
    let out = g.read_output();
    assert!(out.contains("+ XTRACE_N=4"), "got: {out:?}");
  }

  #[test]
  fn xtrace_traces_prefix_assignment() {
    // A prefix assignment on a command (`x=1 cmd`) is traced on its own line,
    // before the command — matching bash's `+ x=1` / `+ cmd`.
    let g = TestGuard::new();
    test_input("set -x; XTRACE_PRE=1 true; set +x").unwrap();
    let out = g.read_output();
    assert!(out.contains("+ XTRACE_PRE=1"), "got: {out:?}");
  }

  #[test]
  fn xtrace_depth_not_inflated_by_function() {
    // Regression: the prefix scaled by scope depth, so a command inside a
    // function traced as `+++ echo` instead of bash's single `+ echo`.
    let g = TestGuard::new();
    test_input("xt_deep() { echo deep; }; set -x; xt_deep; set +x").unwrap();
    let out = g.read_output();
    assert!(out.contains("+ echo deep"), "got: {out:?}");
    assert!(!out.contains("++ echo deep"), "prefix inflated: {out:?}");
  }

  #[test]
  fn xtrace_off_produces_no_trace() {
    let g = TestGuard::new();
    test_input("echo no_trace_here").unwrap();
    let out = g.read_output();
    assert!(!out.contains("+ echo"), "unexpected trace: {out:?}");
  }

  // You can never be too sure!!!!!!
  #[test]
  fn test_true() {
    let _g = TestGuard::new();
    test_input("true").unwrap();

    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn test_false() {
    let _g = TestGuard::new();
    test_input("false").unwrap();

    assert_eq!(state::Shed::get_status(), 1);
  }

  #[test]
  fn test_colon() {
    let _g = TestGuard::new();
    test_input(":").unwrap();

    assert_eq!(state::Shed::get_status(), 0);
  }

  // ===================== let builtin =====================

  #[test]
  fn let_assigns_and_expands() {
    let g = TestGuard::new();
    test_input("let x=3+4; echo $x").unwrap();
    let out = g.read_output();
    assert_eq!(out.trim(), "7");
  }

  #[test]
  fn let_nonzero_result_is_status_zero() {
    let _g = TestGuard::new();
    test_input("let 3+4").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn let_zero_result_is_status_one() {
    let _g = TestGuard::new();
    test_input("let 1-1").unwrap();
    assert_eq!(state::Shed::get_status(), 1);
  }

  #[test]
  fn let_status_reflects_last_expression() {
    // first expr is zero, last is nonzero -> status 0
    let _g = TestGuard::new();
    test_input("let a=0 b=5").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn let_no_args_is_status_one() {
    let _g = TestGuard::new();
    test_input("let").unwrap();
    assert_eq!(state::Shed::get_status(), 1);
  }

  #[test]
  fn let_counter_increment() {
    let g = TestGuard::new();
    test_input("i=1; let i=i+1; echo $i").unwrap();
    let out = g.read_output();
    assert_eq!(out.trim(), "2");
  }

  // ===================== prefix-assignment scoping =====================

  #[test]
  fn prefix_assign_persists_for_special_builtin() {
    let g = TestGuard::new();
    test_input("FOO=bar export; echo $FOO").unwrap();
    let out = g.read_output();
    assert!(
      out.contains("bar"),
      "expected FOO to persist after export, got: {out:?}"
    );
  }

  #[test]
  fn prefix_assign_does_not_persist_for_regular_builtin() {
    let g = TestGuard::new();
    test_input("FOO=bar echo first; echo \"after=[$FOO]\"").unwrap();
    let out = g.read_output();
    assert!(
      out.contains("after=[]"),
      "expected FOO to be cleared after echo, got: {out:?}"
    );
  }

  #[test]
  fn prefix_assign_to_special_with_allexport_persists_and_exports() {
    let _g = TestGuard::new();
    test_input("set -a; FOO=bar export").unwrap();
    let var = Shed::vars(|v| v.try_get_var_meta("FOO")).unwrap();
    let flags = var.flags();
    assert!(flags.contains(VarFlags::EXPORT));
  }

  #[test]
  fn builtin_help_flag_works() {
    let _g = TestGuard::new();
    exec_nonint("echo --help".into(), Some("builtin help test".into())).unwrap();
    assert_status_eq!(0);
  }

  // ===================== command builtin =====================

  #[test]
  fn command_bare_dispatches() {
    let g = TestGuard::new();
    test_input("command echo hello_dispatch").unwrap();
    let out = g.read_output();
    assert!(out.contains("hello_dispatch"), "got: {out:?}");
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn command_v_builtin_prints_just_name() {
    let g = TestGuard::new();
    test_input("command -v echo").unwrap();
    let out = g.read_output();
    assert!(out.contains("echo"), "got: {out:?}");
    assert!(
      !out.contains('/'),
      "builtin should not print a path: {out:?}"
    );
    assert!(!out.contains("is"), "no -V-style prose for -v: {out:?}");
  }

  #[test]
  fn command_v_keyword_prints_just_name() {
    let g = TestGuard::new();
    test_input("command -v if").unwrap();
    let out = g.read_output();
    assert!(out.contains("if"), "got: {out:?}");
    assert!(
      !out.contains('/'),
      "keyword should not print a path: {out:?}"
    );
  }

  #[test]
  fn command_v_function_prints_just_name() {
    let g = TestGuard::new();
    test_input("myfn_for_cmdv() { :; }").unwrap();
    g.read_output();

    test_input("command -v myfn_for_cmdv").unwrap();
    let out = g.read_output();
    assert!(out.contains("myfn_for_cmdv"), "got: {out:?}");
    assert!(
      !out.contains('/'),
      "function should not print a path: {out:?}"
    );
  }

  #[test]
  fn command_v_alias_prints_alias_line() {
    let g = TestGuard::new();
    test_input("alias myalias_for_cmdv='ls -la'").unwrap();
    g.read_output();

    test_input("command -v myalias_for_cmdv").unwrap();
    let out = g.read_output();
    assert!(out.contains("alias myalias_for_cmdv="), "got: {out:?}");
    assert!(out.contains("ls -la"), "got: {out:?}");
  }

  #[test]
  fn command_v_external_prints_absolute_path() {
    if !has_cmd("cat") {
      return;
    }
    let g = TestGuard::new();
    test_input("command -v cat").unwrap();
    let out = g.read_output();
    assert!(out.contains("cat"), "got: {out:?}");
    assert!(out.contains('/'), "external should print a path: {out:?}");
  }

  #[test]
  fn command_v_not_found_is_silent_and_127() {
    let _g = TestGuard::new();
    let res = test_input("command -v __hopefully__not__a__command__");
    assert!(res.is_ok());
    assert_eq!(state::Shed::get_status(), 127);
  }

  #[test]
  fn which_util_resolves_uncached_after_partial_cache() {
    // Regression (#118): a single-command resolution (as a pipeline's external
    // lookup does) marks $PATH as "seen" (`old_path`) while caching only that
    // one command. `which_util` — backing `command -v`/`type` — must still
    // resolve a *different*, un-hashed command via a real PATH walk instead of
    // trusting the (incomplete) cache and reporting it as not-found.
    if !has_cmd("cat") || !has_cmd("env") {
      return;
    }
    let _g = TestGuard::new();
    // Force the exact post-pipeline state: pristine cache, then resolve one
    // command so $PATH is marked seen with only `cat` cached.
    state::Shed::meta_mut(state::meta::MetaTab::clear_path_cache);
    let _ = cmd::lookup_cmd("cat");
    assert!(
      cmd::which_util("env").is_some(),
      "env must resolve even though only `cat` is cached"
    );
  }

  #[test]
  #[expect(non_snake_case)]
  fn command_V_builtin_says_shell_builtin() {
    let g = TestGuard::new();
    test_input("command -V echo").unwrap();
    let out = g.read_output();
    assert!(out.contains("echo"), "got: {out:?}");
    assert!(out.contains("shell builtin"), "got: {out:?}");
  }

  #[test]
  #[expect(non_snake_case)]
  fn command_V_keyword_says_shell_keyword() {
    let g = TestGuard::new();
    test_input("command -V if").unwrap();
    let out = g.read_output();
    assert!(out.contains("if"), "got: {out:?}");
    assert!(out.contains("shell keyword"), "got: {out:?}");
  }

  #[test]
  #[expect(non_snake_case)]
  fn command_V_not_found_writes_stderr_and_127() {
    let g = TestGuard::new();
    test_input("command -V __hopefully__not__a__command__").unwrap();
    let out = g.read_output();
    assert!(out.contains("not found"), "got: {out:?}");
    assert_eq!(state::Shed::get_status(), 127);
  }

  #[test]
  #[expect(non_snake_case)]
  fn command_v_and_V_together_errors() {
    let _g = TestGuard::new();
    let _ = test_input("command -v -V echo");
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  #[expect(non_snake_case)]
  fn command_V_and_v_together_errors() {
    let _g = TestGuard::new();
    let _ = test_input("command -V -v echo");
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn command_double_dash_terminates_option_parsing() {
    // If `--` works, `-V` here is the command_name (not a flag), so
    // dispatching it should fail with 127 (no such command). If `--`
    // is broken, `-V` would be parsed as a flag and the missing
    // command_name path would set a different exit status.
    let _g = TestGuard::new();
    test_input("command -- -V").unwrap();
    assert_eq!(state::Shed::get_status(), 127);
  }

  #[test]
  fn command_invalid_flag_errors() {
    let _g = TestGuard::new();
    let _ = test_input("command -Z something");
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn command_p_restores_path_after_invocation() {
    if !has_cmd("cat") {
      return;
    }
    let g = TestGuard::new();
    // Set a sentinel PATH that wouldn't normally contain `cat`,
    // then run `command -p` which should temporarily switch to the
    // system default PATH to find it, then restore /sentinel afterwards.
    test_input("export PATH=/sentinel_path_xyz").unwrap();
    g.read_output();
    test_input("command -p cat /dev/null").unwrap();
    g.read_output();
    test_input("echo \"PATH_NOW=$PATH\"").unwrap();
    let out = g.read_output();
    assert!(
      out.contains("PATH_NOW=/sentinel_path_xyz"),
      "PATH was not restored after `command -p`: got {out:?}",
    );
  }

  #[test]
  fn command_smuggled_through_variable_dispatches() {
    let g = TestGuard::new();
    test_input(r#"C="command echo smuggled_cmd"; $C"#).unwrap();
    let out = g.read_output();
    assert!(out.contains("smuggled_cmd"), "got: {out:?}");
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn command_v_resolves_name_from_variable() {
    let g = TestGuard::new();
    test_input("F=echo; command -v $F").unwrap();
    let out = g.read_output();
    assert!(out.contains("echo"), "got: {out:?}");
    assert_eq!(state::Shed::get_status(), 0);
  }

  // POSIX: `command` suppresses alias/function lookup but must still
  // execute shell builtins. `cd` has no external counterpart, so this
  // verifies the builtin (not execve) path is taken. See also bash/dash.
  #[test]
  fn command_invokes_cd_builtin() {
    let _g = TestGuard::new();
    let old_dir = env::current_dir().unwrap();
    let temp_dir = TempDir::new().unwrap();

    test_input(format!("command cd {}", temp_dir.path().display())).unwrap();

    let new_dir = env::current_dir().unwrap();
    assert_ne!(
      old_dir, new_dir,
      "cwd unchanged; `command cd` did not run the builtin"
    );
    assert_eq!(
      new_dir.display().to_string(),
      canon(temp_dir.path()).display().to_string()
    );
    assert_eq!(state::Shed::get_status(), 0);
  }

  // A backslash-quoted `\command` still routes through the `command`
  // builtin after expansion, so it must behave the same as `command`.
  #[test]
  fn backslash_command_invokes_cd_builtin() {
    let _g = TestGuard::new();
    let old_dir = env::current_dir().unwrap();
    let temp_dir = TempDir::new().unwrap();

    test_input(format!("\\command cd {}", temp_dir.path().display())).unwrap();

    let new_dir = env::current_dir().unwrap();
    assert_ne!(
      old_dir, new_dir,
      "cwd unchanged; `\\command cd` did not run the builtin"
    );
    assert_eq!(
      new_dir.display().to_string(),
      canon(temp_dir.path()).display().to_string()
    );
    assert_eq!(state::Shed::get_status(), 0);
  }

  // ===================== builtin builtin =====================

  #[test]
  fn builtin_bare_dispatches() {
    let g = TestGuard::new();
    test_input("builtin echo hello_builtin").unwrap();
    let out = g.read_output();
    assert!(out.contains("hello_builtin"), "got: {out:?}");
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn builtin_smuggled_through_variable_dispatches() {
    let g = TestGuard::new();
    test_input(r#"B="builtin echo smuggled_builtin"; $B"#).unwrap();
    let out = g.read_output();
    assert!(out.contains("smuggled_builtin"), "got: {out:?}");
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn builtin_resolves_name_from_variable() {
    let g = TestGuard::new();
    test_input("CMD=echo; builtin $CMD from_var").unwrap();
    let out = g.read_output();
    assert!(out.contains("from_var"), "got: {out:?}");
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn builtin_unknown_name_is_127() {
    let _g = TestGuard::new();
    test_input("builtin __not_a_real_builtin__ x").unwrap();
    assert_eq!(state::Shed::get_status(), 127);
  }
}
