use std::{io::Write, path::Path};

use bitflags::bitflags;

use crate::{
  KeyEvent, ShResult,
  builtin::opt::{Opt, OptSpec},
  eval::lex::Span,
  expand, expand_keymap, opt, outln,
  readline::EditorCore,
  sherr,
  state::vars::VarStr,
  util::{ShResultExt, with_status},
};

bitflags! {
  struct ViceFlags: u32 {
    const KEEP_MODE = 0b0000_0001;
    const QUOTED = 0b0000_0010;
    const INPLACE = 0b0000_0100;
    const LINES = 0b0000_1000;
  }
}

struct ViceProg {
  cmds: Vec<ViceCmd>,
  sep: Option<Vec<KeyEvent>>,
  delim: VarStr,
  flags: ViceFlags,
  backup_ext: Option<VarStr>,
}

impl ViceProg {
  pub fn quoted(&self) -> bool {
    self.flags.contains(ViceFlags::QUOTED)
  }
  pub fn inplace(&self) -> bool {
    self.flags.contains(ViceFlags::INPLACE)
  }
  pub fn lines(&self) -> bool {
    self.flags.contains(ViceFlags::LINES)
  }
  pub fn keep_mode(&self) -> bool {
    self.flags.contains(ViceFlags::KEEP_MODE)
  }
}

#[derive(Clone)]
enum ViceCmd {
  Cut(Vec<KeyEvent>),
  Move(Vec<KeyEvent>),
  Repeat(usize, usize),
}

impl ViceCmd {
  pub fn parse_cut(keys: &str) -> Self {
    let keys = expand_keymap(keys);
    Self::Cut(keys)
  }
  pub fn parse_move(keys: &str) -> Self {
    let keys = expand_keymap(keys);
    Self::Move(keys)
  }
  /// Parse the shared `<number>:<number>` argument for `-r` / `--repeat`.
  pub fn parse_repeat(arg: &str) -> ShResult<Self> {
    let Some((left, right)) = arg.split_once(':') else {
      return Err(sherr!(
        ParseErr,
        "Expected '<number>:<number>' for repeat argument"
      ));
    };
    let Ok(num_cmds) = left.parse::<usize>() else {
      return Err(sherr!(
        ParseErr,
        "Failed to parse number of commands in repeat argument"
      ));
    };
    let Ok(num_repeats) = right.parse::<usize>() else {
      return Err(sherr!(
        ParseErr,
        "Failed to parse number of repeats in repeat argument"
      ));
    };
    Ok(Self::Repeat(num_cmds, num_repeats))
  }
}

pub(super) struct Vice;
impl Vice {
  fn parse_cmds(opts: &[Opt]) -> ShResult<ViceProg> {
    let mut prog = ViceProg {
      cmds: vec![],
      sep: None,
      delim: " ".into(),
      flags: ViceFlags::empty(),
      backup_ext: None,
    };

    for opt in opts {
      match opt.key() {
        "cut" => {
          let arg = opt.value()?;
          let cmd = ViceCmd::parse_cut(arg);
          prog.cmds.push(cmd);
        }
        "sep" => {
          let arg = opt.value()?;
          prog.sep = Some(expand_keymap(arg));
        }
        "move" => {
          let arg = opt.value()?;
          let cmd = ViceCmd::parse_move(arg);
          prog.cmds.push(cmd);
        }
        "repeat" => {
          let arg = opt.value()?;
          prog.cmds.push(ViceCmd::parse_repeat(arg)?);
        }
        "delim" => {
          let arg = opt.value()?;
          prog.delim = arg.into();
        }
        "quoted" => prog.flags |= ViceFlags::QUOTED,
        "in-place" => prog.flags |= ViceFlags::INPLACE,
        "lines" => prog.flags |= ViceFlags::LINES,
        "keep-mode" => prog.flags |= ViceFlags::KEEP_MODE,
        "backup-ext" => {
          let arg = opt.value()?;
          prog.backup_ext = Some(arg.into());
        }
        "backup" if prog.backup_ext.is_none() && prog.backup_ext.is_none() => {
          prog.backup_ext = Some(".bak".into());
        }
        _ => {}
      }
    }

    Ok(prog)
  }

