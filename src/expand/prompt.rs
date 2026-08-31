use std::path::PathBuf;

use bstr::ByteSlice;

use crate::{
  expand::{escape, subshell, var},
  match_loop, shopt, shopt_mut,
  state::{Shed, paths, vars::VarStr},
  status_msg,
  util::{
    self,
    error::ShResult,
    strops::{self, ByteCursor, SliceCursor},
    ui,
  },
  var,
};

use nix::sys::wait::WaitStatus as WtStat;
use smol_str::format_smolstr;

#[derive(Debug)]
pub(crate) enum PromptTk {
  AsciiOct(i32),
  Text(VarStr),
  AnsiSeq(VarStr),
  Color(VarStr),    // plain english color descriptions
  Function(VarStr), // Expands to the output of any defined shell function
  RuntimeMillis,
  RuntimeFormatted,
  Pwd,
  PwdShort,
  Hostname,
  HostnameShort,
  ShellName,
  Username,
  PromptSymbol,
  JobCount,
}

#[expect(clippy::too_many_lines)]
fn tokenize_prompt(raw: &[u8]) -> Vec<PromptTk> {
  let mut cur = SliceCursor::new(raw);
  let mut tk_text = util::scratch_buf();
  let mut tokens = vec![];

  match_loop!(cur.next_byte() => ch, {
    b'\\' => {
      // Push any accumulated text as a token
      if !tk_text.is_empty() {
        tokens.push(PromptTk::Text(std::mem::take(&mut tk_text).into()));
      }

      // Handle the escape sequence
      let Some(ch) = cur.next_byte() else {
        // Handle trailing backslash
        tokens.push(PromptTk::Text("\\".into()));
        break
      };
      match ch {
        b'w' => tokens.push(PromptTk::Pwd),
        b'W' => tokens.push(PromptTk::PwdShort),
        b'h' => tokens.push(PromptTk::HostnameShort),
        b'H' => tokens.push(PromptTk::Hostname),
        b's' => tokens.push(PromptTk::ShellName),
        b'u' => tokens.push(PromptTk::Username),
        b'$' => tokens.push(PromptTk::PromptSymbol),
        b'n' => tokens.push(PromptTk::Text("\n".into())),
        b'r' => tokens.push(PromptTk::Text("\r".into())),
        b't' => tokens.push(PromptTk::RuntimeMillis),
        b'j' => tokens.push(PromptTk::JobCount),
        b'T' => tokens.push(PromptTk::RuntimeFormatted),
        b'\\' => tokens.push(PromptTk::Text("\\".into())),
        b'"' => tokens.push(PromptTk::Text("\"".into())),
        b'\'' => tokens.push(PromptTk::Text("'".into())),
        b'c' => {
          let Some(b'{') = cur.peek_byte() else {
            tk_text.extend_from_slice(b"\\c");
            continue;
          };
          cur.next_byte(); // consume the '{'
          let mut desc = util::scratch_buf();
          match_loop!(cur.next_byte() => ch, {
            b'}' => break,
            _ => desc.push(ch)
          });
          tokens.push(PromptTk::Color(desc.into()));
        }
        b'@' => {
          let mut func_name = util::scratch_buf();
          let is_braced = cur.peek_byte() == Some(b'{');
          let mut handled = false;
          match_loop!(cur.peek_byte() => ch, {
            b'}' if is_braced => {
              cur.next_byte();
              handled = true;
              break;
            }
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' => {
              func_name.push(ch);
              cur.next_byte();
            }
            _ => {
              handled = true;
              if is_braced {
                // Invalid character in braced function name
                tokens.push(PromptTk::Text(format_smolstr!("\\@{{{}", func_name.as_bstr()).into()));
              } else {
                // End of unbraced function name
                let func_exists = Shed::logic(|l| l.get_func(&String::from_utf8_lossy(&func_name)).is_some());
                if func_exists {
                  tokens.push(PromptTk::Function(func_name.clone().into()));
                } else {
                  tokens.push(PromptTk::Text(format_smolstr!("\\@{}", func_name.as_bstr()).into()));
                }
              }
              break;
            }
          });
          // Handle end-of-input: function name collected but loop ended without pushing
          if !handled && !func_name.is_empty() {
            let func_exists = Shed::logic(|l| l.get_func(&String::from_utf8_lossy(&func_name)).is_some());
            if func_exists {
              tokens.push(PromptTk::Function(func_name.into()));
            } else {
              tokens.push(PromptTk::Text(format_smolstr!("\\@{}", func_name.as_bstr()).into()));
            }
          }
        }
        b'e' => {
          if cur.next_byte() == Some(b'[') {
            let mut params = util::scratch_buf();

            // Collect parameters and final character
            match_loop!(cur.next_byte() => ch, {
              b'0'..=b'9' | b';' | b'?' | b':' => params.push(ch), // Valid parameter characters
              b'A'..=b'Z' | b'a'..=b'z' => {
                // Final character (letter)
                params.push(ch);
                break;
              }
              _ => {
                // Invalid character in ANSI sequence
                tokens.push(PromptTk::Text(format_smolstr!("\x1b[{}", params.as_bstr()).into()));
                break;
              }
            });

            tokens.push(PromptTk::AnsiSeq(format_smolstr!("\x1b[{}", params.as_bstr()).into()));
          } else {
            // Handle case where 'e' is not followed by '['
            tokens.push(PromptTk::Text("\\e".into()));
          }
        }
        b'0'..=b'7' => {
          // Handle octal escape
          let mut octal_str = util::scratch_buf();
          octal_str.push(ch);

          // Collect up to 2 more octal digits
          for _ in 0..2 {
            if let Some(next_ch) = cur.peek_byte() {
              if (b'0'..=b'7').contains(&next_ch) {
                octal_str.push(cur.next_byte().unwrap());
              } else {
                break;
              }
            } else {
              break;
            }
          }

          // Parse the octal string into an integer (digits only, so always UTF-8)
          let parsed = std::str::from_utf8(&octal_str)
            .ok()
            .and_then(|s| i32::from_str_radix(s, 8).ok());
          if let Some(octal) = parsed {
            tokens.push(PromptTk::AsciiOct(octal));
          } else {
            // Fallback: treat as raw text
            tokens.push(PromptTk::Text(format_smolstr!("\\{}", octal_str.as_bstr()).into()));
          }
        }
        _ => {
          // Unknown escape sequence: treat as raw text
          tokens.push(PromptTk::Text(format_smolstr!("\\{}", ch as char).into()));
        }
      }
    }
    _ => {
      // Accumulate non-escape characters
      tk_text.push(ch);
    }
  });
  // Push any remaining text as a token
  if !tk_text.is_empty() {
    tokens.push(PromptTk::Text(tk_text.into()));
  }

  tokens
}

