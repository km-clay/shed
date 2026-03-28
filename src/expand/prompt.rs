use crate::expand::subshell::expand_cmd_sub;
use crate::libsh::error::ShResult;
use crate::prelude::*;
use crate::state::{read_jobs, read_logic, read_shopts, write_meta};

#[derive(Debug)]
pub enum PromptTk {
  AsciiOct(i32),
  Text(String),
  AnsiSeq(String),
  Function(String), // Expands to the output of any defined shell function
  VisGrp,
  UserSeq,
  RuntimeMillis,
  RuntimeFormatted,
  Weekday,
  Dquote,
  Squote,
  Return,
  Newline,
  Pwd,
  PwdShort,
  Hostname,
  HostnameShort,
  ShellName,
  Username,
  PromptSymbol,
  ExitCode,
  SuccessSymbol,
  FailureSymbol,
  JobCount,
  VisGroupOpen,
  VisGroupClose,
}

pub fn format_cmd_runtime(dur: std::time::Duration) -> String {
  const ETERNITY: u128 = f32::INFINITY as u128;
  let mut micros = dur.as_micros();
  let mut millis = 0;
  let mut seconds = 0;
  let mut minutes = 0;
  let mut hours = 0;
  let mut days = 0;
  let mut weeks = 0;
  let mut months = 0;
  let mut years = 0;
  let mut decades = 0;
  let mut centuries = 0;
  let mut millennia = 0;
  let mut epochs = 0;
  let mut aeons = 0;
  let mut eternities = 0;

  if micros >= 1000 {
    millis = micros / 1000;
    micros %= 1000;
  }
  if millis >= 1000 {
    seconds = millis / 1000;
    millis %= 1000;
  }
  if seconds >= 60 {
    minutes = seconds / 60;
    seconds %= 60;
  }
  if minutes >= 60 {
    hours = minutes / 60;
    minutes %= 60;
  }
  if hours >= 24 {
    days = hours / 24;
    hours %= 24;
  }
  if days >= 7 {
    weeks = days / 7;
    days %= 7;
  }
  if weeks >= 4 {
    months = weeks / 4;
    weeks %= 4;
  }
  if months >= 12 {
    years = months / 12;
    weeks %= 12;
  }
  if years >= 10 {
    decades = years / 10;
    years %= 10;
  }
  if decades >= 10 {
    centuries = decades / 10;
    decades %= 10;
  }
  if centuries >= 10 {
    millennia = centuries / 10;
    centuries %= 10;
  }
  if millennia >= 1000 {
    epochs = millennia / 1000;
    millennia %= 1000;
  }
  if epochs >= 1000 {
    aeons = epochs / 1000;
    epochs %= aeons;
  }
  if aeons == ETERNITY {
    eternities = aeons / ETERNITY;
    aeons %= ETERNITY;
  }

  // Format the result
  let mut result = Vec::new();
  if eternities > 0 {
    let mut string = format!("{} eternit", eternities);
    if eternities > 1 {
      string.push_str("ies");
    } else {
      string.push('y');
    }
    result.push(string)
  }
  if aeons > 0 {
    let mut string = format!("{} aeon", aeons);
    if aeons > 1 {
      string.push('s')
    }
    result.push(string)
  }
  if epochs > 0 {
    let mut string = format!("{} epoch", epochs);
    if epochs > 1 {
      string.push('s')
    }
    result.push(string)
  }
  if millennia > 0 {
    let mut string = format!("{} millenni", millennia);
    if millennia > 1 {
      string.push_str("um")
    } else {
      string.push('a')
    }
    result.push(string)
  }
  if centuries > 0 {
    let mut string = format!("{} centur", centuries);
    if centuries > 1 {
      string.push_str("ies")
    } else {
      string.push('y')
    }
    result.push(string)
  }
  if decades > 0 {
    let mut string = format!("{} decade", decades);
    if decades > 1 {
      string.push('s')
    }
    result.push(string)
  }
  if years > 0 {
    let mut string = format!("{} year", years);
    if years > 1 {
      string.push('s')
    }
    result.push(string)
  }
  if months > 0 {
    let mut string = format!("{} month", months);
    if months > 1 {
      string.push('s')
    }
    result.push(string)
  }
  if weeks > 0 {
    let mut string = format!("{} week", weeks);
    if weeks > 1 {
      string.push('s')
    }
    result.push(string)
  }
  if days > 0 {
    let mut string = format!("{} day", days);
    if days > 1 {
      string.push('s')
    }
    result.push(string)
  }
  if hours > 0 {
    let string = format!("{}h", hours);
    result.push(string);
  }
  if minutes > 0 {
    let string = format!("{}m", minutes);
    result.push(string);
  }
  if seconds > 0 {
    let string = format!("{}s", seconds);
    result.push(string);
  }
  if result.is_empty() && millis > 0 {
    let string = format!("{}ms", millis);
    result.push(string);
  }
  if result.is_empty() && micros > 0 {
    let string = format!("{}µs", micros);
    result.push(string);
  }

  result.join(" ")
}

