use std::io::{Read, Seek, Write as IoWrite};

use bstr::ByteSlice;
use tempfile::NamedTempFile;

use crate::{
  eval::parse::{Ast, node::NodeId},
  state::vars::VarStr,
  varstr,
};

use super::{
  super::state::meta::MetaTab,
  Shed,
  eval::{
    NdRule,
    execute::{Dispatcher, exec_input},
    lex::{Span, Tk},
  },
  match_loop,
  readline::{HistEntry, History},
  sherr,
  state::{self},
  try_var,
  util::{ShResult, ShResultExt, ordered},
};

use bitflags::bitflags;

bitflags! {
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub struct FixCmdFlags: u32 {
  }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RangeArg {
  Number(i32),
  Prefix(VarStr),
}

impl Default for RangeArg {
  fn default() -> Self {
    Self::Number(-1)
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum FixMode {
  #[default]
  Edit, // default
  List,  // -l
  Rerun, // -s
}

#[derive(Debug, Default)]
pub struct FixCmdOpts {
  editor: Option<VarStr>,
  replace: Option<(VarStr, VarStr)>,
  first: Option<RangeArg>,
  last: Option<RangeArg>,
  mode: FixMode,
  no_numbers: bool,
  reverse: bool,
}

pub fn parse_fc_args(args: &[Tk]) -> ShResult<(Vec<(VarStr, Span)>, FixCmdOpts)> {
  let mut args = args.iter();
  args.next(); // skip "fc" command itself

  let mut words: Vec<(VarStr, Span)> = vec![];
  let mut opts = FixCmdOpts::default();
  for tk in args {
    let span = tk.span.clone();
    let expanded = tk.clone().expand()?;
    for word in expanded.get_words().iter() {
      words.push((word.clone(), span.clone()));
    }
  }

  let mut words_iter = words.into_iter().peekable();
  let mut non_opts = vec![];

  while let Some((word, span)) = words_iter.next() {
    if word == "--" {
      non_opts.push((word, span));
      non_opts.extend(words_iter);
      break;
    }

    if let Ok(num) = word.to_str_lossy().parse::<i32>()
      && num != 0
    {
      if opts.first.is_none() {
        opts.first = Some(RangeArg::Number(num));
      } else if opts.last.is_none() {
        opts.last = Some(RangeArg::Number(num));
      } else {
        non_opts.push((word, span));
      }
      continue;
    }

    if opts.mode != FixMode::List {
      let mut old = String::new();
      let mut new = String::new();
      let mut chars = word.chars();
      match_loop!(chars.next() => ch, {
        '\\' => {
          old.push(ch);
          if let Some(next_ch) = chars.next() {
            old.push(next_ch);
          }
        }
        '=' => {
          new = chars.collect();
          break;
        }
        _ => old.push(ch),
      });

      if !new.is_empty() {
        if opts.replace.is_none() {
          opts.replace = Some((old.into(), new.into()));
        } else {
          non_opts.push((word, span));
        }
        continue;
      }
    }

    match word.to_str_lossy().as_ref() {
      "-r" => opts.reverse = true,
      "-n" => opts.no_numbers = true,
      "-s" => opts.mode = FixMode::Rerun,
      "-l" => opts.mode = FixMode::List,
      "-e" => {
        let Some((word, _)) = words_iter.next() else {
          return Err(sherr!(ParseErr @ span, "Option -e requires an argument"));
        };
        opts.editor = Some(word);
      }
      _ => {
        if opts.first.is_none() {
          opts.first = Some(RangeArg::Prefix(word));
        } else if opts.last.is_none() {
          opts.last = Some(RangeArg::Prefix(word));
        } else {
          non_opts.push((word, span));
        }
      }
    }
  }

  Ok((non_opts, opts))
}

pub(super) struct FixCmd;
impl super::Builtin for FixCmd {
  fn execute(&self, _args: super::BuiltinArgs) -> ShResult<()> {
    unreachable!("fixcmd is a special snowflake command that needs really special handling");
  }
  fn run_builtin(&self, tree: &Ast, node_id: NodeId, _dispatcher: &mut Dispatcher) -> ShResult<()> {
    let node = &tree[node_id];
    let span = node.get_span();
    let NdRule::Command {
      assignments: _,
      argv,
    } = &node.class
    else {
      unreachable!()
    };

    let (_argv, opts) = parse_fc_args(argv).promote_err(span.clone())?;

    let conn = state::util::get_db_conn()
      .ok_or_else(|| sherr!(InternalErr, "database not available"))
      .promote_err(span.clone())?;
    let hist = History::new(conn, "shed_history").promote_err(span.clone())?;
    match opts.mode {
      FixMode::List => {
        fc_list(&hist, opts).promote_err(span)?;
      }
      FixMode::Rerun => {
        fc_reexec(&hist, opts).promote_err(span)?;
      }
      FixMode::Edit => {
        fc_edit(&hist, opts).promote_err(span)?;
      }
    }

    state::Shed::set_status(0);
    Ok(())
  }
}

fn fc_edit(hist: &History, opts: FixCmdOpts) -> ShResult<()> {
  let editor = if let Some(cfg_editor) = opts.editor {
    cfg_editor
  } else if let Some(fc_editor) = try_var!("FCEDIT") {
    fc_editor
  } else if let Some(env_editor) = try_var!("EDITOR") {
    env_editor
  } else {
    return Err(sherr!(ExecFail, "No editor specified for fc command"));
  };
  let first = opts.first.unwrap_or_default();
  let last = opts.last.unwrap_or(first.clone());

  let entries = get_entry_range(
    hist,
    Some(first),
    Some(last),
    opts.reverse,
    Shed::meta(MetaTab::current_cmd_recorded),
  )?;
  let mut should_push;

  Shed::meta_mut(MetaTab::set_no_hist_save);

  for (_, entry) in entries {
    let old_cmd = entry.command;
    let mut new_cmd = String::new();

    let mut tmp = NamedTempFile::new()?;
    tmp.write_all(old_cmd.as_bytes())?;
    tmp.flush()?;

    let editor_cmd = varstr!("{editor} {}", tmp.path().display());

    exec_input(editor_cmd, Some("fc edit".into()))?;

    tmp.rewind()?;
    tmp.read_to_string(&mut new_cmd)?;
    new_cmd = new_cmd.trim().into();

    should_push = new_cmd.as_bytes() != old_cmd.as_bytes();

    exec_input(new_cmd.clone().into(), Some("fc re-exec".into()))?;

    if should_push {
      hist.push(&new_cmd)?;
    }
  }

  Ok(())
}

fn fc_reexec(hist: &History, opts: FixCmdOpts) -> ShResult<()> {
  let first = opts.first.unwrap_or_default();
  let last = opts.last.unwrap_or(first.clone());
  let entries = get_entry_range(
    hist,
    Some(first),
    Some(last),
    opts.reverse,
    Shed::meta(MetaTab::current_cmd_recorded),
  )?;

  Shed::meta_mut(MetaTab::set_no_hist_save);
  for (_, entry) in entries {
    let mut command = entry.command;
    let mut should_push = false;
    if let Some((old, new)) = &opts.replace {
      let new_cmd = command
        .to_str_lossy()
        .replace(old.to_str_lossy().as_ref(), new.to_str_lossy().as_ref());
      if new_cmd.as_bytes() != command.as_bytes() {
        command = new_cmd.into();
        should_push = true;
      }
    }

    exec_input(command.clone(), Some("fc re-exec".into()))?;
    if should_push {
      hist.push(&command.to_str_lossy())?;
    }
  }

  Ok(())
}

fn fc_list(hist: &History, opts: FixCmdOpts) -> ShResult<()> {
  let first = if let Some(first) = opts.first {
    first
  } else {
    RangeArg::Number(-16)
  };
  let last = opts.last.clone().unwrap_or_default();

  let entries = get_entry_range(
    hist,
    Some(first),
    Some(last),
    opts.reverse,
    Shed::meta(MetaTab::current_cmd_recorded),
  )?;

  let mut buf: Vec<u8> = Vec::new();
  for (id, entry) in entries {
    let cmd = entry.command;
    if !opts.no_numbers {
      write!(buf, "{id}\t").unwrap();
    }
    buf.extend_from_slice(cmd.as_bytes());
    buf.push(b'\n');
  }

  crate::procio::out_bytes(&buf);

  Ok(())
}

fn get_entry_range(
  hist: &History,
  first: Option<RangeArg>,
  last: Option<RangeArg>,
  reverse: bool,
  skip_current: bool,
) -> ShResult<Vec<(i64, HistEntry)>> {
  let mut last_id = hist.last_id();
  if skip_current && last_id > 0 {
    last_id -= 1;
  }

  let resolve = |arg: &RangeArg| -> ShResult<i64> {
    match arg {
      // Negative indices count back from the most recent entry: -1 is
      // the last command, -2 the one before it, etc.
      RangeArg::Number(n) if *n < 0 => Ok(last_id + 1 + i64::from(*n)),
      RangeArg::Number(n) => Ok(i64::from(*n)),
      RangeArg::Prefix(p) => Ok(
        hist
          .query_by_prefix(&p.to_str_lossy())?
          .map_or(last_id, |(id, _)| id),
      ),
    }
  };

  let first_id = resolve(&first.unwrap_or(RangeArg::Number(last_id as i32)))?;
  let last_id = resolve(&last.unwrap_or(RangeArg::Number(first_id as i32)))?;

  let (lo, hi) = ordered(first_id, last_id);

  let mut entries = hist.query_range(lo, hi)?;
  if reverse || first_id > last_id {
    entries.reverse();
  }
  Ok(entries)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::eval::lex::{LexFlags, LexStream, TkRule};
  use crate::tests::testutil::TestGuard;

  /// Lex an `fc <args>` invocation into the Tk vec that `parse_fc_args` expects.
  fn lex_fc(input: &str) -> Vec<Tk> {
    LexStream::new(input.as_bytes(), LexFlags::empty())
      .filter_map(Result::ok)
      .filter(|t| {
        !matches!(
          t.class,
          TkRule::Soi | TkRule::Eoi | TkRule::Sep | TkRule::Null
        )
      })
      .collect()
  }

  fn parse(input: &str) -> (Vec<(VarStr, Span)>, FixCmdOpts) {
    let tks = lex_fc(input);
    parse_fc_args(&tks).expect("parse should succeed")
  }

  // ─── Boolean flags ────────────────────────────────────────────────────

  #[test]
  fn fc_no_args_returns_defaults() {
    let _g = TestGuard::new();
    let (non_opts, opts) = parse("fc");
    assert!(non_opts.is_empty());
    assert_eq!(opts.mode, FixMode::Edit);
    assert!(!opts.no_numbers);
    assert!(!opts.reverse);
    assert!(opts.editor.is_none());
    assert!(opts.first.is_none());
    assert!(opts.last.is_none());
    assert!(opts.replace.is_none());
  }

  #[test]
  fn fc_dash_l_sets_list_mode() {
    let _g = TestGuard::new();
    let (_, opts) = parse("fc -l");
    assert_eq!(opts.mode, FixMode::List);
  }

  #[test]
  fn fc_dash_n_sets_no_numbers() {
    let _g = TestGuard::new();
    let (_, opts) = parse("fc -n");
    assert!(opts.no_numbers);
  }

  #[test]
  fn fc_dash_r_sets_reverse() {
    let _g = TestGuard::new();
    let (_, opts) = parse("fc -r");
    assert!(opts.reverse);
  }

  #[test]
  fn fc_dash_s_sets_rerun_mode() {
    let _g = TestGuard::new();
    let (_, opts) = parse("fc -s");
    assert_eq!(opts.mode, FixMode::Rerun);
  }

  #[test]
  fn fc_multiple_flags_compose() {
    let _g = TestGuard::new();
    let (_, opts) = parse("fc -l -n -r");
    assert_eq!(opts.mode, FixMode::List);
    assert!(opts.no_numbers);
    assert!(opts.reverse);
  }

  #[test]
  fn fc_dash_l_overrides_dash_s() {
    // `-l` and `-s` set the same field; whichever comes last wins.
    // This documents the precedence rather than enforcing one — change
    // the assertion if the policy changes.
    let _g = TestGuard::new();
    let (_, opts) = parse("fc -s -l");
    assert_eq!(opts.mode, FixMode::List);
    let (_, opts) = parse("fc -l -s");
    assert_eq!(opts.mode, FixMode::Rerun);
  }

  // ─── -e <editor> ──────────────────────────────────────────────────────

  #[test]
  fn fc_dash_e_consumes_next_arg_as_editor() {
    let _g = TestGuard::new();
    let (_, opts) = parse("fc -e vim");
    assert_eq!(opts.editor, Some("vim".into()));
  }

  #[test]
  fn fc_dash_e_without_arg_errors() {
    let _g = TestGuard::new();
    let tks = lex_fc("fc -e");
    let result = parse_fc_args(&tks);
    assert!(result.is_err());
  }

  // ─── Numeric range args ──────────────────────────────────────────────

  #[test]
  fn fc_single_number_sets_first() {
    let _g = TestGuard::new();
    let (_, opts) = parse("fc 5");
    assert_eq!(opts.first, Some(RangeArg::Number(5)));
    assert!(opts.last.is_none());
  }

  #[test]
  fn fc_two_numbers_set_first_and_last() {
    let _g = TestGuard::new();
    let (_, opts) = parse("fc 5 10");
    assert_eq!(opts.first, Some(RangeArg::Number(5)));
    assert_eq!(opts.last, Some(RangeArg::Number(10)));
  }

  #[test]
  fn fc_negative_numbers_accepted() {
    let _g = TestGuard::new();
    let (_, opts) = parse("fc -3 -1");
    assert_eq!(opts.first, Some(RangeArg::Number(-3)));
    assert_eq!(opts.last, Some(RangeArg::Number(-1)));
  }

  #[test]
  fn fc_zero_is_not_treated_as_number() {
    // `0` parses to i32 but the `num != 0` guard rejects it, so it
    // falls through to the catch-all and becomes a Prefix.
    let _g = TestGuard::new();
    let (_, opts) = parse("fc 0");
    assert_eq!(opts.first, Some(RangeArg::Prefix("0".into())));
  }

  #[test]
  fn fc_third_number_goes_to_non_opts() {
    let _g = TestGuard::new();
    let (non_opts, opts) = parse("fc 1 2 3");
    assert_eq!(opts.first, Some(RangeArg::Number(1)));
    assert_eq!(opts.last, Some(RangeArg::Number(2)));
    assert_eq!(non_opts.len(), 1);
    assert_eq!(non_opts[0].0, "3");
  }

  // ─── Prefix (non-numeric) range args ─────────────────────────────────

  #[test]
  fn fc_word_sets_first_as_prefix() {
    let _g = TestGuard::new();
    let (_, opts) = parse("fc git");
    assert_eq!(opts.first, Some(RangeArg::Prefix("git".into())));
  }

  #[test]
  fn fc_two_words_set_first_and_last_as_prefixes() {
    let _g = TestGuard::new();
    let (_, opts) = parse("fc git cargo");
    assert_eq!(opts.first, Some(RangeArg::Prefix("git".into())));
    assert_eq!(opts.last, Some(RangeArg::Prefix("cargo".into())));
  }

  #[test]
  fn fc_mixed_number_and_prefix() {
    let _g = TestGuard::new();
    let (_, opts) = parse("fc 5 git");
    assert_eq!(opts.first, Some(RangeArg::Number(5)));
    assert_eq!(opts.last, Some(RangeArg::Prefix("git".into())));
  }

  // ─── -s + replacement form: old=new ──────────────────────────────────

  #[test]
  fn fc_dash_s_with_replacement() {
    let _g = TestGuard::new();
    let (_, opts) = parse("fc -s foo=bar");
    assert_eq!(opts.replace, Some(("foo".into(), "bar".into())));
  }

  #[test]
  fn fc_dash_s_replacement_with_escaped_equals() {
    // `\=` should be part of the LHS, not the separator. Single-quote
    // the whole token so the backslash survives shell-level expansion
    // and reaches parse_fc_args literally.
    let _g = TestGuard::new();
    let (_, opts) = parse(r"fc -s 'foo\=baz=bar'");
    assert_eq!(opts.replace, Some((r"foo\=baz".into(), "bar".into())));
  }

  #[test]
  fn fc_dash_s_without_equals_is_treated_as_range_arg() {
    // No `=` in the word: the replacement branch falls through, and
    // the word goes to the range-arg catch-all (becoming a Prefix).
    let _g = TestGuard::new();
    let (_, opts) = parse("fc -s plainword");
    assert_eq!(opts.mode, FixMode::Rerun);
    assert!(opts.replace.is_none());
    assert_eq!(opts.first, Some(RangeArg::Prefix("plainword".into())));
  }

  #[test]
  fn fc_dash_s_second_replacement_goes_to_non_opts() {
    let _g = TestGuard::new();
    let (non_opts, opts) = parse("fc -s a=b c=d");
    assert_eq!(opts.replace, Some(("a".into(), "b".into())));
    assert_eq!(non_opts.len(), 1);
    assert_eq!(non_opts[0].0, "c=d");
  }

  // ─── -- terminator ────────────────────────────────────────────────────

  #[test]
  fn fc_double_dash_collects_remaining_as_non_opts() {
    let _g = TestGuard::new();
    let (non_opts, opts) = parse("fc -l -- -r foo 42");
    assert_eq!(opts.mode, FixMode::List);
    // Everything after `--`, including the literal `--`, lands in non_opts.
    let collected: Vec<&str> = non_opts
      .iter()
      .map(|(s, _)| s.to_str().unwrap_or_default())
      .collect();
    assert_eq!(collected, vec!["--", "-r", "foo", "42"]);
    // Importantly: -r AFTER -- should NOT have set the reverse flag.
    assert!(!opts.reverse);
  }
}

#[cfg(test)]
mod fc_edit_tests {
  use super::*;
  use crate::readline::History;
  use crate::state::{
    self, Shed,
    vars::{VarFlags, VarKind},
  };
  use crate::tests::testutil::TestGuard;
  use std::os::unix::fs::PermissionsExt;
  use std::path::{Path, PathBuf};
  use tempfile::TempDir;

  /// Drop and re-init the history table so each test starts clean (the
  /// in-memory sqlite conn is shared across tests in the thread).
  fn fresh_history() -> History {
    let conn = state::util::get_db_conn().expect("test db conn");
    let _ = conn
      .lock()
      .unwrap()
      .execute_batch("DROP TABLE IF EXISTS shed_history");
    let _ = conn
      .lock()
      .unwrap()
      .execute_batch("PRAGMA user_version = 0");
    History::new(conn, "shed_history").expect("history init")
  }

  /// New View over the same DB without dropping data. For asserting on
  /// what's in history after `fc_edit` consumed the previous handle.
  fn hist_view() -> History {
    let conn = state::util::get_db_conn().unwrap();
    History::new(conn, "shed_history").unwrap()
  }

  fn unset_editor_vars() {
    Shed::vars_mut(|v| {
      v.unset_var("EDITOR").ok();
      v.unset_var("FCEDIT").ok();
    });
  }

  fn set_shell_var(name: &str, val: &str) {
    Shed::vars_mut(|v| {
      v.set_var(name, VarKind::Str(val.into()), VarFlags::empty())
        .unwrap();
    });
  }

  /// Writes a small shell script to a temp dir that, when invoked with a
  /// path arg, overwrites that file with `new_content` (plus a trailing
  /// newline that `fc_edit`'s `.trim()` strips). Returns the `TempDir`
  /// (keep alive!) and the script path.
  fn overwriting_editor(new_content: &str) -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("editor.sh");
    let script = format!("#!/bin/sh\nprintf '%s\\n' \"{new_content}\" > \"$1\"\n");
    std::fs::write(&path, script).unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    (dir, path)
  }

  fn fc_opts(
    editor: Option<VarStr>,
    first: Option<RangeArg>,
    last: Option<RangeArg>,
  ) -> FixCmdOpts {
    FixCmdOpts {
      editor,
      first,
      last,
      ..Default::default()
    }
  }

  // ─── editor resolution priority ─────────────────────────────────────

  #[test]
  fn fc_edit_no_editor_anywhere_errors() {
    let _g = TestGuard::new();
    unset_editor_vars();
    let hist = fresh_history();
    hist.push("true").unwrap();
    let result = fc_edit(&hist, fc_opts(None, None, None));
    assert!(result.is_err(), "expected error when no editor is set");
  }

  #[test]
  fn fc_edit_uses_opts_editor() {
    let _g = TestGuard::new();
    unset_editor_vars();
    let hist = fresh_history();
    hist.push("true").unwrap();
    fc_edit(&hist, fc_opts(Some("true".into()), None, None))
      .expect("fc_edit should succeed with 'true' as opts.editor");
  }

  #[test]
  fn fc_edit_uses_fcedit_when_opts_unset() {
    let _g = TestGuard::new();
    unset_editor_vars();
    set_shell_var("FCEDIT", "true");
    let hist = fresh_history();
    hist.push("true").unwrap();
    fc_edit(&hist, fc_opts(None, None, None)).expect("FCEDIT should be picked up");
  }

  #[test]
  fn fc_edit_uses_editor_var_as_fallback() {
    let _g = TestGuard::new();
    unset_editor_vars();
    set_shell_var("EDITOR", "true");
    let hist = fresh_history();
    hist.push("true").unwrap();
    fc_edit(&hist, fc_opts(None, None, None)).expect("EDITOR should be picked up");
  }

  #[test]
  fn fc_edit_opts_editor_beats_fcedit_and_editor() {
    // Have opts.editor overwrite with "true", but FCEDIT/EDITOR overwrite
    // with something invalid. If priority were wrong, fc_edit's re-exec
    // of the bad content would surface visibly.
    let _g = TestGuard::new();
    unset_editor_vars();
    let (_d_opts, opts_path) = overwriting_editor(": picked-opts");
    let (_d_fcedit, fcedit_path) = overwriting_editor(": picked-fcedit");
    set_shell_var("FCEDIT", &fcedit_path.to_string_lossy());
    set_shell_var("EDITOR", &fcedit_path.to_string_lossy());
    let hist = fresh_history();
    hist.push(": original").unwrap();
    fc_edit(
      &hist,
      fc_opts(Some(opts_path.to_string_lossy().into()), None, None),
    )
    .unwrap();
    // History should now contain the opts-editor's rewrite.
    let entries = hist_view().query_range(1, 100).unwrap();
    let cmds: Vec<&str> = entries
      .iter()
      .map(|(_, e)| e.command.to_str().unwrap_or_default())
      .collect();
    assert!(cmds.contains(&": picked-opts"), "got: {cmds:?}");
    assert!(!cmds.contains(&": picked-fcedit"), "got: {cmds:?}");
  }

  #[test]
  fn fc_edit_fcedit_beats_editor() {
    let _g = TestGuard::new();
    unset_editor_vars();
    let (_d_fcedit, fcedit_path) = overwriting_editor(": picked-fcedit");
    let (_d_editor, editor_path) = overwriting_editor(": picked-editor");
    set_shell_var("FCEDIT", &fcedit_path.to_string_lossy());
    set_shell_var("EDITOR", &editor_path.to_string_lossy());
    let hist = fresh_history();
    hist.push(": original").unwrap();
    fc_edit(&hist, fc_opts(None, None, None)).unwrap();
    let cmds: Vec<VarStr> = hist_view()
      .query_range(1, 100)
      .unwrap()
      .into_iter()
      .map(|(_, e)| e.command)
      .collect();
    assert!(cmds.iter().any(|c| c == ": picked-fcedit"), "got: {cmds:?}");
    assert!(
      !cmds.iter().any(|c| c == ": picked-editor"),
      "got: {cmds:?}"
    );
  }

  // ─── range iteration ───────────────────────────────────────────────

  /// Editor that records each invocation by appending a line to a log
  /// file. Used to confirm `fc_edit` iterates over every entry in its
  /// range (which can't be checked by counting history pushes, because
  /// `hist_ignore_dupes` drops consecutive identical commands).
  fn tally_editor(log_path: &Path) -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("editor.sh");
    let script = format!(
      "#!/bin/sh\nprintf 'ran\\n' >> {log:?}\n",
      log = log_path.display()
    );
    std::fs::write(&path, script).unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    (dir, path)
  }

  #[test]
  fn fc_edit_iterates_over_range() {
    let _g = TestGuard::new();
    unset_editor_vars();
    let log_dir = TempDir::new().unwrap();
    let log_path = log_dir.path().join("invocations.log");
    let (_d, editor_path) = tally_editor(&log_path);
    let hist = fresh_history();
    hist.push(": one").unwrap();
    hist.push(": two").unwrap();
    hist.push(": three").unwrap();
    fc_edit(
      &hist,
      fc_opts(
        Some(editor_path.to_string_lossy().into()),
        Some(RangeArg::Number(1)),
        Some(RangeArg::Number(3)),
      ),
    )
    .unwrap();
    let log = std::fs::read_to_string(&log_path).unwrap();
    assert_eq!(
      log.lines().count(),
      3,
      "editor should have been invoked once per range entry; log: {log:?}"
    );
  }

  // ─── regression: negative-index resolution off-by-one ──────────────

  #[test]
  fn get_entry_range_negative_one_is_last_entry() {
    // Previously `Number(-1)` resolved to last_id - 1 because of an
    // off-by-one in get_entry_range. Now -1 correctly maps to last_id.
    let _g = TestGuard::new();
    let hist = fresh_history();
    hist.push(": first").unwrap();
    hist.push(": second").unwrap();
    hist.push(": third").unwrap();
    let entries = get_entry_range(
      &hist,
      Some(RangeArg::Number(-1)),
      Some(RangeArg::Number(-1)),
      false,
      false,
    )
    .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].1.command, ": third");
  }

  #[test]
  fn get_entry_range_negative_n_back_from_end() {
    let _g = TestGuard::new();
    let hist = fresh_history();
    hist.push(": a").unwrap();
    hist.push(": b").unwrap();
    hist.push(": c").unwrap();
    hist.push(": d").unwrap();
    // -3 to -1 → 3 entries before the end → b, c, d.
    let entries = get_entry_range(
      &hist,
      Some(RangeArg::Number(-3)),
      Some(RangeArg::Number(-1)),
      false,
      false,
    )
    .unwrap();
    let cmds: Vec<&str> = entries
      .iter()
      .map(|(_, e)| e.command.to_str().unwrap_or_default())
      .collect();
    assert_eq!(cmds, vec![": b", ": c", ": d"]);
  }
}

