use std::{iter::Peekable, ops::Range, str::Chars};

use crate::{util::strops::QuoteState, match_loop, readline::markers};

pub const TAG_SEQ: &str = "\x1b[1;33m"; // bold yellow - searchable tags
pub const REF_SEQ: &str = "\x1b[4;36m"; // underline cyan - cross-references
pub const SEARCH_RES_SEQ: &str = "\x1b[1;7;m"; // bold inverse - search result highlight
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
      .map(|pos| pos + 1)
      .unwrap_or(0)
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
  content: String,
}

impl StyledHelp {
  pub fn new(content: &str) -> Self {
    Self {
      content: style_help_content(content),
    }
  }
  pub fn content(&self) -> &str {
    &self.content
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

fn unescape_help(raw: &str) -> String {
  let mut result = String::new();
  let mut chars = raw.chars().peekable();
  let mut qt_state = QuoteState::default();

  let find_closer = |closer: char, res: &mut String, chars: &mut Peekable<Chars>| {
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
  };

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
    _ if qt_state.in_quote() || chars.peek().is_none_or(|ch| ch.is_whitespace()) => {
      result.push(ch);
    }
    '*' => {
      result.push(markers::TAG);
      find_closer('*', &mut result, &mut chars);
    }
    '|' => {
      result.push(markers::REFERENCE);
      find_closer('|', &mut result, &mut chars);
    }
    '#' => {
      result.push(markers::HEADER);
      find_closer('#', &mut result, &mut chars);
    }
    '`' => {
      result.push(markers::CODE);
      find_closer('`', &mut result, &mut chars);
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
      } else {
        match_loop!(chars.next() => ch, {
          '~' => {
            tilde_count = 1;
            while tilde_count != 3 && chars.peek() == Some(&'~') {
              chars.next();
              tilde_count += 1;
            }
            if tilde_count != 3 {
              result.push_str(&"~".repeat(3 + tilde_count));
            } else {
              result.push(markers::RESET);
              break
            }
          }
          _ => result.push(ch),
        })
      }
    }
    _ => result.push(ch),
  });
  result
}
