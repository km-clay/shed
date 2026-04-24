use ariadne::Span as ASpan;
use nix::unistd::Pid;

use crate::{
  getopt::{Opt, OptSpec, get_opts_from_tokens, get_opts_from_tokens_strict}, jobs::ChildProc, parse::{
    NdFlags, NdRule, Node, execute::{AssignBehavior, Dispatcher, exec_nonint, prepare_argv}, lex::{Span, Tk}
  }, sherr, state::read_meta, util::{error::{ShErrKind, ShResult}, guards::var_ctx_guard, with_status}
};

pub mod alias;
pub mod arrops;
pub mod autocmd;
pub mod cd;
pub mod complete;
pub mod dirstack;
pub mod eval;
pub mod echo;
pub mod exec;
pub mod fixcmd;
pub mod flowctl;
pub mod getopts;
pub mod hash;
pub mod help;
pub mod hist;
pub mod intro;
pub mod jobctl;
pub mod keymap;
pub mod map;
pub mod msg;
pub mod pwd;
pub mod read;
pub mod resource;
pub mod seek;
pub mod set;
pub mod shift;
pub mod shopt;
pub mod source;
pub mod stash;
pub mod test; // [[ ]] thing
pub mod times;
pub mod trap;
pub mod varcmds;

macro_rules! register_builtins {
  ($($name:literal => $ty:expr),* $(,)?) => {
    static BUILTIN_TABLE: &[(&str, &dyn Builtin)] = &[
      $(($name, &$ty)),*
    ];

    pub const BUILTIN_NAMES: &[&str] = &[
      $($name),*
    ];

    // credit goes to fish shell for this idea. very nice pattern
    // at compile time, checks to see if the name list is sorted alphabetically
    // if not, compiler error
    const _: () = {
      let mut i = 1;
      while i < BUILTIN_NAMES.len() {
        let prev = BUILTIN_NAMES[i - 1].as_bytes();
        let curr = BUILTIN_NAMES[i].as_bytes();
        let len = if prev.len() < curr.len() {
          prev.len()
        } else {
          curr.len()
        };
        let mut j = 0;
        while j < len {
          if prev[j] > curr[j] {
            panic!("Builtin names must be in alphabetical order");
          }
          if prev[j] < curr[j] {
            break;
          }
          j += 1;
        }

        if j == len && prev.len() >= curr.len() {
          panic!("Builtin names must be in alphabetical order");
        }

        i += 1;
      }
    };
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
  "alias"    => alias::Alias,
  "autocmd"  => autocmd::AutoCmdBuiltin,
  "bg"       => jobctl::Bg,
  "break"    => flowctl::Break,
  "builtin"  => BuiltinBuiltin,
  "cd"       => cd::Cd,
  "command"  => CommandBuiltin,
  "compgen"  => complete::CompGen,
  "complete" => complete::Complete,
  "continue" => flowctl::Continue,
  "dirs"     => dirstack::Dirs,
  "disown"   => jobctl::Disown,
  "echo"     => echo::Echo,
  "eval"     => eval::Eval,
  "exec"     => exec::Exec,
  "exit"     => flowctl::Exit,
  "export"   => varcmds::Export,
  "false"    => False,
  "fc"       => fixcmd::FixCmd,
  "fg"       => jobctl::Fg,
  "fpop"     => arrops::FrontPop,
  "fpush"    => arrops::FrontPush,
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
  "push"     => arrops::Push,
  "pushd"    => dirstack::PushDir,
  "pwd"      => pwd::Pwd,
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
  "times"    => times::Times,
  "trap"     => trap::Trap,
  "true"     => True,
  "type"     => intro::Type,
  "ulimit"   => resource::ULimit,
  "umask"    => resource::UMask,
  "unalias"  => alias::Unalias,
  "unset"    => varcmds::Unset,
  "wait"     => jobctl::Wait,
}

/// Lookup a name in the builtin table via binary search
pub fn lookup_builtin(name: &str) -> Option<&'static dyn Builtin> {
  BUILTIN_TABLE
    .binary_search_by_key(&name, |(n, _)| n)
    .ok()
    .map(|idx| BUILTIN_TABLE[idx].1 as &dyn Builtin)
}

type ArgVector = Vec<(String, Span)>;
pub trait Builtin: Sync {
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
  /// The way that the builtin parses its options. Some of them are weird, like `set`
  fn get_argv_and_opts(&self, argv: Vec<Tk>) -> ShResult<(ArgVector, Vec<Opt>)> {
    let opts = self.opts();
    let (mut argv, opts) = if opts.is_empty() {
      (prepare_argv(argv)?, vec![])
    } else if self.strict_opts() {
      get_opts_from_tokens_strict(argv, &opts)?
    } else {
      get_opts_from_tokens(argv, &opts)?
    };
    if !argv.is_empty() {
      argv.remove(0);
    };
    Ok((argv, opts))
  }
  /// The main entry point for running a builtin. This is responsible for setting up the environment, handling redirections, and catching control flow errors.
  fn setup_builtin(
    &self,
    mut node: Node,
    dispatcher: &mut Dispatcher
  ) -> ShResult<()> {
    let cmd_raw = node.get_command().unwrap().to_string();
    let report_time = node.flags.contains(NdFlags::REPORT_TIME);
    let context = node.context.clone();
    let NdRule::Command { assignments, argv } = &mut node.class else {
      unreachable!()
    };
    let env_vars = dispatcher.set_assignments(std::mem::take(assignments), AssignBehavior::Export)?;
    let _var_guard = var_ctx_guard(env_vars.into_iter().collect());
    let fork_builtins = node.flags.contains(NdFlags::FORK_BUILTINS);

    if argv.len() == 2 && argv[1].as_str() == "--help" {
      // we have been asked for help
      // is this a hack? only the nose knows.
      return exec_nonint(
        format!("help builtin-{cmd_raw}"),
        None,
        Some("<builtin-help>".into()),
      );
    }

    // Set up redirections here so we can attach the guard to propagated errors.
    dispatcher.io_stack.append_to_frame(std::mem::take(&mut node.redirs));
    let guard = dispatcher.io_stack.pop_frame().redirect()?;

    // Register ChildProc in current job
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
      report_time,
    )?;
    job.push_child(child);

