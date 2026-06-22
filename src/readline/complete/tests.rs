use super::*;
use crate::{
  keys::KeyCode as C,
  readline::{Prompt, ShedLine},
  state::{
    Shed,
    terminal::Terminal,
    vars::{VarFlags, VarKind},
  },
  tests::testutil::TestGuard,
};
fn test_vi(initial: &str) -> (ShedLine, TestGuard) {
  let g = TestGuard::new();
  let prompt = Prompt::default();
  let vi = ShedLine::new_no_hist(prompt).unwrap().with_initial(initial);
  (vi, g)
}

// ===================== ScoredCandidate::fuzzy_score =====================

#[test]
fn fuzzy_exact_match() {
  let mut c = ScoredCandidate::new("hello".into());
  let score = c.fuzzy_score("hello");
  assert!(score > 0);
}

#[test]
fn fuzzy_prefix_match() {
  let mut c = ScoredCandidate::new("hello_world".into());
  let score = c.fuzzy_score("hello");
  assert!(score > 0);
}

#[test]
fn fuzzy_no_match() {
  let mut c = ScoredCandidate::new("abc".into());
  let score = c.fuzzy_score("xyz");
  assert_eq!(score, i32::MIN);
}

#[test]
fn fuzzy_empty_query() {
  let mut c = ScoredCandidate::new("anything".into());
  let score = c.fuzzy_score("");
  assert_eq!(score, 0);
}

#[test]
fn fuzzy_boundary_bonus() {
  let mut a = ScoredCandidate::new("foo_bar".into());
  let mut b = ScoredCandidate::new("fxxxbxr".into());
  let score_a = a.fuzzy_score("fbr");
  let score_b = b.fuzzy_score("fbr");
  // word-boundary match should score higher
  assert!(score_a > score_b);
}

// ===================== CompResult::from_candidates =====================

#[test]
fn comp_result_no_match() {
  let result = CompResult::from_candidates(vec![]);
  assert!(matches!(result, CompResult::NoMatch));
}

#[test]
fn comp_result_single() {
  let result = CompResult::from_candidates(vec!["foo".into()]);
  assert!(matches!(result, CompResult::Exact { .. }));
}

#[test]
fn comp_result_many() {
  let result = CompResult::from_candidates(vec!["foo".into(), "bar".into()]);
  assert!(matches!(result, CompResult::Many { .. }));
}

// ===================== CompResult::try_collapse_by_prefix =====================
//
// The zsh-style "first Tab advances to common prefix, second Tab opens
// the selector" behavior. Pre-tab collapse: Many; post-collapse: either
// CommonPrefix (when LCP extends the typed input) or unchanged Many (when
// LCP can't help). Tests cover both branches plus the edge cases.

fn many(cands: &[&str]) -> CompResult {
  CompResult::from_candidates(cands.iter().map(|s| Candidate::from(*s)).collect())
}

fn collapsed_single(result: &CompResult) -> Option<&str> {
  match result {
    CompResult::CommonPrefix { result } | CompResult::Exact { result } => Some(result.content()),
    _ => None,
  }
}

// ─── Happy path: LCP extends user input ───────────────────────────

#[test]
fn collapse_advances_to_common_prefix() {
  // User typed "gi", candidates {git-clean, gitlab, github} share LCP "git".
  let result = many(&["git-clean", "gitlab", "github"]).try_collapse_by_prefix("gi");
  assert_eq!(collapsed_single(&result), Some("git"));
}

#[test]
fn collapse_with_empty_typed_extends_to_full_lcp() {
  // User hasn't typed anything yet; any common prefix wins.
  let result = many(&["foobar", "foobaz"]).try_collapse_by_prefix("");
  assert_eq!(collapsed_single(&result), Some("fooba"));
}

// ─── No-collapse cases: LCP not longer than typed ─────────────────

#[test]
fn collapse_noop_when_lcp_equals_typed() {
  // User already typed the full LCP; nothing more to advance to.
  // (This is the post-first-Tab state where selector should open.)
  let result = many(&["git-clean", "gitlab", "github"]).try_collapse_by_prefix("git");
  assert!(matches!(result, CompResult::Many { .. }));
}

#[test]
fn collapse_noop_when_no_common_prefix() {
  // No shared prefix → no collapse possible.
  let result = many(&["alpha", "beta", "gamma"]).try_collapse_by_prefix("");
  assert!(matches!(result, CompResult::Many { .. }));
}

#[test]
fn collapse_noop_when_case_differs_at_position_0() {
  // Case-sensitive LCP: candidates differing in case at the first char
  // produce an empty common prefix and fall through to selector.
  // Verifies the "show selector for case-ambiguous candidates" property.
  let result = many(&["file", "fiLE", "File"]).try_collapse_by_prefix("fi");
  assert!(matches!(result, CompResult::Many { .. }));
}

// ─── Edge cases ────────────────────────────────────────────────────

#[test]
fn collapse_preserves_single_unchanged() {
  let single = CompResult::Exact {
    result: Candidate::from("only"),
  };
  let result = single.try_collapse_by_prefix("o");
  assert!(matches!(result, CompResult::Exact { .. }));
  assert_eq!(collapsed_single(&result), Some("only"));
}

#[test]
fn collapse_preserves_no_match_unchanged() {
  let result = CompResult::NoMatch.try_collapse_by_prefix("foo");
  assert!(matches!(result, CompResult::NoMatch));
}

#[test]
fn collapse_handles_multibyte_chars_at_lcp_boundary() {
  // Common prefix straddles a multi-byte char. Without proper byte-
  // boundary handling, slicing `first[..end]` would panic with
  // "byte index N is not a char boundary" or produce garbage.
  let result = many(&["café_a", "café_b"]).try_collapse_by_prefix("ca");
  // LCP is "café_" (5 chars, 6 bytes — é is 2 bytes).
  assert_eq!(collapsed_single(&result), Some("café_"));
}

#[test]
fn collapse_first_candidate_is_full_lcp_when_others_extend_it() {
  // first = "foo" (length 3), others = "foobar", "foobaz". The LCP
  // can't exceed len(first) = 3 because first is a prefix of itself.
  // typed = "" so we advance to "foo".
  let result = many(&["foo", "foobar", "foobaz"]).try_collapse_by_prefix("");
  assert_eq!(collapsed_single(&result), Some("foo"));
}

#[test]
fn collapse_lcp_shorter_than_typed_does_not_truncate() {
  // Guard against a hypothetical regression where typed is longer
  // than LCP — we should keep the Many state, not collapse to a
  // prefix shorter than what the user typed.
  let result = many(&["abXYZ", "abLMN"]).try_collapse_by_prefix("abc");
  // LCP is "ab" (2 bytes), typed is "abc" (3 bytes). LCP not > typed.
  assert!(matches!(result, CompResult::Many { .. }));
}

// ===================== complete_signals =====================

#[test]
fn complete_signals_int() {
  let results = complete_signals("INT");
  assert!(results.contains(&Candidate::from("INT")));
}

#[test]
fn complete_signals_empty() {
  let results = complete_signals("");
  assert!(!results.is_empty());
}

#[test]
fn complete_signals_no_match() {
  let results = complete_signals("ZZZZZZZ");
  assert!(results.is_empty());
}

// ===================== COMP_WORDBREAKS =====================

