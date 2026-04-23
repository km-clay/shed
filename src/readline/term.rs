use std::{env, fmt::Debug, fmt::Write as FmtWrite, os::fd::RawFd};

use nix::libc::{self};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::{
  readline::linebuf::Pos,
  state::{read_shopts, with_term},
  util::error::ShResult,
  write_term,
};
use crate::{
  sherr,
  state::{read_meta, write_meta},
};

pub const OSC_PROMPT_START: &str = "\x1b]133;A\x07";
pub const OSC_PROMPT_END: &str = "\x1b]133;B\x07";
pub const OSC_EXEC_START: &str = "\x1b]133;C\x07";
pub fn osc_exec_end(code: i32) -> String {
  format!("\x1b]133;D;{code}\x07")
}

pub type Row = u16;
pub type Col = u16;

// I'd like to thank rustyline for this idea
nix::ioctl_read_bad!(win_size, libc::TIOCGWINSZ, libc::winsize);

pub fn get_win_size(fd: RawFd) -> (Col, Row) {
  use std::mem::zeroed;

  if cfg!(test) {
    return (80, 24);
  }

  unsafe {
    let mut size: libc::winsize = zeroed();
    match win_size(fd, &mut size) {
      Ok(0) => {
        /* rustyline code says:
         In linux pseudo-terminals are created with dimensions of
         zero. If host application didn't initialize the correct
         size before start we treat zero size as 80 columns and
         infinite rows
        */
        let cols = if size.ws_col == 0 { 80 } else { size.ws_col };
        let rows = if size.ws_row == 0 {
          u16::MAX
        } else {
          size.ws_row
        };
        (cols, rows)
      }
      _ => (80, 24),
    }
  }
}

pub fn enumerate_lines(
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
  log::debug!(
    "left_pad: {left_pad}, offset: {offset}, visible_count: {visible_count}, max_num_len: {max_num_len}"
  );
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
          " ".repeat(prefix_len + 1).to_string()
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

pub fn calc_str_width(s: &str) -> usize {
  let mut esc_seq = 0;
  s.graphemes(true).map(|g| width(g, &mut esc_seq)).sum()
}

// Big credit to rustyline for this
fn width(s: &str, esc_seq: &mut u8) -> usize {
  if *esc_seq == 1 {
    if s == "[" {
      // CSI
      *esc_seq = 2;
    } else {
      // two-character sequence
      *esc_seq = 0;
    }
    0
  } else if *esc_seq == 2 {
    if s == ";" || (s.as_bytes()[0] >= b'0' && s.as_bytes()[0] <= b'9') {
      /*} else if s == "m" {
      // last
       *esc_seq = 0;*/
    } else {
      // not supported
      *esc_seq = 0;
    }

    0
  } else if s == "\x1b" {
    *esc_seq = 1;
    0
  } else if s == "\n" {
    0
  } else {
    get_width_calculator().width(s)
  }
}

pub fn width_calculator() -> Box<dyn WidthCalculator> {
  match env::var("TERM_PROGRAM").as_deref() {
    Ok("Apple_Terminal") => Box::new(UnicodeWidth),
    Ok("iTerm.app") => Box::new(UnicodeWidth),
    Ok("WezTerm") => Box::new(UnicodeWidth),
    Err(std::env::VarError::NotPresent) => match std::env::var("TERM").as_deref() {
      Ok("xterm-kitty") => Box::new(NoZwj),
      _ => Box::new(WcWidth),
    },
    _ => Box::new(WcWidth),
  }
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
      col += width(c, &mut esc_seq);
    }
  }
  out
}

pub fn append_digit(left: u32, right: u32) -> u32 {
  left.saturating_mul(10).saturating_add(right)
}

pub trait WidthCalculator: Send + Sync {
  fn width(&self, text: &str) -> usize;
}

static WIDTH_CALC: std::sync::OnceLock<Box<dyn WidthCalculator>> = std::sync::OnceLock::new();

pub fn get_width_calculator() -> &'static dyn WidthCalculator {
  WIDTH_CALC.get_or_init(width_calculator).as_ref()
}

#[derive(Clone, Copy, Debug)]
pub struct UnicodeWidth;

impl WidthCalculator for UnicodeWidth {
  fn width(&self, text: &str) -> usize {
    text.width()
  }
}

#[derive(Clone, Copy, Debug)]
pub struct WcWidth;

impl WcWidth {
  pub fn cwidth(&self, ch: char) -> usize {
    ch.width().unwrap()
  }
}

impl WidthCalculator for WcWidth {
  fn width(&self, text: &str) -> usize {
    let mut width = 0;
    for ch in text.chars() {
      width += self.cwidth(ch)
    }
    width
  }
}

const ZWJ: char = '\u{200D}';
#[derive(Clone, Copy, Debug)]
pub struct NoZwj;