pub(crate) fn expand_prompt(raw: &[u8]) -> ShResult<String> {
  let mut tokens = tokenize_prompt(raw).into_iter();
  let mut result = String::new();

  match_loop!(tokens.next()    => token, {
    PromptTk::Text(txt)        => result.push_str(&txt.to_str_lossy()),
    PromptTk::AnsiSeq(params)  => result.push_str(&params.to_str_lossy()),
    PromptTk::ShellName        => result.push_str("shed"),
    PromptTk::Color(c)         => ansi_color(&c.to_str_lossy(), &mut result),
    PromptTk::RuntimeMillis    => runtime(false, &mut result),
    PromptTk::RuntimeFormatted => runtime(true, &mut result),
    PromptTk::Pwd              => prompt_pwd(false, &mut result),
    PromptTk::PwdShort         => prompt_pwd(true, &mut result),
    PromptTk::Username         => username(&mut result),
    PromptTk::PromptSymbol     => prompt_symbol(&mut result),
    PromptTk::Hostname         => hostname(false, &mut result),
    PromptTk::HostnameShort    => hostname(true, &mut result),
    PromptTk::JobCount         => job_count(&mut result),
    PromptTk::AsciiOct(n)      => ascii_oct(n, &mut result),
    PromptTk::Function(f)      => func_expand(&f.to_str_lossy(), &mut result)?,
  });

  if shopt!(prompt.substitute) {
    let marked = escape::unescape_prompt(&result);
    let expanded = var::expand_raw_inner(&mut marked.cursor(), true, false)?;
    result = String::from_utf8_lossy(&expanded.into_bytes()).into_owned();
  }

  Ok(result)
}

