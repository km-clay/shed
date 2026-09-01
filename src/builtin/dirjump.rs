use crate::{
  expand::escape,
  opt, outln, sherr,
  state::{
    Shed, cwd,
    meta::{JumpTableDirs, MetaTab},
  },
  util::{
    self,
    error::{ShResult, ShResultExt},
  },
};

use super::opt::OptSpec;

type DirListFn = Box<dyn Fn(&MetaTab) -> JumpTableDirs>;
type DirJumpFn = Box<dyn Fn(usize) -> ShResult<()>>;

/// the [`prevd`](PrevD) and [`nextd`](NextD) builtins are essentially the same thing but mirrored
/// so they both implement this trait that performs the same operation in a bidirectional way
trait DirJump {
  /// Function that lists directories for the given direction
  fn dir_list(&self) -> DirListFn;
  /// Function that jumps to the next directory in the given direction
  fn dir_jump(&self) -> DirJumpFn;

  /// Other options
  ///
  /// Default impl gives an empty vec, but we can add more options in the future if needed
  fn other_opts(&self) -> Vec<OptSpec> {
    vec![]
  }
  /// The opts passed to [`Builtin::opts`](super::Builtin::opts) for the builtin
  fn jump_opts(&self) -> Vec<OptSpec> {
    let mut other = self.other_opts();
    other.push(opt!("list" | b'l'));
    other
  }

  /// The function executed in [`Builtin::execute`](super::Builtin::execute)
  fn exec_jump(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let dir_list = self.dir_list();
    let dir_jump = self.dir_jump();

    if args.has_opt("list") {
      let output = Shed::meta(|m| {
        dir_list(m).fold(String::new(), |mut acc, dir| {
          let dir_str = dir.to_string_lossy();
          escape::shell_quote_fmt(&dir_str, &mut acc).ok();
          acc.push(' ');
          acc
        })
      });

      outln!("{output}");
      return util::with_status(0);
    }

    let count = match args.arguments().next() {
      None => 1,
      Some((arg, span)) => util::parse_bytes::<usize>(arg.as_bytes()).ok_or_else(
        || sherr!(ParseErr @ span.clone(), "argument must be a non-negative integer"),
      )?,
    };

    if count == 0 {
      return util::with_status(0);
    }

    dir_jump(count).promote_err(args.cmd_span())?;
    util::with_status(0)
  }
}

pub(super) struct PrevD;
impl DirJump for PrevD {
  fn dir_list(&self) -> DirListFn {
    Box::new(MetaTab::back_dirs)
  }
  fn dir_jump(&self) -> DirJumpFn {
    Box::new(cwd::prev_dir)
  }
}

impl super::Builtin for PrevD {
  fn opts(&self) -> Vec<OptSpec> {
    self.jump_opts()
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    self.exec_jump(args)
  }
}

pub(super) struct NextD;
impl DirJump for NextD {
  fn dir_list(&self) -> DirListFn {
    Box::new(MetaTab::fwd_dirs)
  }
  fn dir_jump(&self) -> DirJumpFn {
    Box::new(cwd::next_dir)
  }
}
impl super::Builtin for NextD {
  fn opts(&self) -> Vec<OptSpec> {
    self.jump_opts()
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    self.exec_jump(args)
  }
}
