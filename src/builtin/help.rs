use std::{env, io::Write, path::Path};

use ariadne::Span as ASpan;
use nix::libc::STDIN_FILENO;

use crate::{
  libsh::{
    error::{ShErr, ShErrKind, ShResult},
    guards::RawModeGuard,
  },
  parse::{
    NdRule, Node, Redir, RedirType,
    execute::{exec_input, prepare_argv},
    lex::{QuoteState, Span},
  },
  procio::{IoFrame, IoMode},
  readline::{complete::ScoredCandidate, markers},
  state,
};

const TAG_SEQ: &str = "\x1b[1;33m"; // bold yellow — searchable tags
const REF_SEQ: &str = "\x1b[4;36m"; // underline cyan — cross-references
const RESET_SEQ: &str = "\x1b[0m";
const HEADER_SEQ: &str = "\x1b[1;35m"; // bold magenta — section headers
const CODE_SEQ: &str = "\x1b[32m"; // green — inline code
const KEYWORD_2_SEQ: &str = "\x1b[1;32m"; // bold green — {keyword}
const KEYWORD_3_SEQ: &str = "\x1b[3;37m"; // italic white — [optional]

pub fn help(node: Node) -> ShResult<()> {
  let NdRule::Command {
    assignments: _,
    argv,
  } = node.class
  else {
    unreachable!()
  };

  let mut argv = prepare_argv(argv)?.into_iter().peekable();
  let help = argv.next().unwrap(); // drop 'help'

  // Join all of the word-split arguments into a single string
  // Preserve the span too
  let (topic, span) = if argv.peek().is_none() {
    ("help.txt".to_string(), help.1)
  } else {
    argv.fold((String::new(), Span::default()), |mut acc, arg| {
      if acc.1 == Span::default() {
        acc.1 = arg.1.clone();
      } else {
        let new_end = arg.1.end();
        let start = acc.1.start();
        acc.1.set_range(start..new_end);
      }

      if acc.0.is_empty() {
        acc.0 = arg.0;
      } else {
        acc.0 = acc.0 + &format!(" {}", arg.0);
      }
      acc
    })
  };

  let hpath = env::var("SHED_HPATH").unwrap_or_default();

  for path in hpath.split(':') {
    let path = Path::new(&path).join(&topic);
    if path.is_file() {
      let Ok(contents) = std::fs::read_to_string(&path) else {
        continue;
      };
      let filename = path.file_stem().unwrap().to_string_lossy().to_string();

      let unescaped = unescape_help(&contents);
      let expanded = expand_help(&unescaped);
      open_help(&expanded, None, Some(filename))?;
      state::set_status(0);
      return Ok(());
    }
  }

  // didn't find an exact filename match, its probably a tag search
  for path in hpath.split(':') {
    let path = Path::new(path);
    if let Ok(entries) = path.read_dir() {
      for entry in entries {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        let filename = path.file_stem().unwrap().to_string_lossy().to_string();

        if !path.is_file() {
          continue;
        }

        let Ok(contents) = std::fs::read_to_string(&path) else {
          continue;
        };

        let unescaped = unescape_help(&contents);
        let expanded = expand_help(&unescaped);
        let tags = read_tags(&expanded);

        for (tag, line) in &tags {}

        if let Some((matched_tag, line)) = get_best_match(&topic, &tags) {
          open_help(&expanded, Some(line), Some(filename))?;
          state::set_status(0);
          return Ok(());
        } else {
        }
      }
    }
  }

  state::set_status(1);
  Err(ShErr::at(
    ShErrKind::NotFound,
    span,
    "No relevant help page found for this topic",
  ))
}

pub fn open_help(content: &str, line: Option<usize>, file_name: Option<String>) -> ShResult<()> {
  let pager = env::var("PAGER").unwrap_or("less -R".into());
  let line_arg = line.map(|ln| format!("+{ln}")).unwrap_or_default();
  let prompt_arg = file_name
    .map(|name| format!("-Ps'{name}'"))
    .unwrap_or_default();

  let mut tmp = tempfile::NamedTempFile::new()?;
  let tmp_path = tmp.path().to_string_lossy().to_string();
  tmp.write_all(content.as_bytes())?;
  tmp.flush()?;

  RawModeGuard::with_cooked_mode(|| {
    exec_input(
      format!("{pager} {line_arg} {prompt_arg} {tmp_path}"),
      None,
      true,
      Some("help".into()),
    )
  })
}

