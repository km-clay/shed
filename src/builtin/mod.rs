use ariadne::Span as ASpan;
use nix::unistd::Pid;
use scopeguard::defer;
use std::{
  cell::RefCell,
  fmt::Write,
  fs,
  io::{self, Read},
};

use crate::{
  procio::{bytes_to_string, out_bytes, stdout_fileno},
  state::meta::UtilKind,
  util::{FdReader, ShResultExt},
};

use super::{
  errln,
  eval::{
    self, NdFlags, NdRule, Node,
    execute::{AssignBehavior, Dispatcher, exec_nonint, prepare_argv_with},
    lex::{KEYWORDS, Span, Tk},
  },
  expand::{self, shell_quote},
  key, keys, match_loop, out, outln,
  procio::{self, RedirSet},
  readline, sherr, shopt, signal,
  state::{self, Shed, jobs::ChildProc, meta::MetaTab, shopt as shopt_internal},
  status_msg, system_msg, try_var,
  util::{self, ShErrKind, ShResult, var_ctx_guard, with_status},
  var,
};

// Stack of output sinks for in-process pipelines; the stack lets internal
// pipelines nest inside command subs.
thread_local! {
  pub(crate) static OUT_SINK: RefCell<Vec<Vec<u8>>> = const { RefCell::new(Vec::new()) };
  pub(crate) static IN_SINK: RefCell<Vec<Option<Vec<u8>>>> = const { RefCell::new(Vec::new()) };
}

fn install_out_sink() {
  OUT_SINK.with(|s| {
    s.borrow_mut().push(vec![]);
  });
}

pub(crate) fn has_out_sink() -> bool {
  OUT_SINK.with(|s| s.borrow().last().is_some())
}

pub(crate) fn has_in_sink() -> bool {
  IN_SINK.with(|s| s.borrow().last().is_some())
}

pub(crate) fn take_stdin() -> Option<Vec<u8>> {
  IN_SINK.with(|s| s.borrow_mut().last_mut().and_then(|opt| opt.take()))
}

pub(crate) struct SinkScope {
  taken: bool,
}
impl SinkScope {
  pub fn new() -> Self {
    install_out_sink();
    Self { taken: false }
  }

  pub fn take(mut self) -> Vec<u8> {
    self.taken = true;
    OUT_SINK
      .with(|s| s.borrow_mut().pop())
      .expect("SinkScope should have an out sink")
  }
}

impl Drop for SinkScope {
  fn drop(&mut self) {
    if !self.taken {
      OUT_SINK
        .with(|s| s.borrow_mut().pop())
        .expect("SinkScope should have an out sink");
    }
  }
}

pub(crate) struct StdinScope;
impl StdinScope {
  pub fn push(input: Vec<u8>) -> Self {
    IN_SINK.with(|s| {
      s.borrow_mut().push(Some(input));
    });
    Self
  }
}

impl Drop for StdinScope {
  fn drop(&mut self) {
    IN_SINK
      .with(|s| s.borrow_mut().pop())
      .expect("StdinScope should have an in sink");
  }
}

mod alias;
mod arrops;
mod autocmd;
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
mod getopt;
mod getopts;
mod hash;
mod help;
mod hist;
mod intro;
mod jobctl;
mod keymap;
mod msg;
mod printf;
mod pwd;
mod quote;
mod read;
mod resource;
mod seek;
mod set;
mod shift;
mod shopt;
mod source;
mod stash;
mod stat;
mod test; // [[ ]] thing
mod times;
mod trap;
mod varcmds;
mod vice;
mod width;

use getopt::{Opt, OptSpec, get_opts_from_tokens, get_opts_from_tokens_strict};
pub(crate) use help::HELP_PAGE_INSTALL_DIR;

macro_rules! register_builtins {
  ($($name:literal => $ty:expr),* $(,)?) => {
    static BUILTIN_TABLE: &[(&str, &dyn Builtin)] = &[
      $(($name, &$ty)),*
    ];

    pub const BUILTIN_NAMES: &[&str] = &[
      $($name),*
    ];

    $crate::assert_sorted!(BUILTIN_NAMES);
  };
}

