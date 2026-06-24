use super::{
  Grapheme, Line, Lines, Pos,
  eval::{ParseFlags, ParsedSrc, lex::LexFlags},
};
use crate::readline::context::CtxTk;
use crate::readline::context::CtxTkRule;

#[derive(Default, Clone, Debug)]
pub struct Edit {
  pub old_cursor: Pos,
  pub new_cursor: Pos,
  pub old: Lines,
  pub new: Lines,
  pub merging: bool,
}

impl Edit {
  pub fn is_empty(&self) -> bool {
    self.old == self.new
  }
}

/// A block whose body is indented one level until a matching closer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Opener {
  If,
  Case,
  For,
  Loop, // while / until
  Select,
  Brace,   // command group { … }
  Paren,   // subshell ( … )
  CaseArm, // one pat) … ;; arm inside a case
}

/// Maps a keyword to the block it opens. Middles (`then`/`do`/`else`/…) are
/// absent: depth changes at the opener, so they're neutral for the count.
fn keyword_opener(kw: &str) -> Option<Opener> {
  Some(match kw {
    "if" => Opener::If,
    "case" => Opener::Case,
    "for" => Opener::For,
    "while" | "until" => Opener::Loop,
    "select" => Opener::Select,
    _ => return None,
  })
}

/// The openers a closer keyword is allowed to close.
fn keyword_closes(kw: &str) -> Option<&'static [Opener]> {
  Some(match kw {
    "fi" => &[Opener::If],
    "done" => &[Opener::For, Opener::Loop, Opener::Select],
    _ => return None, // esac is handled specially (it may also close a dangling arm)
  })
}

fn collect_depth_events(tokens: &[CtxTk], stack: &mut Vec<Opener>, events: &mut Vec<(usize, i32)>) {
  for tk in tokens {
    let start = tk.span().range().start;
    match tk.class() {
      CtxTkRule::Keyword => {
        let kw = tk.span().as_str();
        if let Some(opener) = keyword_opener(kw) {
          stack.push(opener);
          events.push((start, 1));
        } else if kw == "esac" {
          // a final arm may omit its ;;, so close a dangling arm first, then the case
          if stack.last() == Some(&Opener::CaseArm) {
            stack.pop();
            events.push((start, -1));
          }
          if stack.last() == Some(&Opener::Case) {
            stack.pop();
            events.push((start, -1));
          }
        } else if let Some(targets) = keyword_closes(kw)
          && stack.last().is_some_and(|top| targets.contains(top))
        {
          stack.pop();
          events.push((start, -1));
        }
        // middles and unrelated keywords are depth-neutral
      }
      // command-level braces/subshells arrive as flat operators carrying the
      // delimiter text; pair them by text against the stack.
      CtxTkRule::Operator => match tk.span().as_str() {
        "{" => {
          stack.push(Opener::Brace);
          events.push((start, 1));
        }
        // a ( directly inside a case is a pattern's leading paren (a|b), not a
        // subshell, so it opens nothing; the ) opens the arm
        "(" if stack.last() == Some(&Opener::Case) => {}
        "(" => {
          stack.push(Opener::Paren);
          events.push((start, 1));
        }
        "}" if stack.last() == Some(&Opener::Brace) => {
          stack.pop();
          events.push((start, -1));
        }
        ")" if stack.last() == Some(&Opener::Paren) => {
          stack.pop();
          events.push((start, -1));
        }
        // a ) with the case itself on top closes a pattern, opening its arm
        ")" if stack.last() == Some(&Opener::Case) => {
          stack.push(Opener::CaseArm);
          events.push((start, 1));
        }
        _ => {}
      },
      // ;; ends the current arm. Its separator token swallows the leading
      // newline + indent, so find the ;; rather than using the span start
      // (which sits at the end of the previous row).
      CtxTkRule::Separator
        if stack.last() == Some(&Opener::CaseArm)
          && let Some(off) = tk.span().as_str().find(";;") =>
      {
        stack.pop();
        events.push((start + off, -1));
      }
      // $(…), backticks, process subs, arithmetic: their own scope. The parser
      // doesn't count the delimiter toward depth, so neither do we, but we still
      // recurse (isolated) to catch nested keyword blocks, unwinding any left open.
      CtxTkRule::Subshell
      | CtxTkRule::CmdSub
      | CtxTkRule::BacktickSub
      | CtxTkRule::ProcSubIn
      | CtxTkRule::ProcSubOut
      | CtxTkRule::Arithmetic => {
        let end = tk.span().range().end;
        let mut inner = Vec::new();
        collect_depth_events(tk.sub_tokens(), &mut inner, events);
        for _ in 0..inner.len() {
          events.push((end, -1));
        }
      }
      // an `arr=( … )` array literal indents its body, so unlike `$(…)` it does
      // count: open at the `(`, close at the `)` once it's there.
      CtxTkRule::ArrayLiteral => {
        events.push((start, 1));
        let end = tk.span().range().end;
        let mut inner = Vec::new();
        collect_depth_events(tk.sub_tokens(), &mut inner, events);
        for _ in 0..inner.len() {
          events.push((end, -1));
        }
        if tk.span().as_str().ends_with(')') {
          events.push((end - 1, -1));
        }
      }
      // args, strings, etc.: descend to catch nested expansions inside them
      _ => collect_depth_events(tk.sub_tokens(), stack, events),
    }
  }
}

