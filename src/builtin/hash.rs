use std::rc::Rc;

use crate::{
  expand::as_var_val_display,
  getopt::{Opt, OptSpec},
  outln, sherr,
  state::{self, MetaTab, Utility, read_meta, write_meta},
  util::{error::ShResult, with_status},
};

pub struct HashOpts {
  clear: bool,
  refresh: bool,
}

impl HashOpts {
  pub fn from_opts(opts: &[Opt]) -> ShResult<Self> {
    let mut new = Self {
      clear: false,
      refresh: false,
    };

    for opt in opts {
      match opt {
        Opt::Long(s) if s == "refresh" => {
          new.refresh = true;
        }
        Opt::Short('r') => {
          new.clear = true;
        }
        _ => {
          return Err(sherr!(ParseErr, "Invalid hash option: {opt:?}"));
        }
      }
    }

    Ok(new)
  }
}

pub(super) struct Hash;
impl super::Builtin for Hash {
  fn opts(&self) -> Vec<OptSpec> {
    vec![OptSpec::flag('r'), OptSpec::flag("refresh")]
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let mut refresh = false;
    let mut clear = false;

    for opt in &args.opts {
      match opt {
        Opt::Short('r') => clear = true,
        Opt::Long(s) if s == "refresh" => refresh = true,
        _ => return Err(sherr!(ParseErr, "Invalid hash option: {opt:?}").promote(args.span())),
      }
    }

    if args.argv.is_empty() && args.opts.is_empty() {
      let cmds: Vec<Rc<Utility>> = read_meta(|m| m.cached_utils().collect());
      for cmd in cmds {
        if let state::meta::UtilKind::Command(path) = cmd.kind() {
          let path = as_var_val_display(&path.to_string_lossy());
          let name = cmd.name();
          outln!("{name}={path}")?;
        }
      }
    }

    write_meta(|m| {
      if clear {
        m.clear_cache();
      }
      if refresh {
        m.rehash();
      }
    });

    let path_cmds = MetaTab::get_cmds_in_path();

    write_meta(|m| {
      for (arg, span) in args.argv {
        if let Some(cmd) = path_cmds.iter().find(|cmd| cmd.name() == arg) {
          m.cache_util(Rc::clone(cmd));
        } else {
          return Err(sherr!(NotFound, "Command not found: {arg}").promote(span));
        }
      }
      Ok(())
    })?;

    with_status(0)
  }
}
