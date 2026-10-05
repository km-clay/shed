use std::convert::Into;

use crate::{builtin::opt::Opt, state::Shed};

use super::query::HistQuery;

mod query_tests {
  use super::*;
  use crate::tests::testutil::TestGuard;

  fn parse(opts: &[Opt]) -> HistQuery {
    HistQuery::from_opts(opts).expect("from_opts should succeed")
  }

  // ─── Options with an argument → field assignments ────────────────────

  #[test]
  fn opts_after() {
    let q = parse(&[Opt::for_test("after", &["yesterday"])]);
    assert_eq!(q.after, (Some("yesterday".into()), false));
  }

  #[test]
  fn opts_before() {
    let q = parse(&[Opt::for_test("before", &["tomorrow"])]);
    assert_eq!(q.before, (Some("tomorrow".into()), false));
  }

  #[test]
  fn opts_contains() {
    let q = parse(&[Opt::for_test("contains", &["grep"])]);
    assert_eq!(q.contains, (Some("grep".into()), false));
  }

  #[test]
  fn opts_starts_with() {
    let q = parse(&[Opt::for_test("starts-with", &["git"])]);
    assert_eq!(q.starts_with, (Some("git".into()), false));
  }

  #[test]
  fn opts_ends_with() {
    let q = parse(&[Opt::for_test("ends-with", &[".log"])]);
    assert_eq!(q.ends_with, (Some(".log".into()), false));
  }

  #[test]
  fn opts_matches_regex() {
    let q = parse(&[Opt::for_test("matches", &["^cargo"])]);
    assert_eq!(q.matches, (Some("^cargo".into()), false));
  }

  #[test]
  fn opts_duration_gt_lt() {
    let q = parse(&[
      Opt::for_test("duration-gt", &["1s"]),
      Opt::for_test("duration-lt", &["1h"]),
    ]);
    assert_eq!(q.duration_gt, (Some("1s".into()), false));
    assert_eq!(q.duration_lt, (Some("1h".into()), false));
  }

  #[test]
  fn opts_with_token() {
    let q = parse(&[Opt::for_test("with-token", &["abcd-1234"])]);
    assert_eq!(q.with_token, (Some("abcd-1234".into()), false));
  }

  #[test]
  fn opts_with_status_parses_integer() {
    let q = parse(&[Opt::for_test("with-status", &["127"])]);
    assert_eq!(q.with_status, (Some(127), false));
  }

  #[test]
  fn opts_with_status_invalid_errors() {
    let result = HistQuery::from_opts(&[Opt::for_test("with-status", &["notanumber"])]);
    assert!(result.is_err());
  }

  #[test]
  fn opts_lines_gt_lt() {
    let q = parse(&[
      Opt::for_test("lines-gt", &["5"]),
      Opt::for_test("lines-lt", &["20"]),
    ]);
    assert_eq!(q.lines_gt, (Some(5), false));
    assert_eq!(q.lines_lt, (Some(20), false));
  }

  #[test]
  fn opts_lines_gt_invalid_errors() {
    let result = HistQuery::from_opts(&[Opt::for_test("lines-gt", &["abc"])]);
    assert!(result.is_err());
  }

  #[test]
  fn opts_limit() {
    let q = parse(&[Opt::for_test("limit", &["50"])]);
    assert_eq!(q.limit, Some(50));
  }

  #[test]
  fn opts_limit_invalid_falls_back_to_max() {
    // The code uses unwrap_or(u64::MAX) for limit specifically.
    let q = parse(&[Opt::for_test("limit", &["abc"])]);
    assert_eq!(q.limit, Some(u64::MAX));
  }

  #[test]
  fn opts_in_dir_uses_arg_when_not_canonicalizable() {
    let _g = TestGuard::new();
    // A clearly non-existent path falls back to the literal arg.
    let q = parse(&[Opt::for_test(
      "in-dir",
      &["/definitely/not/a/real/dir/xyz123"],
    )]);
    assert_eq!(
      q.in_dir,
      (Some("/definitely/not/a/real/dir/xyz123".into()), false)
    );
  }

  // ─── Flags (no arg) → bool ───────────────────────────────────────────

  #[test]
  fn opts_ex_hist_flag() {
    let q = parse(&[Opt::for_test("ex", &[])]);
    assert!(q.ex_hist);
  }

  #[test]
  fn opts_count_flag() {
    let q = parse(&[Opt::for_test("count", &[])]);
    assert!(q.count);
  }

  #[test]
  fn opts_delete_flag() {
    let q = parse(&[Opt::for_test("delete", &[])]);
    assert!(q.delete);
  }

  #[test]
  fn opts_restore_flag() {
    let q = parse(&[Opt::for_test("restore", &[])]);
    assert!(q.restore);
  }

  #[test]
  fn opts_json_flag() {
    let q = parse(&[Opt::for_test("json", &[])]);
    assert!(q.json);
  }

  // ─── Short flags ─────────────────────────────────────────────────────

  #[test]
  fn opts_short_n_disables_numbers() {
    // `-n` resolves to the "no-numbers" key.
    let q = parse(&[Opt::for_test("no-numbers", &[])]);
    assert!(q.no_numbers);
  }

  #[test]
  fn opts_short_r_reverses() {
    // `-r` resolves to the "reverse" key.
    let q = parse(&[Opt::for_test("reverse", &[])]);
    assert!(q.reverse);
  }

  // ─── --not polarity ──────────────────────────────────────────────────

  #[test]
  fn opts_not_flips_polarity_for_next_arg() {
    let q = parse(&[
      Opt::for_test("not", &[]),
      Opt::for_test("contains", &["rm -rf"]),
    ]);
    assert_eq!(q.contains, (Some("rm -rf".into()), true));
  }

  #[test]
  fn opts_not_only_applies_to_next_arg_then_resets() {
    let q = parse(&[
      Opt::for_test("not", &[]),
      Opt::for_test("contains", &["danger"]),
      Opt::for_test("after", &["yesterday"]),
    ]);
    assert_eq!(q.contains, (Some("danger".into()), true));
    // 'after' should NOT be negated — polarity reset after 'contains'.
    assert_eq!(q.after, (Some("yesterday".into()), false));
  }

