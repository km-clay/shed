use std::str::FromStr;

use ariadne::Fmt;

use crate::{
  getopt::{Opt, OptArg, OptSpec},
  libsh::error::{ShErr, ShResult, ShResultExt, next_color},
  parse::{NdRule, Node, execute::prepare_argv, lex::Span},
  sherr,
  state::{self, VarFlags, VarKind, read_meta, read_vars, write_meta, write_vars},
};

enum OptMatch {
  NoMatch,
  IsMatch,
  WantsArg,
}

struct GetOptsSpec {
  silent_err: bool,
  opt_specs: Vec<OptSpec>,
}

impl GetOptsSpec {
  pub fn matches(&self, ch: char) -> OptMatch {
    for spec in &self.opt_specs {
      let OptSpec { opt, takes_arg } = spec;
      match opt {
        Opt::Short(opt_ch) if ch == *opt_ch => {
          if *takes_arg != OptArg::None {
            return OptMatch::WantsArg;
          } else {
            return OptMatch::IsMatch;
          }
        }
        _ => continue,
      }
    }
    OptMatch::NoMatch
  }
}

impl FromStr for GetOptsSpec {
  type Err = ShErr;
  fn from_str(s: &str) -> Result<Self, Self::Err> {
    let mut s = s;
    let mut opt_specs = vec![];
    let mut silent_err = false;
    if s.starts_with(':') {
      silent_err = true;
      s = &s[1..];
    }

    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.peek() {
      match ch {
        ch if ch.is_alphanumeric() => {
          let opt = Opt::Short(*ch);
          chars.next();
          let has_arg = chars.peek() == Some(&':');
          if has_arg {
            chars.next();
          }
          let takes_arg = if has_arg {
            OptArg::Single
          } else {
            OptArg::None
          };
          opt_specs.push(OptSpec { opt, takes_arg })
        }
        _ => {
          return Err(sherr!(
            ParseErr,
            "unexpected character '{}'",
            ch.fg(next_color()),
          ));
        }
      }
    }

    Ok(GetOptsSpec {
      silent_err,
      opt_specs,
    })
  }
}

fn advance_optind(opt_index: usize, amount: usize) -> ShResult<()> {
  write_vars(|v| {
    v.set_var(
      "OPTIND",
      VarKind::Str((opt_index + amount).to_string()),
      VarFlags::NONE,
    )
  })
}

fn getopts_inner(
  opts_spec: &GetOptsSpec,
  opt_var: &str,
  argv: &[String],
  blame: Span,
) -> ShResult<()> {
  let opt_index = read_vars(|v| v.get_var("OPTIND").parse::<usize>().unwrap_or(1));
  // OPTIND is 1-based
  let arr_idx = opt_index.saturating_sub(1);

  let Some(arg) = argv.get(arr_idx) else {
    state::set_status(1);
    return Ok(());
  };

  // "--" stops option processing
  if arg.as_str() == "--" {
    advance_optind(opt_index, 1)?;
    write_meta(|m| m.reset_getopts_char_offset());
    state::set_status(1);
    return Ok(());
  }

  // Not an option — done
  let Some(opt_str) = arg.strip_prefix('-') else {
    state::set_status(1);
    return Ok(());
  };

  // Bare "-" is not an option
  if opt_str.is_empty() {
    state::set_status(1);
    return Ok(());
  }

  let char_idx = read_meta(|m| m.getopts_char_offset());
  let Some(ch) = opt_str.chars().nth(char_idx) else {
    // Ran out of chars in this arg (shouldn't normally happen),
    // advance to next arg and signal done for this call
    write_meta(|m| m.reset_getopts_char_offset());
    advance_optind(opt_index, 1)?;
    state::set_status(1);
    return Ok(());
  };

  let last_char_in_arg = char_idx >= opt_str.len() - 1;

  // Advance past this character: either move to next char in this
  // arg, or reset offset and bump OPTIND to the next arg.
  let advance_one_char = |last: bool| -> ShResult<()> {
    if last {
      write_meta(|m| m.reset_getopts_char_offset());
      advance_optind(opt_index, 1)?;
    } else {
      write_meta(|m| m.inc_getopts_char_offset());
    }
    Ok(())
  };

  match opts_spec.matches(ch) {
    OptMatch::NoMatch => {
      advance_one_char(last_char_in_arg)?;
      if opts_spec.silent_err {
        write_vars(|v| v.set_var(opt_var, VarKind::Str("?".into()), VarFlags::NONE))?;
        write_vars(|v| v.set_var("OPTARG", VarKind::Str(ch.to_string()), VarFlags::NONE))?;
      } else {
        write_vars(|v| v.set_var(opt_var, VarKind::Str("?".into()), VarFlags::NONE))?;
        sherr!(
          ExecFail @ blame.clone(),
          "illegal option '-{}'", ch.fg(next_color()),
        )
        .print_error();
      }
      state::set_status(0);
    }
    OptMatch::IsMatch => {
      advance_one_char(last_char_in_arg)?;
      write_vars(|v| v.set_var(opt_var, VarKind::Str(ch.to_string()), VarFlags::NONE))?;
      state::set_status(0);
    }
    OptMatch::WantsArg => {
      write_meta(|m| m.reset_getopts_char_offset());

      if !last_char_in_arg {
        // Remaining chars in this arg are the argument: -bVALUE
        let optarg: String = opt_str.chars().skip(char_idx + 1).collect();
        write_vars(|v| v.set_var("OPTARG", VarKind::Str(optarg), VarFlags::NONE))?;
        advance_optind(opt_index, 1)?;
      } else if let Some(next_arg) = argv.get(arr_idx + 1) {
        // Next arg is the argument
        write_vars(|v| v.set_var("OPTARG", VarKind::Str(next_arg.clone()), VarFlags::NONE))?;
        // Skip both the option arg and its value
        advance_optind(opt_index, 2)?;
      } else {
        // Missing required argument
        if opts_spec.silent_err {
          write_vars(|v| v.set_var(opt_var, VarKind::Str(":".into()), VarFlags::NONE))?;
          write_vars(|v| v.set_var("OPTARG", VarKind::Str(ch.to_string()), VarFlags::NONE))?;
        } else {
          write_vars(|v| v.set_var(opt_var, VarKind::Str("?".into()), VarFlags::NONE))?;
          sherr!(
            ExecFail @ blame.clone(),
            "option '-{}' requires an argument", ch.fg(next_color()),
          )
          .print_error();
        }
        advance_optind(opt_index, 1)?;
        state::set_status(0);
        return Ok(());
      }

      write_vars(|v| v.set_var(opt_var, VarKind::Str(ch.to_string()), VarFlags::NONE))?;
      state::set_status(0);
    }
  }

  Ok(())
}

