use std::{
  env,
  io::Write,
  path::{Path, PathBuf},
};

use crate::{
  builtin::join_raw_arg_iter,
  libsh::{error::ShResult, guards::RawModeGuard},
  match_loop,
  parse::{
    NdRule, Node,
    execute::{exec_input, prepare_argv},
    lex::QuoteState,
  },
  readline::{complete::ScoredCandidate, markers},
  sherr, state,
};

const TAG_SEQ: &str = "\x1b[1;33m"; // bold yellow — searchable tags
const REF_SEQ: &str = "\x1b[4;36m"; // underline cyan — cross-references
const RESET_SEQ: &str = "\x1b[0m";
const HEADER_SEQ: &str = "\x1b[1;35m"; // bold magenta — section headers
const CODE_SEQ: &str = "\x1b[32m"; // green — inline code
const KEYWORD_2_SEQ: &str = "\x1b[1;32m"; // bold green — {keyword}
const KEYWORD_3_SEQ: &str = "\x1b[3;37m"; // italic white — [optional]

/// Directory to search for help docs, set at compile time from the `SHED_DOC_DIR` environment variable
/// Useful for package build scripts that also install the help pages, to ensure the correct path is embedded in the binary
const DOC_DIR: &str = match option_env!("SHED_DOC_DIR") {
  Some(dir) => dir,
  None => "doc",
};

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
    ("help".to_string(), help.1)
  } else {
    join_raw_arg_iter(argv)
  };

  let hpath = env::var("SHED_HPATH").unwrap_or_default();
  let hpath = [hpath.as_str(), DOC_DIR].join(":");

  // search for prefixes of help doc filenames
  for path in hpath.split(':') {
    let dir = Path::new(path);
    let Ok(entries) = dir.read_dir() else {
      continue;
    };
    for entry in entries {
      let Ok(entry) = entry else { continue };
      let path = entry.path();
      if !path.is_file() {
        continue;
      }
      let stem = path.file_stem().unwrap().to_string_lossy();
      if stem.starts_with(&topic) {
        let Ok(contents) = std::fs::read_to_string(&path) else {
          continue;
        };

        let unescaped = unescape_help(&contents);
        let expanded = expand_help(&unescaped);
        open_help(&expanded, None, Some(stem.into_owned()))?;
        state::set_status(0);
        return Ok(());
      }
    }
  }

  // didn't find a filename match, its probably a tag search
  let mut tags = vec![];
  for path in hpath.split(':') {
    let path = Path::new(path);
    if let Ok(entries) = path.read_dir() {
      for entry in entries {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if !path.is_file() {
          continue;
        }

        let mut new_tags = read_tags(&path)?;
        score_matches(&topic, &mut new_tags);
        tags.append(&mut new_tags);
      }
    }
  }

  tags.sort_by_key(|t| t.score());
  log::debug!("tags: {tags:#?}");
  if let Some(best) = tags.last() {
    let ScoredTag { tag: _, line, file } = best;
    let file_name = file.file_stem().map(|s| s.to_string_lossy().to_string());
    let contents = std::fs::read_to_string(file)?;
    let expanded = expand_help(&unescape_help(&contents));
    open_help(&expanded, Some(*line), file_name)?;

    state::set_status(0);
    Ok(())
  } else {
    state::set_status(1);
    Err(sherr!(
      NotFound @ span,
      "No relevant help page found for this topic",
    ))
  }
}

pub fn open_help(content: &str, line: Option<usize>, file_name: Option<String>) -> ShResult<()> {
  let pager = env::var("SHED_HPAGER").unwrap_or(env::var("PAGER").unwrap_or("less -R".into()));
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

#[derive(Debug)]
pub struct ScoredTag {
  tag: ScoredCandidate,
  line: usize,
  file: PathBuf,
}

impl ScoredTag {
  pub fn new<P: AsRef<Path>>(tag: ScoredCandidate, line: usize, file: P) -> Self {
    Self {
      tag,
      line,
      file: file.as_ref().to_path_buf(),
    }
  }
  pub fn fuzzy_score(&mut self, topic: &str) {
    self.tag.fuzzy_score(topic);
  }
  pub fn score(&self) -> i32 {
    self.tag.score.unwrap_or(i32::MIN)
  }
}

pub fn score_matches(topic: &str, tags: &mut Vec<ScoredTag>) {
  for tag in tags.iter_mut() {
    tag.fuzzy_score(topic);
  }

  tags.retain(|c| c.score() > i32::MIN);
}

pub fn read_tags(path: &Path) -> ShResult<Vec<ScoredTag>> {
  let contents = std::fs::read_to_string(path)?;

  let unescaped = unescape_help(&contents);
  let raw = expand_help(&unescaped);
  let mut tags = vec![];

  for (line_num, line) in raw.lines().enumerate() {
    let mut rest = line;

    while let Some(pos) = rest.find(TAG_SEQ) {
      let after_seq = &rest[pos + TAG_SEQ.len()..];
      if let Some(end) = after_seq.find(RESET_SEQ) {
        let tag = ScoredTag {
          tag: ScoredCandidate::new(after_seq[..end].into()).with_len_penalty(true),
          line: line_num + 1,
          file: path.to_path_buf(),
        };
        tags.push(tag);
        rest = &after_seq[end + RESET_SEQ.len()..];
      } else {
        break;
      }
    }
  }

  Ok(tags)
}

pub fn expand_help(raw: &str) -> String {
  let mut result = String::new();
  let mut chars = raw.chars();

  match_loop!(chars.next() => ch, {
    markers::RESET => result.push_str(RESET_SEQ),
    markers::TAG => result.push_str(TAG_SEQ),
    markers::REFERENCE => result.push_str(REF_SEQ),
    markers::HEADER => result.push_str(HEADER_SEQ),
    markers::CODE => result.push_str(CODE_SEQ),
    markers::KEYWORD_2 => result.push_str(KEYWORD_2_SEQ),
    markers::KEYWORD_3 => result.push_str(KEYWORD_3_SEQ),
    _ => result.push(ch),
  });
  result
}

pub fn unescape_help(raw: &str) -> String {
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
    _ if qt_state.in_quote() || chars.peek().is_none_or(|ch| ch.is_whitespace()) => {
      result.push(ch);
    }
    '*' => {
      result.push(markers::TAG);
      match_loop!(chars.next() => next_ch, {
        '*' => {
          result.push(markers::RESET);
          break;
        }
         _ => result.push(next_ch),
      });
    }
    '|' => {
      result.push(markers::REFERENCE);
      match_loop!(chars.next() => next_ch, {
        '|' => {
          result.push(markers::RESET);
          break;
        }
         _ => result.push(next_ch),
      });
    }
    '#' => {
      result.push(markers::HEADER);
      match_loop!(chars.next() => next_ch, {
        '#' => {
          result.push(markers::RESET);
          break;
        }
         _ => result.push(next_ch),
      });
    }
    '`' => {
      result.push(markers::CODE);
      match_loop!(chars.next() => next_ch, {
        '`' => {
          result.push(markers::RESET);
          break;
        }
         _ => result.push(next_ch),
      });
    }
    '{' => {
      result.push(markers::KEYWORD_2);
      match_loop!(chars.next() => next_ch, {
        '}' => {
          result.push(markers::RESET);
          break;
        }
         _ => result.push(next_ch),
      });
    }
    '[' => {
      result.push(markers::KEYWORD_3);
      match_loop!(chars.next() => next_ch, {
        ']' => {
          result.push(markers::RESET);
          break;
        }
         _ => result.push(next_ch),
      });
    }
    _ => result.push(ch),
  });
  result
}
