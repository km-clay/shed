use std::rc::Rc;

use nix::libc::STDOUT_FILENO;

use crate::{
  expand::as_var_val_display,
  getopt::{Opt, OptArg, OptSpec, get_opts_from_tokens},
  libsh::error::ShResult,
  parse::{NdRule, Node},
  prelude::*,
  procio::borrow_fd,
  sherr,
  state::{self, MetaTab, Utility, read_meta, write_meta},
};

pub fn hash_opt_spec() -> [OptSpec; 2] {
  [
    OptSpec {
      opt: Opt::Short('r'),
      takes_arg: OptArg::None,
    },
    OptSpec {
      opt: Opt::Long("refresh".into()),
      takes_arg: OptArg::None,
    },
  ]
}

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

pub fn hash_builtin(node: Node) -> ShResult<()> {
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  let (mut argv, opts) = get_opts_from_tokens(argv, &hash_opt_spec())?;
  argv.remove(0);
  if argv.is_empty() && opts.is_empty() {
    let stdout = borrow_fd(STDOUT_FILENO);
    let cmds: Vec<Rc<Utility>> = read_meta(|m| m.cached_utils().collect());
    for cmd in cmds {
      if let state::meta::UtilKind::Command(path) = cmd.kind() {
        let path = as_var_val_display(&path.to_string_lossy());
        let name = cmd.name();
        write(stdout, format!("{name}={path}\n").as_bytes())?;
      }
    }
  }

  let opts = HashOpts::from_opts(&opts)?;

  write_meta(|m| {
    if opts.clear {
      m.clear_cache();
    }
    if opts.refresh {
      m.rehash();
    }
  });

  let path_cmds = MetaTab::get_cmds_in_path();

  write_meta(|m| {
    for (arg, span) in argv {
      if let Some(cmd) = path_cmds.iter().find(|cmd| cmd.name() == arg) {
        m.cache_util(Rc::clone(cmd));
      } else {
        return Err(sherr!(NotFound, "Command not found: {arg}").promote(span));
      }
    }
    Ok(())
  })?;

  state::set_status(0);
  Ok(())
}