  #[test]
  fn opts_double_not_cancels_polarity() {
    let q = parse(&[
      Opt::for_test("not", &[]),
      Opt::for_test("not", &[]),
      Opt::for_test("contains", &["x"]),
    ]);
    assert_eq!(q.contains, (Some("x".into()), false));
  }

  // ─── Unknown / error handling ────────────────────────────────────────
  //
  // In practice only recognized keys reach `from_opts` (the option parser
  // filters the rest), but the catch-all arm defensively errors on anything
  // it doesn't recognize.

  #[test]
  fn opts_unknown_long_errors() {
    let result = HistQuery::from_opts(&[Opt::for_test("totally-made-up", &["x"])]);
    assert!(result.is_err());
  }

  #[test]
  fn opts_unknown_short_errors() {
    let result = HistQuery::from_opts(&[Opt::for_test("x", &["val"])]);
    assert!(result.is_err());
  }

  // ─── Combined / multi-opt sanity check ───────────────────────────────

  #[test]
  fn opts_multiple_fields_compose() {
    let q = parse(&[
      Opt::for_test("reverse", &[]),
      Opt::for_test("json", &[]),
      Opt::for_test("contains", &["cargo"]),
      Opt::for_test("limit", &["10"]),
      Opt::for_test("not", &[]),
      Opt::for_test("in-dir", &["/nonexistent/zzz"]),
    ]);
    assert!(q.reverse);
    assert!(q.json);
    assert_eq!(q.contains, (Some("cargo".into()), false));
    assert_eq!(q.limit, Some(10));
    assert_eq!(q.in_dir, (Some("/nonexistent/zzz".into()), true));
  }

  // ─── HistQuery::execute ──────────────────────────────────────────────
  //
  // Each test builds a fresh in-memory History, seeds it with known
  // entries, then runs a HistQuery and checks the result. The test
  // table name varies per test so the LazyLock cache in history.rs
  // doesn't bleed entries across cases.

  use std::time::{Duration as StdDuration, UNIX_EPOCH};

  use crate::{
    readline::{HistEntry, History},
    util::random::Uuid,
  };

  /// Build a `HistEntry` with the given command and the rest filled in
  /// from defaults. Timestamp is fixed (NOT `now()`) so cross-runs are
  /// deterministic where they need to be.
  fn entry(cmd: &str) -> HistEntry {
    HistEntry {
      runtime: StdDuration::from_micros(0),
      timestamp: UNIX_EPOCH + StdDuration::from_secs(1_700_000_000),
      command: cmd.into(),
      cwd: "/tmp".into(),
      status: 0,
      token: Uuid::new_v4(),
    }
  }

  fn entry_full(
    cmd: &str,
    cwd: &str,
    status: i32,
    runtime_micros: u64,
    secs_since_epoch: u64,
  ) -> HistEntry {
    HistEntry {
      runtime: StdDuration::from_micros(runtime_micros),
      timestamp: UNIX_EPOCH + StdDuration::from_secs(secs_since_epoch),
      command: cmd.into(),
      cwd: cwd.into(),
      status,
      token: Uuid::new_v4(),
    }
  }

  /// Create a History with a unique per-test table name and seed it with
  /// the given entries (oldest first).
  fn hist_with(name: &str, entries: Vec<HistEntry>) -> History {
    let h = History::empty(name, &Shed::hist_branch());
    for e in entries {
      h.push_entry(e).unwrap();
    }
    h
  }

  // ─── No filters ─────────────────────────────────────────────────────

  #[test]
  fn execute_no_conditions_returns_all_entries() {
    let _g = TestGuard::new();
    let h = hist_with("exec_all", vec![entry("a"), entry("b"), entry("c")]);
    let q = HistQuery::new();
    let results = q.execute(&h).unwrap();
    assert_eq!(results.len(), 3);
  }

  // ─── Substring / prefix / suffix filters ────────────────────────────

