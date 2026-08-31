use std::{fmt::Write, iter::Peekable, str::FromStr};

use unicode_width::UnicodeWidthStr;

use crate::{
  eval::{
    execute,
    lex::{Span, Tk},
  },
  expand::escape,
  outln, procio, sherr,
  state::{
    Shed,
    scopes::ScopeStack,
    shopt::ShOptSet,
    vars::{ValueBytes, VarKind, VarStr, VarStrSliceExt},
  },
  util::{
    self,
    error::{ShErr, ShResult, ShResultExt},
  },
};

use super::opt::Parsed;
use bitflags::bitflags;

bitflags! {
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
  pub(crate) struct SetFlags: u32 {
    const ALLEXPORT = 1 << 0;
    const ERREXIT = 1 << 1;
    const IGNORE_EOF = 1 << 2;
    const MONITOR = 1 << 3;
    const NO_CLOBBER = 1 << 4;
    const NO_GLOB = 1 << 5;
    const NO_EXEC = 1 << 6;
    const NO_LOG = 1 << 7;
    const NOTIFY = 1 << 8;
    const NO_UNSET = 1 << 9;
    const VERBOSE = 1 << 10;
    const VI_MODE = 1 << 11;
    const XTRACE = 1 << 12;
    const HASHALL = 1 << 13;
    const EMACS_MODE = 1 << 14;
    const PIPEFAIL = 1 << 15;
  }
}

impl SetFlags {
  pub(crate) fn get_shopt_fields(self) -> Vec<String> {
    let mut fields = vec![];
    for flag in self {
      let opt = match flag {
        _ if flag == SetFlags::ERREXIT => "errexit",
        _ if flag == SetFlags::ALLEXPORT => "allexport",
        _ if flag == SetFlags::IGNORE_EOF => "ignoreeof",
        _ if flag == SetFlags::MONITOR => "monitor",
        _ if flag == SetFlags::NO_CLOBBER => "noclobber",
        _ if flag == SetFlags::NO_GLOB => "noglob",
        _ if flag == SetFlags::NO_EXEC => "noexec",
        _ if flag == SetFlags::NO_LOG => "nolog",
        _ if flag == SetFlags::NOTIFY => "notify",
        _ if flag == SetFlags::NO_UNSET => "nounset",
        _ if flag == SetFlags::VERBOSE => "verbose",
        _ if flag == SetFlags::VI_MODE => "vi",
        _ if flag == SetFlags::EMACS_MODE => "emacs",
        _ if flag == SetFlags::XTRACE => "xtrace",
        _ if flag == SetFlags::HASHALL => "hashall",
        _ if flag == SetFlags::PIPEFAIL => "pipefail",
        _ => continue,
      };
      fields.push(opt.to_string());
    }
    fields
  }

  pub(crate) fn as_char(self) -> Option<char> {
    match self {
      _ if self == Self::ALLEXPORT => Some('a'),
      _ if self == Self::NOTIFY => Some('b'),
      _ if self == Self::NO_CLOBBER => Some('C'),
      _ if self == Self::ERREXIT => Some('e'),
      _ if self == Self::NO_GLOB => Some('f'),
      _ if self == Self::HASHALL => Some('h'),
      _ if self == Self::MONITOR => Some('m'),
      _ if self == Self::NO_EXEC => Some('n'),
      _ if self == Self::NO_UNSET => Some('u'),
      _ if self == Self::VERBOSE => Some('v'),
      _ if self == Self::XTRACE => Some('x'),
      _ => None,
    }
  }
}

impl TryFrom<char> for SetFlags {
  type Error = ShErr;
  fn try_from(value: char) -> Result<Self, Self::Error> {
    // set flags:
    // -abCefhmnuvx
    match value {
      'a' => Ok(Self::ALLEXPORT),
      'b' => Ok(Self::NOTIFY),
      'C' => Ok(Self::NO_CLOBBER),
      'e' => Ok(Self::ERREXIT),
      'f' => Ok(Self::NO_GLOB),
      'h' => Ok(Self::HASHALL),
      'm' => Ok(Self::MONITOR),
      'n' => Ok(Self::NO_EXEC),
      'u' => Ok(Self::NO_UNSET),
      'v' => Ok(Self::VERBOSE),
      'x' => Ok(Self::XTRACE),
      _ => Err(sherr!(ParseErr, "invalid option: {}", value,)),
    }
  }
}

