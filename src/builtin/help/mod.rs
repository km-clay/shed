mod markup;
mod pager;

use std::{
  env,
  path::Path,
};

use crate::{
  builtin::help::{
    markup::StyledHelp,
    pager::{HelpPager, PagerEvent},
  },
  parse::lex::Span,
  procio::borrow_fd,
  readline::complete::ScoredCandidate,
  sherr,
  state::{with_term, write_meta},
  util::error::ShResult,
};

use markup::TAG_SEQ;
use nix::{
  errno::Errno,
  poll::{PollFd, PollFlags, PollTimeout, poll},
};

/// Validates the included help pages
///
/// If the help pages contain tabs or non-ascii characters,
/// that is a compile error. The goal is to keep formatting
/// consistent, and make sure that output looks good even in the tty
const fn validate_help_page(s: &str) {
  let bytes = s.as_bytes();
  let mut i = 0;
  while i < bytes.len() {
    if bytes[i] == b'\t' {
      panic!("help file contains tabs")
    }
    if bytes[i] > 127 {
      panic!("help file contains non-ascii characters")
    }
    i += 1;
  }
}

macro_rules! include_help_pages {
  ($($name:literal),* $(,)?) => {
    const HELP_PAGES: &[(&str, &str)] = &[
      $({
        const S: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/", $name));
        validate_help_page(S);
        ($name, S)
      },)*
    ];
  };
}

include_help_pages! {
  "doc/arith.txt",
  "doc/autocmd.txt",
  "doc/builtin.txt",
  "doc/commands.txt",
  "doc/ex.txt",
  "doc/glob.txt",
  "doc/help.txt",
  "doc/jobs.txt",
  "doc/keybinds.txt",
  "doc/param.txt",
  "doc/prompt.txt",
  "doc/redirect.txt",
  "doc/scripting.txt",
  "doc/socket.txt",
  "doc/variables.txt",
}

pub(super) struct Help;
impl super::Builtin for Help {
  fn execute(&self, args: super::BuiltinArgs) -> ShResult<()> {
    let _guard = scopeguard::guard((), |_| {
      write_meta(|m| m.disable_welcome_message()).unwrap();
    });
    let mut argv = args.argv.into_iter().peekable();

    // Join all of the word-split arguments into a single string
    // Preserve the span too
    let (topic, span) = if argv.peek().is_none() {
      ("help".to_string(), Span::default())
    } else {
      super::join_raw_arg_iter(argv)
    };

    match get_help_content(&topic) {
      Some((line, content, filename)) => open_help(&content, line, filename),
      None => Err(sherr!(
        NotFound @ span,
        "No relevant help page found for this topic",
      )),
    }
  }
}

pub fn get_help_content(topic: &str) -> Option<(usize, String, Option<String>)> {
  let path = Path::new(topic);
  if path.is_file()
    && let Ok(contents) = std::fs::read_to_string(path)
  {
    return Some((
      0,
      contents,
      path.file_stem().map(|s| s.to_string_lossy().to_string()),
    ));
  }

  let hpath = env::var("SHED_HPATH").unwrap_or_default();

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
      if stem.starts_with(topic) {
        let Ok(contents) = std::fs::read_to_string(&path) else {
          continue;
        };

        return Some((0, contents, Some(stem.to_string())));
      }
    }
  }

  // ok, not a filename. let's check our builtin help pages
  for (page,content) in HELP_PAGES {
    if page.starts_with(topic) {
      return Some((0, content.to_string(), Some(page.to_string())));
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

        let mut new_tags = read_tags_from_file(&path).ok()?;
        score_matches(topic, &mut new_tags);
        tags.append(&mut new_tags);
      }
    }
  }

  for (page,content) in HELP_PAGES {
    let mut new_tags = read_tags(content, page).ok()?;
    score_matches(topic, &mut new_tags);
    tags.append(&mut new_tags);
  }

  tags.sort_by_key(|t| t.score());
  log::debug!("tags: {tags:#?}");
  tags.last().and_then(|best| {
    let ScoredTag { tag: _, line, file } = best;

    if let Some((_, content)) = HELP_PAGES.iter().find(|(name, _)| name == file) {
      return Some((line.saturating_sub(2), content.to_string(), Some(file.to_string())));
    }

    std::fs::read_to_string(file)
      .ok()
      .map(|content| (line.saturating_sub(2), content, Some(file.to_string())))
  })
}