// these have to be in alphabetical order, because of the way lookup_builtin() works
// if the list is unsorted, that is a compile error thanks to the const evaluation above
// if you're using vim, you can visual select the block and filter it through ''<,'>:!LC_ALL=C sort'
// you can also yank this macro and execute it with @" -> /^register_builtins!$viB:!LC_ALL=C sort:wviBga=:w
// if you're not using vim, idk. you know the alphabet right?
register_builtins! {
  "."        => source::Source,
  ":"        => Colon,
  "["        => test::Test,
  "[["       => test::Test,
  "alias"    => alias::Alias,
  "autocmd"  => autocmd::AutoCmdBuiltin,
  "bg"       => jobctl::Bg,
  "break"    => flowctl::Break,
  "builtin"  => BuiltinBuiltin,
  "cd"       => cd::Cd,
  "command"  => CommandBuiltin,
  "compadd"  => complete::Compadd,
  "compgen"  => complete::CompGen,
  "complete" => complete::Complete,
  "continue" => flowctl::Continue,
  "declare"  => varcmds::Declare,
  "dirs"     => dirstack::Dirs,
  "disown"   => jobctl::Disown,
  "echo"     => echo::Echo,
  "eval"     => evaluate::Eval,
  "exec"     => exec::Exec,
  "exit"     => flowctl::Exit,
  "export"   => varcmds::Export,
  "false"    => False,
  "fc"       => fixcmd::FixCmd,
  "fg"       => jobctl::Fg,
  "flog"     => flog::Flog,
  "fpop"     => arrops::FrontPop,
  "fpush"    => arrops::FrontPush,
  "genrc"    => genrc::GenRc,
  "getopts"  => getopts::GetOpts,
  "hash"     => hash::Hash,
  "help"     => help::Help,
  "hist"     => hist::Hist,
  "jobs"     => jobctl::Jobs,
  "keymap"   => keymap::KeyMapBuiltin,
  "kill"     => jobctl::Kill,
  "local"    => varcmds::Local,
  "msg"      => msg::Msg,
  "pop"      => arrops::Pop,
  "popd"     => dirstack::PopDir,
  "printf"   => printf::Printf,
  "push"     => arrops::Push,
  "pushd"    => dirstack::PushDir,
  "pwd"      => pwd::Pwd,
  "quote"    => quote::Quote,
  "raise"    => flowctl::Raise,
  "read"     => read::Read,
  "readkey"  => read::ReadKey,
  "readonly" => varcmds::Readonly,
  "return"   => flowctl::Return,
  "rotate"   => arrops::Rotate,
  "seek"     => seek::Seek,
  "set"      => set::Set,
  "shift"    => shift::Shift,
  "shopt"    => shopt::Shopt,
  "source"   => source::Source,
  "stash"    => stash::StashBuiltin,
  "stat"     => stat::Stat,
  "test"     => test::Test,
  "thru"     => Thru,
  "times"    => times::Times,
  "trap"     => trap::Trap,
  "true"     => True,
  "type"     => intro::Type,
  "ulimit"   => resource::ULimit,
  "umask"    => resource::UMask,
  "unalias"  => alias::Unalias,
  "unquote"  => quote::Unquote,
  "unset"    => varcmds::Unset,
  "vice"     => vice::Vice,
  "wait"     => jobctl::Wait,
  "width"    => width::Width,
}

/// Lookup a name in the builtin table via binary search
pub(super) fn lookup_builtin(name: &str) -> Option<&'static dyn Builtin> {
  BUILTIN_TABLE
    .binary_search_by_key(&name, |(n, _)| n)
    .ok()
    .map(|idx| BUILTIN_TABLE[idx].1 as &dyn Builtin)
}

