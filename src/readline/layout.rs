use std::{fmt::Debug, fmt::Write as FmtWrite};

use unicode_segmentation::UnicodeSegmentation;

use crate::{
  queue_term, shopt,
  state::{Shed, terminal::Terminal},
  try_var,
  util::{pos::Pos, ui},
  write_term,
};

fn gutter_width(offset: usize, line_count: usize) -> usize {
  let max_num_len = (offset + line_count).to_string().len();
  max_num_len + 3
}

pub(super) fn pad_prompt_for_gutter(
  prompt: &str,
  line: &str,
  offset: usize,
  term_width: usize,
) -> String {
  let prompt_end = Layout::calc_pos(term_width, prompt, Pos { col: 0, row: 0 }, 0, false);
  let multiline = line.contains('\n') || prompt_end.col == 0;
  if !multiline {
    return prompt.to_string();
  }
  let line_count = line.split('\n').count();
  let pad = gutter_width(offset, line_count).saturating_sub(prompt_end.col);
  if pad == 0 {
    return prompt.to_string();
  }
  let padding = " ".repeat(pad);
  let mut out = prompt.to_string();
  match out.rfind('\n') {
    Some(nl_pos) => out.insert_str(nl_pos + 1, &padding),
    None => out.insert_str(0, &padding),
  }
  out
}

pub(super) fn enumerate_lines(
  s: &str,
  left_pad: usize,
  show_numbers: bool,
  offset: usize,
  _total_buf_lines: usize,
) -> String {
  let lines: Vec<&str> = s.split('\n').collect();
  let visible_count = lines.len();
  let max_num_len = (offset + visible_count).to_string().len();
  let mut first = true;
  let mut last_style = String::new();
  lines
    .into_iter()
    .enumerate()
    .fold(String::new(), |mut acc, (i, ln)| {
      if first {
        first = false;
      } else {
        acc.push('\n');
      }
      if i == 0 && left_pad > 0 {
        acc.push_str(ln);
      } else {
        let num = (i + offset + 1).to_string();
        let num_pad = max_num_len - num.len();
        // " 2 | " - num + padding + " | "
        let prefix_len = max_num_len + 3; // "N | "
        let trail_pad = left_pad.saturating_sub(prefix_len);
        let prefix = if show_numbers {
          format!("\x1b[0m\x1b[90m{}{num} |\x1b[0m ", " ".repeat(num_pad))
        } else {
          " ".repeat(prefix_len)
        };
        write!(acc, "{prefix}{}{last_style}{ln}", " ".repeat(trail_pad)).unwrap();
      }
      // Track the last ANSI escape sequence on this line so we can
      // restore it after the line number prefix on the next line
      let mut rest = ln;
      while let Some(esc_pos) = rest.find("\x1b[") {
        let after_esc = &rest[esc_pos..];
        let after_esc_prefix = &after_esc[2..];

        if let Some(params_len) = after_esc_prefix.find('m') {
          let full_seq_len = params_len + 3; // 3 bytes: \x1b[...m
          let full_seq = &after_esc[..full_seq_len];
          last_style = full_seq.to_string();
          rest = &after_esc[full_seq_len..];
        } else {
          break;
        }
      }
      if last_style == "\x1b[0m" {
        last_style.clear();
      }
      acc
    })
}

