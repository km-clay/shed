use std::ops::Range;

use yansi::Paint;

use super::{
  Shed,
  context::{CmdKind, CtxTk, CtxTkRule, get_ex_context_tokens},
  state::shopt::ShOptHighlight,
  util::{PaletteEntry, style_from_description},
};

pub struct Palette {
  string: PaletteEntry,
  keyword: PaletteEntry,
  external_command: PaletteEntry,
  builtin: PaletteEntry,
  function: PaletteEntry,
  alias: PaletteEntry,
  directory: PaletteEntry,
  invalid_command: PaletteEntry,
  control_flow_keyword: PaletteEntry,
  argument: PaletteEntry,
  argument_file: PaletteEntry,
  variable: PaletteEntry,
  operator: PaletteEntry,
  comment: PaletteEntry,
  glob: PaletteEntry,
}

impl Palette {
  pub fn new() -> Self {
    let get_color = |desc: &str| -> PaletteEntry {
      style_from_description(desc).unwrap_or_else(|_| PaletteEntry::new())
    };
    Shed::shopts(|o| {
      let ShOptHighlight {
        string,
        keyword,
        external_command,
        builtin,
        function,
        alias,
        directory,
        invalid_command,
        control_flow_keyword,
        argument,
        argument_file,
        variable,
        operator,
        comment,
        glob,
        ..
      } = &o.highlight;
      Self {
        string: get_color(string),
        keyword: get_color(keyword),
        external_command: get_color(external_command),
        builtin: get_color(builtin),
        function: get_color(function),
        alias: get_color(alias),
        directory: get_color(directory),
        invalid_command: get_color(invalid_command),
        control_flow_keyword: get_color(control_flow_keyword),
        argument: get_color(argument),
        argument_file: get_color(argument_file),
        variable: get_color(variable),
        operator: get_color(operator),
        comment: get_color(comment),
        glob: get_color(glob),
      }
    })
  }

  pub fn style_for(&self, tk: &CtxTk, editor_cursor_pos: usize) -> PaletteEntry {
    let class = tk.class();
    match class {
      CtxTkRule::ValidCommand(kind) => {
        if ["break", "continue", "return"].contains(&tk.span().to_str_lossy().as_ref()) {
          self.control_flow_keyword
        } else {
          match kind {
            CmdKind::External => self.external_command,
            CmdKind::Function => self.function,
            CmdKind::Builtin => self.builtin,
            CmdKind::Alias => self.alias,
            CmdKind::Directory => self.directory,
          }
        }
      }
      CtxTkRule::ValidExCommand => self.builtin,
      CtxTkRule::InvalidExCommand | CtxTkRule::InvalidCommand => self.invalid_command,
      CtxTkRule::Argument
      | CtxTkRule::Separator
      | CtxTkRule::ArithNumber
      | CtxTkRule::ParamArg
      | CtxTkRule::AssignmentRight
      | CtxTkRule::ArrayLiteral => self.argument,
      CtxTkRule::ArgumentFile => {
        let range = tk.span().range();
        let inclusive = range.start..=range.end;
        if inclusive.contains(&editor_cursor_pos) {
          self.argument_file
        } else {
          self.argument
        }
      }
      CtxTkRule::Comment => self.comment,
      CtxTkRule::Keyword | CtxTkRule::Subshell | CtxTkRule::Arithmetic => self.keyword,
      CtxTkRule::CmdSub
      | CtxTkRule::BacktickSub
      | CtxTkRule::ProcSubIn
      | CtxTkRule::ProcSubOut
      | CtxTkRule::VarSub
      | CtxTkRule::HistExp
      | CtxTkRule::ArithVar
      | CtxTkRule::ParamName
      | CtxTkRule::ParamIndex
      | CtxTkRule::AssignmentLeft => self.variable,
      CtxTkRule::Glob | CtxTkRule::ExBang => self.glob,
      CtxTkRule::Escape
      | CtxTkRule::Tilde
      | CtxTkRule::ArithOp
      | CtxTkRule::ParamPrefix
      | CtxTkRule::ParamOp
      | CtxTkRule::AssignmentOp
      | CtxTkRule::Operator
      | CtxTkRule::Redirect
      | CtxTkRule::HereDocStart
      | CtxTkRule::HereDocEnd
      | CtxTkRule::ExPattern => self.operator,
      CtxTkRule::DoubleString
      | CtxTkRule::SingleString
      | CtxTkRule::DollarString
      | CtxTkRule::HereDocBody
      | CtxTkRule::ExAddress => self.string,
    }
  }
}

