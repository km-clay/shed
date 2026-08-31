use std::{iter::Peekable, ops::Range, str::Chars};

use crate::{expand::markers, match_loop, util::strops::QuoteState};

/// byte index one past its trailing `m`. Otherwise None.
pub(super) fn scan_sgr(bytes: &[u8], pos: usize) -> Option<usize> {
  if bytes.get(pos)? != &0x1b {
    return None;
  }
  if bytes.get(pos + 1)? != &b'[' {
    return None;
  }
  let mut i = pos + 2;
  while let Some(&b) = bytes.get(i) {
    match b {
      b'm' => return Some(i + 1),
      b'0'..=b'9' | b';' => i += 1,
      _ => return None,
    }
  }
  None
}

pub const TAG_SEQ: &str = "\x1b[1;33m"; // bold yellow - searchable tags
pub const REF_SEQ: &str = "\x1b[4;36m"; // underline cyan - cross-references
pub const RESET_SEQ: &str = "\x1b[0m";
pub const HEADER_SEQ: &str = "\x1b[1;35m"; // bold magenta - section headers
pub const CODE_SEQ: &str = "\x1b[32m"; // green - inline code
pub const KEYWORD_1_SEQ: &str = "\x1b[1;32m"; // bold green - {keyword}
pub const KEYWORD_2_SEQ: &str = "\x1b[3;37m"; // italic white - [optional]

#[derive(Debug)]
pub struct MarkedSpan {
  prefix_seq: Range<usize>,
  content: Range<usize>,
  postfix_seq: Range<usize>,
}

impl MarkedSpan {
  pub fn new(prefix_seq: Range<usize>, content: Range<usize>, postfix_seq: Range<usize>) -> Self {
    Self {
      prefix_seq,
      content,
      postfix_seq,
    }
  }

  pub fn content_range(&self) -> Range<usize> {
    self.content.clone()
  }
  pub fn content<'a>(&self, source: &'a str) -> &'a str {
    &source[self.content.clone()]
  }
  pub fn line_no(&self, source: &str) -> usize {
    source[..self.prefix_seq.start]
      .chars()
      .filter(|c| *c == '\n')
      .count()
  }

  pub fn line_start(&self, source: &str) -> usize {
    source[..self.prefix_seq.start]
      .rfind('\n')
      .map_or(0, |pos| pos + 1)
  }

  pub fn rel_to_line(&self, source: &str) -> (Range<usize>, Range<usize>, Range<usize>) {
    let offset = self.line_start(source);
    (
      self.prefix_seq.clone().start - offset..self.prefix_seq.clone().end - offset,
      self.content.clone().start - offset..self.content.clone().end - offset,
      self.postfix_seq.clone().start - offset..self.postfix_seq.clone().end - offset,
    )
  }
}

#[derive(Debug)]
pub struct StyledHelp {
  /// styled help content
  content: String,
  /// visible help content (no ANSI sequences)
  visible: String,
  /// mapping of visible bytes to styled bytes
  visible_to_baked: Vec<usize>,
  ref_targets: Vec<Option<String>>,
}

impl StyledHelp {
  pub fn new(content: &str) -> Self {
    let baked = style_help_content(content);
    let (visible, visible_to_baked) = strip_sgr(&baked);
    Self {
      content: baked,
      visible,
      visible_to_baked,
      ref_targets: extract_ref_targets(content),
    }
  }
  pub fn content(&self) -> &str {
    &self.content
  }
  pub fn visible(&self) -> &str {
    &self.visible
  }
  pub fn visible_to_baked(&self) -> &[usize] {
    &self.visible_to_baked
  }
  pub fn take_ref_targets(&mut self) -> Vec<Option<String>> {
    std::mem::take(&mut self.ref_targets)
  }

  pub fn find_markers(&self, marker: &str) -> Vec<MarkedSpan> {
    let mut markers = vec![];
    let mut cursor = 0;

    while let Some(pos) = self.content[cursor..].find(marker) {
      let abs_pos = cursor + pos;
      let prefix_end = abs_pos + marker.len();

      let Some(end) = self.content[prefix_end..].find(RESET_SEQ) else {
        break;
      };

      let postfix_start = prefix_end + end;
      let postfix_end = postfix_start + RESET_SEQ.len();

      markers.push(MarkedSpan::new(
        abs_pos..prefix_end,
        prefix_end..postfix_start,
        postfix_start..postfix_end,
      ));

      cursor = postfix_end;
    }

    markers
  }
}

