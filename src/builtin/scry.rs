use crate::{
  ShResult, Shed, errln, expand, opt, out, outln, readline::FuzzyBuilder, util::with_status,
};

use super::opt::OptSpec;

pub(super) struct Scry;
impl super::Builtin for Scry {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      opt!("read0" | b'0'),
      opt!("quote-in" | b'q'),
      opt!("quote-out" | b'Q'),
      opt!("prompt" | b'p', 1),
      OptSpec::new_short("no-newline", b'n'),
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

    for opt in args.options() {
      match opt.key() {
        "read0" => null_in = true,
        "quote-in" => quote_in = true,
        "quote-out" => quote_out = true,
        "no-newline" => no_newline = true,
        "prompt" => {
          prompt = Some(opt.value()?.to_string());
        }
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
      super::quote::unquote_records(input.as_bytes())?
        .into_iter()
        // scry's fuzzy picker is String-based, so the (byte-native) records are
        // lossy-decoded here at the UI boundary.
        .map(|r| (String::from_utf8_lossy(&r.join(&b' ')).into_owned(), 0))
        .collect(),
    )
  }

  fn split_input_null(input: &str) -> Vec<(String, i32)> {
    input.split('\0').map(|s| (s.to_string(), 0)).collect()
  }
}
