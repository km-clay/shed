mod markup;
mod pager;
mod render;

use markup::StyledHelp;
use pager::{HelpPager, PagerEvent};

use std::{
  cell::RefCell,
  os::fd::{AsRawFd, BorrowedFd},
  path::Path,
};

use crate::{
  HashSet,
  autoload::{self, Autoloader},
  opt,
  state::meta::MetaTab,
};

use super::{
  super::state::terminal::Terminal,
  Shed, expand, key, keys, match_loop,
  opt::OptSpec,
  outln, procio,
  readline::{self, ScoredCandidate},
  sherr, state,
  util::{self, Direction, ShResult, with_status},
  var,
};

use markup::TAG_SEQ;
use nix::{
  errno::Errno,
  poll::{PollFd, PollFlags, PollTimeout, poll},
};

pub const HELP_PAGE_INSTALL_DIR: Option<&str> = option_env!("SHED_HELP_DIR");

thread_local! {
  static TAG_CACHE: RefCell<Option<(util::PathCache, Vec<ScoredTag>)>> = const { RefCell::new(None) };
}

fn load_help(name: &str) -> Option<String> {
  autoload::HelpLoader
    .get(name)
    .map(|raw| String::from_utf8_lossy(raw).to_string())
}

fn cached_tags<F: FnOnce() -> Vec<ScoredTag>>(build: F) -> Vec<ScoredTag> {
  TAG_CACHE.with(|cache| {
    let mut borrow = cache.borrow_mut();

    let needs_rebuild = match borrow.as_mut() {
      Some((path_cache, _)) => path_cache.update_cache(),
      None => true,
    };

    if needs_rebuild {
      let tags = build();
      let path_cache = borrow.take().map_or_else(
        || util::PathCache::new("SHED_HPATH".to_string()),
        |(pc, _)| pc,
      );
      *borrow = Some((path_cache, tags.clone()));
      tags
    } else {
      borrow.as_ref().unwrap().1.clone()
    }
  })
}

pub(super) struct Help;
impl super::Builtin for Help {
  fn opts(&self) -> Vec<OptSpec> {
    vec![opt!("list-tags" | b'l')]
  }
  fn execute(&self, mut args: super::BuiltinArgs) -> ShResult<()> {
    let _guard = crate::util::guard((), |()| {
      if !Shed::term(Terminal::test_mode) {
        Shed::meta_mut(|_| MetaTab::disable_welcome_message()).unwrap();
      }
    });

    let (arg_vec, opts) = args.take_argv();
    let list_tags = opts.iter().any(|o| o.key() == "list-tags");

    // Join all of the word-split arguments into a single string
    // Preserve the span too
    let topic = (!arg_vec.is_empty()).then(|| super::join_raw_args(arg_vec));

    if list_tags {
      let tags = get_all_tags()?;
      for tag in tags {
        let candidate = tag.tag.candidate;
        outln!("{candidate}");
      }
      with_status(0)
    } else {
      let Some((topic, span)) = topic else {
        // no argument was given. open the index page
        return open_help_index();
      };
      match get_help_content(&topic.to_str_lossy()) {
        Some((line, content, filename)) => open_help(&content, line, filename),
        None => Err(sherr!(
            NotFound @ span,
            "No relevant help page found for this topic",
        )),
      }
    }
  }
}

fn check_hpath_names(hpath_names: &HashSet<String>, page: &str) -> bool {
  let basename = Path::new(page)
    .file_name()
    .and_then(|n| n.to_str())
    .unwrap_or_default();
  hpath_names.contains(basename)
}

/// Embedded help page names, minus any shadowed by an on-disk `SHED_HPATH`
/// file of the same basename. Loading is left to the caller so prefix-match
/// scans can read only the page they keep.
fn embedded_pages(shadowed: &HashSet<String>) -> impl Iterator<Item = &'static str> + '_ {
  autoload::HelpLoader
    .names()
    .filter(move |page| !check_hpath_names(shadowed, page))
}

pub fn get_all_tags() -> ShResult<Vec<ScoredTag>> {
  let mut tags = vec![];

  let hpath = var!("SHED_HPATH");
  let mut hpath_names: HashSet<String> = HashSet::default();

  for entry in util::path_list_entries(&hpath.to_str_lossy()) {
    let path = entry.path();
    if !path.is_file() {
      continue;
    }
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
      hpath_names.insert(name.to_string());
    }
    let mut new_tags = read_tags_from_file(&path)?;
    tags.append(&mut new_tags);
  }

  for page in embedded_pages(&hpath_names) {
    let Some(content) = load_help(page) else {
      continue;
    };
    let mut new_tags = read_tags(&content, page);
    tags.append(&mut new_tags);
  }

  Ok(tags)
}