pub fn getopts(node: Node) -> ShResult<()> {
  let span = node.get_span().clone();
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  let mut argv = prepare_argv(argv)?;
  if !argv.is_empty() {
    argv.remove(0);
  }
  let mut args = argv.into_iter();

  let Some(arg_string) = args.next() else {
    return Err(sherr!(
      ExecFail @ span,
      "getopts: missing option spec",
    ));
  };
  let Some(opt_var) = args.next() else {
    return Err(sherr!(
      ExecFail @ span,
      "getopts: missing variable name",
    ));
  };

  let opts_spec = GetOptsSpec::from_str(&arg_string.0).promote_err(arg_string.1.clone())?;

  let explicit_args: Vec<String> = args.map(|s| s.0).collect();

  if !explicit_args.is_empty() {
    getopts_inner(&opts_spec, &opt_var.0, &explicit_args, span)
  } else {
    let pos_params: Vec<String> = read_vars(|v| v.sh_argv().iter().skip(1).cloned().collect());
    getopts_inner(&opts_spec, &opt_var.0, &pos_params, span)
  }
}

#[cfg(test)]
mod tests {
  use crate::getopt::OptArg;
  use crate::state::{self, read_vars};
  use crate::testutil::{TestGuard, test_input};

  fn get_var(name: &str) -> String {
    read_vars(|v| v.get_var(name))
  }

  // ===================== Spec parsing =====================

  #[test]
  fn parse_simple_spec() {
    use super::GetOptsSpec;
    use std::str::FromStr;
    let spec = GetOptsSpec::from_str("abc").unwrap();
    assert!(!spec.silent_err);
    assert_eq!(spec.opt_specs.len(), 3);
  }

  #[test]
  fn parse_spec_with_args() {
    use super::GetOptsSpec;
    use std::str::FromStr;
    let spec = GetOptsSpec::from_str("a:bc:").unwrap();
    assert!(!spec.silent_err);
    assert_eq!(spec.opt_specs[0].takes_arg, OptArg::Single); // a:
    assert_eq!(spec.opt_specs[1].takes_arg, OptArg::None); // b
    assert_eq!(spec.opt_specs[2].takes_arg, OptArg::Single); // c:
  }

  #[test]
  fn parse_silent_spec() {
    use super::GetOptsSpec;
    use std::str::FromStr;
    let spec = GetOptsSpec::from_str(":ab").unwrap();
    assert!(spec.silent_err);
    assert_eq!(spec.opt_specs.len(), 2);
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
    assert_eq!(get_var("opt"), "a");
    assert_eq!(state::get_status(), 0);
  }