#[cfg(test)]
mod fc_run_builtin_tests {
  //! Tests for the `fc` builtin's `run_builtin` routing — verifies it
  //! dispatches to `fc_list` / `fc_reexec` / `fc_edit` based on opts.

  use crate::readline::History;
  use crate::state::{self, Shed};
  use crate::tests::testutil::{TestGuard, test_input};

  fn fresh_history() -> History {
    let conn = state::util::get_db_conn().expect("test db");
    let _ = conn
      .lock()
      .unwrap()
      .execute_batch("DROP TABLE IF EXISTS shed_history");
    let _ = conn
      .lock()
      .unwrap()
      .execute_batch("PRAGMA user_version = 0");
    History::new(conn, "shed_history").expect("history init")
  }

  // ─── opts.list path → fc_list ────────────────────────────────────

  #[test]
  fn fc_dash_l_dispatches_to_fc_list() {
    let g = TestGuard::new();
    let hist = fresh_history();
    hist.push(": entry_one").unwrap();
    hist.push(": entry_two").unwrap();
    test_input("fc -l").unwrap();
    let out = g.read_output();
    assert!(out.contains(": entry_one"), "got: {out:?}");
    assert!(out.contains(": entry_two"), "got: {out:?}");
    assert_eq!(state::Shed::get_status(), 0);
  }