#[test]
fn wordbreak_equals_default() {
  let _g = TestGuard::new();
  let mut comp = SimpleCompleter::default();

  let line = "cmd --foo=bar".to_string();
  let cursor = line.len();
  let _ = comp.get_candidates(&line, cursor, super::CompSource::Shell);

  let eq_idx = line.find('=').unwrap();
  assert_eq!(
    comp.token_span.0,
    eq_idx + 1,
    "token_span.0 ({}) should be right after '=' ({})",
    comp.token_span.0,
    eq_idx
  );
}

#[test]
fn wordbreak_colon_when_set() {
  let _g = TestGuard::new();
  Shed::vars_mut(|v| {
    v.set_var(
      "COMP_WORDBREAKS",
      VarKind::Str("=:".into()),
      VarFlags::empty(),
    )
  })
  .unwrap();

  let mut comp = SimpleCompleter::default();
  let line = "scp host:foo".to_string();
  let cursor = line.len();
  let _ = comp.get_candidates(&line, cursor, super::CompSource::Shell);

  let colon_idx = line.find(':').unwrap();
  assert_eq!(
    comp.token_span.0,
    colon_idx + 1,
    "token_span.0 ({}) should be right after ':' ({})",
    comp.token_span.0,
    colon_idx
  );
}

#[test]
fn wordbreak_splits_on_subtoken_edge() {
  // Regression: a wordbreak char immediately followed by something that
  // *starts* a sub-token (e.g. `~/...` becomes a tilde-prefix sub-token)
  // used to block the split — `can_split_at` rejected the position because
  // the sub-token's range started right at it. The split is safe on a
  // sub-token edge; only positions strictly inside a sub-token should be
  // rejected. Without the fix, `scp host:~/foo` stays as one token and
  // remote-path completion can't tell where the host ends.
  let _g = TestGuard::new();
  Shed::vars_mut(|v| {
    v.set_var(
      "COMP_WORDBREAKS",
      VarKind::Str("=:".into()),
      VarFlags::empty(),
    )
  })
  .unwrap();

  let mut comp = SimpleCompleter::default();
  let line = "scp host:~/foo".to_string();
  let cursor = line.len();
  let _ = comp.get_candidates(&line, cursor, super::CompSource::Shell);

  let colon_idx = line.find(':').unwrap();
  assert_eq!(
    comp.token_span.0,
    colon_idx + 1,
    "token_span.0 ({}) should be right after ':' ({}) — \
     wordbreak split must fire even when the next char starts a sub-token",
    comp.token_span.0,
    colon_idx
  );
}

#[test]
fn wordbreak_rightmost_wins() {
  let _g = TestGuard::new();
  Shed::vars_mut(|v| {
    v.set_var(
      "COMP_WORDBREAKS",
      VarKind::Str("=:".into()),
      VarFlags::empty(),
    )
  })
  .unwrap();

  let mut comp = SimpleCompleter::default();
  let line = "cmd --opt=host:val".to_string();
  let cursor = line.len();
  let _ = comp.get_candidates(&line, cursor, super::CompSource::Shell);

  let colon_idx = line.rfind(':').unwrap();
  assert_eq!(
    comp.token_span.0,
    colon_idx + 1,
    "should break at rightmost wordbreak char"
  );
}

// ===================== get_candidates: CompStrat dispatch =====================
// The COMP_WORDBREAKS tests above exercise token_span calculation.
// These tests exercise the strategy-routing match arms — Command,
// Var, Tilde, Argument, Files (via redirect), Null, and the
// empty-line fast path.

/// Strip just the content list out of a `CompResult` for assertion.
fn contents(result: &CompResult) -> Vec<&str> {
  match result {
    CompResult::NoMatch => vec![],
    CompResult::CommonPrefix { result } | CompResult::Exact { result } => vec![result.content()],
    CompResult::Many { candidates } => candidates.iter().map(super::Candidate::content).collect(),
  }
}

#[test]
fn get_candidates_empty_line_routes_to_command_strategy() {
  // Empty line / cursor at 0 falls through to the
  // `Self::Command { prefix: "" }` default in CompStrat::resolve.
  let _g = TestGuard::new();
  let mut comp = SimpleCompleter::default();
  let result = comp
    .get_candidates("", 0, super::CompSource::Shell)
    .unwrap();
  // Almost certainly Many — even a minimal PATH has > 1 binary. But
  // accept Exact too in case some sandboxed env has exactly one.
  match result {
    CompResult::Many { .. } | CompResult::Exact { .. } | CompResult::CommonPrefix { .. } => {}
    CompResult::NoMatch => panic!("expected command candidates, got NoMatch"),
  }
}

#[test]
fn get_candidates_var_prefix_completes_with_shell_var() {
  let _g = TestGuard::new();
  // Use a name unlikely to be defined by the environment.
  Shed::vars_mut(|v| {
    v.set_var(
      "UNIQUE_COMP_TEST_VAR_XYZZY",
      VarKind::Str("hello".into()),
      VarFlags::empty(),
    )
    .unwrap();
  });
  let mut comp = SimpleCompleter::default();
  let line = "echo $UNIQUE_COMP_TEST".to_string();
  let cursor = line.len();
  let result = comp
    .get_candidates(&line, cursor, super::CompSource::Shell)
    .unwrap();
  let cs = contents(&result);
  assert!(
    cs.contains(&"UNIQUE_COMP_TEST_VAR_XYZZY"),
    "expected UNIQUE_COMP_TEST_VAR_XYZZY in candidates, got: {cs:?}"
  );
}

#[test]
fn get_candidates_var_prefix_with_no_matches_returns_nomatch() {
  let _g = TestGuard::new();
  let mut comp = SimpleCompleter::default();
  // A prefix that absolutely won't match any shell or env var.
  let line = "echo $ZZZZZZZ_NOT_A_REAL_VAR_PREFIX_QQQ".to_string();
  let cursor = line.len();
  let result = comp
    .get_candidates(&line, cursor, super::CompSource::Shell)
    .unwrap();
  assert!(matches!(result, CompResult::NoMatch));
}

#[test]
fn get_candidates_path_arg_lists_directory_entries() {
  let _g = TestGuard::new();
  let dir = tempfile::TempDir::new().unwrap();
  std::fs::write(dir.path().join("apple.txt"), "").unwrap();
  std::fs::write(dir.path().join("banana.txt"), "").unwrap();
  std::fs::write(dir.path().join("cherry.txt"), "").unwrap();

  let mut comp = SimpleCompleter::default();
  let line = format!("ls {}/", dir.path().display());
  let cursor = line.len();
  let result = comp
    .get_candidates(&line, cursor, super::CompSource::Shell)
    .unwrap();
  let cs = contents(&result);
  assert!(cs.iter().any(|c| c.contains("apple.txt")), "got: {cs:?}");
  assert!(cs.iter().any(|c| c.contains("banana.txt")), "got: {cs:?}");
  assert!(cs.iter().any(|c| c.contains("cherry.txt")), "got: {cs:?}");
}

#[test]
fn get_candidates_path_arg_with_prefix_filters() {
  let _g = TestGuard::new();
  let dir = tempfile::TempDir::new().unwrap();
  std::fs::write(dir.path().join("apple.txt"), "").unwrap();
  std::fs::write(dir.path().join("apricot.txt"), "").unwrap();
  std::fs::write(dir.path().join("banana.txt"), "").unwrap();

  let mut comp = SimpleCompleter::default();
  // Prefix "ap" should match apple + apricot but not banana.
  let line = format!("ls {}/ap", dir.path().display());
  let cursor = line.len();
  let result = comp
    .get_candidates(&line, cursor, super::CompSource::Shell)
    .unwrap();
  let cs = contents(&result);
  assert!(cs.iter().any(|c| c.contains("apple")), "got: {cs:?}");
  assert!(cs.iter().any(|c| c.contains("apricot")), "got: {cs:?}");
  assert!(!cs.iter().any(|c| c.contains("banana")), "got: {cs:?}");
}

