use std::str::FromStr;

use crate::{
  eval::{
    execute,
    lex::{Span, Tk},
  },
  sherr,
  state::{
    self, Shed,
    meta::MetaTab,
    vars::{VarFlags, VarKind, VarStr},
  },
  util::{
    self,
    error::{ShErr, ShResult, ShResultExt},
  },
  var,
};

use super::opt::{Parsed, Word};

enum OptMatch {
  NoMatch,
  IsMatch,
  WantsArg,
}

#[derive(Debug)]
struct GetOptsSpec {
  silent_err: bool,
  // POSIX optstring, decoded to (option char, whether it takes an argument).
  // getopts has its own parser, so it needs only this, not the internal
  // `OptSpec` model (short/long/key/argc).
  opts: Vec<(char, bool)>,
}

impl GetOptsSpec {
  pub(crate) fn matches(&self, ch: char) -> OptMatch {
    match self.opts.iter().find(|(c, _)| *c == ch) {
      Some((_, true)) => OptMatch::WantsArg,
      Some((_, false)) => OptMatch::IsMatch,
      None => OptMatch::NoMatch,
    }
  }
}

impl FromStr for GetOptsSpec {
  type Err = ShErr;
  fn from_str(s: &str) -> Result<Self, Self::Err> {
    let mut s = s;
    let mut opts = vec![];
    let mut silent_err = false;
    if s.starts_with(':') {
      silent_err = true;
      s = &s[1..];
    }

    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.peek() {
      match ch {
        ch if ch.is_alphanumeric() => {
          let opt_ch = *ch;
          chars.next();
          let has_arg = chars.peek() == Some(&':');
          if has_arg {
            chars.next();
          }
          opts.push((opt_ch, has_arg));
        }
        _ => {
          return Err(sherr!(ParseErr, "unexpected character '{ch}'",));
        }
      }
    }

    Ok(GetOptsSpec { silent_err, opts })
  }
}

pub(super) struct GetOpts;
impl super::Builtin for GetOpts {
  /// getopts parses its own operands with POSIX semantics (OPTIND, clustered
  /// flags like `-ab`, attached args like `-bVALUE`, and `--`). The internal
  /// option parser would split those apart, so pass every word through as a
  /// plain argument and let `getopts_inner` do the parsing.
  fn get_argv_and_opts(&self, cmd_span: Span, argv: &[Tk], no_split: bool) -> ShResult<Parsed> {
    let expanded = execute::prepare_argv_with(argv, no_split).promote_err(cmd_span)?;
    let trace = expanded.iter().map(|(word, _)| word.clone()).collect();
    let words = expanded
      .into_iter()
      .map(|(word, span)| Word::Arg(word, span))
      .collect();
    Ok(Parsed { words, trace })
  }

  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let span = args.span();
    let mut arg_vec = args.arguments();

    let Some((arg_string, arg_span)) = arg_vec.next() else {
      return Err(sherr!(
          ExecFail @ span,
          "getopts: missing option spec",
      ));
    };
    let Some((opt_var, _)) = arg_vec.next() else {
      return Err(sherr!(
          ExecFail @ span,
          "getopts: missing variable name",
      ));
    };

    let opts_spec =
      GetOptsSpec::from_str(&arg_string.to_str_lossy()).promote_err(arg_span.clone())?;

    let explicit_args: Vec<VarStr> = arg_vec.map(|(word, _)| word.clone()).collect();
    if explicit_args.is_empty() {
      let pos_params: Vec<VarStr> = Shed::vars(|v| v.sh_argv().iter().skip(1).cloned().collect());
      getopts_inner(&opts_spec, &opt_var.to_str_lossy(), &pos_params, &span)
    } else {
      getopts_inner(&opts_spec, &opt_var.to_str_lossy(), &explicit_args, &span)
    }
  }
}

fn advance_optind(opt_index: usize, amount: usize) -> ShResult<()> {
  Shed::vars_mut(|v| {
    v.update_var(
      "OPTIND",
      VarKind::Str((opt_index + amount).to_string().into()),
    )
  })
}

