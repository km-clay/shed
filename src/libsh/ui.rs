use std::fmt::Write;
use crate::{readline::term::calc_str_width, write_term};

pub const BOT_LEFT: &str = "\x1b[90m╰\x1b[0m";
pub const BOT_RIGHT: &str = "\x1b[90m╯\x1b[0m";
pub const TOP_LEFT: &str = "\x1b[90m╭\x1b[0m";
pub const TOP_RIGHT: &str = "\x1b[90m╮\x1b[0m";
pub const HOR_LINE: &str = "\x1b[90m─\x1b[0m";
pub const VERT_LINE: &str = "\x1b[90m│\x1b[0m";
pub const TREE_LEFT: &str = "\x1b[90m├\x1b[0m";
pub const TREE_RIGHT: &str = "\x1b[90m┤\x1b[0m";

/// Pad `content` with `fill` to `cols` width, appending `right_border` at the end.
pub fn pad_line(content: &str, fill: &str, right_border: &str, cols: usize) {
  let used = calc_str_width(content);
  let padding = cols.saturating_sub(used + 1);
  write_term!("{content}").ok();
  for _ in 0..padding {
    write_term!("{fill}").ok();
  }
  write_term!("{right_border}").ok();
}

/// Pad `content` with `fill` to `cols` width, appending `right_border` at the end.
pub fn pad_line_into(buf: &mut String, content: &str, fill: &str, right_border: &str, cols: usize) {
  let used = calc_str_width(content);
  let padding = cols.saturating_sub(used + 1);
  write!(buf, "{content}").ok();
  for _ in 0..padding {
    write!(buf, "{fill}").ok();
  }
  write!(buf, "{right_border}").ok();
}