type ArgVector = Vec<(String, Span)>;
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

  /// If this is overridden to return `true`, variables
  /// assigned via command prefix, i.e. `FOO=bar command`,
  /// are persisted after the builtin returns. POSIX thing.
  fn is_special(&self) -> bool {
    false
  }

  /// The way that the builtin parses its options. Some of them are weird, like `set`
  fn get_argv_and_opts(
    &self,
    cmd_span: Span,
    argv: &[Tk],
    no_split: bool,
  ) -> ShResult<(ArgVector, Vec<Opt>)> {
    let opts = self.opts();
    let (mut argv, opts) = if opts.is_empty() {
      (
        prepare_argv_with(argv, no_split).promote_err(cmd_span)?,
        vec![],
      )
    } else if self.strict_opts() {
      get_opts_from_tokens_strict(argv, &opts).promote_err(cmd_span)?
    } else {
      get_opts_from_tokens(argv, &opts).promote_err(cmd_span)?
    };
    if !argv.is_empty() {
      argv.remove(0);
    }
    Ok((argv, opts))
  }

  fn get_input_str(&self, args: &mut BuiltinArgs) -> Option<String> {
    self.get_input(args).map(bytes_to_string)
  }

  fn get_input_str_with(
    &self,
    args: &mut BuiltinArgs,
    should_slurp: fn(&BuiltinArgs) -> bool,
  ) -> Option<String> {
    self.get_input_with(args, should_slurp).map(bytes_to_string)
  }

  /// Default input getter
  ///
  /// Slurps stdin if `args.argv` is empty, or if stdin is available
  fn get_input(&self, args: &mut BuiltinArgs) -> Option<Vec<u8>> {
    self.get_input_with(args, |a| a.argv.is_empty() || a.has_stdin())
  }

  /// Input getter. Takes a predicate that decides whether to slurp stdin or not.
  fn get_input_with(
    &self,
    args: &mut BuiltinArgs,
    should_slurp: fn(&BuiltinArgs) -> bool,
  ) -> Option<Vec<u8>> {
    if should_slurp(args) {
      if args.has_stdin() {
        args.take_stdin()
      } else {
        procio::read_input().ok()
      }
    } else {
      None
    }
  }

  /// The main entry point for running a builtin. This is responsible for setting up the environment, handling redirections, and catching control flow errors.
  fn setup_builtin(
    &self,
    node: &Node,
    dispatcher: &mut Dispatcher,
    stdin: Option<Vec<u8>>,
  ) -> ShResult<()> {
    let cmd_raw = node.get_command().unwrap().to_string();
    let context = node.context.clone();
    let NdRule::Command { assignments, argv } = &node.class else {
      unreachable!()
    };
    let assign_behavior = if self.is_special() {
      AssignBehavior::Set
    } else {
      AssignBehavior::Export
    };

    let vars = Dispatcher::set_assignments(assignments, assign_behavior)?;
    let _var_guard = var_ctx_guard(vars.into_iter().collect());
    let fork_builtins = node.flags.contains(NdFlags::FORK_BUILTINS);

    if argv.len() == 2 && argv[1].as_str() == "--help" {
      // we have been asked for help
      // is this a hack? only the nose knows.
      return exec_nonint(
        format!("help builtin-{cmd_raw}"),
        Some("<builtin-help>".into()),
      );
    }

    // Set up redirections here so we can attach the guard to propagated errors.
    let redirs: RedirSet = RedirSet::from(&node.redirs);
    let guard = redirs.apply()?;

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
        Some(&cmd_raw),
        fork_builtins.then_some(child_pgid),
        timer,
      );
      job.push_child(child);
    }

    // Handle exec specially - persist redirections before dispatch
    if cmd_raw.as_str() == "exec"
      && let Some(guard) = guard
    {
      guard.persist();
    }

    let result = self.run_builtin(node, dispatcher, stdin);

    // Now we inspect the error that we got, if any
    match result {
      Ok(()) => Ok(()),
      Err(mut e) => {
        // if we aren't in the context these are looking for
        // then they will bubble all the way up to main
        // which cancels execution. Let's catch that here
        let kind = e.kind_mut();
        let should_propagate = match kind {
          ShErrKind::CleanExit(_) => true, // this one always goes
          ShErrKind::Raised(_, _) => true,
          ShErrKind::LoopBreak(_) | ShErrKind::LoopContinue(_) => {
            state::Shed::meta(MetaTab::in_loop)
          }
          ShErrKind::FuncReturn(_) => state::Shed::meta(MetaTab::in_func),
          _ if shopt!(set.errexit) => {
            // propagate if this is enabled
            *kind = ShErrKind::ErrInterrupt;
            true
          }
          _ => false,
        };

        if should_propagate {
          Shed::set_status(1);
          Err(e.with_context(context.iter()))
        } else {
          e.with_context(context.iter()).print_error();
          with_status(1)
        }
      }
    }
  }
  /// Parse arguments and options, pack `BuiltinArgs`, run `self.execute()`
  fn run_builtin(
    &self,
    node: &Node,
    _dispatcher: &mut Dispatcher,
    stdin: Option<Vec<u8>>,
  ) -> ShResult<()> {
    let span = node.get_span().clone();
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
      .map(|tk| tk.span.clone())
      .unwrap_or_else(|| span.clone());

    let (argv, opts) = self.get_argv_and_opts(cmd_span.clone(), argv, no_split)?;
    let builtin_args = BuiltinArgs {
      argv,
      opts,
      span,
      cmd_span,
      stdin,
    };

    self.execute(builtin_args)
  }
}

/// The arguments for a builtin.
///
/// Contains the argument vector (`argv`), the parsed options (`opts`), the
/// `span` of the entire command for error reporting, and `stdin` piped in
/// from a previous builtin in an in-process pipeline.
pub struct BuiltinArgs {
  argv: Vec<(String, Span)>,
  opts: Vec<Opt>,
  span: Span,     // the entire call
  cmd_span: Span, // just the command
  stdin: Option<Vec<u8>>,
}