fn getopts_inner(
  opts_spec: &GetOptsSpec,
  opt_var: &str,
  argv: &[VarStr],
  blame: &Span,
) -> ShResult<()> {
  let opt_index = var!("OPTIND").to_str_lossy().parse::<usize>().unwrap_or(1);
  // OPTIND is 1-based
  let arr_idx = opt_index.saturating_sub(1);

  let Some(arg) = argv.get(arr_idx) else {
    state::Shed::set_status(1);
    return Ok(());
  };
  // Option syntax (`-`, `--`, the flag chars) is ASCII, so parse over a lossy
  // view; the *argument value* (OPTARG) is pulled from the raw `arg` bytes below.
  let arg_str = arg.to_str_lossy();

  // "--" stops option processing
  if arg_str == "--" {
    advance_optind(opt_index, 1)?;
    Shed::meta_mut(MetaTab::reset_getopts_char_offset);
    return util::with_status(1);
  }

  // Not an option - done
  let Some(opt_str) = arg_str.strip_prefix('-') else {
    return util::with_status(1);
  };

  // Bare "-" is not an option
  if opt_str.is_empty() {
    return util::with_status(1);
  }

  let char_idx = Shed::meta(MetaTab::getopts_char_offset);
  let Some(ch) = opt_str.chars().nth(char_idx) else {
    // Ran out of chars in this arg (shouldn't normally happen),
    // advance to next arg and signal done for this call
    Shed::meta_mut(MetaTab::reset_getopts_char_offset);
    advance_optind(opt_index, 1)?;
    return util::with_status(1);
  };

  let last_char_in_arg = char_idx >= opt_str.len() - 1;

  // Advance past this character: either move to next char in this
  // arg, or reset offset and bump OPTIND to the next arg.
  let advance_one_char = |last: bool| -> ShResult<()> {
    if last {
      Shed::meta_mut(MetaTab::reset_getopts_char_offset);
      advance_optind(opt_index, 1)?;
    } else {
      Shed::meta_mut(MetaTab::inc_getopts_char_offset);
    }
    Ok(())
  };

  let _ = Shed::vars_mut(|v| v.unset_var("OPTARG"));

  match opts_spec.matches(ch) {
    OptMatch::NoMatch => {
      advance_one_char(last_char_in_arg)?;
      if opts_spec.silent_err {
        Shed::vars_mut(|v| v.set_var(opt_var, VarKind::Str("?".into()), VarFlags::empty()))?;
        Shed::vars_mut(|v| {
          v.set_var(
            "OPTARG",
            VarKind::Str(ch.to_string().into()),
            VarFlags::empty(),
          )
        })?;
      } else {
        Shed::vars_mut(|v| v.set_var(opt_var, VarKind::Str("?".into()), VarFlags::empty()))?;
        sherr!(
          ExecFail @ blame.clone(),
          "illegal option '-{ch}'",
        )
        .print_error();
      }
      state::Shed::set_status(0);
    }
    OptMatch::IsMatch => {
      advance_one_char(last_char_in_arg)?;
      Shed::vars_mut(|v| {
        v.set_var(
          opt_var,
          VarKind::Str(ch.to_string().into()),
          VarFlags::empty(),
        )
      })?;
      state::Shed::set_status(0);
    }
    OptMatch::WantsArg => {
      Shed::meta_mut(MetaTab::reset_getopts_char_offset);

      if !last_char_in_arg {
        // Remaining bytes in this arg are the argument: -bVALUE. The option
        // syntax up to here (`-` + flag chars) is single-byte ASCII, so the
        // value begins at byte offset `char_idx + 2` (past `-` and the flag).
        let optarg = VarStr::from(&arg.as_bytes()[char_idx + 2..]);
        Shed::vars_mut(|v| v.set_var("OPTARG", VarKind::string(optarg), VarFlags::empty()))?;
        advance_optind(opt_index, 1)?;
      } else if let Some(next_arg) = argv.get(arr_idx + 1) {
        // Next arg is the argument
        Shed::vars_mut(|v| {
          v.set_var(
            "OPTARG",
            VarKind::string(next_arg.clone()),
            VarFlags::empty(),
          )
        })?;
        // Skip both the option arg and its value
        advance_optind(opt_index, 2)?;
      } else {
        // Missing required argument
        if opts_spec.silent_err {
          Shed::vars_mut(|v| v.set_var(opt_var, VarKind::Str(":".into()), VarFlags::empty()))?;
          Shed::vars_mut(|v| {
            v.set_var(
              "OPTARG",
              VarKind::Str(ch.to_string().into()),
              VarFlags::empty(),
            )
          })?;
        } else {
          Shed::vars_mut(|v| v.set_var(opt_var, VarKind::Str("?".into()), VarFlags::empty()))?;
          sherr!(
            ExecFail @ blame.clone(),
            "option '-{ch}' requires an argument",
          )
          .print_error();
        }
        advance_optind(opt_index, 1)?;
        return util::with_status(0);
      }

      Shed::vars_mut(|v| {
        v.set_var(
          opt_var,
          VarKind::Str(ch.to_string().into()),
          VarFlags::empty(),
        )
      })?;
    }
  }

  util::with_status(0)
}

#[cfg(test)]
mod tests {
  use super::var;
  use crate::state;
  use crate::tests::testutil::{TestGuard, test_input};

  // ===================== Spec parsing =====================

