use itertools::Itertools;

use crate::{
  autoload::{self, AutoloadSrc, Autoloader},
  expand::escape,
  outln, sherr,
  state::{
    Shed,
    logic::{LogTab, ShFunc},
  },
  util::{self, error::ShResult},
  var, varstr,
};

use super::opt::OptSpec;

pub(super) struct Autoload;
impl super::Builtin for Autoload {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::new_long("path").short(b'p'),
      OptSpec::new_long("now").short(b'n'),
      OptSpec::new_long("comp").short(b'c'),
    ]
  }
  fn strict_opts(&self) -> bool {
    true
  }
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let mut path = false;
    let mut now  = false;
    let mut comp = false;

    for opt in args.options() {
      match opt.key() {
        "path" => path = true,
        "now"  => now = true,
        "comp" => comp = true,
        _      => return Err(sherr!(ParseErr @ opt.span(), "unknown option {opt}").with_code(2)),
      }
    }

    if args.no_arguments() {
      let names = if comp {
        Shed::logic(LogTab::get_autoload_comp_names)
      } else {
        Shed::logic(LogTab::get_autoload_func_names)
      };

      let output = names
        .into_iter()
        .map(|name| varstr!("autoload {}", escape::shell_quote(&name)))
        .join("\n");

      outln!("{output}");
    }

    if path {
      // arguments are directories (or `:`-separated lists) to crawl
      for arg in args.arguments() {
        for (name, src) in autoload::crawl(arg) {
          register(&name, src, now, comp)?;
        }
      }
    } else {
      // arguments are names, resolved against the bundled set plus the live
      // $SHED_FUNC_PATH / $SHED_COMPLETE_PATH
      let mut set = if comp {
        autoload::CompLoader.bundled()
      } else {
        autoload::FuncLoader.bundled()
      };
      let search = if comp {
        var!(b"SHED_COMPLETE_PATH")
      } else {
        var!(b"SHED_FUNC_PATH")
      };
      set.extend(autoload::crawl(&search));

      for arg in args.arguments() {
        let Some(src) = set.remove(arg.as_bytes()) else {
          return Err(sherr!(ParseErr ? arg.span(), "no such autoload: {arg}"));
        };
        register(arg, src, now, comp)?;
      }
    }

    util::with_status(0)
  }
}

/// Register a resolved autoload entry, or source it immediately when `now`.
fn register(name: &[u8], src: AutoloadSrc, now: bool, comp: bool) -> ShResult<()> {
  if now {
    return src.source();
  }
  if comp {
    Shed::logic_mut(|l| l.insert_comp_autoload(name, src));
  } else {
    Shed::logic_mut(|l| l.insert_func(name, ShFunc::Autoload(src)));
  }
  Ok(())
}