impl BuiltinArgs {
  pub fn span(&self) -> Span {
    // cloning spans is cheap
    self.span.clone()
  }
  pub fn cmd_span(&self) -> Span {
    self.cmd_span.clone()
  }
  pub fn take_stdin(&mut self) -> Option<Vec<u8>> {
    self.stdin.take()
  }
  pub fn has_stdin(&self) -> bool {
    self.stdin.is_some()
  }
}

// Join all of the word-split arguments into a single string
// Preserve the span too
pub fn join_raw_args(args: Vec<(String, Span)>) -> (String, Span) {
  join_raw_arg_iter(args.into_iter())
}

pub fn join_raw_arg_iter(args: impl Iterator<Item = (String, Span)>) -> (String, Span) {
  args.fold((String::new(), Span::default()), |mut acc, arg| {
    if acc.1 == Span::default() {
      acc.1 = arg.1.clone();
    } else {
      let new_end = arg.1.end();
      let start = acc.1.start();
      acc.1.set_range(start..new_end);
    }

    if acc.0.is_empty() {
      acc.0 = arg.0;
    } else {
      let _ = write!(acc.0, " {}", arg.0);
    }
    acc
  })
}

// The easy ones

struct Colon;
impl Builtin for Colon {
  fn is_special(&self) -> bool {
    true
  }
  fn execute(&self, _args: BuiltinArgs) -> ShResult<()> {
    with_status(0)
  }
}

struct True;
impl Builtin for True {
  fn execute(&self, _args: BuiltinArgs) -> ShResult<()> {
    with_status(0)
  }
}

struct False;
impl Builtin for False {
  fn execute(&self, _args: BuiltinArgs) -> ShResult<()> {
    with_status(1)
  }
}

struct Thru;
impl Builtin for Thru {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::flag('c'),
      OptSpec::flag("count"),
      OptSpec::flag('a'),
      OptSpec::flag("append"),
      OptSpec::single_arg('t'),
      OptSpec::single_arg("tee"),
      OptSpec::single_arg('L'),
      OptSpec::single_arg("limit"),
    ]
  }
  fn execute(&self, mut args: BuiltinArgs) -> ShResult<()> {
    let mut input = if args.argv.is_empty() || args.has_stdin() {
      if let Some(stdin) = args.take_stdin() {
        stdin
      } else {
        procio::read_input()?
      }
    } else {
      return with_status(0);
    };

    let mut count = false;
    let mut append = false;
    let mut tee = None;
    let mut limit = None;

    for opt in &args.opts {
      match opt {
        Opt::LongWithArg(flag, arg) => match flag.as_str() {
          "tee" => tee = Some(arg.clone()),
          "append" => append = true,
          "count" => count = true,
          "limit" => limit = Some(arg.clone()),
          _ => {}
        },
        Opt::ShortWithArg('t', dest) => tee = Some(dest.clone()),
        Opt::ShortWithArg('L', dest) => limit = Some(dest.clone()),
        Opt::Short('c') => count = true,
        Opt::Short('a') => append = true,
        _ => {}
      }
    }

    if append && tee.is_none() {
      errln!("thru: warning: -a/--append ignored without -t/--tee");
    }

    if let Some(limit) = limit {
      match limit.parse::<usize>() {
        Ok(mut n) => {
          n = n.min(input.len());
          while n > 0 && !input.is_char_boundary(n) {
            n -= 1;
          }
          input.truncate(n);
        }
        Err(e) => {
          errln!("thru: invalid limit {limit}: {e}");
        }
      }
    }

    if count {
      let n = input.len();
      errln!("thru: {n} bytes");
    }

    if let Some(dest) = tee {
      let file = if append {
        std::fs::OpenOptions::new()
          .create(true)
          .append(true)
          .open(&dest)
      } else {
        std::fs::File::create(&dest)
      };
      match file {
        Ok(mut f) => {
          use std::io::Write;
          if let Err(e) = f.write_all(input.as_bytes()) {
            errln!("thru: failed to write to {dest}: {e}");
          }
        }
        Err(e) => {
          errln!("thru: failed to open {dest} for writing: {e}");
        }
      }
    }

    out!("{input}");

    with_status(0)
  }
}

