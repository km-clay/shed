use crate::{
  expand::escape,
  opt, outln,
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

type DirListFn = Box<dyn FnOnce(&MetaTab) -> JumpTableDirs>;
type DirJumpFn = Box<dyn FnOnce() -> ShResult<()>>;
trait DirJump {
  fn dir_list(&self) -> DirListFn;
  fn dir_jump(&self) -> DirJumpFn;

  fn other_opts(&self) -> Vec<OptSpec> {
    vec![]
  }
  fn jump_opts(&self) -> Vec<OptSpec> {
    let mut other = self.other_opts();
    other.push(opt!("list" | b'l'));
    other
  }

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

    dir_jump().promote_err(args.cmd_span())?;

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
