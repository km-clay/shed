use crate::{
  outln,
  state::{rc, shopt::ShoptSource},
  util::{self, error::ShResult},
};

use super::opt::OptSpec;

/// `genrc` — print an rc file built from the current shell state to
/// stdout. Used to (re)generate `~/.shedrc` after a shopt rename, or to
/// inspect the live config in re-sourceable form.
pub struct GenRc;

impl super::Builtin for GenRc {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::new_short("shopts", b's'),
      OptSpec::new_short("alias", b'a'),
      OptSpec::new_short("keymaps", b'k'),
      OptSpec::new_short("autocmds", b'A'),
      OptSpec::new_short("functions", b'f'),
      OptSpec::new_short("completions", b'c'),
      OptSpec::new_long("default"),
      OptSpec::new_long("no-comments"),
    ]
  }

  fn strict_opts(&self) -> bool {
    true
  }

  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let mut config = rc::GenRcConfig::default();
    let mut use_defaults = false;
    let mut no_comments = false;

    let mut want_shopts = false;
    let mut want_aliases = false;
    let mut want_keymaps = false;
    let mut want_autocmds = false;
    let mut want_functions = false;
    let mut want_completions = false;
    let mut any_section_flag = false;

    for opt in args.options() {
      match opt.key() {
        "shopt" => {
          want_shopts = true;
          any_section_flag = true;
        }
        "alias" => {
          want_aliases = true;
          any_section_flag = true;
        }
        "keymaps" => {
          want_keymaps = true;
          any_section_flag = true;
        }
        "autocmds" => {
          want_autocmds = true;
          any_section_flag = true;
        }
        "functions" => {
          want_functions = true;
          any_section_flag = true;
        }
        "completions" => {
          want_completions = true;
          any_section_flag = true;
        }
        "default" => use_defaults = true,
        "no-comments" => no_comments = true,
        _ => {}
      }
    }

    if any_section_flag {
      // Everything defaults to off and is opted back in by the section
      // flags the user passed.
      config.include_shopts = want_shopts;
      config.include_aliases = want_aliases;
      config.include_keymaps = want_keymaps;
      config.include_autocmds = want_autocmds;
      config.include_functions = want_functions;
      config.include_completions = want_completions;
    }

    if use_defaults {
      config.source = ShoptSource::Defaults;
    }
    if no_comments {
      config.include_comments = false;
    }

    for line in rc::compose_rc(&config) {
      outln!("{}", line);
    }

    util::with_status(0)
  }
}
