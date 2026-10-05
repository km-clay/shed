use crate::{
  opt, outln,
  readline::{self, FuzzyBuilder},
  sherr,
  state::{Shed, cwd, paths, terminal::Terminal},
  util::{self, error::ShResult},
};

use super::super::{Builtin, BuiltinArgs, opt::OptSpec};

use super::{fuzzy_score_dir, highlight_dir, load_abbreviated_dirs, load_dir_entries};

pub(super) struct ZdJump;
impl Builtin for ZdJump {
  fn opts(&self) -> Vec<OptSpec> {
    vec![opt!("print" | b'p')]
  }
  fn execute(&self, args: BuiltinArgs) -> ShResult<()> {
    // every positional is concatenated into one subsequence query, so
    // `zd pro fern` still finds `~/projects/fern`.
    let query = args
      .arguments()
      .map(|(a, _)| a.to_str_lossy())
      .collect::<String>()
      .trim_end_matches('/')
      .to_string();
    let print_dir = args.options().any(|o| o.key() == "print");

    let entries = if query.is_empty() {
      load_abbreviated_dirs()
    } else {
      load_dir_entries()
    };
    if entries.is_empty() {
      return Err(sherr!(ExecFail @ args.cmd_span(), "no directory history yet"));
    }

    let mut target = if query.is_empty() {
      if !Shed::term(Terminal::interactive) {
        return Err(
          sherr!(ExecFail @ args.cmd_span(), "a directory query is required when non-interactive"),
        );
      }
      // no argument, let's open the fuzzy finder
      let selector = FuzzyBuilder::new()
        .with_entries(entries)
        .with_placeholder("pick a directory (type to filter, enter selects, esc cancels)")
        .with_score_cb(fuzzy_score_dir)
        .with_highlight_cb(highlight_dir);

      selector.pick()?
    } else {
      readline::fuzzy_best_match(&query, entries, Some(fuzzy_score_dir), None)
    };

    if let Some(target) = target.as_mut()
      && target.starts_with('~')
      && let Some(home) = paths::get_home_str()
    {
      *target = target.replacen('~', &home.to_str_lossy(), 1);
    }

    match target {
      Some(path) => {
        if print_dir {
          outln!("{path}");
        } else if let Err(e) = cwd::change_dir(&path) {
          return Err(sherr!(ExecFail @ args.span(), "could not change directory: {e}"));
        }
        util::with_status(0)
      }
      // cancelled, or nothing matched the query
      None => util::with_status(1),
    }
  }
}