/// Replace tabs with `tab_stop` number of spaces.
///
/// Used for rendering the line editor content in a way that respects the user's `tab_width` shell option.
/// Has no effect on the text that is submitted to the shell or saved to history, this is strictly for display.
fn expand_tabs(s: &str, left_margin: usize, tab_stop: usize) -> String {
  let mut out = String::new();
  let mut col = left_margin;
  let mut esc_seq = 0;
  for c in s.graphemes(true) {
    if c == "\t" {
      let spaces = tab_stop - ((col.saturating_sub(left_margin)) % tab_stop);
      (0..spaces).for_each(|_| {
        out.push(' ');
        col += 1;
      });
    } else if c == "\n" {
      out.push('\n');
      col = left_margin;
    } else {
      out.push_str(c);
      col += ui::width(c, &mut esc_seq);
    }
  }
  out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Layout {
  pub prompt_end: Pos,
  pub cursor: Pos,
  pub end: Pos,
  pub psr_end: Option<Pos>,
  pub t_cols: usize,
}

impl Layout {
  pub(super) fn new() -> Self {
    Self {
      prompt_end: Pos::default(),
      cursor: Pos::default(),
      end: Pos::default(),
      psr_end: None,
      t_cols: 0,
    }
  }
  pub(super) fn from_parts(term_width: usize, prompt: &str, to_cursor: &str, to_end: &str) -> Self {
    let prompt_end = Self::calc_pos(term_width, prompt, Pos { col: 0, row: 0 }, 0, false);
    let cursor = Self::calc_pos(term_width, to_cursor, prompt_end, prompt_end.col, true);
    let end = Self::calc_pos(term_width, to_end, prompt_end, prompt_end.col, false);
    Layout {
      prompt_end,
      cursor,
      end,
      psr_end: None,
      t_cols: term_width,
    }
  }

  fn is_ctl_char(gr: &str) -> bool {
    if gr.is_empty() {
      return false;
    }
    let b = gr.as_bytes()[0];
    matches!(b, 0x00..=0x08 | 0x0b..=0x1f | 0x7f)
  }

  pub(super) fn calc_pos(
    term_width: usize,
    s: &str,
    orig: Pos,
    left_margin: usize,
    raw_calc: bool,
  ) -> Pos {
    let tab_stop = shopt!(line.tab_width);
    let mut pos = orig;
    let mut esc_seq = 0;
    for c in s.graphemes(true) {
      if c == "\n" {
        pos.row += 1;
        pos.col = left_margin;
      }
      let c_width = if c == "\t" {
        tab_stop - ((pos.col.saturating_sub(left_margin)) % tab_stop)
      } else if raw_calc && Self::is_ctl_char(c) {
        2
      } else {
        ui::width(c, &mut esc_seq)
      };
      pos.col += c_width;
      if pos.col > term_width {
        pos.row += 1;
        pos.col = c_width;
      }
    }
    if pos.col >= term_width {
      pos.row += 1;
      pos.col = 0;
    }

    pos
  }
}

impl Default for Layout {
  fn default() -> Self {
    Self::new()
  }
}

pub(super) fn redraw(
  prompt: &str,
  line: &str,
  new_layout: &Layout,
  offset: usize,
  total_buf_lines: usize,
) {
  queue_term!(TermCtl::Clear(ScreenFromCursor)).ok();

  let end = new_layout.end;
  let cursor = new_layout.cursor;

  let t_cols = Shed::term(Terminal::t_cols);
  let padded_prompt = pad_prompt_for_gutter(prompt, line, offset, t_cols);

  queue_term!(TermCtl::Osc(PromptStart)).ok();

  // Reset SGR
  write_term!("\x1b[0m").ok();

  if let Some(prefix) = try_var!("SHELL_PROMPT_PREFIX") {
    write_term!("{prefix}").ok();
  }

  write_term!("{padded_prompt}").ok();

  if let Some(suffix) = try_var!("SHELL_PROMPT_SUFFIX") {
    write_term!("{suffix}").ok();
  }

  queue_term!(TermCtl::Osc(PromptEnd)).ok();

  // Reset SGR again
  write_term!("\x1b[0m").ok();

  let tab_width = shopt!(line.tab_width);
  let prompt_end = Layout::calc_pos(t_cols, &padded_prompt, Pos { col: 0, row: 0 }, 0, false);
  let expanded = expand_tabs(line, prompt_end.col, tab_width);
  let multiline = expanded.contains('\n') || prompt_end.col == 0;
  if multiline {
    let show_numbers = shopt!(line.line_numbers);
    let display_line = enumerate_lines(
      &expanded,
      prompt_end.col,
      show_numbers,
      offset,
      total_buf_lines,
    );
    write_term!("{display_line}").ok();
  } else {
    write_term!("{expanded}").ok();
  }

  if end.col == 0 && end.row > prompt_end.row && !Shed::term(Terminal::buf_ends_with_newline) {
    // The line has wrapped. We need to use our own line break.
    queue_term!(TermCtl::PrintChar('\n')).ok();
  }

  Shed::term_mut(|t| t.calc_cursor_movement(end, cursor)).ok();
}

pub(super) fn move_cursor_to_end(layout: &Layout) {
  let t_cols = Shed::term(Terminal::t_cols);
  let mut end = layout.end.row;
  if layout.psr_end.is_some() && layout.t_cols > t_cols && t_cols > 0 {
    let extra = (layout.t_cols.saturating_sub(1)) / t_cols;
    end += extra;
  }
  let cursor_row = layout.cursor.row;

  let cursor_motion = end.saturating_sub(cursor_row);
  queue_term!(TermCtl::Cursor(Down(cursor_motion as u16))).ok();
}

pub(super) fn clear_rows(layout: &Layout) {
  // Account for lines that may have wrapped due to terminal resize.
  // If a PSR was drawn, the last row extended to the old terminal width.
  // When the terminal shrinks, that row wraps into extra physical rows.
  let t_cols = Shed::term(Terminal::t_cols);
  let mut rows_to_clear = layout.end.row;
  if layout.psr_end.is_some() && layout.t_cols > t_cols && t_cols > 0 {
    let extra = (layout.t_cols.saturating_sub(1)) / t_cols;
    rows_to_clear += extra;
  }
  let cursor_row = layout.cursor.row;

  let cursor_motion = rows_to_clear.saturating_sub(cursor_row);
  queue_term!(TermCtl::Cursor(Down(cursor_motion as u16))).ok();

  for _ in 0..rows_to_clear {
    queue_term!(TermCtl::Clear(WholeLine), TermCtl::Cursor(Up(1))).ok();
  }
  queue_term!(TermCtl::Clear(WholeLine), TermCtl::PrintChar('\r')).ok();
}