pub fn open_help_index() -> ShResult<()> {
  let Some(content) = load_help("help/help.txt") else {
    return Err(sherr!(NotFound, "Help index page not found"));
  };

  open_help(&content, 0, Some("help/help.txt".to_string()))
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

  let hpath = var!("SHED_HPATH");
  let mut hpath_names: HashSet<String> = HashSet::default();

  for entry in util::path_list_entries(&hpath.to_str_lossy()) {
    let path = entry.path();
    if !path.is_file() {
      continue;
    }
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
      hpath_names.insert(name.to_string());
    }
    let stem = path.file_stem().unwrap().to_string_lossy();
    if stem.starts_with(topic) {
      let Ok(contents) = std::fs::read_to_string(&path) else {
        continue;
      };

      return Some((0, contents, Some(stem.to_string())));
    }
  }

  for page in embedded_pages(&hpath_names) {
    if page.starts_with(topic) {
      let Some(content) = load_help(page) else {
        continue;
      };
      return Some((0, content, Some(page.to_string())));
    }
  }

  // No filename match, fall through to tag scoring across both sources.
  let mut tags = cached_tags(|| {
    let mut tags = vec![];
    for entry in util::path_list_entries(&hpath.to_str_lossy()) {
      let path = entry.path();
      if !path.is_file() {
        continue;
      }
      tags.extend(read_tags_from_file(&path).ok().unwrap_or_default());
    }
    for page in embedded_pages(&hpath_names) {
      let Some(content) = load_help(page) else {
        continue;
      };
      tags.extend(read_tags(&content, page));
    }
    tags
  });

  score_matches(topic, &mut tags);
  tags.sort_by_key(ScoredTag::get_score);
  log::debug!("tags: {tags:#?}");
  tags.last().and_then(|best| {
    let ScoredTag { tag: _, line, file } = best;

    if let Some(path) = autoload::HelpLoader
      .names()
      .find(|name| *name == file.as_str())
    {
      let content = load_help(path)?;

      return Some((line.saturating_sub(2), content, Some(file.clone())));
    }

    std::fs::read_to_string(file).ok().map(|content| {
      let stem = Path::new(file)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string());
      (line.saturating_sub(2), content, stem)
    })
  })
}