/// Per-row `(start, end)` block depth from an already-built context-token slice.
/// `input` is only used for its newline positions (the token spans are absolute
/// offsets into it).
pub fn depth_levels_from_tokens(tokens: &[CtxTk], input: &str) -> Vec<(usize, usize)> {
  let mut events = Vec::new();
  let mut stack = Vec::new();
  collect_depth_events(tokens, &mut stack, &mut events);
  events.sort_by_key(|(pos, _)| *pos);

  let mut boundaries = vec![0usize];
  for (i, ch) in input.char_indices() {
    if ch == '\n' {
      boundaries.push(i + 1);
    }
  }
  let n_rows = boundaries.len();
  boundaries.push(input.len());

  let mut depth: i32 = 0;
  let mut ei = 0;
  let mut depths = Vec::with_capacity(boundaries.len());
  for &b in &boundaries {
    while ei < events.len() && events[ei].0 < b {
      depth += events[ei].1;
      ei += 1;
    }
    depths.push(depth.max(0) as usize);
  }

  (0..n_rows).map(|i| (depths[i], depths[i + 1])).collect()
}

/// Tokenize `input` and compute its per-row depth. Used where no token cache is
/// available (a cursor prefix) and by the parity test.
pub fn depth_levels_via_ctx(input: &str) -> Vec<(usize, usize)> {
  let tokens = crate::readline::context::get_context_tokens(input);
  depth_levels_from_tokens(&tokens, input)
}

/// Strict parse that flags unterminated structures (open quotes, subshells, …).
pub fn parse_failed_strict(input: &str) -> bool {
  ParsedSrc::new(input.into())
    .with_lex_flags(LexFlags::LEX_UNFINISHED_STRUCTURES)
    .with_parse_flags(ParseFlags::ERR_RETURN)
    .parse_src()
    .is_err()
}

pub(super) fn extract_range_contiguous(buf: &mut Lines, start: Pos, end: Pos) -> Lines {
  let start_col = start.col.min(buf[start.row].len());
  let end_col = end.col.min(buf[end.row].len());

  if start.row == end.row {
    // single line case
    let line = &mut buf[start.row];
    let removed: Vec<Grapheme> = line.0.drain(start_col..end_col).collect();
    return Lines(vec![Line(removed)]);
  }

  // multi line case
  // tail of first line
  let first_tail: Line = buf[start.row].split_off(start_col);

  // all inbetween lines. extracts nothing if only two rows
  let middle: Lines = buf.drain(start.row + 1..end.row).collect();

  // head of last line
  let last_col = end_col.min(buf[start.row + 1].len());
  let last_head: Line = Line::from(buf[start.row + 1].0.drain(..last_col).collect::<Vec<_>>());

  // tail of last line
  let mut last_remainder = buf.remove(start.row + 1);

  // attach tail of last line to head of first line
  buf[start.row].append(&mut last_remainder);

  // construct vector of extracted content
  let mut extracts = vec![first_tail];
  extracts.extend(middle.0);
  extracts.push(last_head);
  Lines(extracts)
}

impl super::LineBuf {
  /// Provides a public interface for editing the buffer in a way that is recognized by the undo system.
  /// Any change made by the provided function will be tracked in the undo stack.
  pub fn edit<T, F: FnMut(&mut Self) -> T>(&mut self, mut f: F) -> T {
    let before = self.lines.clone();
    let old_cursor = self.cursor.pos;

    let res = f(self);

    if self.is_empty() {
      self.set_hint(None);
    }

    let new_cursor = self.cursor.pos;
    self.handle_edit(before, new_cursor, old_cursor);

    res
  }
  pub fn handle_edit(&mut self, old: Lines, new_cursor: Pos, old_cursor: Pos) {
    let last_edit = self.undo_stack.last();
    let edit_is_merging = last_edit.is_some_and(|edit| edit.merging);
    if edit_is_merging {
      // Update the `new` snapshot on the existing edit
      if let Some(edit) = self.undo_stack.last_mut() {
        edit.new = self.lines.clone();
      }
    } else {
      self.undo_stack.push(Edit {
        new_cursor,
        old_cursor,
        old,
        new: self.lines.clone(),
        merging: false,
      });
    }
  }
}