#[test]
fn get_candidates_redirect_uses_files_strategy() {
  let _g = TestGuard::new();
  let dir = tempfile::TempDir::new().unwrap();
  std::fs::write(dir.path().join("output.log"), "").unwrap();

  let mut comp = SimpleCompleter::default();
  let line = format!("echo hi > {}/", dir.path().display());
  let cursor = line.len();
  let result = comp
    .get_candidates(&line, cursor, super::CompSource::Shell)
    .unwrap();
  let cs = contents(&result);
  assert!(cs.iter().any(|c| c.contains("output.log")), "got: {cs:?}");
}

#[test]
fn get_candidates_dirs_only_does_not_filter_argument_path() {
  // dirs_only is consulted only in the Files (redirect) strategy and
  // the Argument-NoSpec-NoMatch tail — NOT the default Argument path
  // that calls `complete_path`. This test pins that: even with
  // dirs_only=true, files surface through plain argument completion.
  let _g = TestGuard::new();
  let dir = tempfile::TempDir::new().unwrap();
  std::fs::write(dir.path().join("file.txt"), "").unwrap();
  std::fs::create_dir(dir.path().join("subdir")).unwrap();

  let mut comp = SimpleCompleter {
    dirs_only: true,
    ..Default::default()
  };

  let line = format!("ls {}/", dir.path().display());
  let cursor = line.len();
  let result = comp
    .get_candidates(&line, cursor, super::CompSource::Shell)
    .unwrap();
  let cs = contents(&result);
  assert!(cs.iter().any(|c| c.contains("file.txt")), "got: {cs:?}");
  assert!(cs.iter().any(|c| c.contains("subdir")), "got: {cs:?}");
}

#[test]
fn get_candidates_token_span_set_for_var_prefix() {
  // get_candidates assigns token_span based on where the leaf starts.
  // Verify it lands on the $ position (well, just after) for a var.
  let _g = TestGuard::new();
  let mut comp = SimpleCompleter::default();
  let line = "echo $PA".to_string();
  let dollar_idx = line.find('$').unwrap();
  let _ = comp
    .get_candidates(&line, line.len(), super::CompSource::Shell)
    .unwrap();
  // The span should cover the $ token; start should be at or just
  // after the $ depending on whether the leaf starts at $ or 'P'.
  assert!(
    comp.token_span.0 >= dollar_idx && comp.token_span.0 <= dollar_idx + 1,
    "expected token_span near $, got {:?}",
    comp.token_span
  );
}

#[test]
fn get_candidates_dedups_and_sorts_many_results() {
  // The post-processing step sorts by length-then-alpha and dedups.
  // Easiest path: create files in a tempdir with predictable sort
  // order.
  let _g = TestGuard::new();
  let dir = tempfile::TempDir::new().unwrap();
  std::fs::write(dir.path().join("a.txt"), "").unwrap();
  std::fs::write(dir.path().join("bbb.txt"), "").unwrap();
  std::fs::write(dir.path().join("c.txt"), "").unwrap();

  let mut comp = SimpleCompleter::default();
  let line = format!("ls {}/", dir.path().display());
  let cursor = line.len();
  let result = comp
    .get_candidates(&line, cursor, super::CompSource::Shell)
    .unwrap();
  if let CompResult::Many { candidates } = result {
    // Sort key is (len, content), so the two shorter names come
    // before the longer one regardless of alphabetical order.
    let a_idx = candidates
      .iter()
      .position(|c| c.content().contains("a.txt"));
    let bbb_idx = candidates
      .iter()
      .position(|c| c.content().contains("bbb.txt"));
    if let (Some(a), Some(bbb)) = (a_idx, bbb_idx) {
      assert!(
        a < bbb,
        "shorter candidate should sort first; got order a@{a}, bbb@{bbb}"
      );
    }
  } else {
    panic!("expected Many with 3 files");
  }
}

// ===================== SimpleCompleter cycling =====================

#[test]
fn cycle_wraps_forward() {
  let _g = TestGuard::new();
  let mut comp = SimpleCompleter {
    candidates: vec!["aaa".into(), "bbb".into(), "ccc".into()],
    selected_idx: 2,
    original_input: String::new(),
    token_span: (0, 0),
    active: true,
    dirs_only: false,
    add_space: false,
    cursor_pos: 0,
  };
  comp.cycle_completion(1);
  assert_eq!(comp.selected_idx, 0);
}

#[test]
fn cycle_wraps_backward() {
  let _g = TestGuard::new();
  let mut comp = SimpleCompleter {
    candidates: vec!["aaa".into(), "bbb".into(), "ccc".into()],
    selected_idx: 0,
    original_input: String::new(),
    token_span: (0, 0),
    active: true,
    dirs_only: false,
    add_space: false,
    cursor_pos: 0,
  };
  comp.cycle_completion(-1);
  assert_eq!(comp.selected_idx, 2);
}

// ===================== Completion escaping =====================

#[test]
fn escape_str_special_chars() {
  use crate::expand::escape_str;
  let escaped = escape_str("hello world", false);
  assert_eq!(escaped, "hello\\ world");
}

#[test]
fn escape_str_multiple_specials() {
  use crate::expand::escape_str;
  let escaped = escape_str("a&b|c", false);
  assert_eq!(escaped, "a\\&b\\|c");
}

#[test]
fn escape_str_no_specials() {
  use crate::expand::escape_str;
  let escaped = escape_str("hello", false);
  assert_eq!(escaped, "hello");
}

#[test]
fn escape_str_all_shell_metacharacters() {
  use crate::expand::escape_str;
  for ch in [
    '\'', '"', '\\', '|', '&', ';', '(', ')', '<', '>', '$', '*', '!', '`', '{', '?', '[', '#',
    ' ', '\t', '\n',
  ] {
    let input = format!("a{ch}b");
    let escaped = escape_str(&input, false);
    let expected = format!("a\\{ch}b");
    assert_eq!(escaped, expected, "failed to escape {ch:?}");
  }
}

#[test]
fn escape_str_kitchen_sink() {
  use crate::expand::escape_str;
  let input = "f$le (with) 'spaces' & {braces} | pipes; #hash ~tilde `backtick` !bang";
  let escaped = escape_str(input, false);
  assert_eq!(
    escaped,
    "f\\$le\\ \\(with\\)\\ \\'spaces\\'\\ \\&\\ \\{braces}\\ \\|\\ pipes\\;\\ \\#hash\\ ~tilde\\ \\`backtick\\`\\ \\!bang"
  );
}

// `get_completed_line` now wholesale-replaces `token_span` with the
// candidate. Escaping happens upstream in `complete_path` (the candidate
// arrives splice-ready), so these tests verify the splice mechanics with
// already-escaped candidates.

#[test]
fn completed_line_only_escapes_new_text() {
  let _g = TestGuard::new();
  // Candidate arrives pre-escaped from upstream: user-typed "hel" stays
  // verbatim, the matched suffix "lo world" was escaped to "lo\ world".
  let comp = SimpleCompleter {
    candidates: vec!["hello\\ world".into()],
    selected_idx: 0,
    original_input: "echo hel".into(),
    token_span: (5, 8),
    active: true,
    dirs_only: false,
    add_space: false,
    cursor_pos: 0,
  };
  let result = comp.get_completed_line();
  assert_eq!(result, "echo hello\\ world");
}

