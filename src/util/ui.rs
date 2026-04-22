use std::fmt::Write;
use crate::{readline::term::calc_str_width, write_term};
use crate::{util::error::ShResult, match_loop, sherr};
use yansi::{Paint, Painted};

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

/// Build an ansi color escape sequence from a plain english description
pub fn color_from_description(desc: &str) -> ShResult<String> {
  let mut style: Painted<&str> = "".primary().on_primary().linger();
  let mut words = desc.split_whitespace();

  match_loop!(words.next() => word, {
    "green" => style = style.green(),
    "red" => style = style.red(),
    "yellow" => style = style.yellow(),
    "blue" => style = style.blue(),
    "magenta" => style = style.magenta(),
    "cyan" => style = style.cyan(),
    "white" => style = style.white(),
    "black" => style = style.black(),
    "bold" => style = style.bold(),
    "dim" => style = style.dim(),
    "italic" => style = style.italic(),
    "underline" => style = style.underline(),
    "strikethrough" => style = style.strike(),
    "hidden" => style = style.attr(yansi::Attribute::Conceal),
    "blink" => style = style.attr(yansi::Attribute::Blink),
    "inverted" => style = style.attr(yansi::Attribute::Invert),
    "reset" => style = style.resetting(),

    "bright" => style = style.bright(),
    "on" => {
      let Some(mut word) = words.next() else {
        return Err(sherr!(ParseErr, "Expected background color after 'on' in color description"));
      };
      if word == "bright" {
        style = style.on_bright();
        let Some(w) = words.next() else {
          return Err(sherr!(ParseErr, "Expected background color after 'on bright' in color description"));
        };
        word = w;
      }
      match word {
        "green" => style = style.on_green(),
        "red" => style = style.on_red(),
        "yellow" => style = style.on_yellow(),
        "blue" => style = style.on_blue(),
        "magenta" => style = style.on_magenta(),
        "cyan" => style = style.on_cyan(),
        "white" => style = style.on_white(),
        "black" => style = style.on_black(),
        hex if word.starts_with('#') => {
          let (r,g,b) = hex_to_rgb(hex)?;
          style = style.on_rgb(r,g,b);
        }
        _ => return Err(sherr!(ParseErr, "Unknown background color '{}' in color description", word)),
      }
    }

    hex if word.starts_with('#') => {
      let (r,g,b) = hex_to_rgb(hex)?;
      style = style.rgb(r,g,b);
    }

    _ => return Err(sherr!(ParseErr, "Unknown style '{}' in color description", word)),
  });

  Ok(style.to_string())
}

pub fn hex_to_rgb(hex: &str) -> ShResult<(u8, u8, u8)> {
  let hex = &hex[1..];
  if hex.len() != 6
    || !hex
      .chars()
      .all(|ch: char| ('a'..='f').contains(&ch) || ch.is_ascii_digit())
  {
    return Err(sherr!(
      ParseErr,
      "Invalid hex color '{}' in color description",
      hex
    ));
  }
  let r = u8::from_str_radix(&hex[..2], 16).unwrap();
  let g = u8::from_str_radix(&hex[2..4], 16).unwrap();
  let b = u8::from_str_radix(&hex[4..6], 16).unwrap();

  Ok((r, g, b))
}
