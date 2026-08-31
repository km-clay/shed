use crate::assert_status_eq;
use crate::state;
use crate::tests::testutil::{TestGuard, test_input};

// ===================== while/until status =====================

#[test]
fn while_loop_status_zero_after_completion() {
  let _g = TestGuard::new();
  test_input("while false; do :; done").unwrap();
  assert_eq!(state::Shed::get_status(), 0);
}

#[test]
fn while_loop_status_zero_after_iterations() {
  let _g = TestGuard::new();
  test_input("X=0; while [[ $X -lt 3 ]]; do X=$((X+1)); done").unwrap();
  assert_eq!(state::Shed::get_status(), 0);
}

#[test]
fn until_loop_status_zero_after_completion() {
  let _g = TestGuard::new();
  test_input("until true; do :; done").unwrap();
  assert_eq!(state::Shed::get_status(), 0);
}

#[test]
fn until_loop_status_zero_after_iterations() {
  let _g = TestGuard::new();
  test_input("X=3; until [[ $X -le 0 ]]; do X=$((X-1)); done").unwrap();
  assert_eq!(state::Shed::get_status(), 0);
}

#[test]
fn while_break_preserves_status() {
  let _g = TestGuard::new();
  test_input("while true; do break; done").unwrap();
  assert_eq!(state::Shed::get_status(), 0);
}

#[test]
fn until_body_status_propagates() {
  let _g = TestGuard::new();
  // Same POSIX rule as `while`: the loop's status is the last body's.
  test_input("X=0; until [[ $X -ge 1 ]]; do X=$((X+1)); false; done").unwrap();
  assert_eq!(state::Shed::get_status(), 1);
}

#[test]
fn for_arith_body_status_propagates() {
  let _g = TestGuard::new();
  // C-style `for` reports the last body's status, captured before the step
  // expression (which would otherwise overwrite `$?`).
  test_input("for ((i = 0; i < 2; i++)); do false; done").unwrap();
  assert_eq!(state::Shed::get_status(), 1);
}

#[test]
fn for_arith_empty_body_status_zero() {
  let _g = TestGuard::new();
  test_input("for ((i = 0; i < 0; i++)); do false; done").unwrap();
  assert_eq!(state::Shed::get_status(), 0);
}

#[test]
fn while_body_status_propagates() {
  let _g = TestGuard::new();
  test_input("X=0; while [[ $X -lt 1 ]]; do X=$((X+1)); false; done").unwrap();
  // POSIX §2.9.4: the loop's status is that of the last body executed. The
  // body ended with `false`, so the loop's status is 1 (matches bash/dash).
  assert_eq!(state::Shed::get_status(), 1);
}

// ===================== pipeline `exit` stage =====================

#[test]
fn pipeline_last_stage_exit_sets_status() {
  // A bare `exit N` pipeline stage sets the pipeline status to N (bash), not
  // print the CleanExit box and drop it. `exit` forks (always_forks), so this
  // exercises the forked-builtin CleanExit handling in `run_fork`/`exec_builtin`.
  let _g = TestGuard::new();
  test_input("false | exit 3").unwrap();
  assert_eq!(state::Shed::get_status(), 3);
}

#[test]
fn pipeline_exit_stage_respects_pipefail() {
  let _g = TestGuard::new();
  test_input("set -o pipefail; true | exit 4").unwrap();
  assert_eq!(state::Shed::get_status(), 4);
}

#[test]
fn pipeline_exit_stage_does_not_kill_shell() {
  // `exit` in a stage terminates only that stage's subshell; the parent shell
  // keeps running the rest of the input.
  let g = TestGuard::new();
  test_input("echo hi | exit 0; echo marker").unwrap();
  assert!(
    g.read_output().contains("marker"),
    "shell did not continue after a pipeline `exit` stage"
  );
}

// ===================== if/elif/else status =====================

#[test]
fn if_true_body_status() {
  let _g = TestGuard::new();
  test_input("if true; then echo ok; fi").unwrap();
  assert_eq!(state::Shed::get_status(), 0);
}

#[test]
fn if_false_no_else_status() {
  let _g = TestGuard::new();
  test_input("if false; then echo ok; fi").unwrap();
  // No branch taken, POSIX says status is 0
  assert_eq!(state::Shed::get_status(), 0);
}

#[test]
fn if_else_branch_status() {
  let _g = TestGuard::new();
  test_input("if false; then true; else false; fi").unwrap();
  assert_eq!(state::Shed::get_status(), 1);
}

// ===================== for loop status =====================

#[test]
fn for_loop_empty_list_status() {
  let _g = TestGuard::new();
  test_input("for x in; do echo $x; done").unwrap();
  assert_eq!(state::Shed::get_status(), 0);
}

#[test]
fn for_loop_body_status() {
  let _g = TestGuard::new();
  test_input("for x in a b c; do true; done").unwrap();
  assert_eq!(state::Shed::get_status(), 0);
}