impl WidthCalculator for NoZwj {
  fn width(&self, text: &str) -> usize {
    if text.contains(ZWJ) {
      // ZWJ sequence renders as a single glyph on supported terminals
      2
    } else {
      UnicodeWidth.width(text)
    }
  }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
  pub prompt_end: Pos,
  pub cursor: Pos,
  pub end: Pos,
  pub psr_end: Option<Pos>,
  pub t_cols: usize,
}

impl Layout {
  pub fn new() -> Self {
    Self {
      prompt_end: Pos::default(),
      cursor: Pos::default(),
      end: Pos::default(),
      psr_end: None,
      t_cols: 0,
    }
  }
  pub fn from_parts(term_width: usize, prompt: &str, to_cursor: &str, to_end: &str) -> Self {
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
    !gr.is_empty() && gr.as_bytes()[0] <= 0x1F && gr != "\n" && gr != "\t" && gr != "\r"
  }

  pub fn calc_pos(
    term_width: usize,
    s: &str,
    orig: Pos,
    left_margin: usize,
    raw_calc: bool,
  ) -> Pos {
    let tab_stop = read_shopts(|o| o.line.tab_width);
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
        width(c, &mut esc_seq)
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

pub fn redraw(
  prompt: &str,
  line: &str,
  new_layout: &Layout,
  offset: usize,
  total_buf_lines: usize,
) -> ShResult<()> {
  let err = |_| sherr!(InternalErr, "Failed to write to LineWriter internal buffer");
  write_term!("\x1b[J").ok(); // Clear from cursor to end of screen to erase any remnants of the old line after the prompt

  let end = new_layout.end;
  let cursor = new_layout.cursor;

  if read_meta(|m| m.system_msg_pending()) {
    let mut system_msg = String::new();
    while let Some(msg) = write_meta(|m| m.pop_system_message()) {
      writeln!(system_msg, "{msg}").map_err(err)?;
    }
    write_term!("{system_msg}").ok();
  }

  write_term!("{OSC_PROMPT_START}").ok();
  if let Ok(prefix) = env::var("SHELL_PROMPT_PREFIX") {
    write_term!("{prefix}").ok();
  }
  write_term!("{prompt}").ok();
  if let Ok(suffix) = env::var("SHELL_PROMPT_SUFFIX") {
    write_term!("{suffix}").ok();
  }
  write_term!("{OSC_PROMPT_END}").ok();
  let t_cols = with_term(|t| t.t_cols());

  let tab_width = read_shopts(|o| o.line.tab_width);
  let prompt_end = Layout::calc_pos(t_cols, prompt, Pos { col: 0, row: 0 }, 0, false);
  let expanded = expand_tabs(line, prompt_end.col, tab_width);
  let multiline = expanded.contains('\n') || prompt_end.col == 0;
  if multiline {
    let show_numbers = read_shopts(|o| o.line.line_numbers);
    let display_line = enumerate_lines(
      &expanded,
      prompt_end.col as usize,
      show_numbers,
      offset,
      total_buf_lines,
    );
    write_term!("{display_line}").ok();
  } else {
    write_term!("{expanded}").ok();
  }

  if end.col == 0 && end.row > prompt_end.row && !with_term(|t| t.buf_ends_with_newline()) {
    // The line has wrapped. We need to use our own line break.
    write_term!("\n").ok();
  }

  with_term(|t| t.calc_cursor_movement(end, cursor)).ok();

  Ok(())
}

pub fn move_cursor_to_end(layout: &Layout) -> ShResult<()> {
  let t_cols = with_term(|t| t.t_cols());
  let mut end = layout.end.row;
  if layout.psr_end.is_some() && layout.t_cols > t_cols && t_cols > 0 {
    let extra = (layout.t_cols.saturating_sub(1)) / t_cols;
    end += extra;
  }
  let cursor_row = layout.cursor.row;

  let cursor_motion = end.saturating_sub(cursor_row);
  if cursor_motion > 0 {
    write_term!("\x1b[{cursor_motion}B").ok();
  }

  Ok(())
}

pub fn clear_rows(layout: &Layout) -> ShResult<()> {
  // Account for lines that may have wrapped due to terminal resize.
  // If a PSR was drawn, the last row extended to the old terminal width.
  // When the terminal shrinks, that row wraps into extra physical rows.
  let t_cols = with_term(|t| t.t_cols());
  let mut rows_to_clear = layout.end.row;
  if layout.psr_end.is_some() && layout.t_cols > t_cols && t_cols > 0 {
    let extra = (layout.t_cols.saturating_sub(1)) / t_cols;
    rows_to_clear += extra;
  }
  let cursor_row = layout.cursor.row;

  let cursor_motion = rows_to_clear.saturating_sub(cursor_row);
  if cursor_motion > 0 {
    write_term!("\x1b[{cursor_motion}B")?;
  }

  for _ in 0..rows_to_clear {
    write_term!("\x1b[2K\x1b[A")?; // Clear line and move up
  }
  write_term!("\x1b[2K\r")?; // Clear line and return to column 0
  Ok(())
}