pub fn open_help(content: &str, line: usize, filename: Option<String>) -> ShResult<()> {
  let Some(pager) = HelpPager::new(content.to_string(), line, filename) else {
    return Ok(()); // means stdout is not a terminal, so return
  };

  let mut page_stack = vec![pager];
  let mut pager = 0usize; // index

  // now we use the same input pattern as in main.rs
  let Some(tty) = with_term(|t| unsafe { t.tty() }) else {
    return Ok(()); // no tty, just return
  };
  let tty_fd = PollFd::new(borrow_fd(tty), PollFlags::POLLIN);

  // restores terminal state on drop
  let _tui_guard = with_term(|t| t.prepare_for_pager());

  loop {
    let res = {
      let Some(pager) = page_stack.get_mut(pager) else {
        break;
      };
      pager.display()?;
      match poll(&mut [tty_fd], PollTimeout::NONE) {
        Ok(0) => {
          // timeout? eof?
          break;
        }
        Ok(_) => { /* fall through */ }
        Err(Errno::EINTR) => continue, // just retry
        Err(e) => {
          return Err(sherr!(
            InternalErr,
            "Error polling for help pager input: {e}"
          ));
        }
      }

      pager.handle_input()?
    };
    // if we are here, we have input to read

    match res {
      PagerEvent::OpenRef(crossref) => match get_help_content(&crossref) {
        // open new pager, push to stack
        Some((line, content, filename)) => {
          let new_pager = HelpPager::new(content, line, filename).ok_or_else(|| {
            sherr!(
              NotFound,
              "No relevant help page found for topic '{crossref}'",
            )
          })?;
          page_stack.truncate(pager + 1); // drop any "forward" history if we navigate to a new page
          page_stack.push(new_pager);
          pager = page_stack.len() - 1;
        }
        None => {
          return Err(sherr!(
            NotFound,
            "No relevant help page found for topic '{crossref}'",
          ));
        }
      },
      PagerEvent::Forward => {
        pager = (pager + 1).min(page_stack.len() - 1);
      }
      PagerEvent::Back => {
        pager = pager.saturating_sub(1);
      }
      PagerEvent::Continue => continue,
      PagerEvent::Exit => {
        if pager > 0 {
          page_stack.truncate(pager); // go back to previous page
          pager -= 1;
        } else {
          break; // exit pager
        }
      }
    }
  }

  Ok(())
}

#[derive(Debug)]
pub struct ScoredTag {
  tag: ScoredCandidate,
  line: usize,
  file: String,
}

impl ScoredTag {
  pub fn new(tag: ScoredCandidate, line: usize, file: &str) -> Self {
    Self {
      tag,
      line,
      file: file.to_string(),
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

pub fn read_tags_from_file(path: &Path) -> ShResult<Vec<ScoredTag>> {
  let contents = std::fs::read_to_string(path)?;
  read_tags(&contents, path.file_stem().unwrap().to_string_lossy().as_ref())
}

pub fn read_tags(content: &str, name: &str) -> ShResult<Vec<ScoredTag>> {
  let styled = StyledHelp::new(content);

  let tags = styled
    .find_markers(TAG_SEQ)
    .into_iter()
    .map(|span| {
      ScoredTag::new(
        ScoredCandidate::new(span.content(styled.content()).into()).with_len_penalty(true),
        span.line_no(styled.content()),
        name
      )
    })
    .collect();

  Ok(tags)
}
