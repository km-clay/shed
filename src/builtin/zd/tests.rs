use std::fs;

use tempfile::TempDir;

use crate::{
  state::db,
  tests::testutil::{TestGuard, test_input},
};

// ===================== zd: frecency =====================

#[test]
fn frecency_recent_outranks_old() {
  // Same visit count, more recent wins.
  assert!(super::dir_frecency(3, 60) > super::dir_frecency(3, 60 * 60 * 24 * 30));
}

#[test]
fn frecency_more_visits_outranks_fewer() {
  // Same age bucket, more visits wins.
  assert!(super::dir_frecency(10, 60) > super::dir_frecency(1, 60));
}

#[test]
fn frecency_saturates_without_overflow() {
  assert_eq!(super::dir_frecency(i64::MAX, 60), i32::MAX);
}

// ===================== zd: directory scoring =====================

fn qchars(s: &str) -> Vec<char> {
  s.chars().collect()
}

#[test]
fn score_dir_exact_path_wins() {
  let q      = qchars("/home/me");
  let exact  = super::fuzzy_score_dir(&"/home/me".into(), &q, false);
  let longer = super::fuzzy_score_dir(&"/home/me/projects".into(), &q, false);
  assert_eq!(exact, i32::MAX);
  assert!(
    exact > longer,
    "exact path must outrank a longer prefix match"
  );
}

#[test]
fn score_dir_basename_outranks_smeared() {
  let q        = qchars("dev");
  // Basename "dev" matches cleanly; the other only matches across parent segments.
  let basename = super::fuzzy_score_dir(&"/a/b/dev".into(), &q, false);
  let smeared  = super::fuzzy_score_dir(&"/d/e/v/zzz".into(), &q, false);
  assert!(basename > smeared);
}

#[test]
fn score_dir_no_match_is_min() {
  let q = qchars("zzz");
  assert_eq!(
    super::fuzzy_score_dir(&"/home/me/projects".into(), &q, false),
    i32::MIN
  );
}

// ===================== zd: highlighting =====================

#[test]
fn highlight_dir_marks_basename_with_offset() {
  // "/home/me/" is 9 chars, so the basename match lands at 9,10,11.
  let pos = super::highlight_dir("/home/me/dev-shells", "dev").unwrap();
  assert_eq!(pos, vec![9, 10, 11]);
}

#[test]
fn highlight_dir_falls_back_when_basename_unmatched() {
  // Query matches only the parents → None, so the caller uses the full-path match.
  assert!(super::highlight_dir("/home/dev/xyz", "dev").is_none());
}

#[test]
fn highlight_dir_none_without_basename() {
  assert!(super::highlight_dir("/", "x").is_none());
}

// ===================== zd: dir_history DB =====================

fn fresh_dir_history() {
  let conn = db::get_db_conn().expect("test db");
  conn
    .lock()
    .unwrap()
    .execute_batch(
      "CREATE TABLE IF NOT EXISTS dir_history (
         path        TEXT     PRIMARY KEY NOT NULL,
         visits      INTEGER  NOT NULL DEFAULT 1,
         last_visit  INTEGER  NOT NULL
       );
       DELETE FROM dir_history;",
    )
    .unwrap();
}

fn dir_visits(path: &str) -> Option<i64> {
  let conn = db::get_db_conn().unwrap();
  let conn = conn.lock().unwrap();
  conn
    .query_row(
      "SELECT visits FROM dir_history WHERE path = ?1",
      [path],
      |r| r.get(0),
    )
    .ok()
}

fn insert_dir(path: &str, visits: i64, last_visit: i64) {
  let conn = db::get_db_conn().unwrap();
  conn
    .lock()
    .unwrap()
    .execute(
      "INSERT OR REPLACE INTO dir_history (path, visits, last_visit) VALUES (?1, ?2, ?3)",
      rusqlite::params![path, visits, last_visit],
    )
    .unwrap();
}

#[test]
fn zd_add_inserts_canonical_path() {
  let _g = TestGuard::new();
  fresh_dir_history();
  let dir = TempDir::new().unwrap();
  test_input(format!("zd add {}", dir.path().display())).unwrap();
  let canon = fs::canonicalize(dir.path()).unwrap().display().to_string();
  assert_eq!(dir_visits(&canon), Some(1));
}

#[test]
fn zd_add_is_idempotent() {
  let _g = TestGuard::new();
  fresh_dir_history();
  let dir = TempDir::new().unwrap();
  let cmd = format!("zd add {}", dir.path().display());
  test_input(&cmd).unwrap();
  test_input(&cmd).unwrap();
  let canon = fs::canonicalize(dir.path()).unwrap().display().to_string();
  // ON CONFLICT DO NOTHING: re-adding must not inflate the visit count.
  assert_eq!(dir_visits(&canon), Some(1));
}

#[test]
fn zd_remove_deletes_entry() {
  let _g = TestGuard::new();
  fresh_dir_history();
  let dir   = TempDir::new().unwrap();
  let canon = fs::canonicalize(dir.path()).unwrap().display().to_string();
  insert_dir(&canon, 5, 1000);
  test_input(format!("zd remove {}", dir.path().display())).unwrap();
  assert_eq!(dir_visits(&canon), None);
}

#[test]
fn zd_clean_prunes_only_dead_dirs() {
  let _g = TestGuard::new();
  fresh_dir_history();
  let live       = TempDir::new().unwrap();
  let live_canon = fs::canonicalize(live.path()).unwrap().display().to_string();
  insert_dir(&live_canon, 1, 1000);
  insert_dir("/nonexistent_zz_dir_12345", 1, 1000);
  test_input("zd clean").unwrap();
  assert!(
    dir_visits(&live_canon).is_some(),
    "existing dir must be kept"
  );
  assert_eq!(
    dir_visits("/nonexistent_zz_dir_12345"),
    None,
    "dead dir must be pruned"
  );
}

#[test]
fn load_dir_entries_skips_missing_dirs() {
  let _g = TestGuard::new();
  fresh_dir_history();
  let live       = TempDir::new().unwrap();
  let live_canon = fs::canonicalize(live.path()).unwrap().display().to_string();
  insert_dir(&live_canon, 3, 1000);
  insert_dir("/nonexistent_zz_dir_98765", 9, 9999);
  let entries = super::load_dir_entries();
  assert!(entries.iter().any(|(p, _)| p == &live_canon));
  assert!(!entries.iter().any(|(p, _)| p.starts_with("/nonexistent")));
}