  // ─── opts.no_editor path → fc_reexec ────────────────────────────

  #[test]
  fn fc_dash_s_dispatches_to_fc_reexec() {
    let _g = TestGuard::new();
    let hist = fresh_history();
    hist.push(": prev_cmd").unwrap();
    // `fc -s` re-executes the previous command. With ":" it's harmless.
    test_input("fc -s").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  // ─── default path → fc_edit ─────────────────────────────────────

  #[test]
  fn fc_default_dispatches_to_fc_edit() {
    // With opts.editor=Some(true) the edit path leaves content
    // unchanged and re-executes the original.
    let _g = TestGuard::new();
    let hist = fresh_history();
    hist.push(": prev").unwrap();
    // Force FCEDIT to "true" so fc_edit picks a no-op editor.
    Shed::vars_mut(|v| {
      v.set_var(
        "FCEDIT",
        crate::state::vars::VarKind::Str("true".into()),
        crate::state::vars::VarFlags::empty(),
      )
      .unwrap();
    });
    test_input("fc").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }
}

#[cfg(test)]
mod fc_reexec_tests {
  use crate::readline::History;
  use crate::state;
  use crate::tests::testutil::{TestGuard, test_input};

  fn fresh_history() -> History {
    let conn = state::util::get_db_conn().expect("test db");
    let _ = conn
      .lock()
      .unwrap()
      .execute_batch("DROP TABLE IF EXISTS shed_history");
    let _ = conn
      .lock()
      .unwrap()
      .execute_batch("PRAGMA user_version = 0");
    History::new(conn, "shed_history").expect("history init")
  }

