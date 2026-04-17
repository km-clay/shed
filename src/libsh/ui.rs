use crate::readline::term::calc_str_width;

pub const BOT_LEFT: &str = "\x1b[90m╰\x1b[0m";
pub const BOT_RIGHT: &str = "\x1b[90m╯\x1b[0m";
pub const TOP_LEFT: &str = "\x1b[90m╭\x1b[0m";
pub const TOP_RIGHT: &str = "\x1b[90m╮\x1b[0m";
pub const HOR_LINE: &str = "\x1b[90m─\x1b[0m";
pub const VERT_LINE: &str = "\x1b[90m│\x1b[0m";
pub const TREE_LEFT: &str = "\x1b[90m├\x1b[0m";
pub const TREE_RIGHT: &str = "\x1b[90m┤\x1b[0m";

/// Pad `content` with `fill` to `cols` width, appending `right_border` at the end.
pub fn pad_line(buf: &mut String, content: &str, fill: &str, right_border: &str, cols: usize) {
  let used = calc_str_width(content) as usize;
  let padding = cols.saturating_sub(used + 1);
  buf.push_str(content);
  for _ in 0..padding {
    buf.push_str(fill);
  }
  buf.push_str(right_border);
}