#[test]
fn completed_line_no_new_text() {
  let _g = TestGuard::new();
  let comp = SimpleCompleter {
    candidates: vec!["hello".into()],
    selected_idx: 0,
    original_input: "echo hello".into(),
    token_span: (5, 10),
    active: true,
    dirs_only: false,
    add_space: false,
    cursor_pos: 0,
  };
  let result = comp.get_completed_line();
  assert_eq!(result, "echo hello");
}

#[test]
fn completed_line_appends_suffix_with_escape() {
  let _g = TestGuard::new();
  // Wholesale replacement of `token_span` with the (pre-escaped) candidate.
  let comp = SimpleCompleter {
    candidates: vec!["hello\\ world".into()],
    selected_idx: 0,
    original_input: "echo hel".into(),
    token_span: (5, 8),
    active: true,
    dirs_only: false,
    add_space: false,
    cursor_pos: 0,
  };
  let result = comp.get_completed_line();
  assert_eq!(result, "echo hello\\ world");
}

#[test]
fn completed_line_suffix_only_escapes_new_part() {
  let _g = TestGuard::new();
  // Candidate arrives with the user's "hello" preserved verbatim and the
  // appended " world&done" already escaped to "\ world\&done".
  let comp = SimpleCompleter {
    candidates: vec!["hello\\ world\\&done".into()],
    selected_idx: 0,
    original_input: "echo hello".into(),
    token_span: (5, 10),
    active: true,
    dirs_only: false,
    add_space: false,
    cursor_pos: 0,
  };
  let result = comp.get_completed_line();
  assert_eq!(result, "echo hello\\ world\\&done");
}