  fn hist_view() -> History {
    let conn = state::util::get_db_conn().unwrap();
    History::new(conn, "shed_history").unwrap()
  }

  // ─── `fc -s` re-executes the previous command ──────────────────

  #[test]
  fn fc_s_reexecutes_previous_command() {
    let g = TestGuard::new();
    let hist = fresh_history();
    hist.push("echo prev_output").unwrap();
    test_input("fc -s").unwrap();
    let out = g.read_output();
    assert!(out.contains("prev_output"), "got: {out:?}");
    assert_eq!(state::Shed::get_status(), 0);
  }

  // ─── `fc -s old=new` substitutes and re-executes ───────────────

  #[test]
  fn fc_s_with_substitution_replaces_and_pushes() {
    let g = TestGuard::new();
    let hist = fresh_history();
    hist.push("echo original_marker").unwrap();
    let before = hist.last_id();
    test_input("fc -s original_marker=replaced_marker").unwrap();
    let out = g.read_output();
    assert!(out.contains("replaced_marker"), "got: {out:?}");
    // The substituted form should be pushed to history.
    let after = hist_view().last_id();
    assert!(after > before, "expected new history entry");
    let last = hist_view()
      .query_range(after, after)
      .unwrap()
      .into_iter()
      .next()
      .unwrap()
      .1
      .command;
    assert!(last.to_str_lossy().contains("replaced_marker"));
  }

  #[test]
  fn fc_s_with_substitution_no_match_does_not_push() {
    let _g = TestGuard::new();
    let hist = fresh_history();
    hist.push(": something").unwrap();
    let before = hist.last_id();
    // Pattern doesn't match → command unchanged → should_push stays false.
    test_input("fc -s zzz_no_match=replacement").unwrap();
    let after = hist_view().last_id();
    assert_eq!(after, before, "no substitution → no push");
  }

  // ─── ranges ─────────────────────────────────────────────────────

  #[test]
  fn fc_s_with_range_reexecutes_each() {
    let g = TestGuard::new();
    let hist = fresh_history();
    hist.push("echo one").unwrap();
    hist.push("echo two").unwrap();
    hist.push("echo three").unwrap();
    // Re-execute entries 1..=3 (all three).
    test_input("fc -s 1 3").unwrap();
    let out = g.read_output();
    assert!(out.contains("one"), "got: {out:?}");
    assert!(out.contains("two"), "got: {out:?}");
    assert!(out.contains("three"), "got: {out:?}");
  }
}