pub fn style_help_content(raw: &str) -> String {
  expand_help(&unescape_help(raw))
}

/// Strips all ansi sequences from the provided string
fn strip_sgr(baked: &str) -> (String, Vec<usize>) {
  let bytes = baked.as_bytes();
  let mut visible = Vec::with_capacity(bytes.len());
  let mut map: Vec<usize> = Vec::with_capacity(bytes.len() + 1);
  let mut i = 0;
  while i < bytes.len() {
    if let Some(end) = scan_sgr(bytes, i) {
      i = end;
      continue;
    }
    map.push(i);
    visible.push(bytes[i]);
    i += 1;
  }
  map.push(i);

  // ansi codes will only ever be ascii
  // so we can unwrap this
  (String::from_utf8(visible).unwrap(), map)
}

/// Consume a cross-reference target
///
/// Cross-references can define a target like `|some text|(some-target)`
fn consume_ref_alias(chars: &mut Peekable<Chars>) -> Option<String> {
  if chars.peek() != Some(&'(') {
    return None;
  }
  chars.next(); // '('
  let mut target = String::new();
  let mut closed = false;
  match_loop!(chars.next() => ch, {
    ')' => { closed = true; break; }
    '\\' if chars.peek() == Some(&')') => {
      target.push(')');
      chars.next();
    }
    _ => target.push(ch),
  });
  if closed && !target.is_empty() {
    Some(target)
  } else {
    None
  }
}

fn extract_ref_targets(source: &str) -> Vec<Option<String>> {
  let mut targets = vec![];
  let mut chars = source.chars().peekable();
  let mut qt_state = QuoteState::default();

  match_loop!(chars.next() => ch, {
    '\\' => {
      chars.next(); // consume escaped char
    }
    '\n' => {
      qt_state = QuoteState::default();
    }
    '"' => qt_state.toggle_double(),
    '\'' => qt_state.toggle_single(),
    '`' => {
      while let Some(inner) = chars.next() {
        if inner == '\\' {
          chars.next();
          continue;
        }
        if inner == '`' {
          break;
        }
      }
    }
    _ if qt_state.in_quote() => {}
    '|' if chars.peek().is_some_and(|c| !c.is_whitespace()) => {
      // find the closer
      match_loop!(chars.next() => inner, {
        '|' => break,
        '\\' => { chars.next(); }
        _ => {}
      });
      // parse any reference targets
      targets.push(consume_ref_alias(&mut chars));
    }
    _ => {}
  });
  targets
}

fn expand_help(raw: &str) -> String {
  let mut result = String::new();
  let mut chars = raw.chars();

  match_loop!(chars.next() => ch, {
    markers::RESET => result.push_str(RESET_SEQ),
    markers::TAG => result.push_str(TAG_SEQ),
    markers::REFERENCE => result.push_str(REF_SEQ),
    markers::HEADER => result.push_str(HEADER_SEQ),
    markers::CODE => result.push_str(CODE_SEQ),
    markers::KEYWORD_1 => result.push_str(KEYWORD_1_SEQ),
    markers::KEYWORD_2 => result.push_str(KEYWORD_2_SEQ),
    _ => result.push(ch),
  });
  result
}

fn find_closer(closer: char, res: &mut String, chars: &mut Peekable<Chars>) {
  match_loop!(chars.next() => next_ch, {
    _ if next_ch == closer => {
      res.push(markers::RESET);
      break;
    }
    '\\' => {
      match chars.peek() {
        Some(ch) if *ch == closer || *ch == '\\' => {
          res.push(chars.next().unwrap());
        }
        _ => res.push(next_ch),
      }
    }
    _ => res.push(next_ch),
  });
}

