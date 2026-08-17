use std::str::FromStr;

use crate::opt;

use super::{
  ShResult, Shed, expand::markers, join_raw_args, match_loop, opt::OptSpec, sherr, try_var,
  util::stylize_loglevel, var, with_status,
};

pub struct Flog;
impl super::Builtin for Flog {
  fn opts(&self) -> Vec<OptSpec> {
    vec![opt!("prefix" | 'p', 1)]
  }
  fn execute(&self, mut args: super::BuiltinArgs) -> ShResult<()> {
    let span = args.span();
    let source = span.span_source().name();
    let (line, col) = span.line_and_col();

    let (arg_vec, opts) = args.take_argv();

    let Some((first, span)) = arg_vec.first() else {
      return Err(sherr!(ExecFail, "Usage: flog <LEVEL> <MESSAGE>"));
    };
    let level = first.to_ascii_uppercase();
    let Some(level) = log::Level::from_str(&String::from_utf8_lossy(&level)).ok() else {
      return Err(sherr!(ExecFail @ span.clone(), "Invalid log level"));
    };

    let cur_level = Self::get_log_level().unwrap_or(log::Level::Error);
    if level > cur_level {
      return with_status(0);
    }

    let level = stylize_loglevel(level);

    let mut prefix_fmt = try_var!("FLOG_FMT").unwrap_or_else(|| "[{level}]".into());

    for opt in opts {
      if opt.key() == "prefix" {
        let prefix = opt.value()?;
        prefix_fmt = prefix.into();
      }
    }

    let (rest, _) = join_raw_args(arg_vec);
    let formatted = Self::expand_prefix_fmt(&prefix_fmt.to_str_lossy(), &level, source, line, col);

    let out = format!("{formatted} {rest}");

    Shed::post_system_msg(out);

    with_status(0)
  }
}

impl Flog {
  fn expand_prefix_fmt(fmt: &str, level: &str, source: &str, line: usize, col: usize) -> String {
    let mut chars = fmt.chars();
    let mut out = String::new();
    match_loop!(chars.next() => ch, {
      '\\' => {
        out.push(ch);
        if let Some(next_ch) = chars.next() {
          out.push(next_ch);
        }
      }
      '{' => {
        let mut fmt_arg = String::new();

        match_loop!(chars.next() => ch, {
          '}' => break,
          _ => fmt_arg.push(ch),
        });

        match fmt_arg.as_str() {
          "level" => out.push_str(level),
          "line" => out.push_str(&line.to_string()),
          "col" => out.push_str(&col.to_string()),
          "source" => {
            let source = source.replace('%', &format!("{}",markers::ESCAPE));
            out.push_str(&source);
          }
          _ => out.push_str(&fmt_arg),
        }
      }
      _ => out.push(ch),
    });

    out = chrono::Local::now()
      .format(&out)
      .to_string()
      .replace(markers::ESCAPE, "%");

    out
  }

  fn get_log_level() -> Option<log::Level> {
    let level = var!("FLOG_LEVEL").to_ascii_uppercase();
    String::from_utf8_lossy(&level).parse::<log::Level>().ok()
  }
}

#[cfg(test)]
mod flog_execute_tests {
  use crate::state;
  use crate::state::Shed;
  use crate::state::vars::{VarFlags, VarKind};
  use crate::tests::testutil::{TestGuard, test_input};

  /// Empty the `system_msg` queue so each test sees a clean slate.
  fn drain_system_msgs() {
    while state::Shed::pop_system_msg().is_some() {}
  }

  fn set_var(name: &str, val: &str) {
    Shed::vars_mut(|v| {
      v.set_var(name, VarKind::Str(val.into()), VarFlags::empty())
        .unwrap();
    });
  }

  /// Pop and concatenate all pending system messages.
  fn collect_system_msgs() -> String {
    let mut out = String::new();
    while let Some(m) = state::Shed::pop_system_msg() {
      out.push_str(&m);
      out.push('\n');
    }
    out
  }

  #[test]
  fn flog_no_args_errors() {
    let _g = TestGuard::new();
    drain_system_msgs();
    test_input("flog").unwrap();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn flog_invalid_level_errors() {
    let _g = TestGuard::new();
    drain_system_msgs();
    test_input("flog NOTALEVEL hello").unwrap();
    assert_ne!(state::Shed::get_status(), 0);
  }

  #[test]
  fn flog_level_above_threshold_is_silent() {
    // Default FLOG_LEVEL is unset → fallback Error. Info > Error, so
    // an info message must be suppressed and produce no system msg.
    let _g = TestGuard::new();
    drain_system_msgs();
    // Make sure FLOG_LEVEL is not set to something that would let info through.
    Shed::vars_mut(|v| v.unset_var("FLOG_LEVEL").ok());
    test_input("flog INFO suppressed_message_xyz").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
    let msgs = collect_system_msgs();
    assert!(!msgs.contains("suppressed_message_xyz"), "got: {msgs:?}");
  }

  #[test]
  fn flog_level_at_threshold_emits_message() {
    let g = TestGuard::new();
    set_var("FLOG_LEVEL", "DEBUG");
    test_input("flog INFO visible_info_message").unwrap();
    let out = g.read_output();
    assert!(out.contains("visible_info_message"), "got: {out:?}");
  }

  #[test]
  fn flog_p_flag_overrides_default_prefix() {
    let g = TestGuard::new();
    set_var("FLOG_LEVEL", "DEBUG");
    test_input("flog -p 'CUSTOM_TAG' INFO body_text").unwrap();
    let out = g.read_output();
    assert!(out.contains("CUSTOM_TAG"), "got: {out:?}");
    assert!(out.contains("body_text"), "got: {out:?}");
  }

  #[test]
  fn flog_long_prefix_flag_overrides_default_prefix() {
    let g = TestGuard::new();
    set_var("FLOG_LEVEL", "DEBUG");
    test_input("flog --prefix 'LONG_TAG' INFO body_text2").unwrap();
    let out = g.read_output();
    assert!(out.contains("LONG_TAG"), "got: {out:?}");
    assert!(out.contains("body_text2"), "got: {out:?}");
  }

  #[test]
  fn flog_default_prefix_contains_level_token() {
    let g = TestGuard::new();
    set_var("FLOG_LEVEL", "DEBUG");
    Shed::vars_mut(|v| v.unset_var("FLOG_FMT").ok());
    test_input("flog INFO check_default_prefix").unwrap();
    let out = g.read_output();
    // Default fmt is "[{level}] …" — at minimum the level name appears.
    assert!(out.contains("INFO"), "got: {out:?}");
    assert!(out.contains("check_default_prefix"), "got: {out:?}");
  }
}
