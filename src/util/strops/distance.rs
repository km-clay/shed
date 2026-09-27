//! Damerau-Levenshtein edit distance, used for typo suggestions.

use std::collections::VecDeque;

// this needs to be at least 2
pub(crate) const EDIT_WEIGHT: usize = 2;

pub(crate) fn levenshtein(left: &[u8], right: &[u8]) -> usize {
  /*
   * Levenshtein algorithm
   * https://en.wikipedia.org/wiki/Levenshtein_distance
   *
   * Given two strings, find the minimum number of edits required for one string to be turned into the other.
   * Useful for check typos, e.g. `gti` -> "Did you mean 'git'?"
   */
  let m = left.len();
  let n = right.len();

  // We are using the Damerau transposition checks, so we need
  // bookkeeping for three rows.
  let mut prev: Vec<usize> = (0..=n).map(|j| j * EDIT_WEIGHT).collect();
  let mut prev2: Vec<usize> = vec![0usize; n + 1]; // this is the row before prev, used for transposition
  let mut curr: Vec<usize> = vec![0usize; n + 1];

  // Since we are tracking three rows, we need an easy way to rotate them
  // as we iterate through the strings. VecDeque will do nicely for this
  let mut rows: VecDeque<&mut Vec<usize>> = [&mut prev2, &mut prev, &mut curr].into();

  // minimum of three values macro thing
  macro_rules! min3 {
    ($a:expr, $b:expr, $c:expr) => {
      ::std::cmp::min(::std::cmp::min($a, $b), $c)
    };
  }

  // Damerau-Levenshtein: check for transposition
  let check_transpose = |i: usize, j: usize| -> bool {
    i >= 2 && j >= 2 && left[i - 1] == right[j - 2] && left[i - 2] == right[j - 1]
  };
  // transposition costs half as much as an edit
  // so that typos like 'gti' -> 'git' match only on 'git'
  let transpose_cost = EDIT_WEIGHT / 2;

  for i in 1..=m {
    rows[2][0] = i * EDIT_WEIGHT; // base case: first column is i deletions from
    // the empty prefix of the other string
    for j in 1..=n {
      rows[2][j] = if left[i - 1] == right[j - 1] {
        // both bytes match, free move
        rows[1][j - 1]
      } else {
        // Price each edit against its own predecessor. A substitution,
        // deletion, or insertion each cost EDIT_WEIGHT
        let sub = rows[1][j - 1] + EDIT_WEIGHT; // substitution
        let del = rows[1][j] + EDIT_WEIGHT; // deletion
        let ins = rows[2][j - 1] + EDIT_WEIGHT; // insertion
        let mut best = min3!(sub, del, ins);

        if check_transpose(i, j) {
          // transpositions cost half
          best = best.min(rows[0][j - 2] + transpose_cost);
        }

        best
      }
    }

    rows.rotate_left(1);
  }

  // return the bottom right value
  // thank you Vladimir Levenshtein
  rows[1][n]
}

#[cfg(test)]
mod levenshtein_tests {
  use super::{EDIT_WEIGHT, levenshtein};

  /// Convenience wrapper so the cases read as strings.
  fn lev(a: &str, b: &str) -> usize {
    levenshtein(a.as_bytes(), b.as_bytes())
  }

  // ─── identity & empty strings ────────────────────────────────────

  #[test]
  fn identical_is_zero() {
    assert_eq!(lev("cat", "cat"), 0);
    assert_eq!(lev("", ""), 0);
  }

  // Regression: the DP boundary must be *weighted*. Building an n-byte string
  // from the empty string is n insertions, each costing EDIT_WEIGHT — not a
  // raw 0,1,2,... count. (Previously `("" -> "abc")` returned 3 instead of 6.)
  #[test]
  fn empty_boundary_is_weighted() {
    assert_eq!(lev("", "abc"), 3 * EDIT_WEIGHT);
    assert_eq!(lev("abc", ""), 3 * EDIT_WEIGHT);
    assert_eq!(lev("", "a"), EDIT_WEIGHT);
  }

  // ─── single ordinary edits each cost EDIT_WEIGHT ─────────────────

  #[test]
  fn one_substitution() {
    assert_eq!(lev("a", "b"), EDIT_WEIGHT);
  }

  #[test]
  fn one_insertion() {
    assert_eq!(lev("a", "ab"), EDIT_WEIGHT);
  }

  #[test]
  fn one_deletion() {
    assert_eq!(lev("ab", "a"), EDIT_WEIGHT);
  }

  // Regression for the base-case bug: a leading deletion runs the optimal
  // alignment along the boundary, and must still cost a full edit. `cat -> at`
  // previously came back as 1 instead of EDIT_WEIGHT.
  #[test]
  fn boundary_hugging_indel_is_full_weight() {
    assert_eq!(lev("cat", "at"), EDIT_WEIGHT); // leading deletion
    assert_eq!(lev("cat", "cats"), EDIT_WEIGHT); // trailing insertion
  }

  // ─── transposition is the Damerau feature: half an edit ──────────

  #[test]
  fn adjacent_transposition_is_half() {
    let half = EDIT_WEIGHT / 2;
    assert_eq!(lev("ab", "ba"), half);
    assert_eq!(lev("teh", "the"), half);
    assert_eq!(lev("gti", "git"), half);
    assert_eq!(lev("grpe", "grep"), half);
    assert_eq!(lev("dokcer", "docker"), half);
  }

  // The whole point of weighting the transposition: `gti` should read as a
  // single swap of `git` (cheap), strictly beating the substitution `gtp` and
  // the deletion `gt` (both a full edit). This is what makes the typo suggester
  // surface `git` alone instead of tied three ways.
  #[test]
  fn transposition_beats_ordinary_edits() {
    assert!(lev("gti", "git") < lev("gti", "gtp"));
    assert!(lev("gti", "git") < lev("gti", "gt"));
    assert_eq!(lev("gti", "gtp"), EDIT_WEIGHT); // substitution
    assert_eq!(lev("gti", "gt"), EDIT_WEIGHT); // deletion
  }

  // Regression for the transpose-leak bug: `ab -> aba` and `eco -> echo` are
  // plain insertions that happen to satisfy the local transposition predicate,
  // but the cheap transposition price must NOT leak onto a move that wasn't
  // actually a transposition. Both previously returned 1 instead of EDIT_WEIGHT.
  #[test]
  fn insertion_masquerading_as_transposition_is_full_weight() {
    assert_eq!(lev("ab", "aba"), EDIT_WEIGHT);
    assert_eq!(lev("eco", "echo"), EDIT_WEIGHT);
  }

  // ─── multi-edit ──────────────────────────────────────────────────

  #[test]
  fn kitten_sitting_is_three_edits() {
    // k→s, e→i, and insert g: three ordinary edits, no transposition.
    assert_eq!(lev("kitten", "sitting"), 3 * EDIT_WEIGHT);
  }

  // ─── metric properties ───────────────────────────────────────────

  #[test]
  fn distance_is_symmetric() {
    for (a, b) in [
      ("kitten", "sitting"),
      ("dokcer", "docker"),
      ("cat", "at"),
      ("ab", "aba"),
      ("gti", "git"),
      ("", "abc"),
    ] {
      assert_eq!(lev(a, b), lev(b, a), "asymmetric on {a:?} / {b:?}");
    }
  }
}