impl FromStr for SetFlags {
  type Err = ShErr;
  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s {
      "ignoreeof" => Ok(Self::IGNORE_EOF),
      "vi" => Ok(Self::VI_MODE),
      "emacs" => Ok(Self::EMACS_MODE),
      "allexport" => Ok(Self::ALLEXPORT),
      "notify" => Ok(Self::NOTIFY),
      "noclobber" => Ok(Self::NO_CLOBBER),
      "errexit" => Ok(Self::ERREXIT),
      "noglob" => Ok(Self::NO_GLOB),
      "hashall" => Ok(Self::HASHALL),
      "pipefail" => Ok(Self::PIPEFAIL),
      "monitor" => Ok(Self::MONITOR),
      "noexec" => Ok(Self::NO_EXEC),
      "nounset" => Ok(Self::NO_UNSET),
      "nolog" => Ok(Self::NO_LOG),
      "verbose" => Ok(Self::VERBOSE),
      "xtrace" => Ok(Self::XTRACE),
      _ => Err(sherr!(ParseErr, "invalid option: {}", s,)),
    }
  }
}

pub(crate) fn build_set_call(readable: bool) -> String {
  // I hope you like iterators :)

  let opts = Shed::shopts_mut(|o| o.query("set").unwrap().unwrap());
  if readable {
    let mut longest_width: usize = 0;
    let lines = opts
      .lines()
      .map(|l| {
        l.split_once('=')
          .map(|(l, r)| {
            (
              l.strip_prefix("set.").unwrap().to_string(),
              if r.parse::<bool>().unwrap() {
                "on".to_string()
              } else {
                "off".to_string()
              },
            )
          })
          .unwrap()
      })
      .collect::<Vec<_>>();

    for (opt, _) in &lines {
      if opt.width() > longest_width {
        longest_width = opt.width();
      }
    }

    lines
      .into_iter()
      .map(|(l, r)| format!("{l:<longest_width$} {r}"))
      .collect::<Vec<_>>()
      .join("\n")
  } else {
    let mut call = String::from("set ");

    let lines: Vec<_> = opts
      .lines()
      .map(|l| l.strip_prefix("set.").unwrap().split_once('=').unwrap())
      .collect();

    let on: Vec<&str> = lines
      .iter()
      .filter(|(_, r)| *r == "true")
      .map(|(l, _)| *l)
      .collect();

    let on_chars: String = on
      .iter()
      .filter_map(|opt| SetFlags::from_str(opt).unwrap().as_char())
      .collect();

    let on_strs: String = on
      .into_iter()
      .filter(|opt| SetFlags::from_str(opt).unwrap().as_char().is_none())
      .map(|o| format!("-o {o}"))
      .collect::<Vec<_>>()
      .join(" ");

    let off: Vec<_> = lines
      .iter()
      .filter(|(_, r)| *r != "true")
      .map(|(l, _)| *l)
      .collect();

    let off_chars: String = off
      .iter()
      .filter_map(|opt| SetFlags::from_str(opt).unwrap().as_char())
      .collect();

    let off_strs: String = off
      .into_iter()
      .filter(|opt| SetFlags::from_str(opt).unwrap().as_char().is_none())
      .map(|o| format!("+o {o}"))
      .collect::<Vec<_>>()
      .join(" ");

    let pos_args = Shed::vars(|v| {
      v.sh_argv()
        .clone()
        .into_iter()
        .skip(1)
        .collect::<Vec<_>>()
        .join_with(" ")
    });

    if !on_chars.is_empty() {
      let _ = write!(call, "-{on_chars} ");
    }
    if !off_chars.is_empty() {
      let _ = write!(call, "+{off_chars} ");
    }
    if !on_strs.is_empty() {
      let _ = write!(call, "{on_strs} ");
    }
    if !off_strs.is_empty() {
      let _ = write!(call, "{off_strs} ");
    }
    if !pos_args.is_empty() {
      let _ = write!(call, "-- {pos_args}");
    }

    call.trim_end().to_string()
  }
}

