use crate::{state::vars::VarStr, varstr};

use super::{
  super::state::shopt::ShOpts,
  Shed,
  opt::OptSpec,
  outln, sherr,
  util::{ShResult, ShResultExt, with_status},
};

/// List of deprecated shopt names, in case we need an entire list at some point.
/// Can't hurt to have.
const DEPRECATED_SHOPTS: &[(&str, &str)] =
  &[("highlight.valid_command", "highlight.external_command")];

pub(super) struct Shopt;
impl super::Builtin for Shopt {
  fn opts(&self) -> Vec<OptSpec> {
    vec![OptSpec::new_short("print-help", b'h')]
  }
  fn execute(&self, mut args: super::BuiltinArgs) -> ShResult<()> {
    let (arg_vec, opts) = args.take_argv();
    let print_help = opts.iter().any(|o| o.key() == "print-help");

    if arg_vec.is_empty() {
      let output = Shed::shopts_mut(ShOpts::display_opts)?;

      outln!("{}", prefix_sourceable(&output));

      return with_status(0);
    }

    for (mut arg, span) in arg_vec {
      // Split into key + optional value so the deprecation check works
      // for both `shopt key` and `shopt key=value`.
      let (key, value) = match arg.to_str_lossy().split_once('=') {
        Some((k, v)) => (k.into(), Some(VarStr::from(v))),
        None => (arg.clone(), None),
      };

      if let Some((_, new_key)) = DEPRECATED_SHOPTS
        .iter()
        .find(|(old, _)| *old == key.to_str_lossy())
      {
        sherr!(DeprecationWarning @ span.clone(),
          "shopt: '{key}' has been renamed to '{new_key}'"
        )
        .print_error();
        arg = match value {
          Some(v) => varstr!("{new_key}={v}"),
          None => (*new_key).into(),
        };
      }

      let Some(output) = Shed::shopts_mut(|s| s.query(&arg.to_str_lossy())).promote_err(span)?
      else {
        continue;
      };

      if print_help {
        outln!("{output}");
      } else if output.lines().count() > 2 {
        outln!("{}", prefix_sourceable(&output));
      } else {
        let value = output.lines().nth(1).unwrap_or("");
        outln!("shopt {arg}={value}");
      }
    }

    with_status(0)
  }
}

fn prefix_sourceable(s: &str) -> String {
  s.lines()
    .map(|line| {
      // A sourceable shopt line is a single 'key=value' with no
      // leading whitespace and at least one '.' in the key (every
      // group is namespaced). Comments and blank lines pass through.
      let trimmed = line.trim_end();
      if trimmed.is_empty() || trimmed.starts_with('#') {
        return trimmed.to_string();
      }
      match trimmed.split_once('=') {
        Some((key, _)) if key.contains('.') && !key.contains(char::is_whitespace) => {
          format!("shopt {trimmed}")
        }
        _ => trimmed.to_string(),
      }
    })
    .collect::<Vec<_>>()
    .join("\n")
}

#[cfg(test)]
mod tests {
  use crate::state::{self, Shed};
  use crate::tests::testutil::{TestGuard, test_input};

  // ===================== Display =====================

  #[test]
  fn shopt_no_args_displays_all() {
    let guard = TestGuard::new();
    test_input("shopt").unwrap();
    let out = guard.read_output();
    assert!(out.contains("dotglob"));
    assert!(out.contains("autocd"));
    assert!(out.contains("max_entries"));
    assert!(out.contains("comp_limit"));
  }

  #[test]
  fn shopt_query_category() {
    let guard = TestGuard::new();
    test_input("shopt core").unwrap();
    let out = guard.read_output();
    assert!(out.contains("dotglob"));
    assert!(out.contains("autocd"));
    // Should not contain prompt opts
    assert!(!out.contains("comp_limit"));
  }

  #[test]
  fn shopt_query_single_opt() {
    let guard = TestGuard::new();
    test_input("shopt core.dotglob").unwrap();
    let out = guard.read_output();
    assert!(out.contains("false"));
  }

  // ===================== Set =====================

  #[test]
  fn shopt_set_bool() {
    let _g = TestGuard::new();
    test_input("shopt core.dotglob=true").unwrap();
    assert!(Shed::shopts(|o| o.core.dotglob));
  }

  #[test]
  fn shopt_set_int() {
    let _g = TestGuard::new();
    test_input("shopt core.max_hist=500").unwrap();
    assert_eq!(Shed::shopts(|o| o.history.max_entries), 500);
  }

  #[test]
  fn shopt_set_string() {
    let _g = TestGuard::new();
    test_input("shopt prompt.leader=space").unwrap();
    assert_eq!(Shed::shopts(|o| o.prompt.leader.clone()), "space");
  }

  #[test]
  fn shopt_set_completion_ignore_case() {
    let _g = TestGuard::new();
    test_input("shopt prompt.completion_ignore_case=true").unwrap();
    assert!(Shed::shopts(|o| o.prompt.completion_ignore_case));
  }

  // ===================== Error cases =====================

  #[test]
  fn shopt_invalid_category() {
    let _g = TestGuard::new();
    test_input("shopt bogus.dotglob").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn shopt_invalid_option() {
    let _g = TestGuard::new();
    test_input("shopt core.nonexistent").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn shopt_invalid_value() {
    let _g = TestGuard::new();
    test_input("shopt core.dotglob=notabool").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  // ===================== Status =====================

  #[test]
  fn shopt_status_zero() {
    let _g = TestGuard::new();
    test_input("shopt core.autocd=true").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }
}