#[test]
fn tab_escapes_special_in_filename() {
  let tmp = std::env::temp_dir().join("shed_test_tab_esc");
  let _ = std::fs::create_dir_all(&tmp);
  std::fs::write(tmp.join("hello world.txt"), "").unwrap();

  let (mut vi, _g) = test_vi("");
  std::env::set_current_dir(&tmp).unwrap();

  Shed::term_mut(|t| t.feed_bytes(b"echo hello\t"));
  let keys = Shed::term_mut(Terminal::drain_keys);
  let _ = vi.process_input(keys);

  let line = vi.core.editor.to_string();
  assert!(
    line.contains("hello\\ world.txt"),
    "expected escaped space in completion: {line:?}"
  );

  std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn tab_does_not_escape_user_text() {
  let tmp = std::env::temp_dir().join("shed_test_tab_noesc");
  let _ = std::fs::create_dir_all(&tmp);
  std::fs::write(tmp.join("my file.txt"), "").unwrap();

  let (mut vi, _g) = test_vi("");
  std::env::set_current_dir(&tmp).unwrap();

  // User types "echo my\ " with the space already escaped
  Shed::term_mut(|t| t.feed_bytes(b"echo my\\ \t"));
  let keys = Shed::term_mut(Terminal::drain_keys);
  let _ = vi.process_input(keys);

  let line = vi.core.editor.to_string();
  // The user's "my\ " should be preserved, not double-escaped to "my\\\ "
  assert!(
    !line.contains("my\\\\ "),
    "user text should not be double-escaped: {line:?}"
  );
  assert!(
    line.contains("my\\ file.txt"),
    "expected completion with preserved user escape: {line:?}"
  );

  std::fs::remove_dir_all(&tmp).ok();
}

// ===================== CompStrat::resolve =====================

/// Run the dispatcher against a literal source string and cursor position.
/// Returns (strategy, replacement-span as a (start, end) tuple).
fn dispatch(input: &str, cursor: usize) -> (CompStrat, (usize, usize)) {
  let tks = get_context_tokens(input);
  let (strat, span, _cursor_pos) = CompStrat::resolve(&tks, cursor);
  (strat, (span.range().start, span.range().end))
}

/// Helper: extract the prefix from a Var/Tilde/Command/Argument/Files/Dirs strat.
fn prefix_of(strat: &CompStrat) -> &str {
  match strat {
    CompStrat::ExCommand { prefix }
    | CompStrat::Var { prefix }
    | CompStrat::Tilde { prefix }
    | CompStrat::Builtin { prefix }
    | CompStrat::Command { prefix } => prefix,
    CompStrat::Argument { path } | CompStrat::Files { path } => path,
    CompStrat::Separator | CompStrat::Null => "",
  }
}

#[test]
fn dispatch_bare_var_sub() {
  let input = "echo $FL";
  let (strat, span) = dispatch(input, input.len());
  assert!(matches!(strat, CompStrat::Var { .. }), "got {strat:?}");
  assert_eq!(prefix_of(&strat), "FL");
  assert_eq!(&input[span.0..span.1], "FL");
}

#[test]
fn dispatch_braced_var_sub_unclosed() {
  let input = "echo ${FL";
  let (strat, span) = dispatch(input, input.len());
  assert!(matches!(strat, CompStrat::Var { .. }), "got {strat:?}");
  assert_eq!(prefix_of(&strat), "FL");
  assert_eq!(&input[span.0..span.1], "FL");
}

#[test]
fn dispatch_braced_var_sub_closed() {
  let input = "echo ${FL}";
  let cursor = input.find("FL").unwrap() + 2; // end of FL, just before `}`
  let (strat, span) = dispatch(input, cursor);
  assert!(matches!(strat, CompStrat::Var { .. }), "got {strat:?}");
  assert_eq!(prefix_of(&strat), "FL");
  assert_eq!(&input[span.0..span.1], "FL");
}

#[test]
fn dispatch_braced_var_with_substitution_op() {
  let input = "echo ${FL/bar";
  let cursor = input.find("FL").unwrap() + 2; // end of FL
  let (strat, span) = dispatch(input, cursor);
  assert!(matches!(strat, CompStrat::Var { .. }), "got {strat:?}");
  assert_eq!(&input[span.0..span.1], "FL");
}

#[test]
fn dispatch_var_sub_inside_path() {
  let input = "echo /foo/$FL/bar";
  let cursor = input.find("$FL").unwrap() + 3; // end of $FL
  let (strat, span) = dispatch(input, cursor);
  assert!(matches!(strat, CompStrat::Var { .. }), "got {strat:?}");
  assert_eq!(prefix_of(&strat), "FL");
  assert_eq!(&input[span.0..span.1], "FL");
}

#[test]
fn dispatch_var_sub_inside_double_quoted_string() {
  let input = "echo \"foo $FL";
  let (strat, span) = dispatch(input, input.len());
  assert!(matches!(strat, CompStrat::Var { .. }), "got {strat:?}");
  assert_eq!(prefix_of(&strat), "FL");
  assert_eq!(&input[span.0..span.1], "FL");
}

#[test]
fn dispatch_braced_var_inside_double_quoted_string() {
  let input = "echo \"foo ${FL}";
  let cursor = input.find("FL").unwrap() + 2; // end of FL
  let (strat, span) = dispatch(input, cursor);
  assert!(matches!(strat, CompStrat::Var { .. }), "got {strat:?}");
  assert_eq!(&input[span.0..span.1], "FL");
}

#[test]
fn dispatch_empty_input_is_command() {
  let (strat, _) = dispatch("", 0);
  assert!(matches!(strat, CompStrat::Command { .. }), "got {strat:?}");
  assert_eq!(prefix_of(&strat), "");
}

#[test]
fn dispatch_after_separator_is_command() {
  let input = "ls foo | ";
  let (strat, _) = dispatch(input, input.len());
  assert!(matches!(strat, CompStrat::Command { .. }), "got {strat:?}");
  assert_eq!(prefix_of(&strat), "");
}

#[test]
fn dispatch_in_gap_after_command_uses_zero_width_span() {
  let input = "echo ";
  let (strat, span) = dispatch(input, input.len());
  assert!(matches!(strat, CompStrat::Argument { .. }), "got {strat:?}");
  assert_eq!(
    span,
    (input.len(), input.len()),
    "expected zero-width span at cursor, got {span:?}"
  );
}

#[test]
fn dispatch_partial_command_name() {
  let input = "ls";
  let (strat, span) = dispatch(input, input.len());
  assert!(matches!(strat, CompStrat::Command { .. }), "got {strat:?}");
  assert_eq!(prefix_of(&strat), "ls");
  assert_eq!(&input[span.0..span.1], "ls");
}

#[test]
fn dispatch_preserves_braces_under_string_recursion() {
  let input = "echo \"foo ${FL}/bar\"";
  let cursor = input.find("FL").unwrap() + 2;
  let (strat, span) = dispatch(input, cursor);
  assert!(matches!(strat, CompStrat::Var { .. }), "got {strat:?}");
  assert_eq!(&input[span.0..span.1], "FL");
}

// ===================== Integration tests (pty) =====================

#[test]
fn tab_completes_filename() {
  let tmp = std::env::temp_dir().join("shed_test_tab_fn");
  let _ = std::fs::create_dir_all(&tmp);
  std::fs::write(tmp.join("unique_shed_test_file.txt"), "").unwrap();

  let (mut vi, _g) = test_vi("");
  std::env::set_current_dir(&tmp).unwrap();

  // Type "echo unique_shed_test" then press Tab
  Shed::term_mut(|t| t.feed_bytes(b"echo unique_shed_test\t"));
  let keys = Shed::term_mut(Terminal::drain_keys);
  let _ = vi.process_input(keys);

  let line = vi.core.editor.to_string();
  assert!(
    line.contains("unique_shed_test_file.txt"),
    "expected completion in line: {line:?}"
  );

  std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn tab_completes_directory_with_slash() {
  let tmp = std::env::temp_dir().join("shed_test_tab_dir");
  let _ = std::fs::create_dir_all(tmp.join("mysubdir"));

  let (mut vi, _g) = test_vi("");
  std::env::set_current_dir(&tmp).unwrap();

  Shed::term_mut(|t| t.feed_bytes(b"cd mysub\t"));
  let keys = Shed::term_mut(Terminal::drain_keys);
  let _ = vi.process_input(keys);

  let line = vi.core.editor.to_string();
  assert!(
    line.contains("mysubdir/"),
    "expected dir completion with trailing slash: {line:?}"
  );

  std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn tab_after_equals() {
  let tmp = std::env::temp_dir().join("shed_test_tab_eq");
  let _ = std::fs::create_dir_all(&tmp);
  std::fs::write(tmp.join("eqfile.txt"), "").unwrap();

  let (mut vi, _g) = test_vi("");
  std::env::set_current_dir(&tmp).unwrap();

  Shed::term_mut(|t| t.feed_bytes(b"cmd --opt=eqf\t"));
  let keys = Shed::term_mut(Terminal::drain_keys);
  let _ = vi.process_input(keys);

  let line = vi.core.editor.to_string();
  assert!(
    line.contains("--opt=eqfile.txt"),
    "expected completion after '=': {line:?}"
  );

  std::fs::remove_dir_all(&tmp).ok();
}

// ===================== get_branch / Null walk-up =====================

#[test]
fn dispatch_walks_up_from_escape_leaf() {
  // Cursor inside an Escape token (`\ `) should not produce Null —
  // the dispatcher should walk up to the parent Argument.
  let input = "echo my\\ ";
  let (strat, _span) = dispatch(input, input.len());
  assert!(
    !matches!(strat, CompStrat::Null),
    "Escape leaf should walk up to parent, got Null"
  );
}

#[test]
fn dispatch_branch_chain_deep_nesting() {
  // Cursor inside the deeply nested `~/file` path through subshell→arg→
  // varsub→paramindex→cmdsub→arg→varsub→paramindex→cmdsub→argfile.
  // Just verify the branch chain resolves without panic and reaches a
  // non-Null strat.
  let input = "(echo foo ${bar[$(echo ${foo[$(cat ~/fil)]}) + 1]})";
  let cursor = input.find("~/fil").unwrap() + 3;
  let (strat, _span) = dispatch(input, cursor);
  assert!(
    !matches!(strat, CompStrat::Null),
    "deeply nested cursor should resolve, got {strat:?}"
  );
}

#[test]
fn dispatch_argument_carries_full_path() {
  // CompStrat::Argument carries `path` (full token), not `prefix`. With
  // cursor in the middle, the strat must contain everything (so postfix
  // is preserved when completing).
  let input = "cd /tmp/foo/bar/baz";
  let cursor = input.find("foo").unwrap() + 2; // after 'fo', mid-token
  let (strat, _span) = dispatch(input, cursor);
  let p = prefix_of(&strat);
  assert!(
    p.contains("/bar/baz"),
    "Argument strat should contain full token incl. postfix; got {p:?}"
  );
}

// ===================== comp function arg quoting =====================
//
// exec_comp_func builds the function-call input as
//   `{fn_name} {shell_quote(cmd)} {shell_quote(cword)} {shell_quote(pword)}`
// The comp function's $1/$2/$3 must receive the original strings even when
// they contain spaces, quotes, $, ;, etc. These tests exercise the same
// formatting path that exec_comp_func uses.

use crate::expand::shell_quote;
use crate::tests::testutil::test_input;

fn run_comp_func_with_args(cmd: &str, cword: &str, pword: &str) -> (String, String, String) {
  test_input("_capture() { CAP1=\"$1\"; CAP2=\"$2\"; CAP3=\"$3\"; }").unwrap();
  let input = format!(
    "_capture {} {} {}",
    shell_quote(cmd),
    shell_quote(cword),
    shell_quote(pword),
  );
  test_input(input).unwrap();
  (
    var!("CAP1").to_string(),
    var!("CAP2").to_string(),
    var!("CAP3").to_string(),
  )
}

#[test]
fn comp_args_plain_strings() {
  let _g = TestGuard::new();
  let (a, b, c) = run_comp_func_with_args("git", "checkout", "master");
  assert_eq!(a, "git");
  assert_eq!(b, "checkout");
  assert_eq!(c, "master");
}

#[test]
fn comp_args_with_spaces() {
  let _g = TestGuard::new();
  let (a, b, c) = run_comp_func_with_args("my cmd", "foo bar", "baz qux");
  assert_eq!(a, "my cmd");
  assert_eq!(b, "foo bar");
  assert_eq!(c, "baz qux");
}

#[test]
fn comp_args_with_dollar_sign() {
  let _g = TestGuard::new();
  let (a, b, _) = run_comp_func_with_args("$VAR", "$cmd", "");
  assert_eq!(a, "$VAR");
  assert_eq!(b, "$cmd");
}

#[test]
fn comp_args_with_semicolon_and_pipe() {
  let _g = TestGuard::new();
  let (a, b, _) = run_comp_func_with_args("a;b", "x|y", "");
  assert_eq!(a, "a;b");
  assert_eq!(b, "x|y");
}

#[test]
fn comp_args_with_single_quote() {
  let _g = TestGuard::new();
  let (a, _b, _c) = run_comp_func_with_args("it's", "", "");
  assert_eq!(a, "it's");
}

// ===================== FuzzySelector::handle_key =====================

mod fuzzy_selector_handle_key {
  use super::*;
  use crate::{key, keys::ModKeys as M};

  fn sel_with(items: &[&str]) -> FuzzySelector {
    let mut sel = FuzzySelector::new("test");
    let cands: Vec<Candidate> = items
      .iter()
      .map(|s| Candidate::from(s.to_string()))
      .collect();
    sel.activate(cands);
    sel
  }

  // ─── Enter ────────────────────────────────────────────────────────

  #[test]
  fn enter_with_empty_filtered_dismisses() {
    let _g = TestGuard::new();
    let mut sel = FuzzySelector::new("test");
    let resp = sel.handle_key(key!(Enter)).unwrap();
    assert!(matches!(resp, SelectorResponse::Dismiss));
  }

  #[test]
  fn enter_accepts_candidate_at_cursor_zero() {
    let _g = TestGuard::new();
    let mut sel = sel_with(&["alpha", "beta", "gamma"]);
    let expected = sel.filtered()[0].candidate.clone();
    let resp = sel.handle_key(key!(Enter)).unwrap();
    match resp {
      SelectorResponse::Accept(c) => assert_eq!(c, expected),
      _ => panic!("expected Accept"),
    }
  }

  #[test]
  fn enter_after_navigation_accepts_correct_candidate() {
    let _g = TestGuard::new();
    let mut sel = sel_with(&["alpha", "beta", "gamma"]);
    sel.handle_key(key!(Down)).unwrap();
    sel.handle_key(key!(Down)).unwrap();
    let expected = sel.filtered()[2].candidate.clone();
    let resp = sel.handle_key(key!(Enter)).unwrap();
    match resp {
      SelectorResponse::Accept(c) => assert_eq!(c, expected),
      _ => panic!("expected Accept"),
    }
  }

  // ─── Dismiss keys ─────────────────────────────────────────────────

  #[test]
  fn esc_dismisses_and_clears_filtered() {
    let _g = TestGuard::new();
    let mut sel = sel_with(&["a", "b", "c"]);
    assert_eq!(sel.filtered().len(), 3);
    let resp = sel.handle_key(key!(Esc)).unwrap();
    assert!(matches!(resp, SelectorResponse::Dismiss));
    assert!(sel.filtered().is_empty());
  }

  #[test]
  fn ctrl_d_dismisses_and_clears_filtered() {
    let _g = TestGuard::new();
    let mut sel = sel_with(&["a", "b", "c"]);
    let resp = sel.handle_key(key!(Ctrl + 'd')).unwrap();
    assert!(matches!(resp, SelectorResponse::Dismiss));
    assert!(sel.filtered().is_empty());
  }

  // ─── Cursor movement: forward ─────────────────────────────────────

  #[test]
  fn tab_advances_cursor() {
    let _g = TestGuard::new();
    let mut sel = sel_with(&["a", "b", "c"]);
    let target = sel.filtered()[1].candidate.clone();
    sel.handle_key(key!(Tab)).unwrap();
    assert_eq!(sel.selected_candidate().unwrap(), target);
  }

  #[test]
  fn down_advances_cursor() {
    let _g = TestGuard::new();
    let mut sel = sel_with(&["a", "b", "c"]);
    let target = sel.filtered()[1].candidate.clone();
    sel.handle_key(key!(Down)).unwrap();
    assert_eq!(sel.selected_candidate().unwrap(), target);
  }

  #[test]
  fn tab_wraps_at_end() {
    let _g = TestGuard::new();
    let mut sel = sel_with(&["a", "b", "c"]);
    let first = sel.filtered()[0].candidate.clone();
    for _ in 0..3 {
      sel.handle_key(key!(Tab)).unwrap();
    }
    assert_eq!(sel.selected_candidate().unwrap(), first);
  }

  #[test]
  fn down_wraps_at_end() {
    let _g = TestGuard::new();
    let mut sel = sel_with(&["a", "b"]);
    let first = sel.filtered()[0].candidate.clone();
    sel.handle_key(key!(Down)).unwrap();
    sel.handle_key(key!(Down)).unwrap();
    assert_eq!(sel.selected_candidate().unwrap(), first);
  }

  #[test]
  fn scroll_down_does_not_wrap() {
    let _g = TestGuard::new();
    let mut sel = sel_with(&["a", "b"]);
    let last = sel.filtered()[1].candidate.clone();
    // ScrollDown 3x: 0→1→1→1 (saturates at max-1).
    sel.handle_key(K(C::ScrollDown, M::NONE)).unwrap();
    sel.handle_key(K(C::ScrollDown, M::NONE)).unwrap();
    sel.handle_key(K(C::ScrollDown, M::NONE)).unwrap();
    assert_eq!(sel.selected_candidate().unwrap(), last);
  }

  // ─── Cursor movement: backward ────────────────────────────────────

  #[test]
  fn shift_tab_retreats_cursor() {
    let _g = TestGuard::new();
    let mut sel = sel_with(&["a", "b", "c"]);
    let first = sel.filtered()[0].candidate.clone();
    sel.handle_key(key!(Down)).unwrap();
    sel.handle_key(key!(Shift + Tab)).unwrap();
    assert_eq!(sel.selected_candidate().unwrap(), first);
  }

  #[test]
  fn shift_tab_clamps_at_top() {
    let _g = TestGuard::new();
    let mut sel = sel_with(&["a", "b", "c"]);
    let first = sel.filtered()[0].candidate.clone();
    // At the top, Up/Shift+Tab clamps rather than wrapping to the far end.
    sel.handle_key(key!(Shift + Tab)).unwrap();
    assert_eq!(sel.selected_candidate().unwrap(), first);
  }

  #[test]
  fn up_clamps_at_top() {
    let _g = TestGuard::new();
    let mut sel = sel_with(&["a", "b"]);
    let first = sel.filtered()[0].candidate.clone();
    sel.handle_key(key!(Up)).unwrap();
    assert_eq!(sel.selected_candidate().unwrap(), first);
  }

  #[test]
  fn scroll_up_does_not_wrap() {
    let _g = TestGuard::new();
    let mut sel = sel_with(&["a", "b"]);
    let first = sel.filtered()[0].candidate.clone();
    // ScrollUp at cursor=0 should stay at 0 (saturating).
    sel.handle_key(K(C::ScrollUp, M::NONE)).unwrap();
    sel.handle_key(K(C::ScrollUp, M::NONE)).unwrap();
    assert_eq!(sel.selected_candidate().unwrap(), first);
  }

  // ─── Response types ───────────────────────────────────────────────

  #[test]
  fn all_movement_keys_return_preview() {
    let _g = TestGuard::new();
    let mut sel = sel_with(&["a", "b", "c"]);
    // With a non-empty list, every move changes the selection, so the response
    // carries the newly highlighted candidate for the caller to preview.
    for key in [
      key!(Down),
      key!(Up),
      key!(Tab),
      key!(Shift + Tab),
      K(C::ScrollDown, M::NONE),
      K(C::ScrollUp, M::NONE),
    ] {
      let resp = sel.handle_key(key).unwrap();
      assert!(matches!(resp, SelectorResponse::Preview(_)));
    }
  }

  // ─── Query editing ────────────────────────────────────────────────

  #[test]
  fn typing_char_filters_candidates() {
    let _g = TestGuard::new();
    let mut sel = sel_with(&["alpha", "beta", "gamma"]);
    assert_eq!(sel.filtered().len(), 3);
    let resp = sel.handle_key(K(C::Char('g'), M::NONE)).unwrap();
    // Filtering leaves a match, so the response previews the new top result.
    assert!(matches!(resp, SelectorResponse::Preview(_)));
    // 'g' only fuzzy-matches "gamma".
    assert_eq!(sel.filtered().len(), 1);
    assert_eq!(sel.filtered()[0].candidate.content(), "gamma");
  }

  #[test]
  fn typing_no_match_empties_filtered() {
    let _g = TestGuard::new();
    let mut sel = sel_with(&["alpha", "beta"]);
    sel.handle_key(K(C::Char('z'), M::NONE)).unwrap();
    assert!(sel.filtered().is_empty());
  }

  #[test]
  fn typing_then_enter_accepts_filtered_match() {
    let _g = TestGuard::new();
    let mut sel = sel_with(&["alpha", "beta", "gamma"]);
    sel.handle_key(K(C::Char('g'), M::NONE)).unwrap();
    let resp = sel.handle_key(key!(Enter)).unwrap();
    match resp {
      SelectorResponse::Accept(c) => assert_eq!(c.content(), "gamma"),
      _ => panic!("expected Accept"),
    }
  }

  #[test]
  fn typing_then_no_match_then_enter_dismisses() {
    let _g = TestGuard::new();
    let mut sel = sel_with(&["alpha", "beta"]);
    sel.handle_key(K(C::Char('z'), M::NONE)).unwrap();
    let resp = sel.handle_key(key!(Enter)).unwrap();
    assert!(matches!(resp, SelectorResponse::Dismiss));
  }

  #[test]
  fn ctrl_c_clears_query_restores_full_list() {
    let _g = TestGuard::new();
    let mut sel = sel_with(&["alpha", "beta", "gamma"]);
    sel.handle_key(K(C::Char('g'), M::NONE)).unwrap();
    assert_eq!(sel.filtered().len(), 1);
    let resp = sel.handle_key(key!(Ctrl + 'c')).unwrap();
    // Clearing the query restores the full list, previewing its top entry.
    assert!(matches!(resp, SelectorResponse::Preview(_)));
    assert_eq!(sel.filtered().len(), 3);
  }

  // ─── Empty filtered: movement is a no-op ─────────────────────────

  #[test]
  fn navigation_on_empty_selector_does_not_panic() {
    let _g = TestGuard::new();
    let mut sel = FuzzySelector::new("test");
    for key in [
      key!(Down),
      key!(Up),
      key!(Tab),
      key!(Shift + Tab),
      K(C::ScrollDown, M::NONE),
      K(C::ScrollUp, M::NONE),
    ] {
      let resp = sel.handle_key(key).unwrap();
      assert!(matches!(resp, SelectorResponse::Consumed));
    }
    assert!(sel.selected_candidate().is_none());
  }

  // ─── Mouse handling ──────────────────────────────────────────────

  #[test]
  fn mouse_pos_returns_consumed() {
    let _g = TestGuard::new();
    let mut sel = sel_with(&["a", "b", "c"]);
    let resp = sel.handle_key(K(C::MousePos(0, 0), M::NONE)).unwrap();
    assert!(matches!(resp, SelectorResponse::Consumed));
  }

  #[test]
  fn left_click_out_of_range_returns_consumed_no_change() {
    let _g = TestGuard::new();
    let mut sel = sel_with(&["a", "b", "c"]);
    let before = sel.selected_candidate().unwrap();
    // row_map is empty without draw(); click row is out-of-range → no-op.
    let resp = sel.handle_key(K(C::LeftClick(99, 0), M::NONE)).unwrap();
    assert!(matches!(resp, SelectorResponse::Consumed));
    assert_eq!(sel.selected_candidate().unwrap(), before);
  }
}

// ===================== complete_jobs =====================

mod complete_jobs_tests {
  use super::*;
  use crate::state::jobs::{ChildProc, JobBldr, JobID};
  use nix::sys::wait::WaitStatus;
  use nix::unistd::Pid;

  fn drain_jobs() {
    Shed::jobs_mut(|j| {
      let ids: Vec<JobID> = j
        .jobs()
        .iter()
        .flatten()
        .filter_map(|job| job.tabid().map(JobID::TableID))
        .collect();
      for id in ids {
        j.remove_job(id);
      }
    });
  }

  fn insert_named_job(pid: i32, cmd: &str) {
    let pid = Pid::from_raw(pid);
    let mut child = ChildProc::new(pid, Some(cmd), Some(pid), None);
    child.set_stat(WaitStatus::StillAlive);
    let mut bldr = JobBldr::new();
    bldr.push_child(child);
    bldr.set_pgid(pid);
    let job = bldr.build();
    Shed::jobs_mut(|j| j.insert_job(job, true));
  }

  #[test]
  fn complete_jobs_empty_table_returns_empty() {
    let _g = TestGuard::new();
    drain_jobs();
    assert!(complete_jobs("").is_empty());
    assert!(complete_jobs("%").is_empty());
  }

  #[test]
  fn complete_jobs_no_percent_returns_pgid_candidates() {
    let _g = TestGuard::new();
    drain_jobs();
    insert_named_job(70001, "uniq_test_cmd_a");
    let out = complete_jobs("");
    assert!(!out.is_empty(), "got: {out:?}");
    // Bare-form candidates are pgid strings; "70001" should appear.
    assert!(out.iter().any(|c| c.content() == "70001"), "got: {out:?}");
  }

  #[test]
  fn complete_jobs_with_percent_prefix_returns_named_candidates() {
    let _g = TestGuard::new();
    drain_jobs();
    insert_named_job(70002, "uniq_named_job_xyz");
    let out = complete_jobs("%");
    // %-prefix means we want jobs *by name*. The candidate's
    // content should be prefixed with `%`.
    assert!(
      out.iter().any(|c| c.content().starts_with('%')),
      "got: {out:?}"
    );
  }
}

// ===================== complete_users =====================

mod complete_users_tests {
  use super::*;

  #[test]
  fn complete_users_finds_root_on_unix() {
    // /etc/passwd reliably contains `root` on any Linux/CI machine.
    // If /etc/passwd is missing entirely, the function returns
    // empty — skip the assertion in that case.
    let _g = TestGuard::new();
    if !std::path::Path::new("/etc/passwd").exists() {
      return;
    }
    let out = complete_users("ro");
    assert!(
      out.iter().any(|c| c.content() == "root"),
      "expected 'root' in candidates, got: {out:?}"
    );
  }

  #[test]
  fn complete_users_filters_by_prefix() {
    let _g = TestGuard::new();
    if !std::path::Path::new("/etc/passwd").exists() {
      return;
    }
    // A nonsense prefix should match nobody.
    let out = complete_users("zzz_no_such_user_prefix_qqq");
    assert!(out.is_empty(), "got: {out:?}");
  }
}

// ===================== complete_builtins =====================

mod complete_builtins_tests {
  use super::*;

  #[test]
  fn complete_builtins_known_prefix_matches() {
    let _g = TestGuard::new();
    let out = complete_builtins("ec"); // echo
    assert!(
      out.iter().any(|c| c.content() == "echo"),
      "expected 'echo' in candidates, got: {out:?}"
    );
  }

  #[test]
  fn complete_builtins_empty_prefix_returns_all() {
    let _g = TestGuard::new();
    let all = complete_builtins("");
    assert_eq!(all.len(), BUILTIN_NAMES.len());
  }

  #[test]
  fn complete_builtins_unknown_prefix_returns_empty() {
    let _g = TestGuard::new();
    assert!(complete_builtins("zzz_no_such_builtin_qqq").is_empty());
  }
}

// ===================== complete_commands './' prefix =====================

mod complete_commands_dotslash_tests {
  use super::*;

  #[test]
  fn dotslash_prefix_routes_through_file_branch() {
    // The `./` prefix takes a separate branch in complete_commands
    // that filters cached_utils to UtilKind::File and re-prepends
    // "./" to the resulting candidates. We can't deterministically
    // populate the file cache from a unit test, so we just verify
    // the function returns successfully on a `./` prefix and that
    // any results that come back start with `./`.
    let _g = TestGuard::new();
    let out = complete_commands("./", 2);
    for c in &out {
      assert!(c.content.starts_with("./"), "got: {c:?}");
    }
  }
}

// ===================== complete_path ignore_case branch =====================

mod complete_path_ignore_case_tests {
  use super::*;

  #[test]
  fn ignore_case_flag_makes_match_case_insensitive() {
    // Build a tempdir with a Mixed-Case filename, then complete with
    // a lowercase prefix. Without ignore_case → no match; with it →
    // match. We just toggle the flag and confirm the on-case path
    // actually returns something.
    let _g = TestGuard::new();
    let dir = tempfile::TempDir::new().unwrap();
    let file_path = dir.path().join("MixedCaseFile.txt");
    std::fs::write(&file_path, "x").unwrap();

    let prev_cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(dir.path()).unwrap();

    // Off: case-sensitive — "mixed" shouldn't match "MixedCaseFile".
    crate::shopt_mut!(prompt.completion_ignore_case = false);
    let strict = complete_path("mixed", 5);

    // On: case-insensitive — "mixed" should match "MixedCaseFile".
    crate::shopt_mut!(prompt.completion_ignore_case = true);
    let relaxed = complete_path("mixed", 5);

    std::env::set_current_dir(prev_cwd).unwrap();
    crate::shopt_mut!(prompt.completion_ignore_case = false);

    assert!(
      strict
        .iter()
        .all(|c| !c.content.ends_with("MixedCaseFile.txt")),
      "case-sensitive shouldn't match, got: {strict:?}"
    );
    assert!(
      relaxed
        .iter()
        .any(|c| c.content.ends_with("MixedCaseFile.txt")),
      "ignore-case should match, got: {relaxed:?}"
    );
  }
}

// ===================== BashCompSpec builders =====================

mod bash_comp_spec_tests {
  use super::*;

  #[test]
  fn every_builder_method_sets_its_field() {
    // Cover all the with_*/enable-style methods on BashCompSpec in
    // one shot. The methods are pure setters, so we just confirm
    // each one flips the corresponding field.
    let spec = BashCompSpec::new()
      .with_func("complete_foo".into())
      .with_wordlist(vec!["a".into(), "b".into()])
      .with_source("complete -F complete_foo cmd".into())
      .files(true)
      .dirs(true)
      .commands(true)
      .builtins(true)
      .users(true)
      .vars(true)
      .signals(true)
      .jobs(true)
      .aliases(true);

    assert_eq!(spec.function, Some("complete_foo".to_string()));
    assert_eq!(spec.wordlist, Some(vec!["a".into(), "b".into()]));
    assert_eq!(spec.source, "complete -F complete_foo cmd");
    assert!(spec.targets.contains(CompFlags::FILES));
    assert!(spec.targets.contains(CompFlags::DIRS));
    assert!(spec.targets.contains(CompFlags::CMDS));
    assert!(spec.targets.contains(CompFlags::BUILTINS));
    assert!(spec.targets.contains(CompFlags::USERS));
    assert!(spec.targets.contains(CompFlags::VARS));
    assert!(spec.targets.contains(CompFlags::SIGNALS));
    assert!(spec.targets.contains(CompFlags::JOBS));
    assert!(spec.targets.contains(CompFlags::ALIAS));
  }

  #[test]
  fn builder_methods_can_disable_via_false() {
    // The enable parameter on each setter is a real boolean, not a
    // marker — passing false should leave (or clear) the field.
    let spec = BashCompSpec::new()
      .files(true)
      .files(false)
      .dirs(true)
      .dirs(false);
    assert!(!spec.targets.contains(CompFlags::FILES));
    assert!(!spec.targets.contains(CompFlags::DIRS));
  }

  #[test]
  fn case_insensitive_complete_preserves_candidate_case() {
    let _g = TestGuard::new();
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();

    let prev_cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(dir.path()).unwrap();
    crate::shopt_mut!(prompt.completion_ignore_case = true);

    let spec = BashCompSpec::new().files(true).dirs(true);
    let ctx = CompContext {
      words: vec!["cd".into(), "S".into()],
      cword: 1,
      line: "cd S".into(),
      cursor_pos: 4,
    };
    let candidates = spec.complete(&ctx).unwrap();

    std::env::set_current_dir(prev_cwd).unwrap();
    crate::shopt_mut!(prompt.completion_ignore_case = false);

    let src_candidates: Vec<_> = candidates
      .iter()
      .filter(|c| {
        let s = c.content.trim_end_matches('/');
        s.eq_ignore_ascii_case("src")
      })
      .collect();
    assert!(
      !src_candidates.is_empty(),
      "expected an `src`-shaped candidate, got: {candidates:?}"
    );
    for c in &src_candidates {
      assert!(
        c.content.starts_with("src"),
        "candidate should preserve lowercase `src` from the filesystem, \
         not the typed `S`; got: {:?}",
        c.content
      );
      assert!(
        !c.content.starts_with("Src"),
        "candidate must not re-introduce typed-case prefix (issue #90); \
         got: {:?}",
        c.content
      );
    }
  }

  // Completion via BashCompSpec should preserve the user's typed structural
  // prefix ($VAR/, ~/, etc.) through expansion.
  #[test]
  fn comp_spec_preserves_structural_prefix_under_expansion() {
    let _g = TestGuard::new();
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();

    // a fresh var name avoids collisions with whatever TestGuard or the host
    // already set for HOME / TMPDIR / etc.
    let dir_path = dir.path().to_string_lossy().to_string();
    state::Shed::vars_mut(|v| v.set_var("TESTDIR", VarKind::string(&dir_path), VarFlags::EXPORT))
      .unwrap();

    let spec = BashCompSpec::new().files(true).dirs(true);
    let ctx = CompContext {
      words: vec!["cd".into(), "$TESTDIR/s".into()],
      cword: 1,
      line: "cd $TESTDIR/s".into(),
      cursor_pos: 13,
    };
    let candidates = spec.complete(&ctx).unwrap();

    let matching: Vec<_> = candidates
      .iter()
      .filter(|c| c.content.trim_end_matches('/').ends_with("src"))
      .collect();
    assert!(
      !matching.is_empty(),
      "expected an src-shaped candidate, got: {candidates:?}"
    );
    for c in &matching {
      assert!(
        c.content.starts_with("$TESTDIR/"),
        "candidate must preserve $TESTDIR literal in displayed content; \
         got: {:?}",
        c.content
      );
      assert!(
        !c.content.contains(&dir_path),
        "candidate must not leak the expanded $TESTDIR path into content; \
         got: {:?}",
        c.content
      );
    }
  }
}
