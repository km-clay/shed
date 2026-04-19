mod markup;
mod pager;

use std::{
  env,
  io::Write,
  ops::Range,
  path::{Path, PathBuf},
  rc::Rc,
};

use crate::{
  builtin::{
    help::{
      markup::StyledHelp,
      pager::{HelpPager, PagerCmd, PagerEvent},
    },
    join_raw_arg_iter,
  },
  libsh::{
    error::ShResult,
    guards::{RawModeGuard, TuiGuard},
    sys::TTY_FILENO,
  },
  parse::{
    NdRule, Node,
    execute::{exec_input, prepare_argv},
  },
  procio::borrow_fd,
  readline::complete::ScoredCandidate,
  sherr,
  state::{self, write_meta},
};

use markup::TAG_SEQ;
use nix::{
  errno::Errno,
  poll::{PollFd, PollFlags, PollTimeout, poll},
};

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

  let _guard = scopeguard::guard((), |_| {
    write_meta(|m| m.disable_welcome_message()).unwrap();
  });

  let mut argv = prepare_argv(argv)?.into_iter().peekable();
  let help = argv.next().unwrap(); // drop 'help'

  // Join all of the word-split arguments into a single string
  // Preserve the span too
  let (topic, span) = if argv.peek().is_none() {
    ("help".to_string(), help.1)
  } else {
    join_raw_arg_iter(argv)
  };

  match get_help_content(&topic) {
    Some((line, content, filename)) => open_help(&content, line, filename),
    None => Err(sherr!(
      NotFound @ span,
      "No relevant help page found for this topic",
    )),
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
      if stem.starts_with(topic) {
        let Ok(contents) = std::fs::read_to_string(&path) else {
          continue;
        };

        return Some((0, contents, Some(stem.to_string())));
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

        let mut new_tags = read_tags(&path).ok()?;
        score_matches(topic, &mut new_tags);
        tags.append(&mut new_tags);
      }
    }
  }

  tags.sort_by_key(|t| t.score());
  log::debug!("tags: {tags:#?}");
  tags.last().and_then(|best| {
    let ScoredTag { tag: _, line, file } = best;
    let file_name = file.file_stem().map(|s| s.to_string_lossy().to_string());
    std::fs::read_to_string(file)
      .ok()
      .map(|content| (line.saturating_sub(2), content, file_name))
  })
}

pub fn open_help(content: &str, line: usize, filename: Option<String>) -> ShResult<()> {
  let Some(mut pager) = HelpPager::new(content.to_string(), line, filename) else {
    return Ok(()); // means stdout is not a terminal, so return
  };

  // now we use the same input pattern as in main.rs
  let tty_fd = PollFd::new(borrow_fd(*TTY_FILENO), PollFlags::POLLIN);
  let _tui_guard = TuiGuard::new(); // enters the alt buffer, hides the cursor
  // restores terminal state on drop

  loop {
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

    // if we are here, we have input to read

    match pager.handle_input()? {
      PagerEvent::OpenRef(crossref) => match get_help_content(&crossref) {
        // recursively open pagers to navigate to other pages
        Some((line, content, filename)) => open_help(&content, line, filename),
        None => Err(sherr!(
          NotFound,
          "No relevant help page found for topic '{crossref}'",
        )),
      }?,
      PagerEvent::Continue => continue,
      PagerEvent::Exit => break,
    }
  }

  Ok(())
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
  let styled = StyledHelp::new(&contents);

  let tags = styled
    .find_markers(TAG_SEQ)
    .into_iter()
    .map(|span| {
      ScoredTag::new(
        ScoredCandidate::new(span.content(styled.content()).into()).with_len_penalty(true),
        span.line_no(styled.content()),
        path,
      )
    })
    .collect();

  Ok(tags)
}