fn tokenize_prompt(raw: &str) -> Vec<PromptTk> {
  let mut chars = raw.chars().peekable();
  let mut tk_text = String::new();
  let mut tokens = vec![];

  while let Some(ch) = chars.next() {
    match ch {
      '\\' => {
        // Push any accumulated text as a token
        if !tk_text.is_empty() {
          tokens.push(PromptTk::Text(std::mem::take(&mut tk_text)));
        }

        // Handle the escape sequence
        if let Some(ch) = chars.next() {
          match ch {
            'w' => tokens.push(PromptTk::Pwd),
            'W' => tokens.push(PromptTk::PwdShort),
            'h' => tokens.push(PromptTk::Hostname),
            'H' => tokens.push(PromptTk::HostnameShort),
            's' => tokens.push(PromptTk::ShellName),
            'u' => tokens.push(PromptTk::Username),
            '$' => tokens.push(PromptTk::PromptSymbol),
            'n' => tokens.push(PromptTk::Text("\n".into())),
            'r' => tokens.push(PromptTk::Text("\r".into())),
            't' => tokens.push(PromptTk::RuntimeMillis),
            'j' => tokens.push(PromptTk::JobCount),
            'T' => tokens.push(PromptTk::RuntimeFormatted),
            '\\' => tokens.push(PromptTk::Text("\\".into())),
            '"' => tokens.push(PromptTk::Text("\"".into())),
            '\'' => tokens.push(PromptTk::Text("'".into())),
            '(' => tokens.push(PromptTk::VisGroupOpen),
            ')' => tokens.push(PromptTk::VisGroupClose),
            '@' => {
              let mut func_name = String::new();
              let is_braced = chars.peek() == Some(&'{');
              let mut handled = false;
              while let Some(ch) = chars.peek() {
                match ch {
                  '}' if is_braced => {
                    chars.next();
                    handled = true;
                    break;
                  }
                  'A'..='Z' | 'a'..='z' | '0'..='9' | '_' => {
                    func_name.push(*ch);
                    chars.next();
                  }
                  _ => {
                    handled = true;
                    if is_braced {
                      // Invalid character in braced function name
                      tokens.push(PromptTk::Text(format!("\\@{{{func_name}")));
                    } else {
                      // End of unbraced function name
                      let func_exists = read_logic(|l| l.get_func(&func_name).is_some());
                      if func_exists {
                        tokens.push(PromptTk::Function(func_name.clone()));
                      } else {
                        tokens.push(PromptTk::Text(format!("\\@{func_name}")));
                      }
                    }
                    break;
                  }
                }
              }
              // Handle end-of-input: function name collected but loop ended without pushing
              if !handled && !func_name.is_empty() {
                let func_exists = read_logic(|l| l.get_func(&func_name).is_some());
                if func_exists {
                  tokens.push(PromptTk::Function(func_name));
                } else {
                  tokens.push(PromptTk::Text(format!("\\@{func_name}")));
                }
              }
            }
            'e' => {
              if chars.next() == Some('[') {
                let mut params = String::new();

                // Collect parameters and final character
                while let Some(ch) = chars.next() {
                  match ch {
                    '0'..='9' | ';' | '?' | ':' => params.push(ch), // Valid parameter characters
                    'A'..='Z' | 'a'..='z' => {
                      // Final character (letter)
                      params.push(ch);
                      break;
                    }
                    _ => {
                      // Invalid character in ANSI sequence
                      tokens.push(PromptTk::Text(format!("\x1b[{params}")));
                      break;
                    }
                  }
                }

                tokens.push(PromptTk::AnsiSeq(format!("\x1b[{params}")));
              } else {
                // Handle case where 'e' is not followed by '['
                tokens.push(PromptTk::Text("\\e".into()));
              }
            }
            '0'..='7' => {
              // Handle octal escape
              let mut octal_str = String::new();
              octal_str.push(ch);

              // Collect up to 2 more octal digits
              for _ in 0..2 {
                if let Some(&next_ch) = chars.peek() {
                  if ('0'..='7').contains(&next_ch) {
                    octal_str.push(chars.next().unwrap());
                  } else {
                    break;
                  }
                } else {
                  break;
                }
              }

              // Parse the octal string into an integer
              if let Ok(octal) = i32::from_str_radix(&octal_str, 8) {
                tokens.push(PromptTk::AsciiOct(octal));
              } else {
                // Fallback: treat as raw text
                tokens.push(PromptTk::Text(format!("\\{octal_str}")));
              }
            }
            _ => {
              // Unknown escape sequence: treat as raw text
              tokens.push(PromptTk::Text(format!("\\{ch}")));
            }
          }
        } else {
          // Handle trailing backslash
          tokens.push(PromptTk::Text("\\".into()));
        }
      }
      _ => {
        // Accumulate non-escape characters
        tk_text.push(ch);
      }
    }
  }

  // Push any remaining text as a token
  if !tk_text.is_empty() {
    tokens.push(PromptTk::Text(tk_text));
  }

  tokens
}