/// How the caller of [`scan_options`] classifies a short flag character.
pub(crate) enum Role {
  Set(SetFlags),
  Invocation,
  Unknown,
}

/// Outcome of scanning a run of `set`-style option words.
pub(crate) struct SetOpts {
  /// A `--` terminator was consumed; operands follow.
  pub terminated: bool,
}

/// Reset every `set`-option to its default value (`set -`).
fn reset_set_opts() {
  Shed::shopts_mut(|o| o.set = ShOptSet::default());
}

/// Apply a resolved [`SetFlags`] set to the shell's shopt table with the given
/// polarity (`on` = enable). `emacs` is special-cased to invert `vi`.
fn apply_set_flags(on: bool, flags: SetFlags, span: &Span) -> ShResult<()> {
  for opt in flags.get_shopt_fields() {
    if opt == "emacs" {
      let val = if on { "false" } else { "true" };
      Shed::shopts_mut(|o| o.query(&format!("set.vi={val}"))).promote_err(span.clone())?;
      continue;
    }
    let val = if on { "true" } else { "false" };
    Shed::shopts_mut(|o| o.query(&format!("set.{opt}={val}"))).promote_err(span.clone())?;
  }
  Ok(())
}

/// Handle a `-o` / `+o` occurrence. The long-option name is taken from the rest
/// of the current cluster if present (`-oerrexit`), otherwise from following
/// operand words (`-o errexit`). With no name at all, prints the current
/// settings, matching `set -o`.
fn apply_long_o<I>(
  on: bool,
  cluster: &mut Peekable<impl Iterator<Item = char>>,
  words: &mut Peekable<I>,
  span: &Span,
) -> ShResult<()>
where
  I: Iterator<Item = (VarStr, Span)>,
{
  let attached: String = cluster.by_ref().collect();
  if !attached.is_empty() {
    let flag = SetFlags::from_str(&attached).promote_err(span.clone())?;
    return apply_set_flags(on, flag, span);
  }

  let mut found = false;
  while let Some((word, _)) = words.peek() {
    let word = word.to_str_lossy();
    if word.starts_with('-') || word.starts_with('+') {
      break;
    }
    found = true;
    let (name, name_span) = words.next().unwrap();
    let flag = SetFlags::from_str(&name.to_str_lossy()).promote_err(name_span)?;
    apply_set_flags(on, flag, span)?;
  }

  if !found {
    outln!("{}", build_set_call(on));
  }
  Ok(())
}

