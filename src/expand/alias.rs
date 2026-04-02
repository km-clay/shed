use std::collections::{HashSet, VecDeque};

use crate::parse::lex::{LexFlags, LexStream, TkFlags};
use crate::readline::keys::{KeyCode, KeyEvent, ModKeys};
use crate::state::{LogTab, read_shopts};

/// Expand aliases in the given input string
///
/// Recursively calls itself until all aliases are expanded
pub fn expand_aliases(
  input: String,
  mut already_expanded: HashSet<String>,
  log_tab: &LogTab,
) -> String {
  let mut result = input.clone();
  let tokens: Vec<_> = LexStream::new(input.into(), LexFlags::empty()).collect();
  let mut expanded_this_iter: Vec<String> = vec![];

  for token_result in tokens.into_iter().rev() {
    let Ok(tk) = token_result else { continue };

    if !tk.flags.contains(TkFlags::IS_CMD) {
      continue;
    }
    if tk.flags.contains(TkFlags::KEYWORD) {
      continue;
    }

    let raw_tk = tk.span.as_str().to_string();

    if already_expanded.contains(&raw_tk) {
      continue;
    }

    if let Some(alias) = log_tab.get_alias(&raw_tk) {
      result.replace_range(tk.span.range(), &alias.to_string());
      expanded_this_iter.push(raw_tk);
    }
  }

  if expanded_this_iter.is_empty() {
    result
  } else {
    already_expanded.extend(expanded_this_iter);
    expand_aliases(result, already_expanded, log_tab)
  }
}

pub fn expand_keymap(s: &str) -> Vec<KeyEvent> {
  let mut keys = Vec::new();
  let mut chars = s.chars().collect::<VecDeque<char>>();
  while let Some(ch) = chars.pop_front() {
    match ch {
      '\\' => {
        if let Some(next_ch) = chars.pop_front() {
          keys.push(KeyEvent(KeyCode::Char(next_ch), ModKeys::NONE));
        }
      }
      '<' => {
        let mut alias = String::new();
        while let Some(a_ch) = chars.pop_front() {
          match a_ch {
            '\\' => {
              if let Some(esc_ch) = chars.pop_front() {
                alias.push(esc_ch);
              }
            }
            '>' => {
              if alias.eq_ignore_ascii_case("leader") {
                let mut leader = read_shopts(|o| o.prompt.leader.clone());
                if leader == "\\" {
                  leader.push('\\');
                }
                keys.extend(expand_keymap(&leader));
              } else if let Some(key) = parse_key_alias(&alias) {
                keys.push(key);
              }
              break;
            }
            _ => alias.push(a_ch),
          }
        }
      }
      _ => {
        keys.push(KeyEvent(KeyCode::Char(ch), ModKeys::NONE));
      }
    }
  }

  keys
}

