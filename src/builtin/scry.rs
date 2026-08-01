use crate::{
  ShResult, Shed, builtin::getopt::Opt, errln, expand, out, outln, readline::FuzzyBuilder,
  util::with_status,
};

use super::getopt::OptSpec;

pub(super) struct Scry;
impl super::Builtin for Scry {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      OptSpec::flag('0'),
      OptSpec::flag("read0"),
      OptSpec::flag('q'),
      OptSpec::flag("quote-in"),
      OptSpec::flag('Q'),
      OptSpec::flag("quote-out"),
      OptSpec::flag('n'),
      OptSpec::single_arg('p'),
      OptSpec::single_arg("prompt"),
    ]
  }
  fn strict_opts(&self) -> bool {
    true
  }
  fn execute(&self, mut args: super::BuiltinArgs) -> ShResult<()> {
    let Some(input) = self.get_input_str(&mut args) else {
      return with_status(0);
    };
    let mut null_in = false;
    let mut quote_in = false;
    let mut quote_out = false;
    let mut prompt = None;
    let mut no_newline = false;

    for opt in &args.opts {
      match opt {
        Opt::Short('0') => null_in = true,
        Opt::Short('n') => no_newline = true,
        Opt::Short('q') => quote_in = true,
        Opt::Short('Q') => quote_out = true,
        Opt::Long(flag) => match flag.as_str() {
          "read0" => null_in = true,
          "quote-in" => quote_in = true,
          "quote-out" => quote_out = true,
          _ => {}
        },

        Opt::ShortWithArg('p', arg) => prompt = Some(arg.clone()),
        Opt::LongWithArg(flag, arg) if flag == "prompt" => prompt = Some(arg.clone()),
        _ => {}
      }
    }

    let entries = if quote_in {
      Self::split_input_quoted(&input)?
    } else if null_in {
      Self::split_input_null(&input)
    } else {
      input.lines().map(|s| (s.to_string(), 0)).collect()
    };

    if entries.is_empty() {
      errln!("scry: received no items to list");
      return with_status(2);
    }

    let mut selector = FuzzyBuilder::new().with_entries(entries).with_inline(false);

    if let Some(prompt) = prompt {
      selector = selector.with_placeholder(prompt.as_str());
    }

    match selector.pick()? {
      Some(item) => {
        if quote_out {
          Shed::sinks(|s| expand::shell_quote_fmt(&item, s)).ok();
        } else if no_newline {
          out!("{item}");
        } else {
          outln!("{item}");
        }
        with_status(0)
      }
      None => with_status(1),
    }
  }
}

impl Scry {
  fn split_input_quoted(input: &str) -> ShResult<Vec<(String, i32)>> {
    Ok(
      super::quote::unquote_records(input)?
        .into_iter()
        .map(|r| (r.join(" "), 0))
        .collect(),
    )
  }

  fn split_input_null(input: &str) -> Vec<(String, i32)> {
    input.split('\0').map(|s| (s.to_string(), 0)).collect()
  }
}