/// Scan and apply a leading run of `set`-style option words from `words`.
///
/// Handles polarity (`-`/`+`), bundled shorts (`-ex`, `+ex`), `-o NAME` /
/// `+o NAME` (including the attached `-oNAME` form and the no-name "print"
/// form), and the `--` terminator. Stops — *without consuming* — at the first
/// operand, a lone `-`, or a `--long` word, leaving it in `words` for the
/// caller. `--` is consumed and reported via [`SetOpts::terminated`].
///
/// `classify` decides what each short char means; `on_invocation` handles any
/// char classified as [`Role::Invocation`], receiving the attached argument
/// (leftover cluster chars) if present and the remaining `words` so it can pull
/// a separate argument. `strict` makes an unknown short flag an error.
pub(crate) fn scan_options<I>(
  words: &mut Peekable<I>,
  classify: impl Fn(char) -> Role,
  mut on_invocation: impl FnMut(char, Option<VarStr>, &mut Peekable<I>, Span) -> ShResult<()>,
  strict: bool,
) -> ShResult<SetOpts>
where
  I: Iterator<Item = (VarStr, Span)>,
{
  while let Some((word, span)) = words.peek().cloned() {
    let word = word.to_str_lossy();
    match word.chars().next() {
      Some('-' | '+') => {}
      _ => break, // first operand — leave it in `words`
    }
    if word == "-" {
      break; // a lone `-` is an operand, not an option
    }
    if word.starts_with("--") {
      if word == "--" {
        words.next();
        return Ok(SetOpts { terminated: true });
      }
      break; // `--long` word: caller handles it; don't consume
    }

    words.next(); // commit: it's a short cluster or `-o`
    let on = word.starts_with('-');
    let mut cluster = word[1..].chars().collect::<Vec<_>>().into_iter().peekable();
    let mut flags = SetFlags::empty();

    while let Some(ch) = cluster.next() {
      if ch == 'o' {
        apply_long_o(on, &mut cluster, words, &span)?;
        continue;
      }
      match classify(ch) {
        Role::Set(f) => flags |= f,
        Role::Invocation => {
          // getopt rule: leftover cluster chars are this option's argument.
          let attached: String = cluster.by_ref().collect();
          let attached = (!attached.is_empty()).then(|| VarStr::from(attached));
          on_invocation(ch, attached, words, span.clone())?;
          break; // an arg-taking option ends the cluster
        }
        Role::Unknown if strict => {
          return Err(sherr!(ParseErr @ span, "invalid option: -{ch}"));
        }
        Role::Unknown => break,
      }
    }

    apply_set_flags(on, flags, &span)?;
  }

  Ok(SetOpts { terminated: false })
}

pub(super) struct Set;
impl super::Builtin for Set {
  fn is_special(&self) -> bool {
    true
  }

  fn get_argv_and_opts(&self, cmd_span: Span, argv: &[Tk], no_split: bool) -> ShResult<Parsed> {
    Ok(
      execute::prepare_argv_with(argv, no_split)
        .promote_err(cmd_span)?
        .into(),
    )
  }