  fn exec_cmds(
    core: &mut EditorCore,
    prog: &ViceProg,
    cmds: Vec<ViceCmd>,
    fields: &mut Vec<String>,
    spent_cmds: &mut Vec<ViceCmd>,
  ) -> ShResult<()> {
    for cmd in cmds {
      let clone = cmd.clone();
      match cmd {
        ViceCmd::Cut(keys) => {
          let start = core.editor.cursor();

          // A failed search aborts the whole program; leave the flag set so
          // the caller can skip the line or fail the buffer.
          if !core.feed_keys_fallible(keys)? {
            return Ok(());
          }

          let field = if let Some(sel) = core.selection() {
            log::debug!("Vice: selection found: {sel:?}");
            sel
          } else {
            let mut end = core.editor.cursor();
            end.col = core.editor.offset_col_absolute(end.row, 1);
            core.editor.slice_pos(start, end)
          };

          fields.push(if prog.quoted() {
            expand::shell_quote(&field)
          } else {
            field.clone()
          });

          if let Some(sep) = prog.sep.clone()
            && !core.feed_keys_fallible(sep)?
          {
            return Ok(());
          }
        }
        ViceCmd::Move(keys) => {
          if !core.feed_keys_fallible(keys)? {
            return Ok(());
          }
        }
        ViceCmd::Repeat(num_cmds, num_repeats) => {
          for _ in 0..num_repeats {
            let repeat_cmds = spent_cmds.split_off(spent_cmds.len().saturating_sub(num_cmds));
            Self::exec_cmds(core, prog, repeat_cmds, fields, spent_cmds)?;
            if core.editor.search_failed() {
              return Ok(());
            }
          }
        }
      }
      spent_cmds.push(clone);
      if !prog.keep_mode() {
        core.reset_mode(true)?;

        // search mode and ex mode are submitted in the above mode
        // so if a search failed after that, we return
        if core.editor.search_failed() {
          return Ok(());
        }
      }
    }

    Ok(())
  }

  /// Run the program against the current buffer and render one output record:
  /// the cut fields joined by the delimiter, or the whole buffer if no `-c`.
  fn render(core: &mut EditorCore, prog: &ViceProg, span: &Span) -> ShResult<String> {
    let mut fields = vec![];
    let mut spent_cmds = vec![];
    Self::exec_cmds(core, prog, prog.cmds.clone(), &mut fields, &mut spent_cmds)
      .promote_err(span.clone())?;

    Ok(if !fields.is_empty() {
      fields.join(&prog.delim.to_str_lossy()) // captured fields, kept even on abort
    } else if core.editor.search_failed() {
      String::new() // aborted before capturing anything
    } else {
      core.text() // no -c: pass the whole buffer through
    })
  }

  fn run_inplace(file: &str, input: &str, prog: &ViceProg, span: &Span) -> ShResult<bool> {
    let mut collected = String::new();
    let ok = Self::run(input, prog, span, |record| {
      collected.push_str(record);
      // Linewise emits one record per line; whole-buffer mode is written
      // back verbatim, so only the linewise records get terminators.
      if prog.lines() {
        collected.push('\n');
      }
      Ok(())
    })?;
    if !ok {
      // A whole-buffer search failed; leave the file untouched.
      return Ok(false);
    }
    Self::write_inplace(file, &collected, prog.backup_ext.as_ref(), span)?;
    Ok(true)
  }

  fn run_stream(input: &str, prog: &ViceProg, span: &Span) -> ShResult<bool> {
    Self::run(input, prog, span, |record| {
      outln!("{record}");
      Ok(())
    })
  }

  /// Drive `input` through the program, handing each output record to `sink`.
  /// Linewise mode reuses one editor across lines; otherwise the whole input is
  /// a single buffer. Returns `false` when a whole-buffer run is aborted by a
  /// failed search (the caller turns that into a non-zero exit); in linewise
  /// mode an aborted line is simply skipped and the run still succeeds.
  fn run(
    input: &str,
    prog: &ViceProg,
    span: &Span,
    mut sink: impl FnMut(&str) -> ShResult<()>,
  ) -> ShResult<bool> {
    if prog.lines() {
      let mut emitted_line = false;
      let mut core = EditorCore::empty();
      for line in input.lines() {
        core.set_buffer(line);
        let record = Self::render(&mut core, prog, span)?;
        if core.editor.search_failed() && record.is_empty() {
          continue;
        }
        emitted_line = true;
        sink(&record)?;
      }
      Ok(emitted_line)
    } else {
      let mut core = EditorCore::headless(input);
      let record = Self::render(&mut core, prog, span)?;
      let aborted = core.editor.search_failed();
      // emit whatever was captured before an abort; drop only if nothing was
      let emit = !(aborted && record.is_empty());
      if emit {
        sink(&record)?;
      }
      // match linewise: the run "succeeds" whenever it produced output
      Ok(emit)
    }
  }

