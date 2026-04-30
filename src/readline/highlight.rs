use std::{fmt::Write, ops::Range};

use yansi::Paint;

use crate::{
  readline::context::{CtxTk, CtxTkRule, get_context_tokens}, shopt::ShOptHighlight, state::read_shopts, util::ui::{PaletteEntry, style_from_description}
};

pub struct Palette {
  string: PaletteEntry,
  keyword: PaletteEntry,
  valid_command: PaletteEntry,
  invalid_command: PaletteEntry,
  control_flow_keyword: PaletteEntry,
  argument: PaletteEntry,
  argument_file: PaletteEntry,
  variable: PaletteEntry,
  operator: PaletteEntry,
  comment: PaletteEntry,
  glob: PaletteEntry,
  selection: PaletteEntry,
}

impl Palette {
  pub fn new() -> Self {
    let get_color = |desc: &str| -> PaletteEntry {
      style_from_description(desc).unwrap_or_else(|_| PaletteEntry::new())
    };
    read_shopts(|o| {
      let ShOptHighlight {
        enable: _,
        string,
        keyword,
        valid_command,
        invalid_command,
        control_flow_keyword,
        argument,
        argument_file,
        variable,
        operator,
        comment,
        glob,
        selection,
      } = &o.highlight;
      Self {
        string: get_color(string),
        keyword: get_color(keyword),
        valid_command: get_color(valid_command),
        invalid_command: get_color(invalid_command),
        control_flow_keyword: get_color(control_flow_keyword),
        argument: get_color(argument),
        argument_file: get_color(argument_file),
        variable: get_color(variable),
        operator: get_color(operator),
        comment: get_color(comment),
        glob: get_color(glob),
        selection: get_color(selection),
      }
    })
  }

  pub fn neutral() -> Self {
    let entry = PaletteEntry::new();
    // no styles. used when syntax highlighting is disabled
    Self {
      string: entry,
      keyword: entry,
      valid_command: entry,
      invalid_command: entry,
      control_flow_keyword: entry,
      argument: entry,
      argument_file: entry,
      variable: entry,
      operator: entry,
      comment: entry,
      glob: entry,
      selection: entry,
    }
  }

  pub fn style_for(&self, tk: &CtxTk, editor_cursor_pos: usize) -> PaletteEntry {
    let class = tk.class();
    match class {
      CtxTkRule::ValidCommand => self.valid_command,
      CtxTkRule::InvalidCommand => self.invalid_command,
      CtxTkRule::Argument => self.argument,
      CtxTkRule::ArgumentFile => {
        let range = tk.span().range();
        let inclusive = range.start..=range.end;
        if inclusive.contains(&editor_cursor_pos) {
          self.argument_file
        } else {
          self.argument
        }
      }
      CtxTkRule::Keyword => self.keyword,
      CtxTkRule::Subshell => self.keyword,
      CtxTkRule::CmdSub => self.variable,
      CtxTkRule::BacktickSub => self.variable,
      CtxTkRule::ProcSubIn => self.variable,
      CtxTkRule::ProcSubOut => self.variable,
      CtxTkRule::VarSub => self.variable,
      CtxTkRule::Comment => self.comment,
      CtxTkRule::Glob => self.glob,
      CtxTkRule::CasePattern => self.glob,
      CtxTkRule::HistExp => self.variable,
      CtxTkRule::Escape => self.operator,
      CtxTkRule::Tilde => self.operator,
      CtxTkRule::Separator => self.argument,
      CtxTkRule::Arithmetic => self.keyword,
      CtxTkRule::ArithOp => self.operator,
      CtxTkRule::ArithNumber => self.argument,
      CtxTkRule::ArithVar => self.variable,
      CtxTkRule::ParamPrefix => self.operator,
      CtxTkRule::ParamName => self.variable,
      CtxTkRule::ParamIndex => self.variable,
      CtxTkRule::ParamOp => self.operator,
      CtxTkRule::ParamArg => self.argument,
      CtxTkRule::DoubleString => self.string,
      CtxTkRule::SingleString => self.string,
      CtxTkRule::DollarString => self.string,
      CtxTkRule::AssignmentLeft => self.variable,
      CtxTkRule::AssignmentOp => self.operator,
      CtxTkRule::AssignmentRight => self.argument,
      CtxTkRule::Operator => self.operator,
      CtxTkRule::Redirect => self.operator,
      CtxTkRule::BraceGroup => self.keyword,
      CtxTkRule::HereDoc => self.string,
      CtxTkRule::HereDocStart => self.operator,
      CtxTkRule::HereDocBody => self.string,
      CtxTkRule::HereDocEnd => self.operator,
      CtxTkRule::Null => PaletteEntry::new(),
    }
  }
}

impl Default for Palette {
  fn default() -> Self {
    Self::new()
  }
}

/// entry point for the highlighter
pub fn highlight(
  input: &str,
  palette: &Palette,
  editor_cursor_pos: usize,
  selections: Vec<Range<usize>>,
) -> String {
  let tks = get_context_tokens(input);
  let mut out = String::with_capacity(input.len() * 2); // pre-allocate some extra space for ANSI codes
  let mut cursor = 0;
  for tk in &tks {
    paint(
      &mut out,
      tk,
      PaletteEntry::new(),
      &mut cursor,
      editor_cursor_pos,
      palette,
      &selections,
    );
  }
  out.push_str("\x1b[0m"); // ensure we reset at the end
  out.push_str(&input[cursor..]); // append any remaining text after the last token
  out
}