  fn execute(&self, mut args: super::BuiltinArgs) -> ShResult<()> {
    if args.no_arguments() {
      // print values of all variables
      let all_vars = Shed::vars(ScopeStack::all_vars);
      for (k, v) in all_vars {
        let mut line = k.into_bytes();
        if let VarKind::Arr(items) = v.kind() {
          line.extend_from_slice(b"=( ");
          let mut item_iter = items.iter().peekable();
          while let Some(item) = item_iter.next() {
            line.extend_from_slice(&escape::shell_quote_bytes(item.as_bytes()));
            if item_iter.peek().is_some() {
              line.push(b' ');
            }
          }
          line.extend_from_slice(b" )");
        } else {
          line.push(b'=');
          line.extend_from_slice(&escape::shell_quote_bytes(&v.value_bytes()));
        }
        procio::outln_bytes(&line);
      }
    }

    let (arg_vec, _) = args.take_argv();
    let mut it = arg_vec.into_iter().peekable();

    // `set -` (a bare dash) resets every set-option to its default.
    if matches!(it.peek(), Some((w, _)) if w == "-") {
      reset_set_opts();
      it.next();
    }

    // The `set` builtin has no invocation-only flags: every short flag is a
    // set-option, and an unknown one is an error.
    let outcome = scan_options(
      &mut it,
      |ch| SetFlags::try_from(ch).map_or(Role::Unknown, Role::Set),
      |ch, _, _, span| Err(sherr!(ParseErr @ span, "invalid option: -{ch}")),
      true,
    )?;

    let pos_args: Vec<VarStr> = it.map(|(arg, _)| arg).collect();

    if !pos_args.is_empty() || outcome.terminated {
      Shed::vars_mut(|v| {
        let argv_scope = v.sh_argv_scope_mut();
        argv_scope.clear_args();
        for arg in pos_args {
          argv_scope.bpush_arg(arg);
        }
      });
    }

    util::with_status(0)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_setflag_parse() {
    assert_eq!(SetFlags::try_from('a').unwrap(), SetFlags::ALLEXPORT);
    assert_eq!(SetFlags::try_from('b').unwrap(), SetFlags::NOTIFY);
    assert_eq!(SetFlags::try_from('C').unwrap(), SetFlags::NO_CLOBBER);
    assert_eq!(SetFlags::try_from('e').unwrap(), SetFlags::ERREXIT);
    assert_eq!(SetFlags::try_from('f').unwrap(), SetFlags::NO_GLOB);
    assert_eq!(SetFlags::try_from('h').unwrap(), SetFlags::HASHALL);
    assert_eq!(SetFlags::try_from('m').unwrap(), SetFlags::MONITOR);
    assert_eq!(SetFlags::try_from('n').unwrap(), SetFlags::NO_EXEC);
    assert_eq!(SetFlags::try_from('u').unwrap(), SetFlags::NO_UNSET);
    assert_eq!(SetFlags::try_from('v').unwrap(), SetFlags::VERBOSE);
    assert_eq!(SetFlags::try_from('x').unwrap(), SetFlags::XTRACE);
    assert!(SetFlags::try_from('z').is_err());

    assert_eq!(
      SetFlags::from_str("allexport").unwrap(),
      SetFlags::ALLEXPORT
    );
    assert_eq!(SetFlags::from_str("notify").unwrap(), SetFlags::NOTIFY);
    assert_eq!(
      SetFlags::from_str("noclobber").unwrap(),
      SetFlags::NO_CLOBBER
    );
    assert_eq!(SetFlags::from_str("errexit").unwrap(), SetFlags::ERREXIT);
    assert_eq!(SetFlags::from_str("noglob").unwrap(), SetFlags::NO_GLOB);
    assert_eq!(SetFlags::from_str("hashall").unwrap(), SetFlags::HASHALL);
    assert_eq!(SetFlags::from_str("monitor").unwrap(), SetFlags::MONITOR);
    assert_eq!(SetFlags::from_str("noexec").unwrap(), SetFlags::NO_EXEC);
    assert_eq!(SetFlags::from_str("nounset").unwrap(), SetFlags::NO_UNSET);
    assert_eq!(SetFlags::from_str("verbose").unwrap(), SetFlags::VERBOSE);
    assert_eq!(SetFlags::from_str("xtrace").unwrap(), SetFlags::XTRACE);
    assert_eq!(SetFlags::from_str("vi").unwrap(), SetFlags::VI_MODE);
    assert_eq!(SetFlags::from_str("emacs").unwrap(), SetFlags::EMACS_MODE);
    assert!(SetFlags::from_str("invalid").is_err());
  }

  // ─── as_char: single-bit → short flag char ────────────────────────

  #[test]
  fn as_char_single_bits_round_trip_via_try_from() {
    // Every single-bit flag that has a short-form char should map both
    // ways consistently.
    let pairs: &[(SetFlags, char)] = &[
      (SetFlags::ALLEXPORT, 'a'),
      (SetFlags::NOTIFY, 'b'),
      (SetFlags::NO_CLOBBER, 'C'),
      (SetFlags::ERREXIT, 'e'),
      (SetFlags::NO_GLOB, 'f'),
      (SetFlags::HASHALL, 'h'),
      (SetFlags::MONITOR, 'm'),
      (SetFlags::NO_EXEC, 'n'),
      (SetFlags::NO_UNSET, 'u'),
      (SetFlags::VERBOSE, 'v'),
      (SetFlags::XTRACE, 'x'),
    ];
    for (flag, ch) in pairs {
      assert_eq!(flag.as_char(), Some(*ch), "as_char for {flag:?}");
      assert_eq!(
        SetFlags::try_from(*ch).unwrap(),
        *flag,
        "try_from('{ch}') round-trip"
      );
    }
  }

  // ─── as_char: long-only flags have no short form → None ──────────

  #[test]
  fn as_char_long_only_flags_return_none() {
    // These flags are settable only via `set -o NAME`, never as `-X`.
    assert_eq!(SetFlags::IGNORE_EOF.as_char(), None);
    assert_eq!(SetFlags::VI_MODE.as_char(), None);
    assert_eq!(SetFlags::EMACS_MODE.as_char(), None);
    assert_eq!(SetFlags::NO_LOG.as_char(), None);
  }

  // ─── as_char: composite/empty inputs → None ───────────────────────

  #[test]
  fn as_char_empty_flagset_returns_none() {
    assert_eq!(SetFlags::empty().as_char(), None);
  }

  #[test]
  fn as_char_multi_bit_combination_returns_none() {
    // The function uses `*self == Self::ONE_FLAG` for matching, which
    // requires exact equality. A combined set never matches any single
    // bit — pinning this strictness.
    let combo = SetFlags::ERREXIT | SetFlags::VERBOSE;
    assert_eq!(combo.as_char(), None);
  }

  // ===================== Set::execute =====================

  mod execute {
    use crate::state::vars::VarStr;
    use crate::state::{self, Shed};
    use crate::tests::testutil::{TestGuard, test_input};

    // ─── single short-flag toggles ─────────────────────────────────

    #[test]
    fn dash_e_enables_errexit() {
      let _g = TestGuard::new();
      assert!(!Shed::shopts(|o| o.set.errexit));
      test_input("set -e").unwrap();
      assert!(Shed::shopts(|o| o.set.errexit));
    }

    #[test]
    fn plus_e_disables_errexit() {
      let _g = TestGuard::new();
      test_input("set -e").unwrap();
      assert!(Shed::shopts(|o| o.set.errexit));
      test_input("set +e").unwrap();
      assert!(!Shed::shopts(|o| o.set.errexit));
    }

    #[test]
    fn dash_a_enables_allexport() {
      let _g = TestGuard::new();
      test_input("set -a").unwrap();
      assert!(Shed::shopts(|o| o.set.allexport));
    }

    #[test]
    fn option_after_positional_is_not_parsed() {
      // POSIX: the first non-option arg ends option recognition, so `-e` here
      // is a positional parameter, not the errexit flag.
      let g = TestGuard::new();
      assert!(!Shed::shopts(|o| o.set.errexit));
      test_input(r#"set foo -e; echo "$# $1 $2""#).unwrap();
      assert_eq!(g.read_output().trim(), "2 foo -e");
      assert!(
        !Shed::shopts(|o| o.set.errexit),
        "-e after a positional must not enable errexit"
      );
    }

    #[test]
    fn dash_u_enables_nounset() {
      let _g = TestGuard::new();
      test_input("set -u").unwrap();
      assert!(Shed::shopts(|o| o.set.nounset));
    }

    // ─── packed short flags ────────────────────────────────────────

    #[test]
    fn dash_ae_enables_both_allexport_and_errexit() {
      let _g = TestGuard::new();
      test_input("set -ae").unwrap();
      assert!(Shed::shopts(|o| o.set.allexport));
      assert!(Shed::shopts(|o| o.set.errexit));
    }

    // ─── long opts via -o NAME ─────────────────────────────────────

    #[test]
    fn dash_o_vi_enables_vi_mode() {
      let _g = TestGuard::new();
      test_input("set -o vi").unwrap();
      assert!(Shed::shopts(|o| o.set.vi));
    }

    #[test]
    fn plus_o_vi_disables_vi_mode() {
      let _g = TestGuard::new();
      test_input("set -o vi").unwrap();
      assert!(Shed::shopts(|o| o.set.vi));
      test_input("set +o vi").unwrap();
      assert!(!Shed::shopts(|o| o.set.vi));
    }

    #[test]
    fn dash_o_emacs_disables_vi_mode() {
      // EMACS_MODE has a special case: it inverts vi instead of having
      // its own field.
      let _g = TestGuard::new();
      test_input("set -o vi").unwrap();
      test_input("set -o emacs").unwrap();
      assert!(!Shed::shopts(|o| o.set.vi));
    }

    #[test]
    fn dash_o_unknown_name_errors() {
      let _g = TestGuard::new();
      test_input("set -o not_a_real_opt").ok();
      assert_ne!(state::Shed::get_status(), 0);
    }

    // ─── unknown short flag errors ─────────────────────────────────

    #[test]
    fn dash_z_unknown_flag_errors() {
      let _g = TestGuard::new();
      test_input("set -z").ok();
      assert_ne!(state::Shed::get_status(), 0);
    }

    // ─── -o with no following name prints current settings ────────

    #[test]
    fn dash_o_alone_prints_current_settings() {
      let g = TestGuard::new();
      test_input("set -e").unwrap();
      g.read_output(); // drain anything set printed
      test_input("set -o").unwrap();
      let out = g.read_output();
      // The exact format is "set -<chars>" or "set -o name"; either way
      // 'e' (or 'errexit') should be mentioned somewhere.
      assert!(out.contains('e') || out.contains("errexit"), "got: {out:?}");
    }

    // ─── positional args ───────────────────────────────────────────

    #[test]
    fn positional_args_replace_argv() {
      let _g = TestGuard::new();
      test_input("set -- one two three").unwrap();
      let args: Vec<VarStr> = Shed::vars(|v| v.sh_argv().clone().into_iter().skip(1).collect());
      assert_eq!(args, vec!["one", "two", "three"]);
    }

    #[test]
    fn bare_positional_args_replace_argv() {
      // Without `--`, positional non-polarity args still get pushed.
      let _g = TestGuard::new();
      test_input("set foo bar").unwrap();
      let args: Vec<VarStr> = Shed::vars(|v| v.sh_argv().clone().into_iter().skip(1).collect());
      assert_eq!(args, vec!["foo", "bar"]);
    }

    #[test]
    fn double_dash_alone_clears_argv() {
      let _g = TestGuard::new();
      test_input("set -- a b c").unwrap();
      test_input("set --").unwrap();
      let args: Vec<VarStr> = Shed::vars(|v| v.sh_argv().clone().into_iter().skip(1).collect());
      assert_eq!(args, Vec::<String>::new());
    }

    #[test]
    fn set_dash_dash_inside_function_sets_positionals() {
      // Regression (issue #110): a function body is a brace group, which
      // pushes a shared scope with an empty argv. `set --` must target the
      // function's argv-owning scope, not that transient lexical scope, or
      // reads of $*/$#/$1 resolve to the wrong scope and come back empty.
      let g = TestGuard::new();
      test_input("u() { set -- test; echo \"$*/$#/$1\"; }").unwrap();
      test_input("u").unwrap();
      assert_eq!(g.read_output().trim(), "test/1/test");
    }

    #[test]
    fn set_dash_dash_in_function_does_not_leak_to_caller() {
      let g = TestGuard::new();
      test_input("set -- outer1 outer2").unwrap();
      test_input("u() { set -- inner; }").unwrap();
      test_input("u").unwrap();
      test_input("echo \"$*/$#\"").unwrap();
      assert_eq!(g.read_output().trim(), "outer1 outer2/2");
    }

    #[test]
    fn dash_double_dash_carries_remaining_as_positional() {
      // `set -e -- foo bar` enables errexit AND sets foo bar.
      let _g = TestGuard::new();
      test_input("set -e -- foo bar").unwrap();
      assert!(Shed::shopts(|o| o.set.errexit));
      let args: Vec<VarStr> = Shed::vars(|v| v.sh_argv().clone().into_iter().skip(1).collect());
      assert_eq!(args, vec!["foo", "bar"]);
    }

    // ─── bare `-` resets shopts.set ────────────────────────────────

    #[test]
    fn bare_dash_resets_shopts_to_defaults() {
      let _g = TestGuard::new();
      test_input("set -e -a -u").unwrap();
      assert!(Shed::shopts(|o| o.set.errexit));
      assert!(Shed::shopts(|o| o.set.allexport));
      test_input("set -").unwrap();
      assert!(!Shed::shopts(|o| o.set.errexit));
      assert!(!Shed::shopts(|o| o.set.allexport));
      assert!(!Shed::shopts(|o| o.set.nounset));
    }

    // ─── no args → print all vars ─────────────────────────────────

    #[test]
    fn no_args_prints_variables() {
      let g = TestGuard::new();
      test_input("UNIQUE_SET_TEST_VAR=hello_marker").unwrap();
      g.read_output();
      test_input("set").unwrap();
      let out = g.read_output();
      assert!(out.contains("UNIQUE_SET_TEST_VAR"), "got: {out:?}");
      assert!(out.contains("hello_marker"), "got: {out:?}");
    }
  }

  // ===================== build_set_call =====================

  mod build_set_call_tests {
    use super::super::build_set_call;
    use crate::tests::testutil::{TestGuard, test_input};

    /// readable=true returns a multi-line aligned listing of every
    /// flag with on/off status.
    #[test]
    fn readable_lists_one_flag_per_line() {
      let _g = TestGuard::new();
      // Force at least one known flag on so the output is non-trivial.
      test_input("set -e").unwrap();
      let out = build_set_call(true);
      assert!(out.contains("errexit"), "got: {out:?}");
      assert!(out.contains("on") || out.contains("off"), "got: {out:?}");
      // Multi-line listing.
      assert!(out.contains('\n'), "got: {out:?}");
    }

    /// readable=true aligns flag names — every line up to the gap has
    /// the same width when measured by display columns.
    #[test]
    fn readable_aligns_flag_names() {
      let _g = TestGuard::new();
      let out = build_set_call(true);
      let lines: Vec<&str> = out.lines().collect();
      // Find the first column index of "on" or "off" on each line.
      let positions: Vec<usize> = lines
        .iter()
        .filter_map(|l| l.find(" on").or_else(|| l.find(" off")))
        .collect();
      assert!(positions.len() > 1, "needed >1 lines, got: {out:?}");
      let first = positions[0];
      for p in &positions {
        assert_eq!(*p, first, "alignment mismatch in: {out:?}");
      }
    }

    /// readable=false returns a `set ...` invocation that round-trips
    /// the currently enabled short flags.
    #[test]
    fn non_readable_emits_set_invocation_with_on_chars() {
      let _g = TestGuard::new();
      test_input("set -ex").unwrap();
      let call = build_set_call(false);
      assert!(call.starts_with("set "), "got: {call:?}");
      // Should mention both e and x in the on-chars cluster.
      // (Order within the cluster isn't fixed, so check membership.)
      let on_part = call
        .split_whitespace()
        .find(|w| w.starts_with('-'))
        .unwrap_or("");
      assert!(on_part.contains('e'), "got: {call:?}");
      assert!(on_part.contains('x'), "got: {call:?}");
    }

    /// Off-flags get a `+` cluster.
    #[test]
    fn non_readable_emits_plus_cluster_for_off_flags() {
      let _g = TestGuard::new();
      test_input("set -e").unwrap();
      test_input("set +e").unwrap();
      let call = build_set_call(false);
      // After +e, errexit is off. There must be a `+` cluster
      // containing 'e' somewhere in the output.
      let plus_part = call.split_whitespace().find(|w| w.starts_with('+'));
      assert!(
        plus_part.is_some(),
        "expected '+...' cluster, got: {call:?}"
      );
      assert!(plus_part.unwrap().contains('e'), "got: {call:?}");
    }

    /// Positional args are appended after `--`.
    #[test]
    fn non_readable_appends_positional_args_after_dash_dash() {
      let _g = TestGuard::new();
      test_input("set -- alpha beta gamma").unwrap();
      let call = build_set_call(false);
      assert!(call.contains("--"), "got: {call:?}");
      assert!(call.contains("alpha"), "got: {call:?}");
      assert!(call.contains("beta"), "got: {call:?}");
      assert!(call.contains("gamma"), "got: {call:?}");
    }

    /// Result has no trailing whitespace.
    #[test]
    fn non_readable_trims_trailing_whitespace() {
      let _g = TestGuard::new();
      let call = build_set_call(false);
      assert!(!call.ends_with(' '), "got: {call:?}");
    }
  }
}