pub fn get_best_match(topic: &str, tags: &[(String, usize)]) -> Option<(String, usize)> {
  let mut candidates: Vec<_> = tags
    .iter()
    .map(|(tag, line)| (ScoredCandidate::new(tag.to_string()), *line))
    .collect();

  for (cand, _) in candidates.iter_mut() {
    cand.fuzzy_score(topic);
  }

  candidates.retain(|(c, _)| c.score.unwrap_or(i32::MIN) > i32::MIN);
  candidates.sort_by_key(|(c, _)| c.score.unwrap_or(i32::MIN));

  candidates
    .first()
    .map(|(c, line)| (c.content.clone(), *line))
}

pub fn read_tags(raw: &str) -> Vec<(String, usize)> {
  let mut tags = vec![];

  for (line_num, line) in raw.lines().enumerate() {
    let mut rest = line;

    while let Some(pos) = rest.find(TAG_SEQ) {
      let after_seq = &rest[pos + TAG_SEQ.len()..];
      if let Some(end) = after_seq.find(RESET_SEQ) {
        let tag = &after_seq[..end];
        tags.push((tag.to_string(), line_num + 1));
        rest = &after_seq[end + RESET_SEQ.len()..];
      } else {
        break;
      }
    }
  }

  tags
}

pub fn expand_help(raw: &str) -> String {
  let mut result = String::new();
  let mut chars = raw.chars();

  while let Some(ch) = chars.next() {
    match ch {
      markers::RESET => result.push_str(RESET_SEQ),
      markers::TAG => result.push_str(TAG_SEQ),
      markers::REFERENCE => result.push_str(REF_SEQ),
      markers::HEADER => result.push_str(HEADER_SEQ),
      markers::CODE => result.push_str(CODE_SEQ),
      markers::KEYWORD_2 => result.push_str(KEYWORD_2_SEQ),
      markers::KEYWORD_3 => result.push_str(KEYWORD_3_SEQ),
      _ => result.push(ch),
    }
  }
  result
}

pub fn unescape_help(raw: &str) -> String {
  let mut result = String::new();
  let mut chars = raw.chars().peekable();
  let mut qt_state = QuoteState::default();

  while let Some(ch) = chars.next() {
    match ch {
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
        while let Some(next_ch) = chars.next() {
          if next_ch == '*' {
            result.push(markers::RESET);
            break;
          } else {
            result.push(next_ch);
          }
        }
      }
      '|' => {
        result.push(markers::REFERENCE);
        while let Some(next_ch) = chars.next() {
          if next_ch == '|' {
            result.push(markers::RESET);
            break;
          } else {
            result.push(next_ch);
          }
        }
      }
      '#' => {
        result.push(markers::HEADER);
        while let Some(next_ch) = chars.next() {
          if next_ch == '#' {
            result.push(markers::RESET);
            break;
          } else {
            result.push(next_ch);
          }
        }
      }
      '`' => {
        result.push(markers::CODE);
        while let Some(next_ch) = chars.next() {
          if next_ch == '`' {
            result.push(markers::RESET);
            break;
          } else {
            result.push(next_ch);
          }
        }
      }
      '{' => {
        result.push(markers::KEYWORD_2);
        while let Some(next_ch) = chars.next() {
          if next_ch == '}' {
            result.push(markers::RESET);
            break;
          } else {
            result.push(next_ch);
          }
        }
      }
      '[' => {
        result.push(markers::KEYWORD_3);
        while let Some(next_ch) = chars.next() {
          if next_ch == ']' {
            result.push(markers::RESET);
            break;
          } else {
            result.push(next_ch);
          }
        }
      }
      _ => result.push(ch),
    }
  }
  result
}