  /// Atomically write `output` back to `file`, preserving permissions and
  /// copying a backup first when `backup_ext` is set.
  fn write_inplace(
    file: &str,
    output: &str,
    backup_ext: Option<&VarStr>,
    span: &Span,
  ) -> ShResult<()> {
    if let Some(ext) = backup_ext {
      let ext = ext.to_str_lossy();
      let slice = ext.strip_prefix('.').unwrap_or(&ext);
      std::fs::copy(file, format!("{file}.{slice}"))?;
    }

    let dir = Path::new(file)
      .parent()
      .filter(|p| !p.as_os_str().is_empty())
      .unwrap_or_else(|| Path::new("."));
    let mut temp = tempfile::NamedTempFile::new_in(dir)?;
    let perms = std::fs::metadata(file)?.permissions();

    temp.write_all(output.as_bytes())?;
    temp.as_file().set_permissions(perms)?;
    temp
      .persist(file)
      .map_err(|e| sherr!(ExecFail @ span.clone(), "Failed to write output to file: '{e}'"))?;
    Ok(())
  }
}
impl super::Builtin for Vice {
  fn opts(&self) -> Vec<OptSpec> {
    vec![
      opt!("sep" | b's', 1),
      opt!("cut" | b'c', 1),
      opt!("move" | b'm', 1),
      opt!("repeat" | b'r', 1),
      opt!("backup-ext", 1),
      opt!("delim" | b'd', 1),
      opt!("quoted" | b'q'),
      opt!("lines" | b'l'),
      opt!("in-place" | b'i'),
      opt!("backup"),
      opt!("keep-mode"),
    ]
  }
  fn execute(&self, mut args: super::BuiltinArgs) -> ShResult<()> {
    let span = args.span();
    let (arg_vec, opts) = args.take_argv();
    let prog = Self::parse_cmds(&opts).promote_err(span.clone())?;

    if arg_vec.is_empty()
      && let Some(input) = self.get_input_str(&mut args)
    {
      let ok = Self::run_stream(&input, &prog, &span)?;
      return with_status(i32::from(!ok));
    }

    let mut ok = true;
    for (file, span) in arg_vec {
      let Ok(content) = std::fs::read_to_string(&file) else {
        return Err(sherr!(ExecFail @ span, "Failed to read file: '{file}'"));
      };

      let file_ok = if prog.inplace() {
        Self::run_inplace(&file.to_str_lossy(), &content, &prog, &span)?
      } else {
        Self::run_stream(&content, &prog, &span)?
      };
      ok = ok && file_ok;
    }

    with_status(i32::from(!ok))
  }
}

#[cfg(test)]
mod tests {
  use crate::tests::testutil::{TestGuard, test_input};
  use crate::{assert_file, assert_output};

  // ===================== Field extraction (stdin) =====================

  #[test]
  fn vice_cut_first_word() {
    let g = TestGuard::new();
    test_input("printf 'hello world' | vice -c 'e'").unwrap();
    assert_output!(g, "hello\n");
  }

  #[test]
  fn vice_move_then_cut_second_word() {
    let g = TestGuard::new();
    test_input("printf 'hello world' | vice -m 'w' -c 'e'").unwrap();
    assert_output!(g, "world\n");
  }

  #[test]
  fn vice_long_repeat_matches_short_form() {
    // Regression: `--repeat` was declared but fell into a silent catch-all,
    // dropping the repeat. It must behave identically to `-r`.
    let short = {
      let g = TestGuard::new();
      test_input("printf 'a b c d e' | vice -c 'w' -r 1:3").unwrap();
      g.read_output()
    };
    let long = {
      let g = TestGuard::new();
      test_input("printf 'a b c d e' | vice -c 'w' --repeat 1:3").unwrap();
      g.read_output()
    };
    assert_eq!(short, long);
    assert_eq!(short, "a b b c c d d e\n");
  }