  #[test]
  fn execute_contains_filter() {
    let _g = TestGuard::new();
    let h = hist_with(
      "exec_contains",
      vec![entry("ls -la"), entry("echo hello"), entry("cat foo")],
    );
    let mut q = HistQuery::new();
    q.contains = (Some("echo".into()), false);
    let results = q.execute(&h).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].1.command, "echo hello");
  }

  #[test]
  fn execute_starts_with_filter() {
    let _g = TestGuard::new();
    let h = hist_with(
      "exec_starts",
      vec![entry("git status"), entry("git log"), entry("ls")],
    );
    let mut q = HistQuery::new();
    q.starts_with = (Some("git".into()), false);
    let results = q.execute(&h).unwrap();
    assert_eq!(results.len(), 2);
  }

  #[test]
  fn execute_ends_with_filter() {
    let _g = TestGuard::new();
    let h = hist_with(
      "exec_ends",
      vec![entry("touch a.log"), entry("rm b.log"), entry("vi c.txt")],
    );
    let mut q = HistQuery::new();
    q.ends_with = (Some(".log".into()), false);
    let results = q.execute(&h).unwrap();
    assert_eq!(results.len(), 2);
  }

  // ─── Status / token / dir filters ───────────────────────────────────

  #[test]
  fn execute_with_status_filter() {
    let _g = TestGuard::new();
    let h = hist_with(
      "exec_status",
      vec![
        entry_full("ok", "/tmp", 0, 0, 100),
        entry_full("fail", "/tmp", 1, 0, 200),
        entry_full("notfound", "/tmp", 127, 0, 300),
      ],
    );
    let mut q = HistQuery::new();
    q.with_status = (Some(127), false);
    let results = q.execute(&h).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].1.command, "notfound");
  }

  #[test]
  fn execute_in_dir_filter() {
    let _g = TestGuard::new();
    let h = hist_with(
      "exec_dir",
      vec![
        entry_full("a", "/home/u", 0, 0, 100),
        entry_full("b", "/tmp", 0, 0, 200),
        entry_full("c", "/home/u", 0, 0, 300),
      ],
    );
    let mut q = HistQuery::new();
    q.in_dir = (Some("/home/u".into()), false);
    let results = q.execute(&h).unwrap();
    assert_eq!(results.len(), 2);
  }

  // ─── Line count filters ─────────────────────────────────────────────

  #[test]
  fn execute_lines_gt_filter() {
    let _g = TestGuard::new();
    let h = hist_with(
      "exec_lines_gt",
      vec![
        entry("one"),
        entry("one\ntwo\nthree"),       // 3 lines
        entry("one\ntwo\nthree\nfour"), // 4 lines
      ],
    );
    let mut q = HistQuery::new();
    q.lines_gt = (Some(2), false); // strictly greater than 2
    let results = q.execute(&h).unwrap();
    assert_eq!(results.len(), 2);
  }

  #[test]
  fn execute_lines_lt_filter() {
    let _g = TestGuard::new();
    let h = hist_with(
      "exec_lines_lt",
      vec![entry("one"), entry("one\ntwo"), entry("a\nb\nc\nd")],
    );
    let mut q = HistQuery::new();
    q.lines_lt = (Some(3), false); // strictly less than 3
    let results = q.execute(&h).unwrap();
    assert_eq!(results.len(), 2);
  }

  // ─── Duration filters ───────────────────────────────────────────────

  #[test]
  fn execute_duration_gt_filter() {
    let _g = TestGuard::new();
    let h = hist_with(
      "exec_dur_gt",
      vec![
        entry_full("fast", "/", 0, 1, 100),           // 1us
        entry_full("medium", "/", 0, 1_000_000, 200), // 1s
        entry_full("slow", "/", 0, 10_000_000, 300),  // 10s
      ],
    );
    let mut q = HistQuery::new();
    q.duration_gt = (Some("5s".into()), false);
    let results = q.execute(&h).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].1.command, "slow");
  }

  #[test]
  fn execute_duration_lt_filter() {
    let _g = TestGuard::new();
    let h = hist_with(
      "exec_dur_lt",
      vec![
        entry_full("fast", "/", 0, 1, 100),
        entry_full("slow", "/", 0, 10_000_000, 200),
      ],
    );
    let mut q = HistQuery::new();
    q.duration_lt = (Some("1s".into()), false);
    let results = q.execute(&h).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].1.command, "fast");
  }

  #[test]
  fn execute_duration_invalid_errors() {
    let _g = TestGuard::new();
    let h = hist_with("exec_dur_bad", vec![entry("x")]);
    let mut q = HistQuery::new();
    q.duration_gt = (Some("not-a-duration".into()), false);
    let result = q.execute(&h);
    assert!(result.is_err());
  }

  // ─── Limit / specific IDs ───────────────────────────────────────────

  #[test]
  fn execute_limit_caps_result_count() {
    let _g = TestGuard::new();
    let h = hist_with(
      "exec_limit",
      vec![entry("a"), entry("b"), entry("c"), entry("d")],
    );
    let mut q = HistQuery::new();
    q.limit = Some(2);
    let results = q.execute(&h).unwrap();
    assert_eq!(results.len(), 2);
  }

  #[test]
  fn execute_specific_id_positive() {
    let _g = TestGuard::new();
    let h = hist_with(
      "exec_id",
      vec![entry("first"), entry("second"), entry("third")],
    );
    let mut q = HistQuery::new();
    q.specific_ids = vec![2]; // literal id=2 (second entry, since ids start at 1)
    let results = q.execute(&h).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].1.command, "second");
  }

  #[test]
  fn execute_specific_id_negative_is_relative_to_end() {
    let _g = TestGuard::new();
    let h = hist_with(
      "exec_id_neg",
      vec![entry("first"), entry("second"), entry("third")],
    );
    let mut q = HistQuery::new();
    q.specific_ids = vec![-1]; // -1 → second-newest entry
    let results = q.execute(&h).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].1.command, "second");
  }

  // ─── --not negation ─────────────────────────────────────────────────

  #[test]
  fn execute_negated_contains_excludes_matches() {
    let _g = TestGuard::new();
    let h = hist_with(
      "exec_not",
      vec![
        entry("danger_rm_command"),
        entry("safe_ls"),
        entry("also_safe"),
      ],
    );
    let mut q = HistQuery::new();
    q.contains = (Some("danger".into()), true); // NOT contains
    let results = q.execute(&h).unwrap();
    assert_eq!(results.len(), 2);
    for r in &results {
      assert!(!r.1.command.contains("danger"));
    }
  }

  // ─── matches (regex, applied post-query) ────────────────────────────

  #[test]
  fn execute_matches_regex_post_filter() {
    let _g = TestGuard::new();
    let h = hist_with(
      "exec_regex",
      vec![entry("cargo build"), entry("cargo test"), entry("git log")],
    );
    let mut q = HistQuery::new();
    q.matches = (Some("^cargo".into()), false);
    let results = q.execute(&h).unwrap();
    assert_eq!(results.len(), 2);
  }

  #[test]
  fn execute_matches_regex_negated() {
    let _g = TestGuard::new();
    let h = hist_with(
      "exec_regex_neg",
      vec![entry("cargo build"), entry("cargo test"), entry("git log")],
    );
    let mut q = HistQuery::new();
    q.matches = (Some("^cargo".into()), true);
    let results = q.execute(&h).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].1.command, "git log");
  }

  // ─── Ordering ───────────────────────────────────────────────────────

  #[test]
  fn execute_default_returns_oldest_first_after_reverse_default() {
    // execute() pulls DESC from sqlite, then reverses (since
    // self.reverse defaults to false). End result: oldest at index 0,
    // newest at the end.
    let _g = TestGuard::new();
    let h = hist_with(
      "exec_order",
      vec![entry("one"), entry("two"), entry("three")],
    );
    let q = HistQuery::new();
    let results = q.execute(&h).unwrap();
    assert_eq!(results[0].1.command, "one");
    assert_eq!(results[2].1.command, "three");
  }

  #[test]
  fn execute_reverse_keeps_desc_order() {
    let _g = TestGuard::new();
    let h = hist_with("exec_rev", vec![entry("one"), entry("two"), entry("three")]);
    let mut q = HistQuery::new();
    q.reverse = true;
    let results = q.execute(&h).unwrap();
    assert_eq!(results[0].1.command, "three");
    assert_eq!(results[2].1.command, "one");
  }

  // ─── Combined filters ──────────────────────────────────────────────

  #[test]
  fn execute_combined_status_and_starts_with() {
    let _g = TestGuard::new();
    let h = hist_with(
      "exec_combo",
      vec![
        entry_full("git push", "/", 0, 0, 100),
        entry_full("git push --force", "/", 128, 0, 200),
        entry_full("ls -la", "/", 0, 0, 300),
      ],
    );
    let mut q = HistQuery::new();
    q.starts_with = (Some("git".into()), false);
    q.with_status = (Some(0), false);
    let results = q.execute(&h).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].1.command, "git push");
  }

  // ─── Bad date input ─────────────────────────────────────────────────

  #[test]
  fn execute_invalid_after_date_errors() {
    let _g = TestGuard::new();
    let h = hist_with("exec_bad_date", vec![entry("x")]);
    let mut q = HistQuery::new();
    q.after = (Some("not-a-real-date-zzz".into()), false);
    let result = q.execute(&h);
    assert!(result.is_err());
  }
}

