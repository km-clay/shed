use std::collections::VecDeque;

use super::{
  eval::lex::{LexFlags, LexStream, Tk, TkFlags},
  keys::{KeyCode, KeyEvent, ModKeys},
  shopt,
  state::Shed,
};
use crate::HashSet;

struct AliasExpander {
  input: String,
  first_expand_pos: Option<usize>, // byte pos of the first expansion, for cursor fixup
}

impl AliasExpander {
  fn new(input: String) -> Self {
    Self {
      input,
      first_expand_pos: None,
    }
  }

  fn expand(mut self) -> (String, Option<usize>) {
    let mut cursor = 0;
    let mut active: HashSet<String> = HashSet::default();

    let mut tokens = self.lex_tokens();
    let mut ti = 0;

    loop {
      while ti < tokens.len() {
        let tk = &tokens[ti];
        if tk.span.range().start >= cursor
          && tk.flags.contains(TkFlags::IS_CMD)
          && !tk.flags.contains(TkFlags::KEYWORD)
        {
          break;
        }
        ti += 1;
      }
      let Some(tk) = tokens.get(ti) else { break };
      let span = tk.span.range();
      let (start, end) = (span.start, span.end);
      let word = tk.to_str_lossy().to_string();

      let alias = if active.contains(&word) {
        None // guarded: re-expanding would recurse
      } else {
        Shed::logic(|l| l.aliases().get(&word).cloned())
      };

      if let Some(alias) = alias {
        self.input.replace_range(start..end, &alias.to_string());
        active.insert(word);
        self.first_expand_pos.get_or_insert(start);
        // `input` changed; token spans past `start` are now stale. Re-lex and
        // re-scan from `cursor` (unchanged) so the replacement is itself
        // examined for chained/recursive expansion.
        tokens = self.lex_tokens();
        ti = 0;
      } else {
        cursor = end;
        active.clear();
      }
    }

    (self.input, self.first_expand_pos)
  }

  /// Lex the current input into a token vector. Each token carries its own
  /// snapshot of the source, so the returned tokens stay valid across a later
  /// `input` mutation (they simply become stale and are dropped on re-lex).
  fn lex_tokens(&self) -> Vec<Tk> {
    LexStream::new(self.input.as_bytes(), LexFlags::empty())
      .filter_map(Result::ok)
      .collect()
  }
}

/// Expand aliases in the given input string.
///
/// Walks command-position words left to right, expanding each alias and
/// re-expanding the result within a per-position recursion guard.
pub fn expand_aliases(input: &str) -> String {
  AliasExpander::new(input.to_string()).expand().0
}

pub fn expand_alias_with_pos(input: String) -> (String, Option<usize>) {
  AliasExpander::new(input).expand()
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
                let mut leader = shopt!(prompt.leader.clone());
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

  let raw_key = key_name.first()?;
  let key = match raw_key.to_uppercase().as_str() {
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
    // F-keys: F1..F12 (any u8 the user writes — let the renderer decide
    // what's reasonable). Matches the rendering produced by as_vim_seq.
    k if k.starts_with('F') && k.len() > 1 && k[1..].parse::<u8>().is_ok() => {
      KeyCode::F(k[1..].parse::<u8>().unwrap())
    }
    k if k.len() == 1 => KeyCode::Char(raw_key.chars().next().unwrap()),
    _ => return None,
  };

  Some(KeyEvent(key, mods))
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::eval::lex::Span;
  use crate::tests::testutil::TestGuard;

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
    assert_eq!(key, KeyEvent(KeyCode::Char('a'), ModKeys::CTRL));
  }

  #[test]
  fn key_alias_ctrl_shift_alt_modifier() {
    let key = parse_key_alias("C-S-A-b").unwrap();
    assert_eq!(
      key,
      KeyEvent(
        KeyCode::Char('b'),
        ModKeys::CTRL | ModKeys::SHIFT | ModKeys::ALT
      )
    );
  }

  #[test]
  fn key_alias_alt_modifier() {
    let key = parse_key_alias("M-x").unwrap();
    assert_eq!(key, KeyEvent(KeyCode::Char('x'), ModKeys::ALT));
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
    assert_eq!(keys, vec![KeyEvent(KeyCode::Char('a'), ModKeys::CTRL)]);
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
    Shed::logic_mut(|l| l.insert_alias("ll", &"ls -la".into(), dummy_span.clone()));

    let result = expand_aliases("ll");
    assert_eq!(result, "ls -la");
  }

  #[test]
  fn alias_circular_prevention() {
    let _guard = TestGuard::new();
    let dummy_span = Span::default();
    Shed::logic_mut(|l| l.insert_alias("foo", &"foo --verbose".into(), dummy_span.clone()));

    let result = expand_aliases("foo");
    // After first expansion: "foo --verbose", then "foo" is in already_expanded
    // so it won't expand again
    assert_eq!(result, "foo --verbose");
  }

  #[test]
  fn alias_expands_every_command_position_only() {
    // Exercises the single-lex + monotonic command-position scan: the alias
    // fires in each command position (after `;`, `&&`, `|`) but never in an
    // argument position (the trailing `g`).
    let _guard = TestGuard::new();
    let sp = Span::default();
    Shed::logic_mut(|l| l.insert_alias("g", &"git".into(), sp.clone()));

    let result = expand_aliases("g status; g log && g diff | g show g");
    assert_eq!(result, "git status; git log && git diff | git show g");
  }

  #[test]
  fn alias_chained_expansion_relexes() {
    // a -> b -> c: each replacement mutates the input and must be re-lexed and
    // re-examined at the same cursor, so the chain resolves fully.
    let _guard = TestGuard::new();
    let sp = Span::default();
    Shed::logic_mut(|l| {
      l.insert_alias("a", &"b".into(), sp.clone());
      l.insert_alias("b", &"c".into(), sp.clone());
    });

    assert_eq!(expand_aliases("a"), "c");
  }

  #[test]
  fn alias_not_expanded_in_argument_position() {
    let _guard = TestGuard::new();
    let sp = Span::default();
    Shed::logic_mut(|l| l.insert_alias("ls", &"ls --color".into(), sp.clone()));

    // `ls` as an argument to `echo` must stay literal.
    assert_eq!(expand_aliases("echo ls"), "echo ls");
  }
}
