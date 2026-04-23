use ariadne::Span as ASpan;

use crate::{
  getopt::{Opt, OptSpec, get_opts_from_tokens, get_opts_from_tokens_strict},
  parse::{
    NdRule, Node,
    execute::prepare_argv,
    lex::{Span, Tk},
  },
  util::{error::ShResult, with_status},
};

pub mod alias;
pub mod arrops;
pub mod autocmd;
pub mod cd;
pub mod complete;
pub mod dirstack;
pub mod echo;
pub mod eval;
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
  "cd"       => cd::Cd,
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
  /// Parse arguments and options, pack BuiltinArgs, run self.execute()
  fn run_builtin(&self, node: Node) -> ShResult<()> {
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