#[cfg(test)]
mod hist_builtin_execute_tests {
  //! Tests for the `Hist` builtin's `execute()` itself — covering the
  //! `hist` command end-to-end via `test_input`. The mod above (`tests`)
  //! exercises `HistQuery` directly; this one exercises argument
  //! dispatch, table selection, output formatting, and the restore/pull/
  //! import branches.

  use std::fs;

  use tempfile::TempDir;

  use crate::readline::{History, MAIN_HIST_TABLE_NAME};
  use crate::state::{Shed, db};
  use crate::tests::testutil::{TestGuard, test_input};

  /// Drop and re-init the named table on the shared in-memory conn so
  /// each test starts with a clean slate. Returns a History handle for
  /// seeding entries.
  fn fresh_history(table: &str) -> History {
    let conn = db::get_db_conn().expect("test db conn");
    let _ = conn
      .lock()
      .unwrap()
      .execute_batch(&format!("DROP TABLE IF EXISTS {table}"));
    let _ = conn
      .lock()
      .unwrap()
      .execute_batch(&format!("DROP TABLE IF EXISTS {table}_backup"));
    let _ = conn
      .lock()
      .unwrap()
      .execute_batch("PRAGMA user_version = 0");
    Shed::set_hist_branch("main".to_string());
    History::new(conn, table, &Shed::hist_branch()).expect("history init")
  }

  /// Like [`fresh_history`], but also drops the shared `branches` table so each
  /// branch test starts from an unborn `main`. Returns a `main`-bound handle.
  fn fresh_branched() -> History {
    let conn = db::get_db_conn().expect("test db conn");
    {
      let c = conn.lock().unwrap();
      let _ = c.execute_batch("DROP TABLE IF EXISTS shed_history");
      let _ = c.execute_batch("DROP TABLE IF EXISTS branches");
      let _ = c.execute_batch("PRAGMA user_version = 0");
    }
    Shed::set_hist_branch("main".to_string());
    History::new(conn, MAIN_HIST_TABLE_NAME, "main").expect("history init")
  }

  // ─── default listing / filtering ───────────────────────────────────

  #[test]
  fn hist_lists_pushed_entries() {
    let g = TestGuard::new();
    let h = fresh_history(MAIN_HIST_TABLE_NAME);
    h.push(": alpha").unwrap();
    h.push(": beta").unwrap();
    test_input("hist").unwrap();
    let out = g.read_output();
    assert!(out.contains(": alpha"), "got: {out:?}");
    assert!(out.contains(": beta"), "got: {out:?}");
    assert_eq!(Shed::get_status(), 0);
  }

  #[test]
  fn hist_n_flag_omits_ids() {
    let g = TestGuard::new();
    let h = fresh_history(MAIN_HIST_TABLE_NAME);
    h.push(": only-entry").unwrap();
    // With -n, lines should NOT start with the id\t prefix.
    test_input("hist -n").unwrap();
    let out = g.read_output();
    assert!(out.contains(": only-entry"));
    // The id form would be "1\t: only-entry". With -n we just have the cmd.
    assert!(!out.contains("1\t"), "got: {out:?}");
  }

  #[test]
  fn hist_count_outputs_entry_count() {
    let g = TestGuard::new();
    let h = fresh_history(MAIN_HIST_TABLE_NAME);
    h.push(": a").unwrap();
    h.push(": b").unwrap();
    h.push(": c").unwrap();
    test_input("hist --count").unwrap();
    let out = g.read_output();
    assert!(out.trim_end().ends_with('3'), "got: {out:?}");
  }

  #[test]
  fn hist_json_outputs_json_object() {
    let g = TestGuard::new();
    let h = fresh_history(MAIN_HIST_TABLE_NAME);
    h.push(": json-entry").unwrap();
    test_input("hist --json").unwrap();
    let out = g.read_output();
    // serde_json::to_string_pretty produces newlines and a {…} wrapper.
    assert!(out.contains("\"command\""), "got: {out:?}");
    assert!(out.contains(": json-entry"), "got: {out:?}");
  }

  // ─── --ex selects ex_history table ─────────────────────────────────

  #[test]
  fn hist_ex_uses_ex_history_table() {
    let g = TestGuard::new();
    let normal = fresh_history(MAIN_HIST_TABLE_NAME);
    let ex = fresh_history("ex_history");
    normal.push(": normal-entry").unwrap();
    ex.push(": ex-entry").unwrap();
    test_input("hist --ex").unwrap();
    let out = g.read_output();
    assert!(out.contains(": ex-entry"), "got: {out:?}");
    assert!(!out.contains(": normal-entry"), "got: {out:?}");
  }

  // ─── --delete and --restore ────────────────────────────────────────

  #[test]
  fn hist_delete_by_id_removes_entry() {
    let g = TestGuard::new();
    let h = fresh_history(MAIN_HIST_TABLE_NAME);
    h.push(": kept").unwrap();
    h.push(": doomed").unwrap();
    // Delete the second entry by id.
    test_input("hist --delete 2").unwrap();
    g.read_output(); // drain --delete output
    // Now re-list; the doomed entry should be gone.
    test_input("hist").unwrap();
    let out = g.read_output();
    assert!(out.contains(": kept"), "got: {out:?}");
    assert!(!out.contains(": doomed"), "got: {out:?}");
  }

