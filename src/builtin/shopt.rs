use crate::{
  outln,
  state::write_shopts,
  util::{
    error::{ShResult, ShResultExt},
    with_status,
  },
};

pub(super) struct Shopt;
impl super::Builtin for Shopt {
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    if args.argv.is_empty() {
      let output = write_shopts(|s| s.display_opts())?;

      outln!("{output}")?;

      return with_status(0);
    }

    for (arg, span) in args.argv {
      let Some(output) = write_shopts(|s| s.query(&arg)).promote_err(span)? else {
        continue;
      };

      outln!("{output}")?;
    }

    with_status(0)
  }
}

#[cfg(test)]
mod tests {
  use crate::state::{self, read_shopts};
  use crate::testutil::{TestGuard, test_input};

  // ===================== Display =====================

  #[test]
  fn shopt_no_args_displays_all() {
    let guard = TestGuard::new();
    test_input("shopt").unwrap();
    let out = guard.read_output();
    assert!(out.contains("dotglob"));
    assert!(out.contains("autocd"));
    assert!(out.contains("max_hist"));
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
    assert!(read_shopts(|o| o.core.dotglob));
  }

  #[test]
  fn shopt_set_int() {
    let _g = TestGuard::new();
    test_input("shopt core.max_hist=500").unwrap();
    assert_eq!(read_shopts(|o| o.core.max_hist), 500);
  }

  #[test]
  fn shopt_set_string() {
    let _g = TestGuard::new();
    test_input("shopt prompt.leader=space").unwrap();
    assert_eq!(read_shopts(|o| o.prompt.leader.clone()), "space");
  }

  #[test]
  fn shopt_set_completion_ignore_case() {
    let _g = TestGuard::new();
    test_input("shopt prompt.completion_ignore_case=true").unwrap();
    assert!(read_shopts(|o| o.prompt.completion_ignore_case));
  }

  // ===================== Error cases =====================

  #[test]
  fn shopt_invalid_category() {
    let _g = TestGuard::new();
    test_input("shopt bogus.dotglob").ok();
    assert_ne!(state::get_status(), 0);
  }

  #[test]
  fn shopt_invalid_option() {
    let _g = TestGuard::new();
    test_input("shopt core.nonexistent").ok();
    assert_ne!(state::get_status(), 0);
  }

  #[test]
  fn shopt_invalid_value() {
    let _g = TestGuard::new();
    test_input("shopt core.dotglob=notabool").ok();
    assert_ne!(state::get_status(), 0);
  }

  // ===================== Status =====================

  #[test]
  fn shopt_status_zero() {
    let _g = TestGuard::new();
    test_input("shopt core.autocd=true").unwrap();
    assert_eq!(state::get_status(), 0);
  }
}