/// given a `CtxTk`, write highlighted output to `out`
///
/// `CtxTk` already did the heavy lifting for figuring out where and what everything is.
/// now we can just paint the spans that it put together.
fn paint(
  out: &mut String,
  node: &CtxTk,
  parent: PaletteEntry,
  cursor: &mut usize, // our position in the input
  editor_cursor_pos: usize, // editor cursor position
  palette: &Palette,
  selections: &[Range<usize>],
) {
  let span = node.span().range();
  let src = node.span().get_source();

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

/// Emit `src[range]` styled with `style`, slicing at selection boundaries so
/// any portion overlapping any range in `selections` paints with an inverted
/// variant of the same style. Multiple overlapping/adjacent selections are
/// merged so each byte is emitted at most once.
fn emit_with_selection(
  out: &mut String,
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
    write!(out, "{}", src[range].paint(style.style())).unwrap();
    return;
  }

  // Sort by start, then merge overlapping/adjacent ranges into disjoint
  // segments so the sweep doesn't emit any byte twice.
  overlapping.sort_by_key(|s| s.start);
  let mut merged: Vec<Range<usize>> = Vec::with_capacity(overlapping.len());
  for sel in overlapping {
    if let Some(last) = merged.last_mut()
    && sel.start <= last.end {
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
      write!(out, "{}", src[pos..sel_start].paint(style.style())).unwrap();
    }

    if sel_start < sel_end {
      write!(out, "{}", src[sel_start..sel_end].paint(sel_style.style())).unwrap();
    }

    pos = sel_end;
  }

  if pos < range.end {
    write!(out, "{}", src[pos..range.end].paint(style.style())).unwrap();
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  /// A palette with distinct, easy-to-spot codes for assertions.
  /// Avoids `Palette::new()` which calls `read_shopts`.
  fn test_palette() -> Palette {
    Palette {
      string: PaletteEntry::new().yellow(),
      keyword: PaletteEntry::new().magenta(),
      valid_command: PaletteEntry::new().green(),
      invalid_command: PaletteEntry::new().red(),
      control_flow_keyword: PaletteEntry::new().magenta(),
      argument: PaletteEntry::new().white(),
      argument_file: PaletteEntry::new().white().underline(),
      variable: PaletteEntry::new().cyan(),
      operator: PaletteEntry::new().bold(),
      comment: PaletteEntry::new().bright_black().italic(),
      glob: PaletteEntry::new().bright_cyan(),
      selection: PaletteEntry::new().black().on_white(),
    }
  }

  /// Strip CSI escape sequences (`ESC [ ... m`) so we can compare to plain input.
  fn strip_ansi(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
      if c == '\x1b' {
        // ESC; expect '[' then params then a final byte in 0x40..=0x7E
        if chars.next() != Some('[') { continue; }
        for end in chars.by_ref() {
          if ('@'..='~').contains(&end) { break; }
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
      let out = highlight(input, &p, 0, vec![]);
      assert_eq!(strip_ansi(&out), input, "round-trip failed for {input:?}\nout: {out:?}");
    }
  }

  /// check to see if there is a color code
  fn contains_sgr_param(out: &str, code: &str) -> bool {
    out.contains(&format!("[{code}m")) || out.contains(&format!(";{code}m"))
  }

  #[test]
  fn paints_var_sub_with_variable_style() {
    let p = test_palette();
    let out = highlight("ls $foo", &p, 0, vec![]);
    // Cyan = ANSI 36 - the variable style for $foo should appear in output.
    assert!(contains_sgr_param(&out, "36"), "expected cyan in output: {out:?}");
    // And `$foo` should sit somewhere after a cyan code.
    let cyan_idx = out.find(";36m").or_else(|| out.find("[36m")).expect("cyan");
    assert!(out[cyan_idx..].contains("foo"), "expected $foo after cyan: {out:?}");
  }

  #[test]
  fn paints_double_string_with_string_style() {
    let p = test_palette();
    let out = highlight(r#""hello""#, &p, 0, vec![]);
    // Yellow = ANSI 33.
    assert!(contains_sgr_param(&out, "33"), "expected yellow in output: {out:?}");
  }

  #[test]
  fn nested_var_in_string_paints_both() {
    let p = test_palette();
    let out = highlight(r#""hi $foo""#, &p, 0, vec![]);
    // Both yellow (string) and cyan (var) should appear.
    assert!(contains_sgr_param(&out, "33"), "expected yellow (string): {out:?}");
    assert!(contains_sgr_param(&out, "36"), "expected cyan (var): {out:?}");
  }

  #[test]
  fn cmd_sub_round_trips() {
    let p = test_palette();
    let input = "echo $(date)";
    let out = highlight(input, &p, 0, vec![]);
    assert_eq!(strip_ansi(&out), input, "cmd sub round-trip: {out:?}");
  }

  #[test]
  fn empty_input_produces_no_visible_text() {
    // yansi may emit empty SGR pairs around zero-width spans; that's fine
    // visually. We just want no actual characters to come through.
    let p = test_palette();
    assert_eq!(strip_ansi(&highlight("", &p, 0, vec![])), "");
  }

  #[test]
  fn trailing_whitespace_preserved() {
    let p = test_palette();
    let out = highlight("ls   ", &p, 0, vec![]);
    assert_eq!(strip_ansi(&out), "ls   ");
  }
}