  #[test]
  fn hist_delete_matches_only_removes_matching_entries() {
    // Regression: `--delete --matches <regex>` used to run the delete on the
    // SQL WHERE (empty when --matches is the only filter) and apply the regex
    // only to the displayed list — wiping the ENTIRE table. It must now delete
    // exactly the regex-matched rows.
    let g = TestGuard::new();
    let h = fresh_history(MAIN_HIST_TABLE_NAME);
    h.push(": cargo build").unwrap();
    h.push(": cargo test").unwrap();
    h.push(": git status").unwrap();
    test_input("hist --delete --matches '^: cargo'").unwrap();
    g.read_output(); // drain --delete output
    test_input("hist").unwrap();
    let out = g.read_output();
    assert!(
      out.contains(": git status"),
      "non-matching entry wiped: {out:?}"
    );
    assert!(
      !out.contains(": cargo build"),
      "matching entry survived: {out:?}"
    );
    assert!(
      !out.contains(": cargo test"),
      "matching entry survived: {out:?}"
    );
  }

  #[test]
  fn hist_delete_matches_none_keeps_all_entries() {
    // A regex that matches nothing must delete nothing (must NOT fall through
    // to an empty WHERE and wipe the table).
    let g = TestGuard::new();
    let h = fresh_history(MAIN_HIST_TABLE_NAME);
    h.push(": alpha").unwrap();
    h.push(": beta").unwrap();
    test_input("hist --delete --matches 'zzz-no-match'").unwrap();
    g.read_output();
    test_input("hist").unwrap();
    let out = g.read_output();
    assert!(out.contains(": alpha"), "entry wiped: {out:?}");
    assert!(out.contains(": beta"), "entry wiped: {out:?}");
  }

  #[test]
  fn init_db_creates_all_tables_on_shared_connection() {
    // Regression: init_db keyed its early-return on the DB-wide `user_version`,
    // so once the first table bumped it to USER_VERSION, every later table on
    // the same connection skipped its CREATE TABLE and silently never
    // persisted. Simulate startup order on one shared connection.
    let _g = TestGuard::new();
    let conn = db::get_db_conn().expect("test db conn");
    {
      let c = conn.lock().unwrap();
      c.execute_batch("DROP TABLE IF EXISTS shed_history").ok();
      c.execute_batch("DROP TABLE IF EXISTS ex_history").ok();
      // Fresh-ish DB so the FIRST init succeeds; the SECOND then hits the
      // (formerly buggy) `user_version == USER_VERSION` early-return.
      c.execute_batch("PRAGMA user_version = 0").ok();
    }

    // First table bumps user_version to USER_VERSION.
    let first = History::new(conn.clone(), MAIN_HIST_TABLE_NAME, &Shed::hist_branch())
      .expect("init first table");
    first.push(": first-table-entry").unwrap();

    // Second table on the same connection, user_version now == USER_VERSION.
    // Before the fix its CREATE TABLE was skipped, so this INSERT would fail.
    let second =
      History::new(conn.clone(), "ex_history", &Shed::hist_branch()).expect("init second table");
    second
      .push(": second-table-entry")
      .expect("second table must exist and be writable");

    let entries = second.query("", &[]).expect("query second table");
    assert!(
      entries
        .iter()
        .any(|(_, e)| e.command() == ": second-table-entry"),
      "second table did not persist its entry: {entries:?}"
    );
  }

  #[test]
  fn hist_restore_brings_back_deleted_entries() {
    let g = TestGuard::new();
    let h = fresh_history(MAIN_HIST_TABLE_NAME);
    h.push(": one").unwrap();
    h.push(": two").unwrap();
    // Delete both — creates the backup table.
    test_input("hist --delete --contains :").unwrap();
    g.read_output();
    // Now restore.
    test_input("hist --restore").unwrap();
    g.read_output();
    // Re-list: both entries should reappear.
    test_input("hist").unwrap();
    let out = g.read_output();
    assert!(out.contains(": one"), "got: {out:?}");
    assert!(out.contains(": two"), "got: {out:?}");
  }

  #[test]
  fn hist_restore_with_no_backup_errors() {
    let _g = TestGuard::new();
    let _h = fresh_history(MAIN_HIST_TABLE_NAME);
    // No prior --delete → no backup table → restore fails.
    test_input("hist --restore").ok();
    assert_ne!(Shed::get_status(), 0);
  }

  // ─── --pull just refreshes caches ──────────────────────────────────

  #[test]
  fn hist_pull_returns_ok() {
    let _g = TestGuard::new();
    let _h = fresh_history(MAIN_HIST_TABLE_NAME);
    test_input("hist pull").unwrap();
    assert_eq!(Shed::get_status(), 0);
  }

  // ─── --import reads a file and pushes entries ──────────────────────

  #[test]
  fn hist_import_adds_entries_from_bash_format_file() {
    let g = TestGuard::new();
    let _h = fresh_history(MAIN_HIST_TABLE_NAME);
    let dir = TempDir::new().unwrap();
    let path = dir.path().join(".bash_history");
    fs::write(
      &path,
      "#1700000000\n: imported-one\n#1700000001\n: imported-two\n",
    )
    .unwrap();
    test_input(format!("hist import {}", path.display())).unwrap();
    g.read_output(); // drain "imported N" + entries dump
    // Verify the entries are queryable via a follow-up list.
    test_input("hist").unwrap();
    let out = g.read_output();
    assert!(out.contains(": imported-one"), "got: {out:?}");
    assert!(out.contains(": imported-two"), "got: {out:?}");
  }

  #[test]
  fn hist_export_import_round_trips_dag_and_branches() {
    let g = TestGuard::new();
    let main_h = fresh_branched();
    main_h.push(": on-main").unwrap();

    test_input("hist branch feat").unwrap();
    g.read_output();
    let conn = db::get_db_conn().unwrap();
    let feat_h = History::new(conn, MAIN_HIST_TABLE_NAME, "feat").unwrap();
    feat_h.push(": feat-work").unwrap();
    test_input("hist merge feat").unwrap();
    g.read_output();

    test_input("hist export").unwrap();
    let dump = g.read_output();

    let dir = TempDir::new().unwrap();
    let path = dir.path().join("backup.json");
    fs::write(&path, &dump).unwrap();

    // wipe everything, then restore from the dump
    fresh_branched();
    test_input(format!("hist import {}", path.display())).unwrap();
    g.read_output();

    // entries reachable from the restored main head, merge node still hidden
    test_input("hist").unwrap();
    let out = g.read_output();
    assert!(out.contains(": on-main"), "lost main history: {out:?}");
    assert!(
      out.contains(": feat-work"),
      "lost merged feat history: {out:?}"
    );

    test_input("hist --count").unwrap();
    let out = g.read_output();
    assert!(
      out.trim_end().ends_with('2'),
      "merge node leaked after restore: {out:?}"
    );

    // branch pointers came back
    test_input("hist branch").unwrap();
    let out = g.read_output();
    assert!(out.contains("* main"), "current branch missing: {out:?}");
    assert!(
      out.contains("feat"),
      "feat branch pointer not restored: {out:?}"
    );

    // feat's own lineage is intact and reachable from its restored head
    test_input("hist checkout feat").unwrap();
    g.read_output();
    test_input("hist").unwrap();
    let out = g.read_output();
    assert!(
      out.contains(": feat-work"),
      "feat head not restored: {out:?}"
    );
    assert!(out.contains(": on-main"), "feat lost its ancestry: {out:?}");
  }