#[cfg(test)]
mod depth_levels_tests {
  use super::depth_levels_via_ctx;

  // Expected per-row (start, end) depths across the shell's block constructs.
  // These were validated against the parser's `block_depth` before that field
  // was retired; this pins the token-based derivation against regressions.
  #[rustfmt::skip]
  const BATTERY: &[(&str, &str, &[(usize, usize)])] = &[
    ("if 1-line", "if true; then\n  echo hi\nfi", &[(0,1),(1,1),(1,0)]),
    ("if multiline", "if true\nthen\n  echo hi\nfi", &[(0,1),(1,1),(1,1),(1,0)]),
    ("if/elif/else", "if a; then\n  b\nelif c; then\n  d\nelse\n  e\nfi", &[(0,1),(1,1),(1,1),(1,1),(1,1),(1,1),(1,0)]),
    ("nested if", "if a; then\n  if b; then\n    c\n  fi\nfi", &[(0,1),(1,2),(2,2),(2,1),(1,0)]),
    ("for/do/done", "for x in a b; do\n  echo $x\ndone", &[(0,1),(1,1),(1,0)]),
    ("while", "while true; do\n  echo hi\ndone", &[(0,1),(1,1),(1,0)]),
    ("func braces", "foo() {\n  bar() {\n    echo hi\n  }\n}", &[(0,1),(1,2),(2,2),(2,1),(1,0)]),
    ("brace group", "{\n  echo a\n  echo b\n}", &[(0,1),(1,1),(1,1),(1,0)]),
    ("subshell", "(\n  echo a\n  echo b\n)", &[(0,1),(1,1),(1,1),(1,0)]),
    ("cmdsub multiline", "x=$(\n  echo a\n)\necho $x", &[(0,0),(0,0),(0,0),(0,0)]),
    ("for inside if", "if a; then\n  for x in y; do\n    z\n  done\nfi", &[(0,1),(1,2),(2,2),(2,1),(1,0)]),
    ("unclosed if", "if true; then\n  echo hi", &[(0,1),(1,1)]),
    ("case", "case $x in\n  a)\n    echo a\n    ;;\nesac", &[(0,1),(1,2),(2,2),(2,1),(1,0)]),
    ("case multi-arm", "case $x in\n  a)\n    echo a\n    ;;\n  b)\n    echo b\n    ;;\nesac", &[(0,1),(1,2),(2,2),(2,1),(1,2),(2,2),(2,1),(1,0)]),
    ("case no trailing ;;", "case $x in\n  a)\n    echo a\nesac", &[(0,1),(1,2),(2,2),(2,0)]),
    ("case in if", "if t; then\n  case $x in\n    a)\n      b\n      ;;\n  esac\nfi", &[(0,1),(1,2),(2,3),(3,3),(3,2),(2,1),(1,0)]),
    ("case leading paren", "case $x in\n  (a|b)\n    echo ab\n    ;;\nesac", &[(0,1),(1,2),(2,2),(2,1),(1,0)]),
    ("subshell in arm", "case $x in\n  a)\n    (\n      echo a\n    )\n    ;;\nesac", &[(0,1),(1,2),(2,3),(3,3),(3,2),(2,1),(1,0)]),
    ("plain lines", "echo a\necho b\necho c", &[(0,0),(0,0),(0,0)]),
    ("array literal", "arr=(\n  a\n  b\n)", &[(0,1),(1,1),(1,1),(1,0)]),
    ("array one-line", "arr=(a b c)\necho hi", &[(0,0),(0,0)]),
    ("array in func", "f() {\n  x=(\n    a\n  )\n}", &[(0,1),(1,2),(2,2),(2,1),(1,0)]),
    ("function kw", "function foo {\n  echo hi\n}", &[(0,1),(1,1),(1,0)]),
    ("dbl bracket test", "if [[ -f x ]]; then\n  echo y\nfi", &[(0,1),(1,1),(1,0)]),
    ("arith command", "if (( 1 + 1 )); then\n  echo y\nfi", &[(0,1),(1,1),(1,0)]),
    ("nested everything", "for x in a; do\n  if [[ $x ]]; then\n    case $x in\n      a)\n        (\n          echo deep\n        )\n        ;;\n    esac\n  fi\ndone", &[(0,1),(1,2),(2,3),(3,4),(4,5),(5,5),(5,4),(4,3),(3,2),(2,1),(1,0)]),
    ("pipe across lines", "echo a |\n  grep b |\n  wc -l", &[(0,0),(0,0),(0,0)]),
    ("heredoc", "cat <<EOF\nbody\nEOF\necho done", &[(0,0),(0,0),(0,0),(0,0)]),
  ];

  #[test]
  fn depth_battery() {
    for (label, input, expected) in BATTERY {
      assert_eq!(depth_levels_via_ctx(input), *expected, "{label}");
    }
  }
}