fn unescape_help(raw: &str) -> String {
  let mut result = String::new();
  let mut chars = raw.chars().peekable();
  let mut qt_state = QuoteState::default();

  match_loop!(chars.next() => ch, {
    '\\' => {
      if let Some(next_ch) = chars.next() {
        result.push(next_ch);
      }
    }
    '\n' => {
      result.push(ch);
      qt_state = QuoteState::default();
    }
    '"' => {
      result.push(ch);
      qt_state.toggle_double();
    }
    '\'' => {
      result.push(ch);
      qt_state.toggle_single();
    }
    '`' => {
      result.push(markers::CODE);
      find_closer('`', &mut result, &mut chars);
    }
    _ if qt_state.in_quote() || chars.peek().is_none_or(|ch| ch.is_whitespace()) => {
      result.push(ch);
    }
    '*' => {
      let lookahead: String = chars.clone()
        .take_while(|c| !c.is_whitespace() && *c != '\n')
        .collect();
      if lookahead.contains('*') {
        result.push(markers::TAG);
        find_closer('*', &mut result, &mut chars);
      } else {
        result.push(ch);
      }
    }
    '|' => {
      result.push(markers::REFERENCE);
      find_closer('|', &mut result, &mut chars);

      // run this here just to skip any (targets)
      // the return of this function is handled in
      // extract_ref_targets, we just drop it here
      consume_ref_alias(&mut chars);
    }
    '#' => {
      result.push(markers::HEADER);
      find_closer('#', &mut result, &mut chars);
    }
    '{' => {
      result.push(markers::KEYWORD_2);
      find_closer('}', &mut result, &mut chars);
    }
    '[' => {
      result.push(markers::KEYWORD_2);
      find_closer(']', &mut result, &mut chars);
    }
    '~' => {
      let mut tilde_count = 1;
      while tilde_count != 3 && chars.peek() == Some(&'~') {
        chars.next();
        tilde_count += 1;
      }
      if tilde_count != 3 {
        result.push_str(&"~".repeat(tilde_count));
        continue
      }
      match_loop!(chars.next() => ch, {
        '~' => {
          tilde_count = 1;
          while tilde_count != 3 && chars.peek() == Some(&'~') {
            chars.next();
            tilde_count += 1;
          }
          if tilde_count == 3 {
            result.push(markers::RESET);
            break
          }
          result.push_str(&"~".repeat(3 + tilde_count));
        }
        _ => result.push(ch),
      });
    }
    _ => result.push(ch),
  });
  result
}

#[cfg(test)]
mod tests {
  use super::*;

  // `extract_ref_targets` probed directly — no need for full StyledHelp.

  #[test]
  fn extract_plain_ref_yields_none() {
    assert_eq!(extract_ref_targets("see |foo|"), vec![None]);
  }

  #[test]
  fn extract_aliased_ref_yields_some() {
    assert_eq!(
      extract_ref_targets("see |this text|(real-tag)"),
      vec![Some("real-tag".into())]
    );
  }

  #[test]
  fn extract_mixed_keeps_order() {
    assert_eq!(
      extract_ref_targets("|a| then |b text|(b-tag) then |c|"),
      vec![None, Some("b-tag".into()), None]
    );
  }

  #[test]
  fn extract_empty_paren_is_not_an_alias() {
    // `|t|()` still counts as a ref but with no target override.
    assert_eq!(extract_ref_targets("|t|()"), vec![None]);
  }

  #[test]
  fn extract_unterminated_paren_is_not_an_alias() {
    // No closing `)`, so the alias parse fails. Still one ref.
    assert_eq!(extract_ref_targets("|t|(abc"), vec![None]);
  }

  #[test]
  fn extract_escaped_close_paren_in_target() {
    assert_eq!(extract_ref_targets("|t|(a\\)b)"), vec![Some("a)b".into())]);
  }

  #[test]
  fn extract_pipe_followed_by_whitespace_is_literal() {
    // `| ` is not a reference opener (matches unescape_help's predicate).
    assert_eq!(extract_ref_targets("a | b"), vec![]);
  }

  #[test]
  fn extract_pipe_inside_quotes_is_literal() {
    assert_eq!(extract_ref_targets("\"a|b|c\""), vec![]);
  }

  #[test]
  fn extract_newline_resets_quote_state() {
    // Open double-quote without close, then newline -- quote state
    // resets, so the `|x|` on line 2 IS a reference.
    assert_eq!(extract_ref_targets("\"unclosed\n|x|"), vec![None]);
  }

  // Cross-pass invariant: count from extract_ref_targets must equal
  // count from find_markers(REF_SEQ). Catches drift if anyone touches
  // the opener predicate in either function.

  fn ref_count_invariant(src: &str) {
    let mut styled = StyledHelp::new(src);
    let from_render = styled.find_markers(REF_SEQ).len();
    let from_source = styled.take_ref_targets().len();
    assert_eq!(
      from_render, from_source,
      "ref count desync on input {src:?}: render={from_render} source={from_source}"
    );
  }

  #[test]
  fn invariant_plain_refs() {
    ref_count_invariant("|a| |b| |c|");
  }