  #[test]
  fn hist_import_refuses_nonempty_without_force() {
    let g = TestGuard::new();
    let h = fresh_branched();
    h.push(": original").unwrap();

    test_input("hist export").unwrap();
    let dump = g.read_output();
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("backup.json");
    fs::write(&path, &dump).unwrap();

    // table is non-empty: bare import must refuse
    test_input(format!("hist import {}", path.display())).ok();
    assert_ne!(
      Shed::get_status(),
      0,
      "import into non-empty history should error"
    );
    g.read_output();

    // --force wipes and restores
    test_input(format!("hist import --force {}", path.display())).unwrap();
    g.read_output();
    test_input("hist").unwrap();
    let out = g.read_output();
    assert!(
      out.contains(": original"),
      "force import lost entries: {out:?}"
    );
  }

  // ─── branch / checkout subcommands ─────────────────────────────────

  #[test]
  fn hist_branch_creates_and_lists() {
    let g = TestGuard::new();
    let h = fresh_branched();
    h.push(": on-main").unwrap();

    test_input("hist branch feat").unwrap();
    test_input("hist branch").unwrap();
    let out = g.read_output();
    assert!(out.contains("feat"), "new branch not listed: {out:?}");
    // still on main after `branch` (create doesn't switch), marked current
    assert!(out.contains("* main"), "current branch not marked: {out:?}");
  }

  #[test]
  fn hist_checkout_switches_session_branch() {
    let _g = TestGuard::new();
    let h = fresh_branched();
    h.push(": x").unwrap();

    test_input("hist branch feat").unwrap();
    test_input("hist checkout feat").unwrap();
    assert_eq!(Shed::hist_branch(), "feat");
    assert_eq!(Shed::get_status(), 0);
  }

  #[test]
  fn hist_checkout_nonexistent_errors_and_stays_put() {
    let _g = TestGuard::new();
    let h = fresh_branched();
    h.push(": x").unwrap();

    test_input("hist checkout nope").ok();
    assert_ne!(Shed::get_status(), 0);
    assert_eq!(
      Shed::hist_branch(),
      "main",
      "checkout should not switch on error"
    );
  }

  #[test]
  fn hist_list_is_scoped_to_current_branch() {
    let g = TestGuard::new();
    let main_h = fresh_branched();
    main_h.push(": shared").unwrap();

    // fork feat from here (feat shares the pre-fork history)
    test_input("hist branch feat").unwrap();

    // push a feat-only entry on a feat-bound handle (test_input doesn't record)
    let conn = db::get_db_conn().unwrap();
    let feat_h = History::new(conn, MAIN_HIST_TABLE_NAME, "feat").unwrap();
    feat_h.push(": feat-only").unwrap();

    // on feat: reaches shared history AND its own entry
    test_input("hist checkout feat").unwrap();
    g.read_output(); // drain the "switched to branch" message
    test_input("hist").unwrap();
    let out = g.read_output();
    assert!(
      out.contains(": shared"),
      "feat lost pre-fork history: {out:?}"
    );
    assert!(
      out.contains(": feat-only"),
      "feat missing its own entry: {out:?}"
    );

    // back on main: sees shared, NOT feat's divergent entry
    test_input("hist checkout main").unwrap();
    g.read_output();
    test_input("hist").unwrap();
    let out = g.read_output();
    assert!(out.contains(": shared"), "main lost its history: {out:?}");
    assert!(
      !out.contains(": feat-only"),
      "main leaked feat's entry: {out:?}"
    );
  }

  #[test]
  fn hist_merge_brings_in_other_branch_and_hides_node() {
    let g = TestGuard::new();
    let main_h = fresh_branched();
    main_h.push(": on-main").unwrap();

    test_input("hist branch feat").unwrap();
    g.read_output();

    // feat-only work on a feat-bound handle
    let conn = db::get_db_conn().unwrap();
    let feat_h = History::new(conn, MAIN_HIST_TABLE_NAME, "feat").unwrap();
    feat_h.push(": feat-work").unwrap();

    // before merge: main doesn't reach feat's work
    test_input("hist").unwrap();
    let out = g.read_output();
    assert!(
      !out.contains(": feat-work"),
      "feat work leaked pre-merge: {out:?}"
    );

    // merge feat into main
    test_input("hist merge feat").unwrap();
    g.read_output();

    // after merge: main reaches both its own and feat's history
    test_input("hist").unwrap();
    let out = g.read_output();
    assert!(out.contains(": on-main"), "lost main history: {out:?}");
    assert!(
      out.contains(": feat-work"),
      "merge didn't bring in feat: {out:?}"
    );

    // the synthetic merge node must be invisible: count is 2, not 3
    test_input("hist --count").unwrap();
    let out = g.read_output();
    assert!(
      out.trim_end().ends_with('2'),
      "merge node leaked into listing: {out:?}"
    );
  }

