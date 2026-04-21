use std::collections::{HashSet, VecDeque};

use crate::parse::lex::{LexFlags, LexStream, Tk, TkFlags};
use crate::readline::keys::{KeyCode, KeyEvent, ModKeys};
use crate::state::{read_logic, read_shopts};

pub struct AliasExpander<'a> {
  input: String,
  tokens: VecDeque<Tk>,
  expanded: &'a mut HashSet<String>,
  first_expand_pos: Option<usize>, // byte pos
}

impl<'a> AliasExpander<'a> {
  pub fn new(input: String, expanded: &'a mut HashSet<String>) -> Self {
    let tokens: VecDeque<Tk> = LexStream::new(input.clone().into(), LexFlags::empty())
      .filter_map(|tk| tk.ok())
      .collect();

    Self {
      input,
      tokens,
      expanded,
      first_expand_pos: None,
    }
  }

  pub fn expand(mut self) -> (String, Option<usize>) {
    let mut changed = false;

    while let Some(tk) = self.tokens.pop_front() {
      if !tk.flags.contains(TkFlags::IS_CMD) {
        continue;
      }
      if tk.flags.contains(TkFlags::KEYWORD) {
        continue;
      }

      let word = tk.as_str();
      if self.expanded.contains(word) {
        continue;
      }

      let Some(alias) = read_logic(|l| l.aliases().get(word).cloned()) else {
        continue;
      };

      let expansion = alias.to_string();

      // Check if the input from this token already starts with the expansion
      let rest = &self.input[tk.span.range().start..];
      if rest.starts_with(&expansion) {
        // Already expanded — skip, but still mark it
        self.expanded.insert(word.to_string());
        continue;
      }

      // Perform the expansion
      self.input.replace_range(tk.span.range(), &expansion);
      self.expanded.insert(word.to_string());
      changed = true;
      if self.first_expand_pos.is_none() {
        self.first_expand_pos = Some(tk.span.range().start);
      }

      // Re-lex from the expansion point since spans shifted
      break;
    }

    if changed {
      self.tokens = LexStream::new(self.input.clone().into(), LexFlags::empty())
        .filter_map(|tk| tk.ok())
        .collect();

      self.expand()
    } else {
      (self.input, self.first_expand_pos)
    }
  }
}

/// Expand aliases in the given input string
///
/// Recursively calls itself until all aliases are expanded
pub fn expand_aliases(input: String) -> String {
  let mut seen = HashSet::new();
  AliasExpander::new(input, &mut seen).expand().0
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
    "CR" | "ENTER" | "RETURN" => KeyCode::Enter,
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
  use crate::state::write_logic;
use crate::testutil::TestGuard;

  // ===================== parse_key_alias =====================

  #[test]
  fn key_alias_cr() {
    let key = parse_key_alias("CR").unwrap();
    assert_eq!(key, KeyEvent(KeyCode::Enter, ModKeys::NONE));
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
    assert_eq!(keys[1], KeyEvent(KeyCode::Enter, ModKeys::NONE));
    assert_eq!(keys[2], KeyEvent(KeyCode::Char('b'), ModKeys::NONE));
  }

  // ===================== Alias Expansion (TestGuard) =====================

  #[test]
  fn alias_simple() {
    let _guard = TestGuard::new();
    let dummy_span = Span::default();
    write_logic(|l| l.insert_alias("ll", "ls -la", dummy_span.clone()));

    let result = expand_aliases("ll".to_string());
    assert_eq!(result, "ls -la");
  }

  #[test]
  fn alias_circular_prevention() {
    let _guard = TestGuard::new();
    let dummy_span = Span::default();
    write_logic(|l| l.insert_alias("foo", "foo --verbose", dummy_span.clone()));

    let result = expand_aliases("foo".to_string());
    // After first expansion: "foo --verbose", then "foo" is in already_expanded
    // so it won't expand again
    assert_eq!(result, "foo --verbose");
  }
}