  #[test]
  fn invariant_aliased_refs() {
    ref_count_invariant("|a|(x) |b|(y) |c|(z)");
  }

  #[test]
  fn invariant_mixed() {
    ref_count_invariant("|a| |b|(y) |c| |d|(z) |e|");
  }

  #[test]
  fn invariant_literal_pipes_dont_count() {
    ref_count_invariant("a | b, then |real| and \"|quoted|\"");
  }

  #[test]
  fn extract_pipe_inside_backticks_is_literal() {
    // `||` inside backticks is shell syntax, not a link opener.
    assert_eq!(extract_ref_targets("`foo || bar`"), vec![]);
  }

  #[test]
  fn extract_backticks_dont_swallow_following_links() {
    // The link AFTER the code span should still parse correctly.
    assert_eq!(
      extract_ref_targets("`a | b` then |real|(real-tag)"),
      vec![Some("real-tag".into())]
    );
  }

  #[test]
  fn extract_backtick_escape_does_not_close_early() {
    assert_eq!(extract_ref_targets("`a \\` b | c` |real|"), vec![None]);
  }

  #[test]
  fn invariant_pipes_inside_backticks() {
    ref_count_invariant("`shell || syntax` and |real|");
    ref_count_invariant("`a | b | c` then |t|(target)");
    ref_count_invariant("`|inline pipes|` and |outside|");
  }

  // ─── strip_sgr / visible-to-baked map ────────────────────────────

  #[test]
  fn strip_sgr_removes_all_sgr_sequences() {
    let baked = format!("{REF_SEQ}hello{RESET_SEQ} {TAG_SEQ}world{RESET_SEQ}");
    let (visible, _) = strip_sgr(&baked);
    assert_eq!(visible, "hello world");
  }

  #[test]
  fn strip_sgr_map_indexes_each_visible_byte_to_baked() {
    // "abc<REF>def<RST>ghi" — visible = "abcdefghi"
    let baked = format!("abc{REF_SEQ}def{RESET_SEQ}ghi");
    let (visible, map) = strip_sgr(&baked);
    assert_eq!(visible, "abcdefghi");
    // Every visible byte should map back to a baked byte that contains
    // the same character.
    for (i, vb) in visible.bytes().enumerate() {
      assert_eq!(
        baked.as_bytes()[map[i]],
        vb,
        "visible[{i}] = {:?} but baked[map[{i}]={}] = {:?}",
        vb as char,
        map[i],
        baked.as_bytes()[map[i]] as char,
      );
    }
    // Sentinel: one past the last byte of baked.
    assert_eq!(map.len(), visible.len() + 1);
    assert_eq!(map[visible.len()], baked.len());
  }

  #[test]
  fn strip_sgr_preserves_utf8() {
    // Two-byte UTF-8 characters around an SGR sequence.
    let baked = format!("café{REF_SEQ}naïve{RESET_SEQ}");
    let (visible, map) = strip_sgr(&baked);
    assert_eq!(visible, "cafénaïve");
    // The 'é' in "café" lives at byte indices 3..5 (UTF-8 0xC3 0xA9).
    // Both bytes should map to themselves in baked.
    assert_eq!(map[3], 3);
    assert_eq!(map[4], 4);
    // The 'n' in "naïve" is the first byte after REF_SEQ in baked.
    assert_eq!(map[5], 5 + REF_SEQ.len());
  }

  #[test]
  fn strip_sgr_handles_empty_input() {
    let (visible, map) = strip_sgr("");
    assert_eq!(visible, "");
    assert_eq!(map, vec![0]);
  }

  #[test]
  fn strip_sgr_handles_only_sgr_sequences() {
    let baked = format!("{REF_SEQ}{RESET_SEQ}");
    let (visible, map) = strip_sgr(&baked);
    assert_eq!(visible, "");
    assert_eq!(map, vec![baked.len()]);
  }

  // Integration: search shouldn't be able to target bytes inside an SGR
  // sequence. The visible view is what the regex sees.

  #[test]
  fn visible_view_excludes_sgr_bytes() {
    // Baked content contains "[1;33m" inside TAG_SEQ. The visible view
    // must NOT contain those bytes.
    let raw = "see *taghere* there";
    let styled = StyledHelp::new(raw);
    assert!(
      !styled.visible().contains("[1;33m"),
      "visible view leaked SGR bytes: {:?}",
      styled.visible()
    );
    assert!(styled.visible().contains("taghere"));
  }
}