pub fn open_help(content: &str, line: usize, filename: Option<String>) -> ShResult<()> {
  if Shed::term(Terminal::test_mode) {
    return with_status(0);
  }
  let Some(pager) = HelpPager::new(content, line, filename) else {
    return with_status(0); // means stdout is not a terminal, so return
  };

  let mut page_stack = vec![pager];
  let mut pager = 0usize; // index

  // now we use the same input pattern as in main.rs
  let Some(tty) = Shed::term(|t| t.tty().map(|fd| fd.as_raw_fd())) else {
    return with_status(0); // no tty, just return
  };
  let tty_fd = PollFd::new(unsafe { BorrowedFd::borrow_raw(tty) }, PollFlags::POLLIN);

  // restores terminal state on drop
  let _tui_guard = Shed::term_mut(Terminal::prepare_for_pager);

  loop {
    let res = {
      let Some(pager) = page_stack.get_mut(pager) else {
        break;
      };
      pager.display()?;
      match poll(&mut [tty_fd.clone()], PollTimeout::NONE) {
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
          let new_pager = HelpPager::new(&content, line, filename).ok_or_else(|| {
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
      PagerEvent::ClosePage => {
        if pager == 0 {
          break; // if we close the last page, just exit
        }
        page_stack.pop();
        pager -= 1;
      }
      PagerEvent::Continue => (),
      PagerEvent::ExitPager => break,
    }
  }

  with_status(0)
}

#[derive(Clone, Debug)]
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
  pub fn get_score(&self) -> i32 {
    self.tag.score.unwrap_or(i32::MIN)
  }
}

pub fn score_matches(topic: &str, tags: &mut Vec<ScoredTag>) {
  for tag in tags.iter_mut() {
    tag.fuzzy_score(topic);
  }

  tags.retain(|c| c.get_score() > i32::MIN);
}

pub fn read_tags_from_file(path: &Path) -> ShResult<Vec<ScoredTag>> {
  let contents = std::fs::read_to_string(path)?;
  // Pass the full path as `name` so that get_help_content's tag-search
  // restore step can re-read the file. The display filename (a bare stem)
  // is recovered at return time.
  Ok(read_tags(&contents, path.to_string_lossy().as_ref()))
}

pub fn read_tags(content: &str, name: &str) -> Vec<ScoredTag> {
  let styled = StyledHelp::new(content);

  styled
    .find_markers(TAG_SEQ)
    .into_iter()
    .map(|span| {
      ScoredTag::new(
        ScoredCandidate::new(span.content(styled.content()).into()).with_len_penalty(true),
        span.line_no(styled.content()),
        name,
      )
    })
    .collect()
}

#[cfg(test)]
mod open_help_tests {
  use super::*;
  use crate::state::Shed;
  use crate::state::terminal::Terminal;
  use crate::state::vars::{VarFlags, VarKind, VarStr};
  use crate::tests::testutil::TestGuard;

  const SAMPLE: &str =
    "line 1\nline 2\nline 3\nline 4\nline 5\nline 6\nline 7\nline 8\nline 9\nline 10";

  fn arm_raw_tty() {
    Shed::term_mut(Terminal::enforce_raw_mode).unwrap();
  }

  // ─── Exit paths ──────────────────────────────────────────────────────

  #[test]
  fn q_exits_returning_ok() {
    let g = TestGuard::new();
    arm_raw_tty();
    g.feed_tty(b"q");
    assert!(open_help(SAMPLE, 0, None).is_ok());
  }

  #[test]
  fn esc_with_no_state_exits_returning_ok() {
    let g = TestGuard::new();
    arm_raw_tty();
    g.feed_tty(b"\x1b");
    assert!(open_help(SAMPLE, 0, None).is_ok());
  }

  // ─── Various non-exit keys folded into one cycle ─────────────────────
  // drain_keys parses every buffered byte into a key, and handle_input
  // returns only the final event. So all of these test that the key
  // sequences don't break or panic — they don't separately exercise
  // open_help's Forward/Back/OpenRef arms (those need separate poll
  // cycles).

  #[test]
  fn scroll_keys_followed_by_quit_succeed() {
    let g = TestGuard::new();
    arm_raw_tty();
    g.feed_tty(b"jjjjjkkggGq");
    assert!(open_help(SAMPLE, 0, None).is_ok());
  }

  #[test]
  fn page_motions_followed_by_quit_succeed() {
    let g = TestGuard::new();
    arm_raw_tty();
    g.feed_tty(b"duduq");
    assert!(open_help(SAMPLE, 0, None).is_ok());
  }

  #[test]
  fn unknown_key_then_quit_succeeds() {
    let g = TestGuard::new();
    arm_raw_tty();
    // 'z' falls into the catchall (PagerEvent::Continue) before 'q' exits.
    g.feed_tty(b"zq");
    assert!(open_help(SAMPLE, 0, None).is_ok());
  }

  #[test]
  fn navigation_keys_then_quit_succeed() {
    let g = TestGuard::new();
    arm_raw_tty();
    // 'h' / 'l' would be Back / Forward, but folded into one cycle with
    // 'q' the final event is Exit. Still exercises pager.handle_key for
    // those codes.
    g.feed_tty(b"hlhlq");
    assert!(open_help(SAMPLE, 0, None).is_ok());
  }

  // ─── Argument variants ───────────────────────────────────────────────

  #[test]
  fn with_filename_some_exits_ok() {
    let g = TestGuard::new();
    arm_raw_tty();
    g.feed_tty(b"q");
    assert!(open_help(SAMPLE, 0, Some("topic.help".into())).is_ok());
  }

  #[test]
  fn with_nonzero_line_offset_exits_ok() {
    let g = TestGuard::new();
    arm_raw_tty();
    g.feed_tty(b"q");
    assert!(open_help(SAMPLE, 5, None).is_ok());
  }

  #[test]
  fn empty_content_exits_ok() {
    let g = TestGuard::new();
    arm_raw_tty();
    g.feed_tty(b"q");
    assert!(open_help("", 0, None).is_ok());
  }

  #[test]
  fn single_line_content_exits_ok() {
    let g = TestGuard::new();
    arm_raw_tty();
    g.feed_tty(b"q");
    assert!(open_help("just one line", 0, None).is_ok());
  }

  #[test]
  fn get_hpath_help() {
    let _g = TestGuard::new();
    let dir = tempfile::TempDir::new().unwrap();
    let dir_raw = dir.path().display().to_string();
    Shed::vars_mut(|v| v.set_var("SHED_HPATH", VarKind::Str(dir_raw.into()), VarFlags::EXPORT))
      .unwrap();
    let file_path = dir.path().join("some_help_file.txt");
    std::fs::write(
      &file_path,
      "This is some help content\nmore content  *more-content*\nfoo bar biz",
    )
    .unwrap();
    let (line, content, filename) = get_help_content("some_help_file").unwrap();
    assert!(content.contains("This is some help content"));
    assert_eq!(filename, Some("some_help_file".to_string()));
    assert_eq!(line, 0);
  }

  #[test]
  fn get_hpath_tag_help() {
    let _g = TestGuard::new();
    let dir = tempfile::TempDir::new().unwrap();
    let dir_raw = dir.path().display().to_string();
    Shed::vars_mut(|v| v.set_var("SHED_HPATH", VarKind::Str(dir_raw.into()), VarFlags::EXPORT))
      .unwrap();
    let file_path = dir.path().join("some_help_file.txt");

    let body = "This is some help content\nmore content  *more-content*\nfoo bar biz";
    std::fs::write(&file_path, body).unwrap();
    let (line, content, filename) = get_help_content("more-content").unwrap();

    assert!(content.contains("This is some help content"));
    assert_eq!(filename, Some("some_help_file".to_string()));
    // Tag is on line 1 (0-indexed); the function subtracts 2 for scroll
    // context but saturates at 0.
    assert_eq!(line, 0);
  }

  // ─── get_help_content: direct file path ────────────────────────────

  #[test]
  fn get_help_content_direct_absolute_file_path() {
    let _g = TestGuard::new();
    // Wipe HPATH so we don't accidentally match a system file.
    Shed::vars_mut(|v| {
      v.set_var(
        "SHED_HPATH",
        VarKind::Str(VarStr::default()),
        VarFlags::EXPORT,
      )
    })
    .unwrap();
    let dir = tempfile::TempDir::new().unwrap();
    let file_path = dir.path().join("mytopic.txt");
    std::fs::write(&file_path, "direct file body").unwrap();

    let (line, content, filename) = get_help_content(&file_path.to_string_lossy()).unwrap();
    assert_eq!(line, 0);
    assert_eq!(content, "direct file body");
    assert_eq!(filename, Some("mytopic".to_string()));
  }

  #[test]
  fn get_help_content_direct_file_takes_precedence_over_hpath_match() {
    let _g = TestGuard::new();
    let hpath_dir = tempfile::TempDir::new().unwrap();
    // Stage an hpath file that *would* match if direct lookup failed.
    let shadow = hpath_dir.path().join("mytopic.txt");
    std::fs::write(&shadow, "hpath shadow").unwrap();
    Shed::vars_mut(|v| {
      v.set_var(
        "SHED_HPATH",
        VarKind::Str(hpath_dir.path().to_string_lossy().into()),
        VarFlags::EXPORT,
      )
    })
    .unwrap();

    // The direct lookup hits a different file with the same stem.
    let other_dir = tempfile::TempDir::new().unwrap();
    let direct = other_dir.path().join("mytopic.txt");
    std::fs::write(&direct, "direct wins").unwrap();

    let (_, content, _) = get_help_content(&direct.to_string_lossy()).unwrap();
    assert_eq!(content, "direct wins");
  }

  // ─── get_help_content: HELP_PAGES prefix match ─────────────────────

  #[test]
  fn get_help_content_builtin_page_prefix_match() {
    let _g = TestGuard::new();
    Shed::vars_mut(|v| {
      v.set_var(
        "SHED_HPATH",
        VarKind::Str(VarStr::default()),
        VarFlags::EXPORT,
      )
    })
    .unwrap();
    // "help/help" is a prefix of the bundled "help/help.txt" page name.
    let (line, _content, filename) = get_help_content("help/help").unwrap();
    assert_eq!(line, 0);
    assert_eq!(filename, Some("help/help.txt".to_string()));
  }

  #[test]
  fn get_help_content_builtin_page_first_match_wins() {
    // The prefix "help/" matches every bundled page, but we return the
    // first one — "help/arith.txt" is the first entry in HELP_PAGES.
    let _g = TestGuard::new();
    Shed::vars_mut(|v| {
      v.set_var(
        "SHED_HPATH",
        VarKind::Str(VarStr::default()),
        VarFlags::EXPORT,
      )
    })
    .unwrap();
    let (_, _, filename) = get_help_content("help/").unwrap();
    assert_eq!(filename, Some("help/arith.txt".to_string()));
  }

  // ─── get_help_content: no-match path ───────────────────────────────

  #[test]
  fn get_help_content_no_match_returns_none() {
    let _g = TestGuard::new();
    Shed::vars_mut(|v| {
      v.set_var(
        "SHED_HPATH",
        VarKind::Str(VarStr::default()),
        VarFlags::EXPORT,
      )
    })
    .unwrap();
    // A long random topic ensures fuzzy_score returns i32::MIN for every
    // tag (some char in the topic won't appear in any tag), so tags is
    // empty after score_matches retains, and tags.last() yields None.
    assert!(get_help_content("zzzz_nonexistent_topic_qqq").is_none());
  }

  // ─── get_help_content: HPATH edge cases ────────────────────────────

  #[test]
  fn get_help_content_hpath_nonexistent_dir_falls_through() {
    let _g = TestGuard::new();
    Shed::vars_mut(|v| {
      v.set_var(
        "SHED_HPATH",
        VarKind::Str("/this/path/should/never/exist/xyz".into()),
        VarFlags::EXPORT,
      )
    })
    .unwrap();
    // The bogus HPATH read_dir errors → continue. Falls through to
    // HELP_PAGES, which matches "help/help.txt" by prefix.
    let (_, _, filename) = get_help_content("help/help").unwrap();
    assert_eq!(filename, Some("help/help.txt".to_string()));
  }

  #[test]
  fn get_help_content_hpath_skips_subdir_entries() {
    // HPATH dir contains a subdirectory AND a file. The loop must skip
    // the subdir (`!path.is_file() → continue`) but still find the file.
    let _g = TestGuard::new();
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::create_dir(dir.path().join("a_subdir")).unwrap();
    let file_path = dir.path().join("zzzz_unique.txt");
    std::fs::write(&file_path, "found me").unwrap();
    Shed::vars_mut(|v| {
      v.set_var(
        "SHED_HPATH",
        VarKind::Str(dir.path().to_string_lossy().into()),
        VarFlags::EXPORT,
      )
    })
    .unwrap();
    let (_, content, filename) = get_help_content("zzzz_unique").unwrap();
    assert_eq!(content, "found me");
    assert_eq!(filename, Some("zzzz_unique".to_string()));
  }

  #[test]
  fn get_help_content_directory_path_falls_through_to_other_lookups() {
    // Passing a directory path means `is_file()` is false, so direct
    // lookup is skipped and we proceed to the HPATH/HELP_PAGES loops.
    let _g = TestGuard::new();
    Shed::vars_mut(|v| {
      v.set_var(
        "SHED_HPATH",
        VarKind::Str(VarStr::default()),
        VarFlags::EXPORT,
      )
    })
    .unwrap();
    let dir = tempfile::TempDir::new().unwrap();
    // The dir path won't match anything; expect None (no tags will
    // fuzzy-match a full /tmp/... path either).
    assert!(get_help_content(&dir.path().to_string_lossy()).is_none());
  }
}

#[cfg(test)]
mod help_execute_tests {
  use crate::state;
  use crate::state::meta::MetaTab;
  use crate::state::terminal::Terminal;
  use crate::tests::testutil::{TestGuard, test_input};

  fn arm_raw_tty() {
    state::Shed::term_mut(Terminal::enforce_raw_mode).unwrap();
  }

  /// `Help::execute`'s scopeguard calls `disable_welcome_message` which
  /// INSERTs into the `meta` table; that table is only created during
  /// `interactive_setup`. Tests need to set it up themselves.
  fn ensure_meta_table() {
    state::Shed::meta(|_| MetaTab::ensure_meta_table()).unwrap();
  }

  // ─── help -l / --list-tags ──────────────────────────────────────

  #[test]
  fn help_dash_l_lists_tags() {
    let g = TestGuard::new();
    ensure_meta_table();
    test_input("help -l").unwrap();
    let out = g.read_output();
    // Should print at least one known builtin tag.
    assert!(!out.is_empty(), "expected non-empty tag list");
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn help_long_list_tags_flag() {
    let g = TestGuard::new();
    ensure_meta_table();
    test_input("help --list-tags").unwrap();
    let out = g.read_output();
    assert!(!out.is_empty());
    assert_eq!(state::Shed::get_status(), 0);
  }

  // ─── help <unknown topic> → NotFound ────────────────────────────

  #[test]
  fn help_unknown_topic_errors() {
    let _g = TestGuard::new();
    ensure_meta_table();
    test_input("help xyzzzz_nonexistent_topic_qqq").ok();
    assert_ne!(state::Shed::get_status(), 0);
  }

  // ─── help (no args) → opens help page ──────────────────────────
  //
  // No args defaults to topic="help" which resolves to the bundled
  // help/help.txt. open_help enters the pager loop; we feed 'q' before
  // calling so the loop exits immediately.

  #[test]
  fn help_no_args_opens_default_topic_and_exits_cleanly() {
    let g = TestGuard::new();
    ensure_meta_table();
    arm_raw_tty();
    g.feed_tty(b"q");
    test_input("help").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }

  #[test]
  fn help_with_topic_argument_opens_pager() {
    // 'help builtin' resolves to help/builtin.txt via prefix match.
    let g = TestGuard::new();
    ensure_meta_table();
    arm_raw_tty();
    g.feed_tty(b"q");
    test_input("help help/builtin").unwrap();
    assert_eq!(state::Shed::get_status(), 0);
  }
}

#[cfg(test)]
mod get_all_tags_tests {
  use super::*;
  use crate::state::vars::{VarFlags, VarKind, VarStr};
  use crate::tests::testutil::TestGuard;

  #[test]
  fn returns_at_least_builtin_tags() {
    let _g = TestGuard::new();
    // Empty HPATH so only the builtin HELP_PAGES contribute.
    Shed::vars_mut(|v| {
      v.set_var(
        "SHED_HPATH",
        VarKind::Str(VarStr::default()),
        VarFlags::EXPORT,
      )
      .unwrap();
    });
    let tags = get_all_tags().unwrap();
    assert!(
      !tags.is_empty(),
      "expected builtin pages to contribute tags"
    );
  }

  #[test]
  fn hpath_files_contribute_tags() {
    let _g = TestGuard::new();
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("custom_help.txt");
    std::fs::write(
      &path,
      "intro line\nthis line tags *unique-test-tag-xyz*\nmore content",
    )
    .unwrap();
    Shed::vars_mut(|v| {
      v.set_var(
        "SHED_HPATH",
        VarKind::Str(dir.path().to_string_lossy().into()),
        VarFlags::EXPORT,
      )
      .unwrap();
    });
    let tags = get_all_tags().unwrap();
    let names: Vec<&str> = tags.iter().map(|t| t.tag.candidate.content()).collect();
    assert!(names.contains(&"unique-test-tag-xyz"), "got: {names:?}");
  }

  #[test]
  fn hpath_nonexistent_dir_skipped() {
    let _g = TestGuard::new();
    Shed::vars_mut(|v| {
      v.set_var(
        "SHED_HPATH",
        VarKind::Str("/this/dir/never/exists/zzz".into()),
        VarFlags::EXPORT,
      )
      .unwrap();
    });
    // Should still succeed and return builtin tags.
    let tags = get_all_tags().unwrap();
    assert!(!tags.is_empty());
  }

  #[test]
  fn hpath_subdirectory_entries_skipped() {
    let _g = TestGuard::new();
    let dir = tempfile::TempDir::new().unwrap();
    // Create a subdir (should be skipped) and a regular file with a tag.
    std::fs::create_dir(dir.path().join("a_subdir")).unwrap();
    std::fs::write(
      dir.path().join("with_tag.txt"),
      "line\n*hpath-skip-test-tag* more",
    )
    .unwrap();
    Shed::vars_mut(|v| {
      v.set_var(
        "SHED_HPATH",
        VarKind::Str(dir.path().to_string_lossy().into()),
        VarFlags::EXPORT,
      )
      .unwrap();
    });
    let tags = get_all_tags().unwrap();
    let names: Vec<&str> = tags.iter().map(|t| t.tag.candidate.content()).collect();
    assert!(names.contains(&"hpath-skip-test-tag"), "got: {names:?}");
  }
}