    // Handle exec specially - persist redirections before dispatch
    if cmd_raw.as_str() == "exec" {
      guard.persist();
    }

    let result = self.run_builtin(node, dispatcher);

    // Now we inspect the error that we got, if any
    match result {
      Ok(()) => Ok(()),
      Err(e) => {
        // if we aren't in the context these are looking for
        // then they will bubble all the way up to main
        // which cancels execution. Let's catch that here
        let should_propagate = match e.kind() {
          ShErrKind::CleanExit(_) => true, // this one always goes
          ShErrKind::LoopBreak(_) | ShErrKind::LoopContinue(_) => read_meta(|m| m.in_loop()),
          ShErrKind::FuncReturn(_) => read_meta(|m| m.in_func()),
          _ => false,
        };

        if should_propagate {
          Err(e.with_context(context))
        } else {
          e.with_context(context).print_error();
          with_status(1)
        }
      }
    }
  }
  /// Parse arguments and options, pack BuiltinArgs, run self.execute()
  fn run_builtin(
    &self,
    node: Node,
    _dispatcher: &mut Dispatcher
  ) -> ShResult<()> {
    let span = node.get_span().clone();
    let NdRule::Command {
      assignments: _,
      argv,
    } = node.class
    else {
      unreachable!()
    };

    let (argv, opts) = self.get_argv_and_opts(argv)?;
    let builtin_args = BuiltinArgs { argv, opts, span };

    self.execute(builtin_args)
  }
}

/// The arguments for a builtin.
///
/// Contains the argument vector (`argv`), the parsed options (`opts`), and the `span` of the entire command for error reporting.
pub struct BuiltinArgs {
  argv: Vec<(String, Span)>,
  opts: Vec<Opt>,
  span: Span,
}

impl BuiltinArgs {
  pub fn span(&self) -> Span {
    // cloning spans is cheap
    self.span.clone()
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
      acc.0 = acc.0 + &format!(" {}", arg.0);
    }
    acc
  })
}

// The easy ones

struct Colon;
impl Builtin for Colon {
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

struct BuiltinBuiltin;
impl Builtin for BuiltinBuiltin { // lol
  fn execute(&self, _args: BuiltinArgs) -> ShResult<()> {
    unreachable!("this one operates on the node directly")
  }
  fn setup_builtin(
    &self,
    mut node: Node,
    dispatcher: &mut Dispatcher
  ) -> ShResult<()> {
    let span = node.get_span();
    let NdRule::Command {
      assignments: _,
      ref mut argv,
    } = node.class else {
      unreachable!()
    };
    *argv = argv
      .iter_mut()
      .skip(1)
      .map(|tk| tk.clone())
      .collect::<Vec<Tk>>();

    let cmd = argv.first().map(|tk| tk.as_str()).unwrap_or("");
    let Some(builtin) = lookup_builtin(cmd) else {
      sherr!(NotFound @ span, "builtin not found: {cmd}")
        .print_error();
      return with_status(127);
    };

    builtin.setup_builtin(node, dispatcher)
  }
}

pub struct CommandBuiltin;
impl Builtin for CommandBuiltin {
  fn execute(&self, _args: BuiltinArgs) -> ShResult<()> {
    unreachable!("this one operates on the node directly")
  }
  fn run_builtin(
    &self,
    mut node: Node,
    dispatcher: &mut Dispatcher
  ) -> ShResult<()> {
    let NdRule::Command {
      assignments: _,
      ref mut argv,
    } = node.class else {
      unreachable!()
    };
    *argv = argv
      .iter_mut()
      .skip(1)
      .map(|tk| tk.clone())
      .collect::<Vec<Tk>>();

    node.flags |= NdFlags::NO_FORK;

    // this one has to offload to the dispatcher
    dispatcher.exec_cmd(node)
  }
}

#[cfg(test)]
pub mod tests {
  use crate::{
    state,
    testutil::{TestGuard, test_input},
  };

  // You can never be too sure!!!!!!
  #[test]
  fn test_true() {
    let _g = TestGuard::new();
    test_input("true").unwrap();

    assert_eq!(state::get_status(), 0);
  }

  #[test]
  fn test_false() {
    let _g = TestGuard::new();
    test_input("false").unwrap();

    assert_eq!(state::get_status(), 1);
  }

  #[test]
  fn test_colon() {
    let _g = TestGuard::new();
    test_input(":").unwrap();

    assert_eq!(state::get_status(), 0);
  }
}
