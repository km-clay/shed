# Test plan: recent feature additions

Coverage gaps for features added during the recent development push. Each
section lists specific cases to exercise. Tests should land in the file
nearest the implementation (typically `mod tests` block at the bottom).

## 1. `defer` / scope teardown — `src/builtin/varcmds.rs` or `src/util/guards.rs`

- `defer 'echo a'; defer 'echo b'` runs in LIFO: prints `b\na`.
- `defer` registered inside a function fires on `return`.
- `defer` registered inside a `{ ... }` brace group fires on block exit.
- `defer` registered at top-level fires on shell exit.
- `defer 'echo $x'` body sees `local x` from the same scope when it runs.
- `defer -c` clears existing defers in the current scope.
- `defer` with no args lists currently-registered defers.
- `defer` inside subshell `(...)` doesn't escape the subshell's scope.

## 2. Block-scoped `local` — `src/builtin/varcmds.rs`

- `{ local x=1; }; echo $x` echoes empty.
- Nested `{ { local x=inner; }; echo $x; }` empty in outer block.
- `local x=outer; { local x=inner; echo $x; }; echo $x` prints `inner` then `outer`.
- `local` outside any function or brace group at top-level (decide: works in shed, errors in bash; document and assert shed's behavior).

## 3. Param expansion exit status — `src/expand/param.rs`

For each operator, two cases: it fires (`$? = 0`) and it's a no-op (`$? = 1`).

- `${var#pat}` / `##` / `%` / `%%` — match vs no-match.
- `${var/pat/rep}` / `//` / `/#` / `/%` — match vs no-match.
- `${var^}` / `^^` / `,` / `,,` — case-changes-something vs already-cased.
- `${var:offset}` / `${var:offset:length}` — in-range vs out-of-range.
- Loop pattern: `path=foo/bar/baz; while path=${path%/*}; do echo "$path"; done` prints `foo/bar`, `foo/bar`/`foo`, `foo`, terminates.
- Operators that should NOT change status: `${var:-d}`, `${var:=d}`, `${var:+a}`, `${#var}`, plain `${var}` (assert no spurious `$? = 1`).

## 4. `compadd` builtin — `src/builtin/complete.rs`

- `compadd a b c` adds three candidates with no description.
- Multiple `compadd` calls in same function accumulate (LIFO not required, but all present).
- `compadd -d desc_arr -a cand_arr` zips parallel arrays.
- `compadd -d desc_arr foo bar` mixes positional + array? (decide and pin)
- `compadd -P 'pre' -S 'suf' x y` produces inserts `prexsuf`, `preysuf`.
- Descriptions survive through to the candidate set returned by `exec_comp_func` — important regression test for the `desc`-stripping bug we just fixed in the candidate-rebuild map.
- Accumulator drains and resets between completion invocations (don't bleed candidates from one tab to the next).

## 5. Array literals in declaration builtins — `src/builtin/varcmds.rs`

- `local arr=(a b c); echo "${arr[0]}"` prints `a`.
- `local arr=( a b c ); echo "${arr[*]}"` (whitespace inside parens).
- `local` multi-line array literal:
  ```sh
  local arr=(
    a
    b
    c
  )
  ```
- `readonly arr=(1 2 3)` then access via `${arr[1]}`.
- `export arr=(...)` (note: arrays don't really export to env meaningfully; assert shed's chosen behavior).
- `local x=$HOME` still expands `$HOME` (non-array path through expansion still works).
- Mixed: `local x=foo arr=(a b c) y=$HOME` — all three correct.

## 6. CtxTk subshell + arithmetic classification — `src/readline/context.rs`

Already partially covered (`arithmetic_atoms`, `cmd_sub_span_includes_closer`). Add:

- `(echo foo; ls -la)` produces a `Subshell` token with sub-tokens for the body's commands/args (full lexer pass via `from_cmd_sub`, not bare `scan_subspans`).
- `((1 + var * 2))` produces `ArithNumber`/`ArithOp`/`ArithVar` with full-span coverage (no off-by-one — explicit length assertion on each).
- `$((x = 5))` produces correct ArithVar/ArithOp/ArithNumber tree, span = full `$((x = 5))`.

## 7. Completion dispatch / `get_branch` — `src/readline/complete.rs`

- Cursor inside an `Escape` token `\ ` walks up to the parent `Argument` and dispatches as `Files`/`Argument`.
- Deep nesting: `(echo foo ${bar[$(echo ${foo[$(cat ~/file.txt)]}) + 1]})` — cursor inside the innermost `~/fi` produces a 10-deep branch chain. Test the chain length and the resolved strat.
- `cd /home/user/Doc<TAB>uments/foo` — cursor mid-token, `Argument` strat carries full `path`, postfix preserved when completing.

## 8. Comp function arg quoting — `src/readline/complete.rs`

- Comp function receives correct `$1`/`$2`/`$3` when:
  - `cmd_name = "my cmd"` (space)
  - `cword = "foo bar"` (space)
  - any of them contains `'`, `"`, `$`, `;`, `&`
- Round-trip via `as_var_val_display`: special chars don't break parsing on the receiving end.

## 9. Glob escape — `src/expand/var.rs` and `src/readline/complete.rs`

- `escape_glob` converts `\*` (or marker-equivalent) → `[*]`, `\?` → `[?]`, `\[` → `[[]`.
- `expand_glob` matches `my\ *` against `my file.txt` (the original bug).
- `complete_path` with `path = "my\ "` (trailing escaped space) tabs to `my\ file.txt`.
- `complete_path` with `path = "my\"` (lone trailing backslash) tabs to `my\ file.txt` (close-open-escape fix).

## 10. `!` lexer disambiguation — `src/parse/lex.rs`

- `! cmd` (space after `!`) — lexer produces `TkRule::Bang` token with KEYWORD flag.
- `!cmd` (no space) — lexer produces a single Str token containing `!cmd` (so the inner CtxTk scan can classify the `!` as `HistExp`).
- `!` followed by `;`, `|`, `&`, EOF — Bang.
- `!` followed by `$`, `!`, alphanumeric — not Bang, lexed as start of word.

## Priority

1. compadd descriptions (just hit a real bug)
2. defer / scope teardown (lots of edge cases, easy to regress)
3. array literals in declaration builtins (recent override is barely tested)
4. param expansion exit status (extension over POSIX, behavior must be pinned)

The rest are good-to-have but already partially exercised by existing tests
or have lower regression risk.