pub fn expand_prompt(raw: &str) -> ShResult<String> {
  let mut tokens = tokenize_prompt(raw).into_iter();
  let mut result = String::new();

  while let Some(token) = tokens.next() {
    match token {
      PromptTk::AsciiOct(_) => todo!(),
      PromptTk::Text(txt) => result.push_str(&txt),
      PromptTk::AnsiSeq(params) => result.push_str(&params),
      PromptTk::RuntimeMillis => {
        if let Some(runtime) = write_meta(|m| m.get_time()) {
          let runtime_millis = runtime.as_millis().to_string();
          result.push_str(&runtime_millis);
        }
      }
      PromptTk::RuntimeFormatted => {
        if let Some(runtime) = write_meta(|m| m.get_time()) {
          let runtime_fmt = format_cmd_runtime(runtime);
          result.push_str(&runtime_fmt);
        }
      }
      PromptTk::Pwd => {
        let mut pwd = std::env::var("PWD").unwrap();
        let home = std::env::var("HOME").unwrap();
        if pwd.starts_with(&home) {
          pwd = pwd.replacen(&home, "~", 1);
        }
        result.push_str(&pwd);
      }
      PromptTk::PwdShort => {
        let mut path = std::env::var("PWD").unwrap();
        let home = std::env::var("HOME").unwrap();
        if path.starts_with(&home) {
          path = path.replacen(&home, "~", 1);
        }
        let pathbuf = PathBuf::from(&path);
        let mut segments = pathbuf.iter().count();
        let mut path_iter = pathbuf.iter();
        let max_segments = crate::state::read_shopts(|s| s.prompt.trunc_prompt_path);
        while segments > max_segments {
          path_iter.next();
          segments -= 1;
        }
        let path_rebuilt: PathBuf = path_iter.collect();
        let mut path_rebuilt = path_rebuilt.to_str().unwrap().to_string();
        if path_rebuilt.starts_with(&home) {
          path_rebuilt = path_rebuilt.replacen(&home, "~", 1);
        }
        result.push_str(&path_rebuilt);
      }
      PromptTk::Hostname => {
        let hostname = std::env::var("HOST").unwrap();
        result.push_str(&hostname);
      }
      PromptTk::HostnameShort => todo!(),
      PromptTk::ShellName => result.push_str("shed"),
      PromptTk::Username => {
        let username = std::env::var("USER").unwrap();
        result.push_str(&username);
      }
      PromptTk::PromptSymbol => {
        let uid = std::env::var("UID").unwrap();
        let symbol = if &uid == "0" { '#' } else { '$' };
        result.push(symbol);
      }
      PromptTk::ExitCode => todo!(),
      PromptTk::SuccessSymbol => todo!(),
      PromptTk::FailureSymbol => todo!(),
      PromptTk::JobCount => {
        let count = read_jobs(|j| {
          j.jobs()
            .iter()
            .filter(|j| {
              j.as_ref().is_some_and(|j| {
                j.get_stats()
                  .iter()
                  .all(|st| matches!(st, WtStat::StillAlive))
              })
            })
            .count()
        });
        result.push_str(&count.to_string());
      }
      PromptTk::Function(f) => {
        let output = expand_cmd_sub(&f)?;
        result.push_str(&output);
      }
      PromptTk::VisGrp => todo!(),
      PromptTk::UserSeq => todo!(),
      PromptTk::Weekday => todo!(),
      PromptTk::Dquote => todo!(),
      PromptTk::Squote => todo!(),
      PromptTk::Return => todo!(),
      PromptTk::Newline => todo!(),
      PromptTk::VisGroupOpen => todo!(),
      PromptTk::VisGroupClose => todo!(),
    }
  }

  Ok(result)
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::time::Duration;

  // ===================== tokenize_prompt =====================

  #[test]
  fn prompt_username() {
    let tokens = tokenize_prompt("\\u");
    assert_eq!(tokens.len(), 1);
    assert!(matches!(tokens[0], PromptTk::Username));
  }

  #[test]
  fn prompt_hostname() {
    let tokens = tokenize_prompt("\\h");
    assert_eq!(tokens.len(), 1);
    assert!(matches!(tokens[0], PromptTk::Hostname));
  }

  #[test]
  fn prompt_pwd() {
    let tokens = tokenize_prompt("\\w");
    assert_eq!(tokens.len(), 1);
    assert!(matches!(tokens[0], PromptTk::Pwd));
  }

  #[test]
  fn prompt_pwd_short() {
    let tokens = tokenize_prompt("\\W");
    assert_eq!(tokens.len(), 1);
    assert!(matches!(tokens[0], PromptTk::PwdShort));
  }

  #[test]
  fn prompt_symbol() {
    let tokens = tokenize_prompt("\\$");
    assert_eq!(tokens.len(), 1);
    assert!(matches!(tokens[0], PromptTk::PromptSymbol));
  }

  #[test]
  fn prompt_newline() {
    let tokens = tokenize_prompt("\\n");
    assert_eq!(tokens.len(), 1);
    assert!(matches!(tokens[0], PromptTk::Text(ref t) if t == "\n"));
  }

  #[test]
  fn prompt_shell_name() {
    let tokens = tokenize_prompt("\\s");
    assert_eq!(tokens.len(), 1);
    assert!(matches!(tokens[0], PromptTk::ShellName));
  }

  #[test]
  fn prompt_literal_backslash() {
    let tokens = tokenize_prompt("\\\\");
    assert_eq!(tokens.len(), 1);
    assert!(matches!(tokens[0], PromptTk::Text(ref t) if t == "\\"));
  }

  #[test]
  fn prompt_mixed() {
    let tokens = tokenize_prompt("\\u@\\h \\w\\$ ");
    // \u, Text("@"), \h, Text(" "), \w, \$, Text(" ")
    assert_eq!(tokens.len(), 7);
    assert!(matches!(tokens[0], PromptTk::Username));
    assert!(matches!(tokens[1], PromptTk::Text(ref t) if t == "@"));
    assert!(matches!(tokens[2], PromptTk::Hostname));
    assert!(matches!(tokens[3], PromptTk::Text(ref t) if t == " "));
    assert!(matches!(tokens[4], PromptTk::Pwd));
    assert!(matches!(tokens[5], PromptTk::PromptSymbol));
    assert!(matches!(tokens[6], PromptTk::Text(ref t) if t == " "));
  }

  #[test]
  fn prompt_ansi_sequence() {
    let tokens = tokenize_prompt("\\e[31m");
    assert_eq!(tokens.len(), 1);
    assert!(matches!(tokens[0], PromptTk::AnsiSeq(ref s) if s == "\x1b[31m"));
  }

  #[test]
  fn prompt_octal() {
    let tokens = tokenize_prompt("\\141"); // 'a' in octal
    assert_eq!(tokens.len(), 1);
    assert!(matches!(tokens[0], PromptTk::AsciiOct(97)));
  }

  // ===================== format_cmd_runtime =====================

  #[test]
  fn runtime_millis() {
    let dur = Duration::from_millis(500);
    assert_eq!(format_cmd_runtime(dur), "500ms");
  }

  #[test]
  fn runtime_seconds() {
    let dur = Duration::from_secs(5);
    assert_eq!(format_cmd_runtime(dur), "5s");
  }

  #[test]
  fn runtime_minutes_and_seconds() {
    let dur = Duration::from_secs(125);
    assert_eq!(format_cmd_runtime(dur), "2m 5s");
  }

  #[test]
  fn runtime_hours() {
    let dur = Duration::from_secs(3661);
    assert_eq!(format_cmd_runtime(dur), "1h 1m 1s");
  }

  #[test]
  fn runtime_micros() {
    let dur = Duration::from_micros(500);
    assert_eq!(format_cmd_runtime(dur), "500µs");
  }
}