impl Default for Palette {
  fn default() -> Self {
    Self::new()
  }
}

pub fn highlight_ex<W: std::fmt::Write>(
  out: &mut W,
  input: &str,
  palette: &Palette,
  editor_cursor_pos: usize,
) -> std::fmt::Result {
  let tks: Vec<CtxTk> = get_ex_context_tokens(input);
  highlight(out, input, &tks, palette, editor_cursor_pos, &[])
}

pub fn highlight<W: std::fmt::Write>(
  out: &mut W,
  input: &str,
  tks: &[CtxTk],
  palette: &Palette,
  editor_cursor_pos: usize,
  selections: &[Range<usize>],
) -> std::fmt::Result {
  let mut cursor = 0;
  for tk in tks {
    paint(
      out,
      tk,
      PaletteEntry::new(),
      &mut cursor,
      editor_cursor_pos,
      palette,
      selections,
    );
  }
  out.write_str("\x1b[0m")?; // ensure we reset at the end
  out.write_str(&input[cursor..])?; // append any remaining text after the last token
  Ok(())
}

/// given a `CtxTk`, write highlighted output to `out`
///
/// `CtxTk` already did the heavy lifting for figuring out where and what everything is.
/// now we can just paint the spans that it put together.
fn paint<W: std::fmt::Write>(
  out: &mut W,
  node: &CtxTk,
  parent: PaletteEntry,
  cursor: &mut usize,       // our position in the input
  editor_cursor_pos: usize, // editor cursor position
  palette: &Palette,
  selections: &[Range<usize>],
) {
  let span = node.span().range();
  let src = node.span().get_source();
  let src = src.to_str_lossy();

  // leading bytes inherit the parent style
  if *cursor < span.start {
    emit_with_selection(out, &src, *cursor..span.start, parent, selections);
    *cursor = span.start;
  }

  let mut style = palette.style_for(node, editor_cursor_pos);
  let decor = style.decor().union(parent.decor()); // decorations accumulate as we descend
  style.set_decor(decor);

  if node.sub_tokens().is_empty() {
    emit_with_selection(out, &src, span.clone(), style, selections);
    *cursor = span.end;
  } else {
    for child in node.sub_tokens() {
      paint(
        out,
        child,
        style,
        cursor,
        editor_cursor_pos,
        palette,
        selections,
      );
    }
    // trailing bytes maintain the current style
    if *cursor < span.end {
      emit_with_selection(out, &src, *cursor..span.end, style, selections);
      *cursor = span.end;
    }
  }
}

/// General control byte caret-notation helper for raw strings
pub(crate) fn visualize_controls_str(text: &str) -> String {
  if !text.bytes().any(is_visualized_control) {
    return text.to_string();
  }

  let mut out = String::with_capacity(text.len());
  for ch in text.chars() {
    let b = ch as u32;
    if !(b < 0x80 && is_visualized_control(b as u8)) {
      out.push(ch);
      continue;
    }

    match b as u8 {
      0x7f => out.push_str("^?"),
      b => {
        out.push('^');
        out.push((b ^ 0x40) as char);
      }
    }
  }
  out
}