fn ansi_color(c: &str, out: &mut String) {
  match ui::ansi_from_description(c) {
    Ok(esc_seq) => out.push_str(esc_seq.as_str()),
    Err(e) => status_msg!("{e}"),
  }
}

fn runtime(formatted: bool, out: &mut String) {
  let Some(runtime) = Shed::meta_mut(|m| m.get_time()) else {
    return;
  };
  if formatted {
    let runtime_fmt = strops::format_time(runtime);
    out.push_str(&runtime_fmt);
  } else {
    let runtime_millis = runtime.as_millis().to_string();
    out.push_str(&runtime_millis);
  }
}

fn prompt_pwd(short: bool, out: &mut String) {
  let pwd = paths::display_path(var!("PWD"));

  if !short {
    out.push_str(&pwd);
    return;
  }

  let pathbuf = PathBuf::from(&pwd);

  let mut segments = pathbuf.iter().count();
  let mut path_iter = pathbuf.iter();
  let max_segments = shopt!(prompt.trunc_prompt_path);
  while segments > max_segments {
    path_iter.next();
    segments -= 1;
  }
  let path_rebuilt: PathBuf = path_iter.collect();
  let path_rebuilt = path_rebuilt.to_str().unwrap().to_string();

  out.push_str(&path_rebuilt);
}

fn username(out: &mut String) {
  let username = var!("USER");
  out.push_str(&username.to_str_lossy());
}

fn prompt_symbol(out: &mut String) {
  let uid = var!("UID");
  let symbol = if &uid == "0" { '#' } else { '$' };
  out.push(symbol);
}

