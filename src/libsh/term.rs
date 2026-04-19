use nix::{libc::STDOUT_FILENO, unistd::write};
use yansi::{Paint, Painted};

use crate::{libsh::error::ShResult, match_loop, procio::borrow_fd, sherr};

/// Enables or disables bracketed paste mode in the terminal.
pub fn set_bracketed_paste(on: bool) -> ShResult<()> {
  let stdout = borrow_fd(STDOUT_FILENO);

  let control = if on { b"\x1b[?2004h" } else { b"\x1b[?2004l" };

  write(stdout, control)
    .map_err(|e| sherr!(InternalErr, "Failed to enable bracketed paste: {e}"))?;

  Ok(())
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