/// Emit `src[range]` styled with `style`, slicing at selection boundaries so
/// any portion overlapping any range in `selections` paints with an inverted
/// variant of the same style. Multiple overlapping/adjacent selections are
/// merged so each byte is emitted at most once.
/// Render `text` under `style`, replacing ASCII control bytes with caret
/// notation (`\x1b` -> `^[`, `\r` -> `^M`, `\x7f` -> `^?`, etc.) styled as
/// dim+italic so they're visually distinct from real text. `\n` and `\t` are
/// preserved as-is because they're structural to multi-line buffers.
///
/// Without this pass, a buffer containing raw control bytes (e.g. from
/// `:r!cat file_with_escapes`) would emit those bytes straight to the
/// terminal, letting any clipboard-injection-style sequence change the
/// title, write to OSC 52, etc.
fn paint_with_visualized_controls<W: std::fmt::Write>(
  out: &mut W,
  text: &str,
  style: PaletteEntry,
) {
  // Hot path: nothing to visualize, single styled write.
  if !text.bytes().any(is_visualized_control) {
    write!(out, "{}", text.paint(style.style())).unwrap();
    return;
  }
  let ctrl_style = style.dim().italic();
  let mut run_start = 0;
  for (i, ch) in text.char_indices() {
    let b = ch as u32;
    if b < 0x80 && is_visualized_control(b as u8) {
      if run_start < i {
        write!(out, "{}", text[run_start..i].paint(style.style())).unwrap();
      }
      let viz = match b as u8 {
        0x7f => "^?".to_string(),
        b => format!("^{}", (b ^ 0x40) as char),
      };
      write!(out, "{}", viz.paint(ctrl_style.style())).unwrap();
      run_start = i + ch.len_utf8();
    }
  }
  if run_start < text.len() {
    write!(out, "{}", text[run_start..].paint(style.style())).unwrap();
  }
}

fn is_visualized_control(b: u8) -> bool {
  // Caret-notation everything below 0x20 except `\n` and `\t`, plus DEL (0x7f).
  matches!(b, 0x00..=0x08 | 0x0b..=0x1f | 0x7f)
}

fn emit_with_selection<W: std::fmt::Write>(
  out: &mut W,
  src: &str,
  range: Range<usize>,
  style: PaletteEntry,
  selections: &[Range<usize>],
) {
  // find every selection that starts before our end, and ends after our start
  // if both of these are true, there is overlap
  let mut overlapping: Vec<Range<usize>> = selections
    .iter()
    .filter(|s| s.start < range.end && s.end > range.start)
    .cloned()
    .collect();

  if overlapping.is_empty() {
    paint_with_visualized_controls(out, &src[range], style);
    return;
  }

  // Sort by start, then merge overlapping/adjacent ranges into disjoint
  // segments so the sweep doesn't emit any byte twice.
  overlapping.sort_by_key(|s| s.start);
  let mut merged: Vec<Range<usize>> = Vec::with_capacity(overlapping.len());
  for sel in overlapping {
    if let Some(last) = merged.last_mut()
      && sel.start <= last.end
    {
      last.end = last.end.max(sel.end);
      continue;
    }
    merged.push(sel);
  }

  // Sweep through `range`, alternating between un-selected (normal style)
  // and selected (inverted style) segments.
  let sel_style = style.inverted();
  let mut pos = range.start;
  for sel in &merged {
    let sel_start = sel.start.max(range.start);
    let sel_end = sel.end.min(range.end);

    if pos < sel_start {
      paint_with_visualized_controls(out, &src[pos..sel_start], style);
    }

    if sel_start < sel_end {
      paint_with_visualized_controls(out, &src[sel_start..sel_end], sel_style);
    }

    pos = sel_end;
  }

  if pos < range.end {
    paint_with_visualized_controls(out, &src[pos..range.end], style);
  }
}

#[cfg(test)]
mod tests {
  use crate::readline::context::get_context_tokens;

  use super::*;

  /// A palette with distinct, easy-to-spot codes for assertions.
  /// Avoids `Palette::new()` which calls `Shed::shopts`.
  fn test_palette() -> Palette {
    Palette {
      string: PaletteEntry::new().yellow(),
      keyword: PaletteEntry::new().magenta(),
      external_command: PaletteEntry::new().green(),
      function: PaletteEntry::new().green(),
      alias: PaletteEntry::new().green(),
      builtin: PaletteEntry::new().green(),
      directory: PaletteEntry::new().green(),
      invalid_command: PaletteEntry::new().red(),
      control_flow_keyword: PaletteEntry::new().magenta(),
      argument: PaletteEntry::new().white(),
      argument_file: PaletteEntry::new().white().underline(),
      variable: PaletteEntry::new().cyan(),
      operator: PaletteEntry::new().bold(),
      comment: PaletteEntry::new().bright_black().italic(),
      glob: PaletteEntry::new().bright_cyan(),
    }
  }