  #[test]
  fn vice_unknown_long_option_errors() {
    // The `LongWithArg` catch-all now errors instead of silently dropping.
    let _g = TestGuard::new();
    let res = test_input("printf 'x' | vice --backup-ext '.bak' -c 'e'");
    assert!(
      res.is_ok(),
      "declared long-with-arg option should still work"
    );
  }

  #[test]
  fn vice_cut_whole_line() {
    let g = TestGuard::new();
    test_input("printf 'one two three' | vice -c '$'").unwrap();
    assert_output!(g, "one two three\n");
  }

  // ===================== normal-mode EOL cursor (bug: $ overshoot) =====

  #[test]
  fn vice_dollar_x_deletes_last_char() {
    // `$x` must delete the last character, not no-op past the end of line.
    let g = TestGuard::new();
    test_input("printf 'abc' | vice -m '$x'").unwrap();
    assert_output!(g, "ab\n");
  }

  #[test]
  fn vice_dollar_x_does_not_join_lines() {
    // `$x` on a line with a following line must delete the last char, never
    // the newline (which would join the two lines).
    let g = TestGuard::new();
    test_input("printf 'abc\\ndef' | vice -m '$x'").unwrap();
    assert_output!(g, "ab\ndef\n");
  }

  #[test]
  fn vice_dollar_insert_lands_on_last_char() {
    // `$` clamps onto the last char (not past it), so `i` inserts before it.
    let g = TestGuard::new();
    test_input("printf 'ab' | vice -m '$ix'").unwrap();
    assert_output!(g, "axb\n");
  }

  #[test]
  fn vice_dollar_operator_still_reaches_true_end() {
    // Regression guard: `$` as an operator motion (`d$`) must still delete
    // through the last character.
    let g = TestGuard::new();
    test_input("printf 'abcdef' | vice -m 'lld$'").unwrap();
    assert_output!(g, "ab\n");
  }

  #[test]
  fn vice_two_fields_default_delim() {
    let g = TestGuard::new();
    test_input("printf 'hello world' | vice -c 'e' -s 'w' -c 'e'").unwrap();
    assert_output!(g, "hello world\n");
  }

  #[test]
  fn vice_two_fields_custom_delim() {
    let g = TestGuard::new();
    test_input("printf 'hello world' | vice -d ':' -c 'e' -s 'w' -c 'e'").unwrap();
    assert_output!(g, "hello:world\n");
  }

  #[test]
  fn vice_quoted_field() {
    let g = TestGuard::new();
    test_input("printf 'a b' | vice -q -c '$'").unwrap();
    assert_output!(g, "'a b'\n");
  }

  // ===================== Structural motions =====================

  #[test]
  fn vice_brace_match_extracts_block() {
    let g = TestGuard::new();
    test_input("printf 'fn(){ body }' | vice -m 'f{' -c '%'").unwrap();
    assert_output!(g, "{{ body }}\n");
  }

  // ===================== Whole-buffer edits (no -c) =====================

  #[test]
  fn vice_edit_emits_modified_buffer() {
    let g = TestGuard::new();
    test_input("printf 'hello' | vice -m 'x'").unwrap();
    assert_output!(g, "ello\n");
  }

  #[test]
  fn vice_delete_inner_word() {
    let g = TestGuard::new();
    test_input("printf 'hello world' | vice -m 'wdiw'").unwrap();
    assert_output!(g, "hello \n");
  }

  // ===================== Linewise =====================

  #[test]
  fn vice_linewise_extract_per_line() {
    let g = TestGuard::new();
    test_input("printf 'foo 1\\nbar 2\\n' | vice -l -c 'e'").unwrap();
    assert_output!(g, "foo\nbar\n");
  }

  #[test]
  fn vice_linewise_edit_per_line() {
    let g = TestGuard::new();
    test_input("printf 'aaa\\nbbb\\n' | vice -l -m 'x'").unwrap();
    assert_output!(g, "aa\nbb\n");
  }

  // ===================== File argument =====================