pub fn parse_key_alias(alias: &str) -> Option<KeyEvent> {
  let parts: Vec<&str> = alias.split('-').collect();
  let (mods_parts, key_name) = parts.split_at(parts.len() - 1);
  let mut mods = ModKeys::NONE;
  for m in mods_parts {
    match m.to_uppercase().as_str() {
      "C" => mods |= ModKeys::CTRL,
      "A" | "M" => mods |= ModKeys::ALT,
      "S" => mods |= ModKeys::SHIFT,
      _ => return None,
    }
  }

  let key = match key_name.first()?.to_uppercase().as_str() {
    "CR" => KeyCode::Char('\r'),
    "ENTER" | "RETURN" => KeyCode::Enter,
    "ESC" | "ESCAPE" => KeyCode::Esc,
    "TAB" => KeyCode::Tab,
    "BS" | "BACKSPACE" => KeyCode::Backspace,
    "DEL" | "DELETE" => KeyCode::Delete,
    "INS" | "INSERT" => KeyCode::Insert,
    "SPACE" => KeyCode::Char(' '),
    "UP" => KeyCode::Up,
    "DOWN" => KeyCode::Down,
    "LEFT" => KeyCode::Left,
    "RIGHT" => KeyCode::Right,
    "HOME" => KeyCode::Home,
    "END" => KeyCode::End,
    "CMD" => KeyCode::ExMode,
    "PGUP" | "PAGEUP" => KeyCode::PageUp,
    "PGDN" | "PAGEDOWN" => KeyCode::PageDown,
    k if k.len() == 1 => KeyCode::Char(k.chars().next().unwrap()),
    _ => return None,
  };

  Some(KeyEvent(key, mods))
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::parse::lex::Span;
  use crate::testutil::TestGuard;

  // ===================== parse_key_alias =====================

  #[test]
  fn key_alias_cr() {
    let key = parse_key_alias("CR").unwrap();
    assert_eq!(key, KeyEvent(KeyCode::Char('\r'), ModKeys::NONE));
  }

  #[test]
  fn key_alias_enter() {
    let key = parse_key_alias("ENTER").unwrap();
    assert_eq!(key, KeyEvent(KeyCode::Enter, ModKeys::NONE));
  }

  #[test]
  fn key_alias_esc() {
    let key = parse_key_alias("ESC").unwrap();
    assert_eq!(key, KeyEvent(KeyCode::Esc, ModKeys::NONE));
  }

  #[test]
  fn key_alias_tab() {
    let key = parse_key_alias("TAB").unwrap();
    assert_eq!(key, KeyEvent(KeyCode::Tab, ModKeys::NONE));
  }

  #[test]
  fn key_alias_backspace() {
    let key = parse_key_alias("BS").unwrap();
    assert_eq!(key, KeyEvent(KeyCode::Backspace, ModKeys::NONE));
  }

  #[test]
  fn key_alias_space() {
    let key = parse_key_alias("SPACE").unwrap();
    assert_eq!(key, KeyEvent(KeyCode::Char(' '), ModKeys::NONE));
  }

  #[test]
  fn key_alias_arrows() {
    assert_eq!(
      parse_key_alias("UP").unwrap(),
      KeyEvent(KeyCode::Up, ModKeys::NONE)
    );
    assert_eq!(
      parse_key_alias("DOWN").unwrap(),
      KeyEvent(KeyCode::Down, ModKeys::NONE)
    );
    assert_eq!(
      parse_key_alias("LEFT").unwrap(),
      KeyEvent(KeyCode::Left, ModKeys::NONE)
    );
    assert_eq!(
      parse_key_alias("RIGHT").unwrap(),
      KeyEvent(KeyCode::Right, ModKeys::NONE)
    );
  }

  #[test]
  fn key_alias_ctrl_modifier() {
    let key = parse_key_alias("C-a").unwrap();
    assert_eq!(key, KeyEvent(KeyCode::Char('A'), ModKeys::CTRL));
  }

  #[test]
  fn key_alias_ctrl_shift_alt_modifier() {
    let key = parse_key_alias("C-S-A-b").unwrap();
    assert_eq!(
      key,
      KeyEvent(
        KeyCode::Char('B'),
        ModKeys::CTRL | ModKeys::SHIFT | ModKeys::ALT
      )
    );
  }

  #[test]
  fn key_alias_alt_modifier() {
    let key = parse_key_alias("M-x").unwrap();
    assert_eq!(key, KeyEvent(KeyCode::Char('X'), ModKeys::ALT));
  }

  #[test]
  fn key_alias_shift_modifier() {
    let key = parse_key_alias("S-TAB").unwrap();
    assert_eq!(key, KeyEvent(KeyCode::Tab, ModKeys::SHIFT));
  }

  #[test]
  fn key_alias_invalid() {
    assert!(parse_key_alias("INVALID_KEY").is_none());
  }

  // ===================== expand_keymap =====================

  #[test]
  fn keymap_single_char() {
    let keys = expand_keymap("a");
    assert_eq!(keys, vec![KeyEvent(KeyCode::Char('a'), ModKeys::NONE)]);
  }

  #[test]
  fn keymap_sequence() {
    let keys = expand_keymap("abc");
    assert_eq!(keys.len(), 3);
    assert_eq!(keys[0], KeyEvent(KeyCode::Char('a'), ModKeys::NONE));
    assert_eq!(keys[1], KeyEvent(KeyCode::Char('b'), ModKeys::NONE));
    assert_eq!(keys[2], KeyEvent(KeyCode::Char('c'), ModKeys::NONE));
  }

  #[test]
  fn keymap_ctrl_key() {
    let keys = expand_keymap("<C-a>");
    assert_eq!(keys, vec![KeyEvent(KeyCode::Char('A'), ModKeys::CTRL)]);
  }

  #[test]
  fn keymap_escaped_char() {
    let keys = expand_keymap("\\<");
    assert_eq!(keys, vec![KeyEvent(KeyCode::Char('<'), ModKeys::NONE)]);
  }

  #[test]
  fn keymap_mixed() {
    let keys = expand_keymap("a<CR>b");
    assert_eq!(keys.len(), 3);
    assert_eq!(keys[0], KeyEvent(KeyCode::Char('a'), ModKeys::NONE));
    assert_eq!(keys[1], KeyEvent(KeyCode::Char('\r'), ModKeys::NONE));
    assert_eq!(keys[2], KeyEvent(KeyCode::Char('b'), ModKeys::NONE));
  }

  // ===================== Alias Expansion (TestGuard) =====================

  #[test]
  fn alias_simple() {
    let _guard = TestGuard::new();
    let dummy_span = Span::default();
    crate::state::SHED.with(|s| {
      s.logic
        .borrow_mut()
        .insert_alias("ll", "ls -la", dummy_span.clone());
    });

    let log_tab = crate::state::SHED.with(|s| s.logic.borrow().clone());
    let result = expand_aliases("ll".to_string(), HashSet::new(), &log_tab);
    assert_eq!(result, "ls -la");
  }

  #[test]
  fn alias_circular_prevention() {
    let _guard = TestGuard::new();
    let dummy_span = Span::default();
    crate::state::SHED.with(|s| {
      s.logic
        .borrow_mut()
        .insert_alias("foo", "foo --verbose", dummy_span.clone());
    });

    let log_tab = crate::state::SHED.with(|s| s.logic.borrow().clone());
    let result = expand_aliases("foo".to_string(), HashSet::new(), &log_tab);
    // After first expansion: "foo --verbose", then "foo" is in already_expanded
    // so it won't expand again
    assert_eq!(result, "foo --verbose");
  }
}