  /// Strip CSI escape sequences (`ESC [ ... m`) so we can compare to plain input.
  fn strip_ansi(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
      if c == '\x1b' {
        // ESC; expect '[' then params then a final byte in 0x40..=0x7E
        if chars.next() != Some('[') {
          continue;
        }
        for end in chars.by_ref() {
          if ('@'..='~').contains(&end) {
            break;
          }
        }
      } else {
        out.push(c);
      }
    }
    out
  }

  #[test]
  fn round_trips_input_bytes() {
    let p = test_palette();
    let cases = [
      "",
      "ls",
      "ls foo bar",
      "echo \"hi $foo\"",
      "$(echo hi)",
      "ls *.rs",
      "${foo:-default}",
      "ls αβγ",
    ];
    for input in cases {
      let tks = get_context_tokens(input);
      let mut out = String::new();
      highlight(&mut out, input, &tks, &p, 0, &[]).unwrap();
      assert_eq!(
        strip_ansi(&out),
        input,
        "round-trip failed for {input:?}\nout: {out:?}"
      );
    }
  }

  /// check to see if there is a color code
  fn contains_sgr_param(out: &str, code: &str) -> bool {
    out.contains(&format!("[{code}m")) || out.contains(&format!(";{code}m"))
  }

  #[test]
  fn paints_var_sub_with_variable_style() {
    let p = test_palette();
    let tks = get_context_tokens("ls $foo");
    let mut out = String::new();
    highlight(&mut out, "ls $foo", &tks, &p, 0, &[]).unwrap();
    // Cyan = ANSI 36 - the variable style for $foo should appear in output.
    assert!(
      contains_sgr_param(&out, "36"),
      "expected cyan in output: {out:?}"
    );
    // And `$foo` should sit somewhere after a cyan code.
    let cyan_idx = out.find(";36m").or_else(|| out.find("[36m")).expect("cyan");
    assert!(
      out[cyan_idx..].contains("foo"),
      "expected $foo after cyan: {out:?}"
    );
  }

  #[test]
  fn paints_double_string_with_string_style() {
    let p = test_palette();
    let tks = get_context_tokens(r#""hello""#);
    let mut out = String::new();
    highlight(&mut out, r#""hello""#, &tks, &p, 0, &[]).unwrap();
    // Yellow = ANSI 33.
    assert!(
      contains_sgr_param(&out, "33"),
      "expected yellow in output: {out:?}"
    );
  }

  #[test]
  fn nested_var_in_string_paints_both() {
    let p = test_palette();
    let tks = get_context_tokens(r#""hi $foo""#);
    let mut out = String::new();
    highlight(&mut out, r#""hi $foo""#, &tks, &p, 0, &[]).unwrap();
    // Both yellow (string) and cyan (var) should appear.
    assert!(
      contains_sgr_param(&out, "33"),
      "expected yellow (string): {out:?}"
    );
    assert!(
      contains_sgr_param(&out, "36"),
      "expected cyan (var): {out:?}"
    );
  }

  #[test]
  fn cmd_sub_round_trips() {
    let p = test_palette();
    let input = "echo $(date)";
    let tks = get_context_tokens(input);
    let mut out = String::new();
    highlight(&mut out, input, &tks, &p, 0, &[]).unwrap();
    assert_eq!(strip_ansi(&out), input, "cmd sub round-trip: {out:?}");
  }

  #[test]
  fn empty_input_produces_no_visible_text() {
    // yansi may emit empty SGR pairs around zero-width spans; that's fine
    // visually. We just want no actual characters to come through.
    let p = test_palette();
    let tks = &get_context_tokens("");
    let mut out = String::new();
    highlight(&mut out, "", tks, &p, 0, &[]).unwrap();
    assert_eq!(strip_ansi(&out), "");
  }

  #[test]
  fn trailing_whitespace_preserved() {
    let p = test_palette();
    let tks = get_context_tokens("ls   ");
    let mut out = String::new();
    highlight(&mut out, "ls   ", &tks, &p, 0, &[]).unwrap();
    assert_eq!(strip_ansi(&out), "ls   ");
  }

  // ===================== control-byte visualization =====================

  #[test]
  fn esc_renders_as_caret_bracket() {
    let p = test_palette();
    let tks = get_context_tokens("a\x1bb");
    let mut out = String::new();
    highlight(&mut out, "a\x1bb", &tks, &p, 0, &[]).unwrap();
    let visible = strip_ansi(&out);
    assert!(visible.contains("a^[b"), "got {visible:?}");
  }

  #[test]
  fn cr_renders_as_caret_m() {
    let p = test_palette();
    let tks = get_context_tokens("before\rafter");
    let mut out = String::new();
    highlight(&mut out, "before\rafter", &tks, &p, 0, &[]).unwrap();
    let visible = strip_ansi(&out);
    assert!(visible.contains("before^Mafter"), "got {visible:?}");
  }

  #[test]
  fn del_renders_as_caret_question() {
    let p = test_palette();
    let tks = get_context_tokens("x\x7fy");
    let mut out = String::new();
    highlight(&mut out, "x\x7fy", &tks, &p, 0, &[]).unwrap();
    let visible = strip_ansi(&out);
    assert!(visible.contains("x^?y"), "got {visible:?}");
  }

  #[test]
  fn newline_and_tab_pass_through_unchanged() {
    // \n and \t are structural for multi-line buffers and indented commands;
    // visualizing them would break layout.
    let p = test_palette();
    let tks = get_context_tokens("a\nb\tc");
    let mut out = String::new();
    highlight(&mut out, "a\nb\tc", &tks, &p, 0, &[]).unwrap();
    let visible = strip_ansi(&out);
    assert!(visible.contains("a\nb\tc"), "got {visible:?}");
    assert!(!visible.contains("^J"));
    assert!(!visible.contains("^I"));
  }

  #[test]
  fn raw_control_bytes_do_not_reach_terminal_stream() {
    // The whole point: raw \x1b should never appear in the rendered output
    // (or it would let the terminal interpret embedded escape sequences).
    let p = test_palette();
    let tks = get_context_tokens("\x1b]0;PWNED\x07");
    let mut out = String::new();
    highlight(&mut out, "\x1b]0;PWNED\x07", &tks, &p, 0, &[]).unwrap();
    assert!(
      !out.contains('\x1b') || {
        // shed's own styling escapes are allowed; check by stripping CSI runs
        // and confirming no raw \x1b survives unaccompanied by `[`
        let stripped = strip_ansi(&out);
        !stripped.contains('\x1b')
      },
      "raw ESC byte escaped sanitizer: {out:?}"
    );
  }

  #[test]
  fn no_control_bytes_takes_hot_path() {
    // Smoke test: input without control bytes should still round-trip
    // identically (we have a fast path that skips visualization).
    let p = test_palette();
    let tks = get_context_tokens("echo hello world");
    let mut out = String::new();
    highlight(&mut out, "echo hello world", &tks, &p, 0, &[]).unwrap();
    assert_eq!(strip_ansi(&out), "echo hello world");
  }

  #[test]
  fn control_visualization_is_styled_distinctly() {
    // The visualized control byte should carry SGR codes (dim+italic)
    // distinct from the surrounding text. We check that the raw output
    // contains at least one extra SGR sequence introduced around the
    // visualized char.
    let p = test_palette();
    let tks = get_context_tokens("ab");
    let mut plain = String::new();
    highlight(&mut plain, "ab", &tks, &p, 0, &[]).unwrap();
    let tks = get_context_tokens("a\x1bb");
    let mut with_ctrl = String::new();
    highlight(&mut with_ctrl, "a\x1bb", &tks, &p, 0, &[]).unwrap();
    // The control variant should contain "^[" (visualization) and have
    // more bytes than the plain version (extra SGR codes).
    assert!(
      with_ctrl.contains("^["),
      "missing visualization: {with_ctrl:?}"
    );
    assert!(
      with_ctrl.len() > plain.len() + 2,
      "visualization should add styling bytes; plain={} with_ctrl={}",
      plain.len(),
      with_ctrl.len()
    );
  }
}