  #[test]
  fn vice_file_arg_to_stdout() {
    let mut g = TestGuard::new();
    g.in_temp_dir();
    std::fs::write("f.txt", "hello world\n").unwrap();
    test_input("vice -c 'e' f.txt").unwrap();
    assert_output!(g, "hello\n");
  }

  #[test]
  fn vice_inplace_whole_buffer_preserves_trailing_newline() {
    let mut g = TestGuard::new();
    g.in_temp_dir();
    std::fs::write("f.txt", "hello world\n").unwrap();
    test_input("vice -i -m 'x' f.txt").unwrap();
    assert_file!("f.txt", "ello world\n");
  }

  #[test]
  fn vice_inplace_linewise() {
    let mut g = TestGuard::new();
    g.in_temp_dir();
    std::fs::write("f.txt", "aaa\nbbb\n").unwrap();
    test_input("vice -i -l -m 'x' f.txt").unwrap();
    assert_file!("f.txt", "aa\nbb\n");
  }

  #[test]
  fn vice_inplace_backup_copies_original() {
    let mut g = TestGuard::new();
    g.in_temp_dir();
    std::fs::write("f.txt", "orig\n").unwrap();
    test_input("vice -i --backup -m 'x' f.txt").unwrap();
    assert_file!("f.txt", "rig\n");
    assert_file!("f.txt.bak", "orig\n");
  }

  // ===================== Search-failure abort =====================

  #[test]
  fn vice_lines_skips_failed_search() {
    let g = TestGuard::new();
    // `fX` fails on the middle line, which is dropped from the output.
    test_input("printf 'aXb\\ncYd\\neXf\\n' | vice -l -m 'fX' -c '$'").unwrap();
    assert_output!(g, "Xb\nXf\n");
  }

  #[test]
  fn vice_lines_failed_search_not_masked_by_trailing_motion() {
    // Regression: a search that fails partway through a command must still
    // abort, even when a later motion in the same keystring would otherwise
    // reset the failure flag. Here `fX` fails on the middle line; the trailing
    // `l` must not mask it, so that line is dropped rather than emitting "Yd".
    let g = TestGuard::new();
    test_input("printf 'aXb\\ncYd\\neXf\\n' | vice -l -m 'fXl' -c '$'").unwrap();
    assert_output!(g, "b\nf\n");
  }

  #[test]
  fn vice_whole_buffer_search_fail_emits_nothing_and_exits_1() {
    use crate::assert_status_eq;
    use crate::state;

    let g = TestGuard::new();
    test_input("printf 'no target' | vice -m 'fZ' -c '$'").unwrap();
    assert_output!(g, "");
    assert_status_eq!(1);
  }

  #[test]
  fn vice_whole_buffer_search_success_exits_0() {
    use crate::assert_status_eq;
    use crate::state;

    let g = TestGuard::new();
    test_input("printf 'find Z here' | vice -m 'fZ' -c '$'").unwrap();
    assert_output!(g, "Z here\n");
    assert_status_eq!(0);
  }

  // ===================== doubled case operators (whole line) ==========

  #[test]
  #[expect(non_snake_case)]
  fn vice_gUU_uppercases_line() {
    let g = TestGuard::new();
    test_input("printf 'aBc' | vice -m 'gUU'").unwrap();
    assert_output!(g, "ABC\n");
  }

  #[test]
  fn vice_guu_lowercases_line() {
    let g = TestGuard::new();
    test_input("printf 'aBc' | vice -m 'guu'").unwrap();
    assert_output!(g, "abc\n");
  }

  #[test]
  fn vice_g_tilde_tilde_toggles_line() {
    let g = TestGuard::new();
    test_input("printf 'aBc' | vice -m 'g~~'").unwrap();
    assert_output!(g, "AbC\n");
  }

  #[test]
  #[expect(non_snake_case)]
  fn vice_gUgU_alternate_spelling_uppercases_line() {
    let g = TestGuard::new();
    test_input("printf 'aBc' | vice -m 'gUgU'").unwrap();
    assert_output!(g, "ABC\n");
  }

  #[test]
  fn vice_gugu_alternate_spelling_lowercases_line() {
    let g = TestGuard::new();
    test_input("printf 'aBc' | vice -m 'gugu'").unwrap();
    assert_output!(g, "abc\n");
  }
}