fn job_count(out: &mut String) {
  let count = Shed::jobs(|j| {
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
  out.push_str(&count.to_string());
}

fn ascii_oct(n: i32, out: &mut String) {
  if let Some(ch) = std::char::from_u32(n.cast_unsigned()) {
    out.push(ch);
  }
}

fn func_expand(input: &str, out: &mut String) -> ShResult<()> {
  let errexit = shopt!(set.errexit);
  let noexec = shopt!(set.noexec);
  let xtrace = shopt!(set.xtrace);

  shopt_mut!(set.errexit = false);
  shopt_mut!(set.noexec = false);
  shopt_mut!(set.xtrace = false);
  let res = subshell::expand_cmd_sub(input);
  shopt_mut!(set.errexit = errexit);
  shopt_mut!(set.noexec = noexec);
  shopt_mut!(set.xtrace = xtrace);

  out.push_str(&res?.to_str_lossy());
  Ok(())
}

fn hostname(short: bool, out: &mut String) {
  let hostname = var!("HOST");

  if short && let Some(first) = hostname.to_str_lossy().split('.').next() {
    out.push_str(first);
  } else {
    out.push_str(&hostname.to_str_lossy());
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::time::Duration;

  // ===================== tokenize_prompt =====================

  #[test]
  fn prompt_username() {
    let tokens = tokenize_prompt(b"\\u");
    assert_eq!(tokens.len(), 1);
    assert!(matches!(tokens[0], PromptTk::Username));
  }

  #[test]
  fn prompt_hostname() {
    let tokens = tokenize_prompt(b"\\H");
    assert_eq!(tokens.len(), 1);
    assert!(matches!(tokens[0], PromptTk::Hostname));
  }

  #[test]
  fn prompt_pwd() {
    let tokens = tokenize_prompt(b"\\w");
    assert_eq!(tokens.len(), 1);
    assert!(matches!(tokens[0], PromptTk::Pwd));
  }

  #[test]
  fn prompt_pwd_short() {
    let tokens = tokenize_prompt(b"\\W");
    assert_eq!(tokens.len(), 1);
    assert!(matches!(tokens[0], PromptTk::PwdShort));
  }

  #[test]
  fn prompt_symbol() {
    let tokens = tokenize_prompt(b"\\$");
    assert_eq!(tokens.len(), 1);
    assert!(matches!(tokens[0], PromptTk::PromptSymbol));
  }

  #[test]
  fn prompt_newline() {
    let tokens = tokenize_prompt(b"\\n");
    assert_eq!(tokens.len(), 1);
    assert!(matches!(tokens[0], PromptTk::Text(ref t) if t == "\n"));
  }

  #[test]
  fn prompt_shell_name() {
    let tokens = tokenize_prompt(b"\\s");
    assert_eq!(tokens.len(), 1);
    assert!(matches!(tokens[0], PromptTk::ShellName));
  }

  #[test]
  fn prompt_literal_backslash() {
    let tokens = tokenize_prompt(b"\\\\");
    assert_eq!(tokens.len(), 1);
    assert!(matches!(tokens[0], PromptTk::Text(ref t) if t == "\\"));
  }

  #[test]
  fn prompt_mixed() {
    let tokens = tokenize_prompt(b"\\u@\\h \\w\\$ ");
    // \u, Text("@"), \h, Text(" "), \w, \$, Text(" ")
    assert_eq!(tokens.len(), 7);
    assert!(matches!(tokens[0], PromptTk::Username));
    assert!(matches!(tokens[1], PromptTk::Text(ref t) if t == "@"));
    assert!(matches!(tokens[2], PromptTk::HostnameShort));
    assert!(matches!(tokens[3], PromptTk::Text(ref t) if t == " "));
    assert!(matches!(tokens[4], PromptTk::Pwd));
    assert!(matches!(tokens[5], PromptTk::PromptSymbol));
    assert!(matches!(tokens[6], PromptTk::Text(ref t) if t == " "));
  }

  #[test]
  fn prompt_ansi_sequence() {
    let tokens = tokenize_prompt(b"\\e[31m");
    assert_eq!(tokens.len(), 1);
    assert!(matches!(tokens[0], PromptTk::AnsiSeq(ref s) if s == "\x1b[31m"));
  }

  #[test]
  fn prompt_octal() {
    let tokens = tokenize_prompt(b"\\141"); // 'a' in octal
    assert_eq!(tokens.len(), 1);
    assert!(matches!(tokens[0], PromptTk::AsciiOct(97)));
  }

  // ===================== format_cmd_runtime =====================

  #[test]
  fn runtime_millis() {
    let dur = Duration::from_millis(500);
    assert_eq!(strops::format_time(dur), "500ms");
  }

  #[test]
  fn runtime_seconds() {
    let dur = Duration::from_secs(5);
    assert_eq!(strops::format_time(dur), "5s");
  }

  #[test]
  fn runtime_minutes_and_seconds() {
    let dur = Duration::from_secs(125);
    assert_eq!(strops::format_time(dur), "2m 5s");
  }

  #[test]
  fn runtime_hours() {
    let dur = Duration::from_secs(3661);
    assert_eq!(strops::format_time(dur), "1h 1m 1s");
  }

  #[test]
  fn runtime_micros() {
    let dur = Duration::from_micros(500);
    assert_eq!(strops::format_time(dur), "500µs");
  }

  // ===================== tokenize_prompt extra escapes =====================

  #[test]
  fn prompt_carriage_return_escape() {
    let tokens = tokenize_prompt(b"\\r");
    assert_eq!(tokens.len(), 1);
    assert!(matches!(tokens[0], PromptTk::Text(ref t) if t == "\r"));
  }

  #[test]
  fn prompt_runtime_millis_token() {
    let tokens = tokenize_prompt(b"\\t");
    assert_eq!(tokens.len(), 1);
    assert!(matches!(tokens[0], PromptTk::RuntimeMillis));
  }

  #[test]
  fn prompt_runtime_formatted_token() {
    let tokens = tokenize_prompt(b"\\T");
    assert_eq!(tokens.len(), 1);
    assert!(matches!(tokens[0], PromptTk::RuntimeFormatted));
  }

  #[test]
  fn prompt_job_count_token() {
    let tokens = tokenize_prompt(b"\\j");
    assert_eq!(tokens.len(), 1);
    assert!(matches!(tokens[0], PromptTk::JobCount));
  }

  #[test]
  fn prompt_escaped_double_quote() {
    let tokens = tokenize_prompt(b"\\\"");
    assert_eq!(tokens.len(), 1);
    assert!(matches!(tokens[0], PromptTk::Text(ref t) if t == "\""));
  }

  #[test]
  fn prompt_escaped_single_quote() {
    let tokens = tokenize_prompt(b"\\'");
    assert_eq!(tokens.len(), 1);
    assert!(matches!(tokens[0], PromptTk::Text(ref t) if t == "'"));
  }

  #[test]
  fn prompt_color_braced() {
    let tokens = tokenize_prompt(b"\\c{red}");
    assert_eq!(tokens.len(), 1);
    assert!(matches!(tokens[0], PromptTk::Color(ref c) if c == "red"));
  }

  #[test]
  fn prompt_color_without_brace_falls_back_to_text() {
    // `\c` not followed by `{` is treated as raw `\c` text. Note: the
    // current implementation `break`s out of the outer tokenize loop in
    // this arm, so any chars following `\c` are dropped from the token
    // stream — we pin that observed behavior here.
    let tokens = tokenize_prompt(b"\\cfoo");
    assert_eq!(tokens.len(), 1);
    assert!(matches!(tokens[0], PromptTk::Text(ref t) if t == "\\cfoo"));
  }

  #[test]
  fn prompt_function_undefined_becomes_text() {
    // `\@somename` with no defined function falls back to Text.
    let _g = crate::tests::testutil::TestGuard::new();
    let tokens = tokenize_prompt(b"\\@nope_unlikely_to_exist 1");
    // The non-alphanumeric ' ' terminates the function name → Text fallback
    assert!(matches!(tokens[0], PromptTk::Text(ref t) if t == "\\@nope_unlikely_to_exist"));
  }

  #[test]
  fn prompt_function_defined_becomes_function_token() {
    let _g = crate::tests::testutil::TestGuard::new();
    crate::tests::testutil::test_input("prompt_fn() { echo hi; }").unwrap();
    let tokens = tokenize_prompt(b"\\@prompt_fn ");
    assert!(matches!(tokens[0], PromptTk::Function(ref f) if f == "prompt_fn"));
  }

  #[test]
  fn prompt_trailing_backslash_is_literal() {
    let tokens = tokenize_prompt(b"foo\\");
    // First token: Text("foo"), second: Text("\\")
    assert_eq!(tokens.len(), 2);
    assert!(matches!(tokens[0], PromptTk::Text(ref t) if t == "foo"));
    assert!(matches!(tokens[1], PromptTk::Text(ref t) if t == "\\"));
  }

  // Note: the `Err(_)` arm of the octal `i32::from_str_radix` parse and the
  // `None` arm of HostnameShort's `segments.next()` are unreachable in
  // practice — 3-digit octal fits comfortably in i32, and `str::split` always
  // yields at least one element. No tests written for those.

  // ===================== expand_prompt =====================

  #[test]
  fn expand_color_emits_ansi_sequence() {
    let _g = crate::tests::testutil::TestGuard::new();
    let out = expand_prompt(b"\\c{red}").unwrap();
    // ansi_from_description("red") yields a CSI sequence containing "31".
    assert!(out.contains("\x1b["), "no escape in {out:?}");
    assert!(out.contains("31"), "no red code in {out:?}");
  }

  #[test]
  fn expand_color_unknown_falls_through_silently() {
    // Unknown color description → status_msg fires, nothing appended.
    let _g = crate::tests::testutil::TestGuard::new();
    let out = expand_prompt(b"X\\c{notacolor}Y").unwrap();
    assert_eq!(out, "XY");
  }

  #[test]
  fn expand_runtime_millis_when_timer_unset_emits_nothing() {
    let _g = crate::tests::testutil::TestGuard::new();
    let out = expand_prompt(b"X\\tY").unwrap();
    assert_eq!(out, "XY");
  }

  #[test]
  fn expand_runtime_millis_when_timer_set_emits_digits() {
    let _g = crate::tests::testutil::TestGuard::new();
    Shed::meta_mut(|m| {
      m.start_timer();
      // give it some measurable time
      std::thread::sleep(Duration::from_millis(2));
      m.stop_timer();
    });
    let out = expand_prompt(b"\\t").unwrap();
    assert!(
      out.chars().all(|c| c.is_ascii_digit()),
      "expected only digits, got {out:?}"
    );
    assert!(!out.is_empty());
  }

  #[test]
  fn expand_runtime_formatted_when_timer_set_emits_unit() {
    let _g = crate::tests::testutil::TestGuard::new();
    Shed::meta_mut(|m| {
      m.start_timer();
      std::thread::sleep(Duration::from_millis(2));
      m.stop_timer();
    });
    let out = expand_prompt(b"\\T").unwrap();
    // format_time appends "ms"/"s"/"µs" — at minimum the output is non-empty
    // and contains at least one non-digit unit suffix character.
    assert!(!out.is_empty());
    assert!(out.chars().any(|c| !c.is_ascii_digit()));
  }

  #[test]
  fn expand_pwd_with_home_prefix_collapses_to_tilde() {
    let _g = crate::tests::testutil::TestGuard::new();
    Shed::vars_mut(|v| {
      v.set_var(
        "PWD",
        crate::state::vars::VarKind::string("/home/testuser/proj".into()),
        crate::state::vars::VarFlags::empty(),
      )
      .unwrap();
      v.set_var(
        "HOME",
        crate::state::vars::VarKind::string("/home/testuser".into()),
        crate::state::vars::VarFlags::empty(),
      )
      .unwrap();
    });
    let out = expand_prompt(b"\\w").unwrap();
    assert_eq!(out, "~/proj");
  }

  #[test]
  fn expand_pwd_short_truncates_to_max_segments() {
    let _g = crate::tests::testutil::TestGuard::new();
    Shed::shopts_mut(|o| o.prompt.trunc_prompt_path = 2);
    Shed::vars_mut(|v| {
      v.set_var(
        "PWD",
        crate::state::vars::VarKind::string("/a/b/c/d/e".into()),
        crate::state::vars::VarFlags::empty(),
      )
      .unwrap();
      v.set_var(
        "HOME",
        crate::state::vars::VarKind::string("/nowhere".into()),
        crate::state::vars::VarFlags::empty(),
      )
      .unwrap();
    });
    let out = expand_prompt(b"\\W").unwrap();
    // 5 segments + leading "/" → 6 PathBuf components; trim down to 2 → "d/e"
    // PathBuf iter on "/a/b/c/d/e" yields ["/", "a", "b", "c", "d", "e"] (6 segments).
    // We trim while segments > 2: 6→5→4→3→2 stops. Last two: ["d","e"] → "d/e".
    assert_eq!(out, "d/e");
  }

  #[test]
  fn expand_pwd_short_rebuilt_path_replaces_home_again() {
    // If the truncated path itself still starts with $HOME, the second
    // starts_with(&home) replacement collapses it to "~".
    let _g = crate::tests::testutil::TestGuard::new();
    // Make truncation a no-op so we hit the second tilde-replacement on
    // the rebuilt path even when no segments were dropped.
    Shed::shopts_mut(|o| o.prompt.trunc_prompt_path = 100);
    Shed::vars_mut(|v| {
      v.set_var(
        "PWD",
        crate::state::vars::VarKind::string("/home/testuser/proj".into()),
        crate::state::vars::VarFlags::empty(),
      )
      .unwrap();
      v.set_var(
        "HOME",
        crate::state::vars::VarKind::string("/home/testuser".into()),
        crate::state::vars::VarFlags::empty(),
      )
      .unwrap();
    });
    let out = expand_prompt(b"\\W").unwrap();
    assert_eq!(out, "~/proj");
  }

  #[test]
  fn expand_hostname_full() {
    let _g = crate::tests::testutil::TestGuard::new();
    Shed::vars_mut(|v| {
      v.set_var(
        "HOST",
        crate::state::vars::VarKind::string("box.example.com".into()),
        crate::state::vars::VarFlags::empty(),
      )
      .unwrap();
    });
    let out = expand_prompt(b"\\H").unwrap();
    assert_eq!(out, "box.example.com");
  }

  #[test]
  fn expand_hostname_short_takes_first_segment() {
    let _g = crate::tests::testutil::TestGuard::new();
    Shed::vars_mut(|v| {
      v.set_var(
        "HOST",
        crate::state::vars::VarKind::string("box.example.com".into()),
        crate::state::vars::VarFlags::empty(),
      )
      .unwrap();
    });
    let out = expand_prompt(b"\\h").unwrap();
    assert_eq!(out, "box");
  }

  #[test]
  fn expand_job_count_is_zero_with_no_jobs() {
    let _g = crate::tests::testutil::TestGuard::new();
    let out = expand_prompt(b"\\j").unwrap();
    assert_eq!(out, "0");
  }

  #[test]
  fn expand_ascii_octal_emits_char() {
    // \141 → octal 141 = 0x61 = 'a'
    let _g = crate::tests::testutil::TestGuard::new();
    let out = expand_prompt(b"\\141").unwrap();
    assert_eq!(out, "a");
  }

  #[test]
  fn expand_function_runs_and_appends_output() {
    // The terminating space is what ends the unbraced \@name, so it's
    // preserved as part of the surrounding Text token after the function
    // expands: "[" + "hello" + " ]".
    let _g = crate::tests::testutil::TestGuard::new();
    crate::tests::testutil::test_input("prompt_greet() { printf hello; }").unwrap();
    let out = expand_prompt(b"[\\@prompt_greet ]").unwrap();
    assert_eq!(out, "[hello ]");
  }

  // ===================== prompt.substitute =====================

  fn with_substitute<F: FnOnce()>(enabled: bool, body: F) {
    let prev = Shed::shopts(|o| o.prompt.substitute);
    Shed::shopts_mut(|o| o.prompt.substitute = enabled);
    body();
    Shed::shopts_mut(|o| o.prompt.substitute = prev);
  }

  fn set_var(name: &str, value: &str) {
    use crate::state::vars::{VarFlags, VarKind};
    Shed::vars_mut(|v| v.set_var(name, VarKind::string(value.into()), VarFlags::empty())).unwrap();
  }

  #[test]
  fn substitute_off_leaves_dollar_var_literal() {
    let _g = crate::tests::testutil::TestGuard::new();
    set_var("MY_PROMPT_VAR", "hello");
    with_substitute(false, || {
      let out = expand_prompt(b"$MY_PROMPT_VAR").unwrap();
      assert_eq!(out, "$MY_PROMPT_VAR");
    });
  }

  #[test]
  fn substitute_on_expands_dollar_var() {
    let _g = crate::tests::testutil::TestGuard::new();
    set_var("MY_PROMPT_VAR", "hello");
    with_substitute(true, || {
      let out = expand_prompt(b"$MY_PROMPT_VAR").unwrap();
      assert_eq!(out, "hello");
    });
  }

  #[test]
  fn substitute_expands_braced_var() {
    let _g = crate::tests::testutil::TestGuard::new();
    set_var("MY_PROMPT_VAR", "hello");
    with_substitute(true, || {
      let out = expand_prompt(b"[${MY_PROMPT_VAR}]").unwrap();
      assert_eq!(out, "[hello]");
    });
  }

  #[test]
  fn substitute_uses_default_for_unset_var() {
    // ${UNSET:-fallback} → "fallback" when UNSET is undefined.
    let _g = crate::tests::testutil::TestGuard::new();
    with_substitute(true, || {
      let out = expand_prompt(b"${DEFINITELY_UNSET:-fallback}").unwrap();
      assert_eq!(out, "fallback");
    });
  }

  #[test]
  fn substitute_runs_after_prompt_escapes() {
    // Order matters: prompt-escape pass runs first, then substitution. A
    // function call produces a value the substitution pass operates on.
    let _g = crate::tests::testutil::TestGuard::new();
    set_var("SUFFIX", "world");
    with_substitute(true, || {
      let out = expand_prompt(b"\\s/$SUFFIX").unwrap();
      assert_eq!(out, "shed/world");
    });
  }

  #[test]
  fn substitute_preserves_unrelated_text() {
    let _g = crate::tests::testutil::TestGuard::new();
    set_var("MID", "X");
    with_substitute(true, || {
      let out = expand_prompt(b"-- $MID --").unwrap();
      assert_eq!(out, "-- X --");
    });
  }

  #[test]
  fn substitute_preserves_ansi_escape_after_dollar() {
    // Regression: read_varsub used to consume the byte after a non-varname
    // `$`, which silently ate the `\x1b` in colored prompts like `\e[0m`.
    let _g = crate::tests::testutil::TestGuard::new();
    with_substitute(true, || {
      let out = expand_prompt(b"\\e[32m$ \\e[0m").unwrap();
      assert!(out.contains("$ \x1b[0m"), "got {out:?}");
      assert!(out.starts_with("\x1b[32m"), "got {out:?}");
    });
  }

  #[test]
  fn substitute_handles_special_param_in_braces() {
    // Regression: ${?:-?} used to fail because `?` was treated as the
    // operator char in perform_param_expansion, leaving an empty var_name
    // and erroring downstream. Statline left_string would then fall back
    // to displaying its raw template.
    let _g = crate::tests::testutil::TestGuard::new();
    Shed::set_status(7);
    with_substitute(true, || {
      let out = expand_prompt(b"[${?:-fallback}]").unwrap();
      assert_eq!(out, "[7]");
    });
  }

  #[test]
  fn substitute_special_param_default_fires_when_empty() {
    // When $? is "0" it's still set (non-null), so :- shouldn't kick in.
    // This pins the behavior — change here means a behavioral change.
    let _g = crate::tests::testutil::TestGuard::new();
    Shed::set_status(0);
    with_substitute(true, || {
      let out = expand_prompt(b"${?:-fallback}").unwrap();
      assert_eq!(out, "0");
    });
  }

  #[test]
  fn substitute_multiple_vars_one_expand() {
    let _g = crate::tests::testutil::TestGuard::new();
    set_var("A", "1");
    set_var("B", "2");
    with_substitute(true, || {
      let out = expand_prompt(b"$A-$B").unwrap();
      assert_eq!(out, "1-2");
    });
  }

  #[test]
  fn substitute_strips_escape_markers_from_backslashes() {
    // Regression: `\$shed` in a prompt context used to leave a PUA ESCAPE
    // marker glued to the `$` after the substitute pass, because the word-
    // context strip step was never reached. The final prompt should contain
    // just the literal `$shed`, no marker char.
    let _g = crate::tests::testutil::TestGuard::new();
    with_substitute(true, || {
      let out = expand_prompt(b"\\\\$foo").unwrap();
      assert_eq!(out, "$foo");
      assert!(
        !out.contains(crate::expand::markers::ESCAPE),
        "escape marker leaked: {out:?}"
      );
    });
  }

  #[test]
  fn substitute_does_not_recurse_on_value() {
    // A var whose value contains $X stays literal after one substitution
    // pass; we don't re-run the tokenizer/expander on output.
    let _g = crate::tests::testutil::TestGuard::new();
    set_var("OUTER", "$INNER");
    set_var("INNER", "should-not-appear");
    with_substitute(true, || {
      let out = expand_prompt(b"$OUTER").unwrap();
      assert_eq!(out, "$INNER");
    });
  }
}