  #[test]
  fn hist_delete_preserves_branch_topology() {
    // main: a ← b ← c   feat: a ← b ← feat-x   (ids interleave: a,b,feat-x,c)
    // Deleting shared `b` must stitch each lineage to `a` — NOT re-chain by id,
    // which would wrongly make main's `c` point at feat's `feat-x`.
    let g = TestGuard::new();
    let main_h = fresh_branched();
    main_h.push(": a").unwrap();
    main_h.push(": b").unwrap();

    test_input("hist branch feat").unwrap();
    g.read_output();

    let conn = db::get_db_conn().unwrap();
    let feat_h = History::new(conn, MAIN_HIST_TABLE_NAME, "feat").unwrap();
    feat_h.push(": feat-x").unwrap(); // id after b, on feat

    main_h.push(": c").unwrap(); // id after feat-x, on main

    // delete the shared middle entry
    test_input("hist --delete --contains ': b'").unwrap();
    g.read_output();

    // main keeps a, c — must NOT have leaked feat-x via an id-order re-chain
    test_input("hist").unwrap();
    let out = g.read_output();
    assert!(
      out.contains(": a") && out.contains(": c"),
      "main lost its history: {out:?}"
    );
    assert!(!out.contains(": b"), "deleted entry survived: {out:?}");
    assert!(
      !out.contains(": feat-x"),
      "main leaked feat's entry after delete: {out:?}"
    );

    // feat keeps a, feat-x (its chain stitched around b), and doesn't gain c
    test_input("hist checkout feat").unwrap();
    g.read_output();
    test_input("hist").unwrap();
    let out = g.read_output();
    assert!(
      out.contains(": a") && out.contains(": feat-x"),
      "feat lost its history: {out:?}"
    );
    assert!(!out.contains(": b"), "feat kept deleted entry: {out:?}");
    assert!(!out.contains(": c"), "feat leaked main's entry: {out:?}");
  }

  #[test]
  fn hist_merge_divergent_creates_hidden_node() {
    // main and feat both advance past the fork, so neither tip is an ancestor
    // of the other → a real merge node (not a fast-forward).
    let g = TestGuard::new();
    let main_h = fresh_branched();
    main_h.push(": base").unwrap();

    test_input("hist branch feat").unwrap();
    g.read_output();

    let conn = db::get_db_conn().unwrap();
    let feat_h = History::new(conn, MAIN_HIST_TABLE_NAME, "feat").unwrap();
    feat_h.push(": feat-x").unwrap();

    // main diverges with a commit feat lacks
    main_h.push(": main-y").unwrap();

    test_input("hist merge feat").unwrap();
    g.read_output();

    test_input("hist").unwrap();
    let out = g.read_output();
    assert!(out.contains(": base"), "lost shared base: {out:?}");
    // a fast-forward would have discarded main's own commit; a real merge keeps it
    assert!(
      out.contains(": main-y"),
      "merge dropped main's commit (wrongly fast-forwarded?): {out:?}"
    );
    assert!(
      out.contains(": feat-x"),
      "merge didn't bring in feat: {out:?}"
    );

    // three real commits; the synthetic merge node stays hidden
    test_input("hist --count").unwrap();
    let out = g.read_output();
    assert!(
      out.trim_end().ends_with('3'),
      "merge node leaked or history wrong: {out:?}"
    );
  }

  #[test]
  fn hist_checkout_b_creates_and_switches() {
    let g = TestGuard::new();
    let h = fresh_branched();
    h.push(": x").unwrap();

    test_input("hist checkout -b feat").unwrap();
    g.read_output();
    assert_eq!(Shed::hist_branch(), "feat");
    assert_eq!(Shed::get_status(), 0);

    // the branch now exists and we're on it
    test_input("hist branch").unwrap();
    let out = g.read_output();
    assert!(out.contains("* feat"), "not on the created branch: {out:?}");
  }

  #[test]
  fn hist_checkout_orphan_starts_disconnected() {
    let g = TestGuard::new();
    let main_h = fresh_branched();
    main_h.push(": on-main").unwrap();

    test_input("hist checkout --orphan void").unwrap();
    g.read_output();
    assert_eq!(Shed::hist_branch(), "void");

    // orphan is empty and disconnected until its first push
    test_input("hist").unwrap();
    let out = g.read_output();
    assert!(
      !out.contains(": on-main"),
      "orphan reached main's history: {out:?}"
    );

    let conn = db::get_db_conn().unwrap();
    let void_h = History::new(conn, MAIN_HIST_TABLE_NAME, "void").unwrap();
    void_h.push(": fresh-start").unwrap();

    test_input("hist").unwrap();
    let out = g.read_output();
    assert!(
      out.contains(": fresh-start"),
      "orphan didn't record its own commit: {out:?}"
    );
    assert!(
      !out.contains(": on-main"),
      "orphan leaked main's history: {out:?}"
    );
  }

  #[test]
  fn ex_and_shed_history_dont_collide() {
    // Regression: shed_history and ex_history both used branch `main`, sharing
    // one `branches` row, so an ex push clobbered shed's head and the next shed
    // push became a fresh root. Now `branches` is keyed by (table_name, name).
    let _g = TestGuard::new();
    let conn = db::get_db_conn().unwrap();
    {
      let c = conn.lock().unwrap();
      let _ = c.execute_batch("DROP TABLE IF EXISTS shed_history");
      let _ = c.execute_batch("DROP TABLE IF EXISTS ex_history");
      let _ = c.execute_batch("DROP TABLE IF EXISTS branches");
      let _ = c.execute_batch("DROP TABLE IF EXISTS reflog");
      let _ = c.execute_batch("PRAGMA user_version = 0");
    }
    Shed::set_hist_branch("main".to_string());
    let shed = History::new(conn.clone(), MAIN_HIST_TABLE_NAME, "main").unwrap();
    let ex = History::new(conn.clone(), "ex_history", "main").unwrap();

    let a = shed.push(": shed-a").unwrap().unwrap();
    ex.push(": ex-x").unwrap(); // used to clobber shed's shared 'main' head
    shed.push(": shed-b").unwrap();

    // shed-b must chain onto shed-a, not root itself off a dangling ex head
    let parent: Option<String> = conn
      .lock()
      .unwrap()
      .query_row(
        "SELECT parent FROM shed_history WHERE command = ': shed-b'",
        [],
        |r| r.get(0),
      )
      .unwrap();
    assert_eq!(
      parent,
      Some(a.to_string()),
      "ex push severed shed's chain (branch collision): parent={parent:?}"
    );
  }