  #[test]
  fn getopts_second_flag() {
    let _g = TestGuard::new();
    test_input("getopts ab opt -b").unwrap();
    assert_eq!(get_var("opt"), "b");
  }

  // ===================== Option with argument =====================

  #[test]
  fn getopts_option_with_separate_arg() {
    let _g = TestGuard::new();
    test_input("getopts a: opt -a value").unwrap();
    assert_eq!(get_var("opt"), "a");
    assert_eq!(get_var("OPTARG"), "value");
  }

  #[test]
  fn getopts_option_with_attached_arg() {
    let _g = TestGuard::new();
    test_input("getopts a: opt -avalue").unwrap();
    assert_eq!(get_var("opt"), "a");
    assert_eq!(get_var("OPTARG"), "value");
  }

  // ===================== Bundled options =====================

  #[test]
  fn getopts_bundled_flags() {
    let _g = TestGuard::new();

    // First call gets 'a' from -ab
    test_input("getopts abc opt -ab").unwrap();
    assert_eq!(get_var("opt"), "a");

    // Second call gets 'b' from same -ab
    test_input("getopts abc opt -ab").unwrap();
    assert_eq!(get_var("opt"), "b");
  }

  // ===================== OPTIND advancement =====================

  #[test]
  fn getopts_advances_optind() {
    let _g = TestGuard::new();
    test_input("getopts ab opt -a").unwrap();

    let optind: usize = get_var("OPTIND").parse().unwrap();
    assert_eq!(optind, 2); // Advanced past -a
  }

  #[test]
  fn getopts_arg_option_advances_by_two() {
    let _g = TestGuard::new();
    test_input("getopts a: opt -a val").unwrap();

    let optind: usize = get_var("OPTIND").parse().unwrap();
    assert_eq!(optind, 3); // Advanced past both -a and val
  }

  // ===================== Multiple calls (loop simulation) =====================

  #[test]
  fn getopts_multiple_separate_args() {
    let _g = TestGuard::new();

    test_input("getopts ab opt -a -b").unwrap();
    assert_eq!(get_var("opt"), "a");
    assert_eq!(state::get_status(), 0);

    test_input("getopts ab opt -a -b").unwrap();
    assert_eq!(get_var("opt"), "b");
    assert_eq!(state::get_status(), 0);

    // Third call: no more options
    test_input("getopts ab opt -a -b").unwrap();
    assert_eq!(state::get_status(), 1);
  }

  // ===================== End of options =====================

  #[test]
  fn getopts_no_options_returns_1() {
    let _g = TestGuard::new();
    test_input("getopts ab opt foo").unwrap();
    assert_eq!(state::get_status(), 1);
  }

  #[test]
  fn getopts_double_dash_stops() {
    let _g = TestGuard::new();
    test_input("getopts ab opt -- -a").unwrap();
    assert_eq!(state::get_status(), 1);
  }

  #[test]
  fn getopts_bare_dash_stops() {
    let _g = TestGuard::new();
    test_input("getopts ab opt -").unwrap();
    assert_eq!(state::get_status(), 1);
  }

  // ===================== Unknown option =====================

  #[test]
  fn getopts_unknown_option() {
    let _g = TestGuard::new();
    test_input("getopts ab opt -z").unwrap();
    assert_eq!(get_var("opt"), "?");
    assert_eq!(state::get_status(), 0);
  }

  // ===================== Silent error mode =====================

  #[test]
  fn getopts_silent_unknown_sets_optarg() {
    let _g = TestGuard::new();
    test_input("getopts :ab opt -z").unwrap();
    assert_eq!(get_var("opt"), "?");
    assert_eq!(get_var("OPTARG"), "z");
  }

  #[test]
  fn getopts_silent_missing_arg() {
    let _g = TestGuard::new();
    test_input("getopts :a: opt -a").unwrap();
    assert_eq!(get_var("opt"), ":");
    assert_eq!(get_var("OPTARG"), "a");
  }

  // ===================== Missing required argument (non-silent) =====================

  #[test]
  fn getopts_missing_arg_non_silent() {
    let _g = TestGuard::new();
    test_input("getopts a: opt -a").unwrap();
    assert_eq!(get_var("opt"), "?");
  }

  // ===================== Error cases =====================

  #[test]
  fn getopts_missing_spec() {
    let _g = TestGuard::new();
    let result = test_input("getopts");
    assert!(result.is_err());
  }

  #[test]
  fn getopts_missing_varname() {
    let _g = TestGuard::new();
    let result = test_input("getopts ab");
    assert!(result.is_err());
  }
}