struct BuiltinBuiltin;
impl Builtin for BuiltinBuiltin {
  // lol
  fn execute(&self, _args: BuiltinArgs) -> ShResult<()> {
    unreachable!("this one operates on the node directly")
  }
  fn setup_builtin(
    &self,
    node: &Node,
    dispatcher: &mut Dispatcher,
    stdin: Option<Vec<u8>>,
  ) -> ShResult<()> {
    let span = node.get_span();
    let NdRule::Command { assignments, argv } = &node.class else {
      unreachable!()
    };
    let inner_argv = argv.iter().skip(1).cloned().collect::<Vec<Tk>>();

    let cmd = inner_argv.first().map_or("", Tk::as_str);
    let Some(builtin) = lookup_builtin(cmd) else {
      sherr!(NotFound @ span, "builtin not found: {cmd}").print_error();
      return with_status(127);
    };

    let mut forwarded = node.clone();
    forwarded.class = NdRule::Command {
      assignments: assignments.clone(),
      argv: inner_argv,
    };
    builtin.setup_builtin(&forwarded, dispatcher, stdin)
  }
}

pub struct CommandBuiltin;
impl Builtin for CommandBuiltin {
  fn execute(&self, _args: BuiltinArgs) -> ShResult<()> {
    unreachable!("this one operates on the node directly")
  }
  fn run_builtin(
    &self,
    node: &Node,
    dispatcher: &mut Dispatcher,
    _stdin: Option<Vec<u8>>,
  ) -> ShResult<()> {
    let NdRule::Command { assignments, argv } = &node.class else {
      unreachable!()
    };
    let mut argv = argv.to_vec();

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

      match tk.as_str() {
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
      return with_status(0);
    }

    argv = rest;

    let node = Node {
      class: NdRule::Command {
        assignments: assignments.clone(),
        argv,
      },
      ..node.clone()
    };

    if use_default_path {
      let Some(default_path) = state::util::get_default_path() else {
        #[cfg(target_os = "android")]
        return Err(sherr!(ExecFail @ node.get_span(), "the -p flag is not supported on Android"));

        #[cfg(not(target_os = "android"))]
        return Err(sherr!(ExecFail @ node.get_span(), "unable to get default path"));
      };
      // TODO: Find a way to do this that doesn't involve forcing a full PATH rehash twice
      defer! {
        Shed::meta_mut(MetaTab::rehash_path_cache);
      }
      state::util::with_vars([("PATH".to_string(), default_path)], || {
        Shed::meta_mut(MetaTab::rehash_path_cache);
        Self::execute_inner(print_path, print_type, &node, dispatcher)
      })
    } else {
      Self::execute_inner(print_path, print_type, &node, dispatcher)
    }
  }
}

impl CommandBuiltin {
  fn execute_inner(
    print_path: bool,
    print_type: bool,
    node: &Node,
    dispatcher: &mut Dispatcher,
  ) -> ShResult<()> {
    let NdRule::Command { argv, .. } = &node.class else {
      unreachable!()
    };
    if print_path {
      let Some(name) = argv.first() else {
        return with_status(2);
      };
      let name_str = name.as_str();
      match state::util::which_util(name_str) {
        Some(util) => match util.kind() {
          UtilKind::Alias => {
            let Some(alias) = Shed::logic(|l| l.get_alias(name_str)) else {
              return with_status(127);
            };
            outln!("alias {name_str}={}", shell_quote(alias.body()));
          }
          UtilKind::Function | UtilKind::Builtin => outln!("{name_str}"),
          UtilKind::Command(p) | UtilKind::File(p) => outln!("{}", p.display()),
        },
        None if KEYWORDS.contains(&name_str) => outln!("{name_str}"),
        None => return with_status(127),
      }

      return with_status(0);
    }
    if print_type {
      let Some(name) = argv.first() else {
        return with_status(2);
      };
      let name_str = name.as_str();
      match state::util::which_util(name_str) {
        Some(util) => match util.kind() {
          UtilKind::Alias => {
            let Some(alias) = Shed::logic(|l| l.get_alias(name_str)) else {
              return with_status(127);
            };
            outln!("{name_str} is an alias for {}", shell_quote(alias.body()));
          }
          UtilKind::Function => outln!("{name_str} is a function"),
          UtilKind::Builtin => outln!("{name_str} is a shell builtin"),
          UtilKind::Command(p) | UtilKind::File(p) => {
            outln!("{name_str} is {}", p.display());
          }
        },
        None if KEYWORDS.contains(&name_str) => outln!("{name_str} is a shell keyword"),
        None => {
          errln!("command: {name_str}: not found");
          return with_status(127);
        }
      }

      return with_status(0);
    }

    // this one has to offload to the dispatcher
    dispatcher.exec_cmd(node)
  }
}

#[cfg(test)]
pub mod tests {
  use crate::{
    Shed, assert_status_eq,
    eval::execute::exec_nonint,
    state::{self, vars::VarFlags},
    tests::testutil::{TestGuard, has_cmd, test_input},
  };

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
}