  #[test]
  fn init_db_repairs_fragmented_linear_history() {
    // A linear timeline with more than one NULL-parent root is the signature of
    // the release bug; init_db should re-chain it by id order on next open.
    let _g = TestGuard::new();
    let conn = db::get_db_conn().unwrap();
    {
      let c = conn.lock().unwrap();
      let _ = c.execute_batch("DROP TABLE IF EXISTS shed_history");
      let _ = c.execute_batch("DROP TABLE IF EXISTS branches");
      let _ = c.execute_batch("DROP TABLE IF EXISTS reflog");
      let _ = c.execute_batch("PRAGMA user_version = 0");
    }
    Shed::set_hist_branch("main".to_string());

    // create the schema (empty), then plant a fragmented chain: t3 is a spurious
    // root, orphaning t1/t2 from the head at t5.
    History::new(conn.clone(), MAIN_HIST_TABLE_NAME, "main").unwrap();
    {
      let c = conn.lock().unwrap();
      for (id, tok, parent) in [
        (1, "t1", None),
        (2, "t2", Some("t1")),
        (3, "t3", None),
        (4, "t4", Some("t3")),
        (5, "t5", Some("t4")),
      ] {
        c.execute(
          "INSERT INTO shed_history (id, timestamp, runtime, command, cwd, status, token, parent)
           VALUES (?1, ?1, 0, ?2, '', 0, ?3, ?4)",
          rusqlite::params![id, format!("cmd{id}"), tok, parent],
        )
        .unwrap();
      }
      c.execute(
        "INSERT INTO branches (table_name, name, head) VALUES ('shed_history','main','t5')
         ON CONFLICT(table_name,name) DO UPDATE SET head='t5'",
        [],
      )
      .unwrap();
    }

    // re-opening triggers the repair
    let _h = History::new(conn.clone(), MAIN_HIST_TABLE_NAME, "main").unwrap();

    let roots: i64 = conn
      .lock()
      .unwrap()
      .query_row(
        "SELECT COUNT(*) FROM shed_history WHERE parent IS NULL",
        [],
        |r| r.get(0),
      )
      .unwrap();
    assert_eq!(roots, 1, "repair should collapse to a single root");

    // t3 (formerly a spurious root) now chains onto t2 → t1/t2 reconnected
    let t3_parent: Option<String> = conn
      .lock()
      .unwrap()
      .query_row(
        "SELECT parent FROM shed_history WHERE token='t3'",
        [],
        |r| r.get(0),
      )
      .unwrap();
    assert_eq!(
      t3_parent,
      Some("t2".to_string()),
      "t3 should re-chain onto t2"
    );
  }

  #[test]
  fn hist_branch_delete_merged_unmerged_and_current() {
    let g = TestGuard::new();
    let main_h = fresh_branched();
    main_h.push(": base").unwrap();

    // a fully-merged branch (forks from base, never advances) → safe to -d
    test_input("hist branch merged").unwrap();
    g.read_output();
    test_input("hist branch -d merged").unwrap();
    g.read_output();
    assert_eq!(
      Shed::get_status(),
      0,
      "deleting a merged branch should succeed"
    );
    test_input("hist branch").unwrap();
    assert!(
      !g.read_output().contains("merged"),
      "branch not actually deleted"
    );

    // an unmerged branch (has a commit main lacks) → -d refuses, -D forces
    test_input("hist branch feat").unwrap();
    g.read_output();
    let conn = db::get_db_conn().unwrap();
    let feat_h = History::new(conn, MAIN_HIST_TABLE_NAME, "feat").unwrap();
    feat_h.push(": feat-only").unwrap();

    test_input("hist branch -d feat").ok();
    assert_ne!(Shed::get_status(), 0, "unmerged -d should be refused");
    test_input("hist branch").unwrap();
    assert!(
      g.read_output().contains("feat"),
      "refused delete removed it anyway"
    );

    test_input("hist branch -D feat").unwrap();
    g.read_output();
    assert_eq!(
      Shed::get_status(),
      0,
      "-D should force-delete an unmerged branch"
    );
    test_input("hist branch").unwrap();
    assert!(
      !g.read_output().contains("feat"),
      "-D did not delete the branch"
    );

    // can't delete the branch you're on
    test_input("hist branch -d main").ok();
    assert_ne!(
      Shed::get_status(),
      0,
      "deleting the current branch should be refused"
    );
  }

  #[test]
  fn delete_branch_nonexistent_errors() {
    let _g = TestGuard::new();
    let h = fresh_branched();
    h.push(": x").unwrap();
    assert!(
      h.delete_branch("ghost", false).is_err(),
      "deleting a nonexistent branch should error"
    );
  }

  #[test]
  fn delete_branch_keeps_entries_removes_pointer() {
    let _g = TestGuard::new();
    let main_h = fresh_branched();
    main_h.push(": base").unwrap();
    main_h.create_branch("feat").unwrap();

    let conn = db::get_db_conn().unwrap();
    let feat_h = History::new(conn.clone(), MAIN_HIST_TABLE_NAME, "feat").unwrap();
    feat_h.push(": feat-only").unwrap();

    // force-delete the unmerged branch
    main_h.delete_branch("feat", true).unwrap();

    assert!(
      !main_h.branch_exists("feat").unwrap(),
      "the branch pointer should be gone"
    );
    // the entry itself survives (unreachable, but not deleted)
    let count: i64 = conn
      .lock()
      .unwrap()
      .query_row(
        "SELECT COUNT(*) FROM shed_history WHERE command = ': feat-only'",
        [],
        |r| r.get(0),
      )
      .unwrap();
    assert_eq!(
      count, 1,
      "delete should keep entries, only drop the pointer"
    );
  }

  #[test]
  fn delete_branch_logs_reflog_with_recoverable_head() {
    let _g = TestGuard::new();
    let main_h = fresh_branched();
    let base = main_h.push(": base").unwrap().unwrap();
    main_h.create_branch("temp").unwrap(); // temp.head == base (merged: never advanced)

    main_h.delete_branch("temp", false).unwrap();

    let conn = db::get_db_conn().unwrap();
    let (branch, op, old_head, new_head, tbl): (
      String,
      String,
      Option<String>,
      Option<String>,
      String,
    ) = conn
      .lock()
      .unwrap()
      .query_row(
        "SELECT branch, op, old_head, new_head, table_name FROM reflog
           WHERE op = 'delete' ORDER BY id DESC LIMIT 1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
      )
      .unwrap();
    assert_eq!(branch, "temp");
    assert_eq!(op, "delete");
    assert_eq!(tbl, "shed_history");
    assert_eq!(new_head, None, "a delete has no new head");
    assert_eq!(
      old_head,
      Some(base.to_string()),
      "reflog should record the deleted branch's head for recovery"
    );
  }
}