  #[test]
  fn parse_simple_spec() {
    use super::GetOptsSpec;
    use std::str::FromStr;
    let spec = GetOptsSpec::from_str("abc").unwrap();
    assert!(!spec.silent_err);
    assert_eq!(spec.opts.len(), 3);
  }

  #[test]
  fn parse_spec_with_args() {
    use super::GetOptsSpec;
    use std::str::FromStr;
    let spec = GetOptsSpec::from_str("a:bc:").unwrap();
    assert!(!spec.silent_err);
    assert!(spec.opts[0].1); // a: takes an arg
    assert!(!spec.opts[1].1); // b does not
    assert!(spec.opts[2].1); // c: takes an arg
  }

  #[test]
  fn parse_silent_spec() {
    use super::GetOptsSpec;
    use std::str::FromStr;
    let spec = GetOptsSpec::from_str(":ab").unwrap();
    assert!(spec.silent_err);
    assert_eq!(spec.opts.len(), 2);
  }

  #[test]
  fn parse_invalid_char() {
    use super::GetOptsSpec;
    use std::str::FromStr;
    let result = GetOptsSpec::from_str("a@b");
    assert!(result.is_err());
  }

  // ===================== Basic option matching =====================

  #[test]
  fn getopts_simple_flag() {
    let _g = TestGuard::new();
    test_input("getopts ab opt -a").unwrap();
    assert_eq!(var!("opt"), "a");
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn getopts_second_flag() {
    let _g = TestGuard::new();
    test_input("getopts ab opt -b").unwrap();
    assert_eq!(var!("opt"), "b");
  }

  // ===================== Option with argument =====================

  #[test]
  fn getopts_option_with_separate_arg() {
    let _g = TestGuard::new();
    test_input("getopts a: opt -a value").unwrap();
    assert_eq!(var!("opt"), "a");
    assert_eq!(var!("OPTARG"), "value");
  }

  #[test]
  fn getopts_option_with_attached_arg() {
    let _g = TestGuard::new();
    test_input("getopts a: opt -avalue").unwrap();
    assert_eq!(var!("opt"), "a");
    assert_eq!(var!("OPTARG"), "value");
  }

  #[test]
  fn getopts_optarg_preserves_non_utf8_bytes() {
    // OPTARG must hold raw bytes, both for an attached (`-aVAL`) and a
    // separate (`-a VAL`) argument.
    let _g = TestGuard::new();
    test_input(r#"set -- "-a$(printf 'v\377w')"; getopts a: o"#).unwrap();
    assert_eq!(super::var!("OPTARG").as_bytes(), &b"v\xffw"[..]);

    test_input(r#"OPTIND=1; set -- -a "$(printf 'v\377w')"; getopts a: o"#).unwrap();
    assert_eq!(super::var!("OPTARG").as_bytes(), &b"v\xffw"[..]);
  }

  // ===================== Bundled options =====================

  #[test]
  fn getopts_bundled_flags() {
    let _g = TestGuard::new();

    // First call gets 'a' from -ab
    test_input("getopts abc opt -ab").unwrap();
    assert_eq!(var!("opt"), "a");

    // Second call gets 'b' from same -ab
    test_input("getopts abc opt -ab").unwrap();
    assert_eq!(var!("opt"), "b");
  }

  // ===================== OPTIND advancement =====================

  #[test]
  fn getopts_advances_optind() {
    let _g = TestGuard::new();
    test_input("getopts ab opt -a").unwrap();

    let optind: usize = var!("OPTIND").to_str_lossy().parse().unwrap();
    assert_eq!(optind, 2); // Advanced past -a
  }

  #[test]
  fn getopts_arg_option_advances_by_two() {
    let _g = TestGuard::new();
    test_input("getopts a: opt -a val").unwrap();

    let optind: usize = var!("OPTIND").to_str_lossy().parse().unwrap();
    assert_eq!(optind, 3); // Advanced past both -a and val
  }

  #[test]
  fn optind_scoped_to_function_call() {
    // Each function call starts with a fresh OPTIND=1, set via
    // VarFlags::LOCAL inside exec_func. Modifications inside the
    // function's while-getopts loop are visible throughout the
    // function body (survive the loop scope), but vanish when the
    // function returns, restoring the caller's OPTIND. This is more
    // user-friendly than bash's "global by default; remember to
    // local OPTIND yourself" convention.
    let g = TestGuard::new();
    test_input(
      r#"
			func() {
				while getopts ab opt; do
					echo "opt: $opt, OPTIND: $OPTIND"
				done
			}

			func -a -b
			echo OPTIND: $OPTIND
		"#,
    )
    .unwrap();

    let output = g.read_output();
    assert_eq!(output, "opt: a, OPTIND: 2\nopt: b, OPTIND: 3\nOPTIND: 1\n");
  }

  #[test]
  fn optind_fresh_for_each_function_call() {
    // Pipeline-style use case: multiple functions each parse their
    // own args via getopts. Each function should see OPTIND=1 at
    // entry, regardless of what previous getopts calls left behind.
    let g = TestGuard::new();
    test_input(
      r#"
			outer() {
				while getopts ab opt; do :; done
				echo "outer after: $OPTIND"
				inner -x
				echo "outer after inner: $OPTIND"
			}
			inner() {
				echo "inner entry: $OPTIND"
				while getopts x opt; do :; done
				echo "inner after: $OPTIND"
			}
			outer -a -b
		"#,
    )
    .unwrap();

    let output = g.read_output();
    assert_eq!(
      output,
      "outer after: 3\ninner entry: 1\ninner after: 2\nouter after inner: 3\n"
    );
  }

  #[test]
  fn optind_survives_loop_scope_within_function() {
    // The specific regression case from the bug report: OPTIND must be
    // correct AFTER the while-getopts loop exits but BEFORE the function
    // returns. The old VarFlags::LOCAL behavior made it revert to 1 the
    // moment the loop body's scope frame was popped.
    let g = TestGuard::new();
    test_input(
      r#"
			func() {
				while getopts abcd opt; do :; done
				echo "after loop: $OPTIND"
			}
			func -a -b -c -d
		"#,
    )
    .unwrap();

    let output = g.read_output();
    assert_eq!(output.trim(), "after loop: 5");
  }

  // ===================== Multiple calls (loop simulation) =====================

  #[test]
  fn getopts_multiple_separate_args() {
    let _g = TestGuard::new();

    test_input("getopts ab opt -a -b").unwrap();
    assert_eq!(var!("opt"), "a");
    assert_eq!(state::Shed::get_status(), 0);

    test_input("getopts ab opt -a -b").unwrap();
    assert_eq!(var!("opt"), "b");
    assert_eq!(state::Shed::get_status(), 0);

    // Third call: no more options
    test_input("getopts ab opt -a -b").unwrap();
    assert_eq!(state::Shed::get_status(), 1);
  }

  // ===================== End of options =====================

  #[test]
  fn getopts_no_options_returns_1() {
    let _g = TestGuard::new();
    test_input("getopts ab opt foo").unwrap();
    assert_eq!(state::Shed::get_status(), 1);
  }

  #[test]
  fn getopts_double_dash_stops() {
    let _g = TestGuard::new();
    test_input("getopts ab opt -- -a").unwrap();
    assert_eq!(state::Shed::get_status(), 1);
  }

  #[test]
  fn getopts_bare_dash_stops() {
    let _g = TestGuard::new();
    test_input("getopts ab opt -").unwrap();
    assert_eq!(state::Shed::get_status(), 1);
  }

  // ===================== Unknown option =====================

  #[test]
  fn getopts_unknown_option() {
    let _g = TestGuard::new();
    test_input("getopts ab opt -z").unwrap();
    assert_eq!(var!("opt"), "?");
    assert_eq!(state::Shed::get_status(), 0);
  }

  // ===================== Silent error mode =====================

  #[test]
  fn getopts_silent_unknown_sets_optarg() {
    let _g = TestGuard::new();
    test_input("getopts :ab opt -z").unwrap();
    assert_eq!(var!("opt"), "?");
    assert_eq!(var!("OPTARG"), "z");
  }

  #[test]
  fn getopts_silent_missing_arg() {
    let _g = TestGuard::new();
    test_input("getopts :a: opt -a").unwrap();
    assert_eq!(var!("opt"), ":");
    assert_eq!(var!("OPTARG"), "a");
  }

  // ===================== Missing required argument (non-silent) =====================

  #[test]
  fn getopts_missing_arg_non_silent() {
    let _g = TestGuard::new();
    test_input("getopts a: opt -a").unwrap();
    assert_eq!(var!("opt"), "?");
  }

  // ===================== Error cases =====================

  #[test]
  fn getopts_optarg_cleared_for_flag_after_arg_option() {
    // Regression: an option taking an argument (-b val) set OPTARG, and a
    // following no-arg option (-c) left the stale value in place.
    let g = TestGuard::new();
    test_input(r#"set -- -b val -c; while getopts "b:c" o; do echo "$o=${OPTARG-}"; done"#)
      .unwrap();
    let out = g.read_output();
    assert_eq!(out, "b=val\nc=\n");
  }

  #[test]
  fn getopts_missing_spec() {
    let _g = TestGuard::new();
    test_input("getopts").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn getopts_missing_varname() {
    let _g = TestGuard::new();
    test_input("getopts ab").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }
}