#[test]
fn for_loop_empty_array_at_zero_iterations() {
  // Regression: ${arr[@]} on an empty array used to emit one empty
  // word instead of zero words, causing `for i in "${arr[@]}"` to
  // loop once with $i="" instead of looping zero times. Symmetric
  // with "$@" when there are no positional args.
  //
  // Note: bash distinguishes arr=() (zero words) from arr=("")
  // (one empty word). Shed currently collapses both at the downstream
  // expansion layer; only the zero-elements case is fixed. The
  // one-empty-element case is a separate, deeper bug that needs
  // word-splitting changes — not covered here.
  let guard = TestGuard::new();
  test_input("arr=()").unwrap();
  test_input(r#"for i in "${arr[@]}"; do echo loop; done"#).unwrap();
  let out = guard.read_output();
  assert_eq!(out, "");
}

#[test]
fn for_loop_empty_assoc_array_zero_iterations() {
  let guard = TestGuard::new();
  test_input("declare -A am").unwrap();
  test_input(r#"for i in "${am[@]}"; do echo loop; done"#).unwrap();
  let out = guard.read_output();
  assert_eq!(out, "");
}

// ===================== case status =====================

#[test]
fn case_match_status() {
  let _g = TestGuard::new();
  test_input("case foo in foo) true;; esac").unwrap();
  assert_eq!(state::Shed::get_status(), 0);
}

#[test]
fn case_no_match_status() {
  let _g = TestGuard::new();
  test_input("case foo in bar) true;; esac").unwrap();
  assert_eq!(state::Shed::get_status(), 0);
}

// ===================== case pattern whitespace / paren / alternative =====================
// Regressions for issue #52: POSIX permits whitespace before `)`, an
// optional leading `(`, and `|` alternatives with arbitrary whitespace.

#[test]
fn case_space_before_close_paren() {
  let g = TestGuard::new();
  test_input("case x in * ) echo hit ;; esac").unwrap();
  assert_eq!(g.read_output(), "hit\n");
}

#[test]
fn case_leading_open_paren() {
  let g = TestGuard::new();
  test_input("case x in (*) echo hit ;; esac").unwrap();
  assert_eq!(g.read_output(), "hit\n");
}

#[test]
fn case_leading_paren_with_inner_whitespace() {
  let g = TestGuard::new();
  test_input("case x in ( * ) echo hit ;; esac").unwrap();
  assert_eq!(g.read_output(), "hit\n");
}

#[test]
fn case_pipe_alternatives_no_spaces() {
  let g = TestGuard::new();
  test_input("case b in a|b|c) echo hit ;; esac").unwrap();
  assert_eq!(g.read_output(), "hit\n");
}

#[test]
fn case_pipe_alternatives_with_spaces() {
  let g = TestGuard::new();
  test_input("case b in a | b | c ) echo hit ;; esac").unwrap();
  assert_eq!(g.read_output(), "hit\n");
}

#[test]
fn case_paren_wrapped_alternatives() {
  let g = TestGuard::new();
  test_input("case b in (a | b | c) echo hit ;; esac").unwrap();
  assert_eq!(g.read_output(), "hit\n");
}

#[test]
fn case_quoted_pattern_with_space_is_literal() {
  let g = TestGuard::new();
  test_input("case 'foo bar' in \"foo bar\") echo hit ;; *) echo miss ;; esac").unwrap();
  assert_eq!(g.read_output(), "hit\n");
}

#[test]
fn case_glob_pattern_still_works() {
  let g = TestGuard::new();
  test_input("case hello.txt in *.txt ) echo hit ;; esac").unwrap();
  assert_eq!(g.read_output(), "hit\n");
}

#[test]
fn case_question_matches_exactly_one_char() {
  // `?` is one character, not one-or-more, and the pattern matches the whole
  // string — not a substring (issue #129).
  let g = TestGuard::new();
  test_input("case ab in ?) echo 1 ;; ??) echo 2 ;; ???) echo 3 ;; esac").unwrap();
  assert_eq!(g.read_output(), "2\n");
}

#[test]
fn case_glob_is_anchored_not_substring() {
  // A `?`/`[...]` pattern must match the whole word, not appear inside it.
  let g = TestGuard::new();
  test_input("case xabcy in a?c) echo hit ;; *) echo miss ;; esac").unwrap();
  assert_eq!(g.read_output(), "miss\n");
  let g = TestGuard::new();
  test_input("case zab in [ab][ab]) echo hit ;; *) echo miss ;; esac").unwrap();
  assert_eq!(g.read_output(), "miss\n");
}

#[test]
fn case_empty_matched_arm_status_zero() {
  // A matched arm that runs no command exits 0, not the prior status (#128).
  let _g = TestGuard::new();
  test_input("false; case a in a) ;; esac").unwrap();
  assert_eq!(state::Shed::get_status(), 0);
}

#[test]
fn case_no_match_status_zero() {
  let _g = TestGuard::new();
  test_input("false; case a in b) echo no ;; esac").unwrap();
  assert_eq!(state::Shed::get_status(), 0);
}

#[test]
fn case_word_expansion_status_does_not_leak() {
  // The status of the word's own expansion must not become the case's status.
  let _g = TestGuard::new();
  test_input("case $(exit 7) in x) ;; esac").unwrap();
  assert_eq!(state::Shed::get_status(), 0);
}

#[test]
fn case_matched_arm_command_status_wins() {
  // A matched arm that runs a command reports that command's status.
  let _g = TestGuard::new();
  test_input("true; case a in a) false ;; esac").unwrap();
  assert_eq!(state::Shed::get_status(), 1);
}

#[test]
fn case_multiple_paren_wrapped_arms() {
  let g = TestGuard::new();
  test_input("case mid in (first) echo a ;; (mid) echo b ;; (*) echo c ;; esac").unwrap();
  assert_eq!(g.read_output(), "b\n");
}

// ===================== other stuff =====================

#[test]
fn for_loop_var_zip() {
  let g = TestGuard::new();
  test_input("for a b in 1 2 3 4 5 6; do echo $a $b; done").unwrap();
  let out = g.read_output();
  assert_eq!(out, "1 2\n3 4\n5 6\n");
}

#[test]
fn for_loop_unsets_zipped() {
  let g = TestGuard::new();
  test_input("for a b c d in 1 2 3 4 5 6; do echo $a $b $c $d; done").unwrap();
  let out = g.read_output();
  assert_eq!(out, "1 2 3 4\n5 6\n");
}

// ===================== set -e + builtin failure =====================

#[test]
fn set_e_aborts_on_failing_builtin() {
  // `cd /nonexistent` exits non-zero from a builtin. Under `set -e`
  // the failure should propagate as ErrInterrupt and prevent the
  // following command from running, same as for external commands.
  // Regression guard against builtins being silently exempted from
  // errexit checks.
  let g = TestGuard::new();
  let result = test_input("set -e; cd /__set_e_test_no_such_dir_xyz__; echo SHOULD_NOT_RUN");
  assert!(
    result.is_err(),
    "expected set -e to surface ErrInterrupt for failing builtin"
  );
  let out = g.read_output();
  assert!(
    !out.contains("SHOULD_NOT_RUN"),
    "set -e should have aborted before the second command ran; got: {out:?}"
  );
}

#[test]
fn no_set_e_continues_past_failing_builtin() {
  // Companion to the above: without `set -e`, the failing cd should
  // set $? but execution should continue to the echo. Establishes
  // that the previous test is meaningful (the abort is *because of*
  // set -e, not because builtin errors always abort).
  let g = TestGuard::new();
  test_input("cd /__set_e_test_no_such_dir_xyz__; echo did_continue").unwrap();
  let out = g.read_output();
  assert!(
    out.contains("did_continue"),
    "without set -e, execution should continue past the failed cd; got: {out:?}"
  );
}

// ===================== negation (!) status =====================

#[test]
fn negate_true() {
  let _g = TestGuard::new();
  test_input("! true").unwrap();
  assert_eq!(state::Shed::get_status(), 1);
}

#[test]
fn negate_false() {
  let _g = TestGuard::new();
  test_input("! false").unwrap();
  assert_eq!(state::Shed::get_status(), 0);
}

#[test]
fn double_negate_true() {
  let _g = TestGuard::new();
  test_input("! ! true").unwrap();
  assert_eq!(state::Shed::get_status(), 0);
}

#[test]
fn double_negate_false() {
  let _g = TestGuard::new();
  test_input("! ! false").unwrap();
  assert_eq!(state::Shed::get_status(), 1);
}

#[test]
fn negate_pipeline_last_cmd() {
  let _g = TestGuard::new();
  // pipeline status = last cmd (false) = 1, negated -> 0
  test_input("! true | false").unwrap();
  assert_eq!(state::Shed::get_status(), 0);
}

#[test]
fn negate_pipeline_last_cmd_true() {
  let _g = TestGuard::new();
  // pipeline status = last cmd (true) = 0, negated -> 1
  test_input("! false | true").unwrap();
  assert_eq!(state::Shed::get_status(), 1);
}

#[test]
fn negate_in_conjunction() {
  let _g = TestGuard::new();
  // ! binds to pipeline, not conjunction: (! (true && false)) && true
  test_input("! (true && false) && true").unwrap();
  assert_eq!(state::Shed::get_status(), 0);
}

#[test]
fn negate_in_if_condition() {
  let g = TestGuard::new();
  test_input("if ! false; then echo yes; fi").unwrap();
  assert_eq!(state::Shed::get_status(), 0);
  assert_eq!(g.read_output(), "yes\n");
}

#[test]
fn empty_var_in_test() {
  let _g = TestGuard::new();
  // Quoted unset variable expands to an empty string — `[ -n "" ]` is false.
  test_input("[ -n \"$EMPTYVAR_PROBABLY_NOT_SET_TO_ANYTHING\" ]").unwrap();
  assert_eq!(state::Shed::get_status(), 1);
  // POSIX `[`: the unset/unquoted operand vanishes via word-splitting, so
  // argv reaching the builtin is just `[ -n ]`. The arity-1 rule treats the
  // lone `-n` as a literal string to test for non-emptiness (true, status 0).
  test_input("[ -n $EMPTYVAR_PROBABLY_NOT_SET_TO_ANYTHING ]").unwrap();
  assert_eq!(state::Shed::get_status(), 0);
}

// ===================== command lists in compound statements =====================
// POSIX §2.9.4: conditions and bodies of compound statements are command lists,
// not single commands. Multiple statements separated by `;` or `\n` are valid;
// the exit status of the last command in the list determines the condition.

#[test]
fn if_multi_stmt_condition_last_true() {
  let _g = TestGuard::new();
  test_input("if true; true; then false; fi").unwrap();
  // Condition's last command (true) → enters then-branch → false → status 1
  assert_eq!(state::Shed::get_status(), 1);
}

#[test]
fn if_multi_stmt_condition_last_false() {
  let _g = TestGuard::new();
  test_input("if true; false; then echo a; else echo b; fi").unwrap();
  // Condition's last command (false) → else-branch
  assert_eq!(state::Shed::get_status(), 0);
}

#[test]
fn if_multi_stmt_condition_output() {
  let g = TestGuard::new();
  test_input("if echo a; echo b; then echo c; fi").unwrap();
  // All three commands run; condition's last echo is success
  let out = g.read_output();
  assert_eq!(out, "a\nb\nc\n");
}

#[test]
fn if_multi_stmt_body() {
  let g = TestGuard::new();
  test_input("if true; then echo a; echo b; echo c; fi").unwrap();
  let out = g.read_output();
  assert_eq!(out, "a\nb\nc\n");
}

#[test]
fn if_multi_stmt_body_status_is_last() {
  let _g = TestGuard::new();
  test_input("if true; then true; false; fi").unwrap();
  // Body's last command (false) determines if-statement's status
  assert_eq!(state::Shed::get_status(), 1);
}

// ===================== scope sharing between cond and body =====================

#[test]
fn if_cond_assignment_visible_in_body() {
  // Assignments in an if-condition need to be visible to the matching
  // body — that's the motivation for sharing a scope between cond and
  // body. Regression for `if foo=$(...); then use $foo; fi`.
  let g = TestGuard::new();
  test_input("if foo=hello; then echo \"got $foo\"; fi").unwrap();
  assert_eq!(g.read_output(), "got hello\n");
}

#[test]
fn while_cond_assignment_visible_in_body() {
  // `while line=$(read_next); do use $line; done` style: cond writes a
  // variable each iteration, body reads it.
  let g = TestGuard::new();
  test_input(
    r#"
    i=0
    while i=$((i + 1)); [ $i -le 3 ]; do
    echo "iter $i"
    done
    "#,
  )
  .unwrap();
  assert_eq!(g.read_output(), "iter 1\niter 2\niter 3\n");
}

#[test]
fn while_cond_mutations_persist_across_iterations() {
  // Regression for the OPTIND-in-getopts case: a while-loop's cond and
  // body share a single scope spanning all iterations, so mutations made
  // in cond on iteration N are visible on iteration N+1. If the scope
  // were per-iteration, this would loop forever (counter resets each
  // time) or exit immediately depending on the variable.
  let g = TestGuard::new();
  test_input(
    r#"
    n=0
    while n=$((n + 1)); [ $n -lt 4 ]; do
    echo "tick"
    done
    echo "final n=$n"
    "#,
  )
  .unwrap();
  assert_eq!(g.read_output(), "tick\ntick\ntick\nfinal n=4\n");
}

#[test]
fn while_multi_stmt_condition_never_enters() {
  let g = TestGuard::new();
  test_input("while echo a; false; do echo b; done").unwrap();
  // Condition's last command (false) → loop never enters body
  let out = g.read_output();
  assert_eq!(out, "a\n");
}

#[test]
fn until_multi_stmt_condition() {
  let g = TestGuard::new();
  test_input("x=0; until echo iter; [ $x -ge 2 ]; do x=$((x+1)); done").unwrap();
  // Condition's last command negated → loops until [ $x -ge 2 ] is true
  let out = g.read_output();
  assert_eq!(out, "iter\niter\niter\n");
}

#[test]
fn brc_grp_multi_stmt_body() {
  let g = TestGuard::new();
  test_input("{ echo a; echo b; echo c; }").unwrap();
  let out = g.read_output();
  assert_eq!(out, "a\nb\nc\n");
}

#[test]
fn func_multi_stmt_body() {
  let g = TestGuard::new();
  test_input("f() { echo a; echo b; echo c; }; f").unwrap();
  let out = g.read_output();
  assert_eq!(out, "a\nb\nc\n");
}

#[test]
fn for_multi_stmt_body() {
  let g = TestGuard::new();
  test_input("for x in 1 2; do echo $x; echo done-$x; done").unwrap();
  let out = g.read_output();
  assert_eq!(out, "1\ndone-1\n2\ndone-2\n");
}

#[test]
fn case_arm_multi_stmt_body() {
  let g = TestGuard::new();
  test_input("case foo in foo) echo a; echo b; echo c;; esac").unwrap();
  let out = g.read_output();
  assert_eq!(out, "a\nb\nc\n");
}

#[test]
fn backgrounded_brace_group_forks_no_state_leak() {
  // Regression (Bug A): a backgrounded builtin/brace-group must run in a
  // child, not inline. If it ran inline, the assignment would leak into the
  // parent and `$x` would be 5.
  let g = TestGuard::new();
  test_input("x=0; { x=5; } & wait; echo \"[$x]\"").unwrap();
  let out = g.read_output();
  assert!(
    out.contains("[0]"),
    "brace group leaked into parent: {out:?}"
  );
}

#[test]
fn backgrounded_builtin_reports_real_status_via_wait() {
  // Regression (Bug A + wait ECHILD): `wait` on a finished backgrounded
  // builtin reports its real exit status, not 0.
  let g = TestGuard::new();
  test_input("false & wait %1; echo \"[$?]\"").unwrap();
  let out = g.read_output();
  assert!(out.contains("[1]"), "got: {out:?}");
}

#[test]
fn mixed_and_or_with_sequence() {
  let g = TestGuard::new();
  test_input("true && echo a; false || echo b; echo c").unwrap();
  // && and || chains coexist with ; sequencing — all three echos run
  let out = g.read_output();
  assert_eq!(out, "a\nb\nc\n");
}

#[test]
fn nested_compounds_with_lists() {
  let g = TestGuard::new();
  test_input("if true; true; then if true; then echo a; echo b; fi; echo c; fi").unwrap();
  let out = g.read_output();
  assert_eq!(out, "a\nb\nc\n");
}

#[test]
fn top_level_sequence_runs_all() {
  let g = TestGuard::new();
  test_input("echo a; echo b; echo c").unwrap();
  let out = g.read_output();
  assert_eq!(out, "a\nb\nc\n");
}

#[test]
fn top_level_sequence_status_is_last() {
  let _g = TestGuard::new();
  test_input("true; true; false").unwrap();
  assert_eq!(state::Shed::get_status(), 1);
}

// ===================== function bodies as compound commands =====================
// POSIX §2.9.5: function_body is compound_command, not just a brace group.
// Every compound command type should be a valid function body.

#[test]
fn func_body_subshell() {
  let g = TestGuard::new();
  test_input("f() ( echo a; echo b ); f").unwrap();
  let out = g.read_output();
  assert_eq!(out, "a\nb\n");
}

#[test]
fn func_body_subshell_isolates_state() {
  let g = TestGuard::new();
  // Subshell-bodied function shouldn't leak variable changes to caller.
  test_input("x=outer; f() ( x=inner; echo $x ); f; echo $x").unwrap();
  let out = g.read_output();
  assert_eq!(out, "inner\nouter\n");
}

#[test]
fn func_body_brace_grp_leaks_state() {
  let g = TestGuard::new();
  // Counter-test: brace-bodied function DOES leak (no fork).
  test_input("x=outer; f() { x=inner; echo $x; }; f; echo $x").unwrap();
  let out = g.read_output();
  assert_eq!(out, "inner\ninner\n");
}

#[test]
fn func_body_if() {
  let g = TestGuard::new();
  test_input("f() if true; then echo yes; else echo no; fi; f").unwrap();
  let out = g.read_output();
  assert_eq!(out, "yes\n");
}

#[test]
fn func_body_if_takes_arg() {
  let g = TestGuard::new();
  test_input("f() if [ \"$1\" = ok ]; then echo good; else echo bad; fi; f ok; f nope").unwrap();
  let out = g.read_output();
  assert_eq!(out, "good\nbad\n");
}

#[test]
fn func_body_while() {
  let g = TestGuard::new();
  test_input("f() while [ $i -lt 3 ]; do echo $i; i=$((i+1)); done; i=0; f").unwrap();
  let out = g.read_output();
  assert_eq!(out, "0\n1\n2\n");
}

#[test]
fn func_body_until() {
  let g = TestGuard::new();
  test_input("f() until [ $i -ge 2 ]; do echo $i; i=$((i+1)); done; i=0; f").unwrap();
  let out = g.read_output();
  assert_eq!(out, "0\n1\n");
}

#[test]
fn func_body_for() {
  let g = TestGuard::new();
  test_input("f() for x in a b c; do echo $x; done; f").unwrap();
  let out = g.read_output();
  assert_eq!(out, "a\nb\nc\n");
}

#[test]
fn func_body_case() {
  let g = TestGuard::new();
  test_input(
    "classify() case $1 in foo) echo F;; bar) echo B;; *) echo other;; esac; \
    classify foo; classify bar; classify quux",
  )
  .unwrap();
  let out = g.read_output();
  assert_eq!(out, "F\nB\nother\n");
}

#[test]
fn func_body_status_propagates() {
  let _g = TestGuard::new();
  // Function exit status should be the last command's status, regardless
  // of which compound command shape the body uses.
  test_input("f() ( false ); f").unwrap();
  assert_eq!(state::Shed::get_status(), 1);
}

#[test]
fn func_body_recursive_with_if() {
  let g = TestGuard::new();
  // Recursive function whose body is an if-else (not a brace group).
  test_input(
    "countdown() if [ $1 -le 0 ]; then echo done; else echo $1; countdown $(($1 - 1)); fi; \
    countdown 3",
  )
  .unwrap();
  let out = g.read_output();
  assert_eq!(out, "3\n2\n1\ndone\n");
}

#[test]
fn nested_cmd_sub_index() {
  let g = TestGuard::new();
  test_input("foo=(bar biz bam); echo \"$(echo ${foo[$(echo 1)+1]})\"").unwrap();
  let out = g.read_output();
  assert_eq!(out, "bam\n");
}

#[test]
fn nested_cmd_sub_index_with_space() {
  let g = TestGuard::new();
  test_input("foo=(bar biz bam); echo \"$(echo ${foo[$(echo 1) + 1]})\"").unwrap();
  let out = g.read_output();
  assert_eq!(out, "bam\n");
}

// ===================== Assignment operators =====================

use crate::var;

// ─── Eq ─────────────────────────────────────────────────────────────

#[test]
fn assign_eq_basic() {
  let _g = TestGuard::new();
  test_input("x=hello").unwrap();
  assert_eq!(var!("x"), "hello");
}

#[test]
fn assign_eq_overwrites() {
  let _g = TestGuard::new();
  test_input("x=hello").unwrap();
  test_input("x=world").unwrap();
  assert_eq!(var!("x"), "world");
}

#[test]
fn assign_eq_cmd_sub_preserves_whitespace() {
  // Regression: assignment RHS was going through expand_to_words+join,
  // collapsing runs of whitespace into single spaces. POSIX says
  // assignment context does not apply word splitting.
  let _g = TestGuard::new();
  test_input(r#"ws=$(echo "FOO    BAR")"#).unwrap();
  assert_eq!(var!("ws"), "FOO    BAR");
}

#[test]
fn assign_eq_cmd_sub_preserves_newlines() {
  let _g = TestGuard::new();
  test_input(r"ml=$(printf 'a\nb\nc')").unwrap();
  assert_eq!(var!("ml"), "a\nb\nc");
}

// ─── Assignment exit status (POSIX §2.9.1) ─────────────────────────
// An assignment-only command's exit status is the status of the last
// command substitution performed, or 0 if there were none. The prior
// command's status must NOT leak through.

#[test]
fn assign_only_status_is_zero_with_no_cmdsub() {
  let _g = TestGuard::new();
  test_input("false; r=hi").unwrap();
  assert_status_eq!(0);
}

#[test]
fn assign_only_status_zero_does_not_leak_after_success() {
  let _g = TestGuard::new();
  test_input("true; r=hi").unwrap();
  assert_status_eq!(0);
}

#[test]
fn assign_only_status_reflects_failing_cmdsub() {
  let _g = TestGuard::new();
  test_input(r"r=$(false)").unwrap();
  assert_status_eq!(1);
}

#[test]
fn assign_only_status_reflects_failing_cmdsub_after_failure() {
  // The prior `false` must not be what sets the status; the cmdsub does.
  let _g = TestGuard::new();
  test_input(r"false; r=$(false)").unwrap();
  assert_status_eq!(1);
}

#[test]
fn assign_only_status_reflects_succeeding_cmdsub_after_failure() {
  // Key zoxide case: a failing condition before the assignment must not
  // leak through and make a succeeding cmdsub look like it failed.
  let _g = TestGuard::new();
  test_input(r"false; r=$(true)").unwrap();
  assert_status_eq!(0);
}

#[test]
fn assign_only_status_is_last_cmdsub_across_multiple() {
  let _g = TestGuard::new();
  test_input(r"r=$(false) s=$(true)").unwrap();
  assert_status_eq!(0);
}

#[test]
fn assign_only_status_is_last_cmdsub_failing() {
  let _g = TestGuard::new();
  test_input(r"r=$(true) s=$(false)").unwrap();
  assert_status_eq!(1);
}

#[test]
fn assign_only_status_is_last_cmdsub_then_literal() {
  // A trailing literal assignment doesn't reset the status; the last
  // cmdsub's status wins.
  let _g = TestGuard::new();
  test_input(r"r=$(false) s=hello").unwrap();
  assert_status_eq!(1);
}

// ─── PlusEq on strings ──────────────────────────────────────────────

#[test]
fn assign_plus_eq_string_var_concatenates() {
  // Even when both sides parse as int, `+=` on a plain string var
  // concatenates (per POSIX/bash). Arithmetic only happens for
  // `declare -i` typed vars. Regression: was doing arithmetic when
  // both sides looked numeric, producing sum-of-digits bugs when
  // building strings character-by-character.
  let _g = TestGuard::new();
  test_input("x=5; x+=3").unwrap();
  assert_eq!(var!("x"), "53");
}

#[test]
fn assign_plus_eq_int_var_does_arithmetic() {
  // declare -i opts into arithmetic +=.
  let _g = TestGuard::new();
  test_input("declare -i y=5; y+=3").unwrap();
  assert_eq!(var!("y"), "8");
}

#[test]
fn assign_plus_eq_non_numeric_concatenates() {
  let _g = TestGuard::new();
  test_input("x=hello; x+=world").unwrap();
  assert_eq!(var!("x"), "helloworld");
}

#[test]
fn assign_plus_eq_sum_of_digits_regression() {
  // Specific case from the CSV parser bug: building up a string
  // one numeric char at a time. With the broken arithmetic-sniff
  // path, this produced "9" (2+0+1+6) instead of "2016".
  let _g = TestGuard::new();
  test_input(r#"buf="""#).unwrap();
  test_input(r#"buf+="2""#).unwrap();
  test_input(r#"buf+="0""#).unwrap();
  test_input(r#"buf+="1""#).unwrap();
  test_input(r#"buf+="6""#).unwrap();
  assert_eq!(var!("buf"), "2016");
}

#[test]
fn assign_plus_eq_mixed_falls_back_to_concat() {
  let _g = TestGuard::new();
  test_input("x=5; x+=hello").unwrap();
  // RHS not parseable as int → concatenation.
  assert_eq!(var!("x"), "5hello");
}

// ─── MinusEq / MultEq / DivEq ───────────────────────────────────────

#[test]
fn assign_minus_eq_int_subtracts() {
  let _g = TestGuard::new();
  test_input("x=10; x-=4").unwrap();
  assert_eq!(var!("x"), "6");
}

#[test]
fn assign_mult_eq_multiplies() {
  let _g = TestGuard::new();
  test_input("x=6; x*=7").unwrap();
  assert_eq!(var!("x"), "42");
}

#[test]
fn assign_div_eq_divides() {
  let _g = TestGuard::new();
  test_input("x=20; x/=4").unwrap();
  assert_eq!(var!("x"), "5");
}

// Failed standalone assignments set status=1 AND leave the var
// unchanged. We check both, since either alone is weaker.

#[test]
fn assign_div_eq_by_zero_errors_and_leaves_var_unchanged() {
  let _g = TestGuard::new();
  test_input("x=5; x/=0").ok();
  assert_ne!(state::Shed::get_status(), 0);
  assert_eq!(var!("x"), "5");
}

#[test]
fn assign_minus_eq_on_non_numeric_string_errors_and_leaves_var_unchanged() {
  let _g = TestGuard::new();
  test_input("x=hello; x-=3").ok();
  assert_ne!(state::Shed::get_status(), 0);
  assert_eq!(var!("x"), "hello");
}

#[test]
fn assign_mult_eq_on_non_numeric_string_errors_and_leaves_var_unchanged() {
  let _g = TestGuard::new();
  test_input("x=hello; x*=3").ok();
  assert_ne!(state::Shed::get_status(), 0);
  assert_eq!(var!("x"), "hello");
}

// ─── Compound ops on undefined var (treated as empty) ───────────────

#[test]
fn assign_plus_eq_on_undefined_var_uses_empty_string() {
  let _g = TestGuard::new();
  // No prior `x=`. += starts from an empty Str default.
  test_input("x+=hello").unwrap();
  assert_eq!(var!("x"), "hello");
}

// ─── Compound ops on arrays ─────────────────────────────────────────

#[test]
fn assign_plus_eq_on_array_appends_scalar() {
  let g = TestGuard::new();
  test_input("arr=(a b c); arr+=d; echo ${arr[3]}").unwrap();
  let out = g.read_output();
  assert!(out.contains('d'), "got: {out:?}");
}

#[test]
fn assign_plus_eq_on_array_extends_with_array() {
  let g = TestGuard::new();
  test_input("arr=(a b); arr+=(c d); echo \"${arr[@]}\"").unwrap();
  let out = g.read_output();
  assert_eq!(out.trim(), "a b c d");
}

#[test]
fn assign_minus_eq_on_array_errors_and_leaves_array_unchanged() {
  let g = TestGuard::new();
  // Standalone `arr-=1` errors and sets status=1; subsequent
  // statements still run, so the echo proves the array wasn't
  // mutated.
  test_input("arr=(a b); arr-=1").ok();
  assert_ne!(state::Shed::get_status(), 0);
  test_input("echo \"${arr[@]}\"").unwrap();
  let out = g.read_output();
  assert!(
    out.ends_with("a b") || out.contains("\na b"),
    "got: {out:?}"
  );
}

#[test]
fn assign_mult_eq_on_array_errors_and_leaves_array_unchanged() {
  let g = TestGuard::new();
  test_input("arr=(a b); arr*=2").ok();
  assert_ne!(state::Shed::get_status(), 0);
  test_input("echo \"${arr[@]}\"").unwrap();
  let out = g.read_output();
  assert!(
    out.ends_with("a b") || out.contains("\na b"),
    "got: {out:?}"
  );
}

// ─── Indexed-array assignment ───────────────────────────────────────

#[test]
fn assign_eq_with_index_sets_element() {
  let g = TestGuard::new();
  test_input("arr=(a b c); arr[1]=X; echo \"${arr[@]}\"").unwrap();
  let out = g.read_output();
  assert_eq!(out.trim(), "a X c");
}

#[test]
fn assign_eq_with_index_extends_array() {
  let g = TestGuard::new();
  // Setting an index past the end should extend.
  test_input("arr=(a b); arr[3]=z; echo ${arr[3]}").unwrap();
  let out = g.read_output();
  assert!(out.contains('z'));
}

// ─── Export behavior ────────────────────────────────────────────────

#[test]
fn assign_with_export_sets_export_flag() {
  let g = TestGuard::new();
  // Inline assignment before a command (env var for that command).
  test_input("FOO=bar env | grep ^FOO=").unwrap();
  let out = g.read_output();
  assert!(out.contains("FOO=bar"), "got: {out:?}");
}

// ─── allexport shopt ────────────────────────────────────────────────

#[test]
fn assign_with_allexport_promotes_to_export() {
  let g = TestGuard::new();
  test_input("set -a; FOO=allexported; env | grep ^FOO=").unwrap();
  let out = g.read_output();
  assert!(out.contains("FOO=allexported"), "got: {out:?}");
}

// ===================== is_in_path =====================
mod is_in_path_tests {
  use super::super::classify::is_in_path;
  use super::super::{Span, Tk};
  use crate::eval::lex::TkRule;
  use crate::tests::testutil::{TestGuard, test_input};
  use std::os::unix::fs::PermissionsExt;
  use std::path::Path;
  use std::rc::Rc;
  use tempfile::TempDir;

  fn tk(s: &str) -> Tk {
    let src: Rc<str> = s.into();
    let span = Span::new(0..s.len(), src.as_bytes().into());
    Tk::new(TkRule::Str, span)
  }

  fn make_exec(dir: &Path, name: &str) -> std::path::PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, "#!/bin/sh\n").unwrap();
    let mut perms = std::fs::metadata(&p).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&p, perms).unwrap();
    p
  }

  fn make_non_exec(dir: &Path, name: &str) -> std::path::PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, "data").unwrap();
    let mut perms = std::fs::metadata(&p).unwrap().permissions();
    perms.set_mode(0o644);
    std::fs::set_permissions(&p, perms).unwrap();
    p
  }

  // ─── absolute paths ──────────────────────────────────────────────

  #[test]
  fn abs_path_to_executable_returns_true() {
    let _g = TestGuard::new();
    let dir = TempDir::new().unwrap();
    let exe = make_exec(dir.path(), "prog");
    assert!(is_in_path(&tk(&exe.to_string_lossy())));
  }

  #[test]
  fn abs_path_to_nonexistent_returns_false() {
    let _g = TestGuard::new();
    assert!(!is_in_path(&tk("/this/path/should/never/exist/xyz123")));
  }

  #[test]
  fn abs_path_to_directory_returns_false() {
    let _g = TestGuard::new();
    let dir = TempDir::new().unwrap();
    assert!(!is_in_path(&tk(&dir.path().to_string_lossy())));
  }

  #[test]
  fn abs_path_to_non_executable_returns_false() {
    let _g = TestGuard::new();
    let dir = TempDir::new().unwrap();
    let p = make_non_exec(dir.path(), "data.txt");
    assert!(!is_in_path(&tk(&p.to_string_lossy())));
  }

  #[test]
  fn abs_path_executable_only_group_bit_returns_true() {
    // 0o111 mask matches any of user/group/other exec bits.
    let _g = TestGuard::new();
    let dir = TempDir::new().unwrap();
    let p = dir.path().join("only_group");
    std::fs::write(&p, "").unwrap();
    let mut perms = std::fs::metadata(&p).unwrap().permissions();
    perms.set_mode(0o010); // group-execute only
    std::fs::set_permissions(&p, perms).unwrap();
    assert!(is_in_path(&tk(&p.to_string_lossy())));
  }

  // ─── bare names searched in PATH ─────────────────────────────────

  #[test]
  fn bare_name_found_in_path() {
    let _g = TestGuard::new();
    let dir = TempDir::new().unwrap();
    make_exec(dir.path(), "myprog");
    test_input(format!("PATH={}", dir.path().display())).unwrap();
    assert!(is_in_path(&tk("myprog")));
  }

  #[test]
  fn bare_name_not_in_path_returns_false() {
    let _g = TestGuard::new();
    let dir = TempDir::new().unwrap();
    test_input(format!("PATH={}", dir.path().display())).unwrap();
    assert!(!is_in_path(&tk("definitely_not_a_program_xyz")));
  }

  #[test]
  fn bare_name_found_in_second_path_entry() {
    let _g = TestGuard::new();
    let d1 = TempDir::new().unwrap();
    let d2 = TempDir::new().unwrap();
    make_exec(d2.path(), "second");
    test_input(format!(
      "PATH={}:{}",
      d1.path().display(),
      d2.path().display()
    ))
    .unwrap();
    assert!(is_in_path(&tk("second")));
  }

  #[test]
  fn bare_name_first_match_wins_even_if_later_entries_have_it() {
    // First entry has it; we still return true. Sanity check that the
    // loop terminates on first hit (no panic, correct result).
    let _g = TestGuard::new();
    let d1 = TempDir::new().unwrap();
    let d2 = TempDir::new().unwrap();
    make_exec(d1.path(), "dup");
    make_exec(d2.path(), "dup");
    test_input(format!(
      "PATH={}:{}",
      d1.path().display(),
      d2.path().display()
    ))
    .unwrap();
    assert!(is_in_path(&tk("dup")));
  }

  #[test]
  fn bare_name_skips_directory_entry() {
    let _g = TestGuard::new();
    let dir = TempDir::new().unwrap();
    // Create a *directory* with the program name — should not match.
    std::fs::create_dir(dir.path().join("subprog")).unwrap();
    test_input(format!("PATH={}", dir.path().display())).unwrap();
    assert!(!is_in_path(&tk("subprog")));
  }

  #[test]
  fn bare_name_skips_non_executable() {
    let _g = TestGuard::new();
    let dir = TempDir::new().unwrap();
    make_non_exec(dir.path(), "noexec");
    test_input(format!("PATH={}", dir.path().display())).unwrap();
    assert!(!is_in_path(&tk("noexec")));
  }

  #[test]
  fn bare_name_falls_through_nonexistent_path_entry() {
    let _g = TestGuard::new();
    let dir = TempDir::new().unwrap();
    make_exec(dir.path(), "real");
    test_input(format!("PATH=/nonexistent/xyz:{}", dir.path().display())).unwrap();
    assert!(is_in_path(&tk("real")));
  }

  #[test]
  fn bare_name_with_unset_path_returns_false() {
    let _g = TestGuard::new();
    // Use `unset` so try_var!("PATH") returns None.
    test_input("unset PATH").unwrap();
    assert!(!is_in_path(&tk("ls")));
  }

  // ─── relative paths ──────────────────────────────────────────────

  #[test]
  fn dot_slash_executable_in_cwd_returns_true() {
    let mut g = TestGuard::new();
    let dir = g.in_temp_dir();
    make_exec(&dir, "prog");
    assert!(is_in_path(&tk("./prog")));
  }

  #[test]
  fn dot_slash_nonexistent_returns_false() {
    let mut g = TestGuard::new();
    let _dir = g.in_temp_dir();
    assert!(!is_in_path(&tk("./nope_xyz")));
  }

  #[test]
  fn dot_slash_non_executable_returns_false() {
    let mut g = TestGuard::new();
    let dir = g.in_temp_dir();
    make_non_exec(&dir, "data.txt");
    assert!(!is_in_path(&tk("./data.txt")));
  }

  #[test]
  fn dot_slash_directory_returns_false() {
    let mut g = TestGuard::new();
    let dir = g.in_temp_dir();
    std::fs::create_dir(dir.join("subdir")).unwrap();
    assert!(!is_in_path(&tk("./subdir")));
  }

  #[test]
  fn dotdot_slash_executable_returns_true() {
    let mut g = TestGuard::new();
    let dir = g.in_temp_dir();
    make_exec(&dir, "outerprog");
    let inner = dir.join("inner");
    std::fs::create_dir(&inner).unwrap();
    std::env::set_current_dir(&inner).unwrap();
    assert!(is_in_path(&tk("../outerprog")));
  }

  // ─── absolute paths take precedence over PATH ────────────────────

  #[test]
  fn absolute_path_does_not_consult_path_var() {
    // Even with a bogus PATH, an absolute path is resolved directly.
    let _g = TestGuard::new();
    let dir = TempDir::new().unwrap();
    let exe = make_exec(dir.path(), "prog");
    test_input("PATH=/nonexistent/xyz").unwrap();
    assert!(is_in_path(&tk(&exe.to_string_lossy())));
  }

  // ─── expansion behavior ──────────────────────────────────────────

  #[test]
  fn expansion_resolves_var_to_path() {
    let _g = TestGuard::new();
    let dir = TempDir::new().unwrap();
    let exe = make_exec(dir.path(), "prog");
    test_input(format!("MYEXE={}", exe.display())).unwrap();
    assert!(is_in_path(&tk("$MYEXE")));
  }

  #[test]
  fn expansion_resolves_var_to_bare_name_in_path() {
    let _g = TestGuard::new();
    let dir = TempDir::new().unwrap();
    make_exec(dir.path(), "myprog");
    test_input(format!("PATH={}", dir.path().display())).unwrap();
    test_input("NAME=myprog").unwrap();
    assert!(is_in_path(&tk("$NAME")));
  }

  #[test]
  fn unset_var_expansion_yields_empty_returns_false() {
    let _g = TestGuard::new();
    // An unset, unquoted var expands to nothing; first word is None,
    // so the function bails out with false.
    assert!(!is_in_path(&tk("$UNSET_VAR_FOR_ISINPATH_TEST_xyz")));
  }
}

// ===================== $_ (last argument) =====================

#[test]
fn underscore_is_previous_commands_last_arg() {
  let guard = TestGuard::new();
  test_input("echo one two\necho \"[$_]\"").unwrap();
  assert!(guard.read_output().contains("[two]"));
}

#[test]
fn underscore_mid_command_refers_to_previous_not_self() {
  // `$_` in a non-final position must resolve to the previous command's last
  // arg, not this command's (regression against set-before-exec/double-expand).
  let guard = TestGuard::new();
  test_input("echo one two\necho $_ END").unwrap();
  assert!(guard.read_output().contains("two END"));
}

#[test]
fn pipeline_leaves_underscore_untouched() {
  // A pipeline's stages are subshells, so the shell's `$_` is whatever it was
  // before the pipeline, not a value leaked from a stage.
  let guard = TestGuard::new();
  test_input("echo keep\necho a | echo b\necho \"[$_]\"").unwrap();
  assert!(guard.read_output().contains("[keep]"));
}

#[test]
fn assignment_rhs_sees_previous_status() {
  // `rv=$?` must expand `$?` against the pre-assignment status, not a value
  // reset by the assignment itself (issue #123).
  let guard = TestGuard::new();
  test_input("false\nrv=${?}\necho \"[$rv]\"").unwrap();
  assert!(guard.read_output().contains("[1]"));
}

#[test]
fn plain_assignment_has_zero_status() {
  // A plain assignment (no command substitution) succeeds: `$?` is 0
  // afterward, regardless of the prior command's status.
  let guard = TestGuard::new();
  test_input("false\nx=$?\necho \"after=$?\"").unwrap();
  assert!(guard.read_output().contains("after=0"));
}

#[test]
fn assignment_cmdsub_status_propagates() {
  // When the RHS runs a command substitution, its status becomes the
  // assignment command's status.
  let guard = TestGuard::new();
  test_input("x=$(exit 7)\necho \"after=$?\"").unwrap();
  assert!(guard.read_output().contains("after=7"));
}

// Prefix assignments on an *external* command are resolved in the parent
// (not the forked exec child), so their RHS command subs run with full shell
// state, intra-list references work, and nothing leaks to the parent.

#[test]
fn prefix_assign_cmdsub_resolves_for_external() {
  // The RHS command sub must resolve in the parent so the external command's
  // environment gets the right value. (Pipe through `grep` so the harness
  // captures the external `env`'s output.)
  let g = TestGuard::new();
  test_input("FOO=$(echo resolved) env | grep ^FOO=").unwrap();
  let out = g.read_output();
  assert!(out.contains("FOO=resolved"), "got: {out:?}");
}

#[test]
fn prefix_assign_intra_reference_for_external() {
  let g = TestGuard::new();
  test_input("A=1 B=$A env | grep ^B=").unwrap();
  let out = g.read_output();
  assert!(out.contains("B=1"), "got: {out:?}");
}

#[test]
fn prefix_assign_does_not_leak_to_parent_for_external() {
  let guard = TestGuard::new();
  test_input("PFXLEAK=x sh -c ':'; printf '[%s]' \"${PFXLEAK-unset}\"").unwrap();
  assert_eq!(guard.read_output(), "[unset]");
}
