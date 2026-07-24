/*
 * Tests based on the Shell & Utilities volume of IEEE Std 1003.1-2017
 *
 * This module goes through the Shell Command Language chapter, one section at a time
 * and provides tests that assert the specified behaviors.
 *
 * Note on compliance: shed's posix compliance is best effort, not a hard requirement.
 * There are areas where shed does not conform with posix, such as the lexing/parsing strategy.
 * These known deviations are noted where applicable.
 *
 * Throughout this module, C-style comments /* like this */ are used to quote the specification.
 * Regular comments like // are used for my own notes and commentary.
 */

#[macro_export]
macro_rules! __test_setup_vars {
  ([$($var:literal => $value:literal),* $(,)?]) => {
    $(
      $crate::eval::execute::exec_nonint(
        $crate::varstr!("{}='{}'", $var, $value),
        Some("test_input".into())
      ).unwrap();
    )*
  };
}

#[macro_export]
macro_rules! __test_setup_files {
  ($guard:expr, [$($path:literal => $content:expr),* $(,)?]) => {
    $($guard.with_file($path, $content);)*
  };
}

#[macro_export]
macro_rules! __test_setup_params {
  ([$($value:literal),* $(,)?]) => {
    $crate::state::Shed::vars_mut(|v| {
      v.set_param($crate::state::vars::ShellParam::ShellName, "test_input");
      let scope = v.cur_scope_mut();
      scope.sh_argv_mut().clear();
      scope.bpush_arg("test_input".into()); // $0
      $(scope.bpush_arg($value.into());)*
    });
  };
}

// this is gonna save us like 10,000 lines of boilerplate
#[macro_export]
macro_rules! test_input {
  { setup: $setup:tt, $($name:ident: $input:expr => $expected:expr;)* } => {
    $(#[test]
      fn $name() {
        let g = $crate::tests::testutil::TestGuard::new();
        $setup
        $crate::eval::execute::exec_nonint(
          $input.into(),
          Some("test_input".into())
        ).unwrap();
        $crate::assert_output!(g, $expected);
      }
    )*
  };

  // Plain (no setup)
  { $($name:ident: $input:expr => $expected:expr;)* } => {
    $(#[test]
      fn $name() {
        let g = $crate::tests::testutil::TestGuard::new();
        $crate::eval::execute::exec_nonint(
          $input.into(),
          Some("test_input".into())
        ).unwrap();
        $crate::assert_output!(g, $expected);
      }
    )*
  };
}

mod shell_intro_2_1 {
  /*
   * §2.1 Shell Introduction
   *
   * 1. The shell reads its input from a file (see sh), from the -c option or from the system()
   * and popen() functions defined in the System Interfaces volume of POSIX.1-2017.
   * If the first line of a file of shell commands starts with the characters "#!", the results are unspecified.
   */

  use crate::{
    assert_output,
    eval::execute::exec_dash_c,
    input,
    state::{
      Shed,
      vars::{VarFlags, VarKind},
    },
    tests::testutil::TestGuard,
  };
  use std::io::Write;

  #[test]
  fn test_input_script() {
    let g = TestGuard::new();
    let mut file = tempfile::NamedTempFile::new().unwrap();
    writeln!(file, "echo hello world; echo $1 $2").unwrap();
    input::run_script(file.path(), vec!["world".into(), "hello".into()]).unwrap();
    assert_output!(g, "hello world\nworld hello\n");
  }

  #[test]
  fn test_input_dash_c() {
    let g = TestGuard::new();
    let args = vec!["foo".into(), "bar".into(), "biz".into()];
    exec_dash_c("echo hello world; echo $0 $1 $2".into(), args).unwrap();
    assert_output!(g, "hello world\nfoo bar biz\n");
  }

  #[test]
  fn test_stdin_commands() {
    let mut g = TestGuard::new();
    g.feed_stdin(b"echo hello world; echo $1 $2\n");
    input::read_commands(vec!["world".into(), "hello".into()]).unwrap();
    assert_output!(g, "hello world\nworld hello\n");
  }

  // -c tests
  // args after command string are used for $0, $1, $2 params etc
  #[test]
  fn dash_c_sets_dollar_zero() {
    let g = TestGuard::new();
    exec_dash_c("echo $0".into(), vec!["myscript".into()]).unwrap();
    assert_output!(g, "myscript\n");
  }

  #[test]
  fn dash_c_positional_params_in_order() {
    let g = TestGuard::new();
    exec_dash_c(
      "echo $1 $2 $3".into(),
      vec!["s".into(), "foo".into(), "bar".into(), "biz".into()],
    )
    .unwrap();
    assert_output!(g, "foo bar biz\n");
  }

  #[test]
  fn dash_c_arg_count() {
    let g = TestGuard::new();
    // $# is the count of positional params, NOT including $0.
    exec_dash_c(
      "echo $#".into(),
      vec!["s".into(), "a".into(), "b".into(), "c".into()],
    )
    .unwrap();
    assert_output!(g, "3\n");
  }

  #[test]
  fn dash_c_arg_count_no_args() {
    let g = TestGuard::new();
    exec_dash_c("echo $#".into(), vec!["s".into()]).unwrap();
    assert_output!(g, "0\n");
  }

  #[test]
  fn dash_c_at_expansion_unquoted() {
    let g = TestGuard::new();
    exec_dash_c(
      "echo $@".into(),
      vec!["s".into(), "a".into(), "b".into(), "c".into()],
    )
    .unwrap();
    assert_output!(g, "a b c\n");
  }

  #[test]
  fn dash_c_star_expansion_quoted_joins_with_ifs() {
    let g = TestGuard::new();
    let args = vec!["s".into(), "a".into(), "b".into(), "c".into()];
    exec_dash_c("echo \"$*\"".into(), args.clone()).unwrap();
    assert_output!(g, "a b c\n");

    Shed::vars_mut(|v| v.set_var("IFS", VarKind::Str(":".into()), VarFlags::empty())).unwrap();
    exec_dash_c("echo \"$*\"".into(), args.clone()).unwrap();
    assert_output!(g, "a:b:c\n");
  }

  #[test]
  fn dash_c_at_expansion_quoted_preserves_arg_boundaries() {
    let g = TestGuard::new();
    exec_dash_c(
      "for x in \"$@\"; do echo $x; done".into(),
      vec!["s".into(), "a b".into(), "c".into()],
    )
    .unwrap();
    assert_output!(g, "a b\nc\n");
  }

  #[test]
  fn dash_c_empty_arg_preserved() {
    let g = TestGuard::new();
    exec_dash_c(
      "echo $#:$1:$2:$3".into(),
      vec!["s".into(), "first".into(), String::new(), "third".into()],
    )
    .unwrap();
    assert_output!(g, "3:first::third\n");
  }

  #[test]
  fn dash_c_no_args_dollar_zero_set() {
    let g = TestGuard::new();
    exec_dash_c("echo $0".into(), vec![]).unwrap();
    let out = g.read_output();
    assert!(
      !out.trim().is_empty(),
      "$0 should be set to something even with no args (got {out:?})"
    );
  }

  #[test]
  fn dash_c_args_not_re_expanded() {
    let g = TestGuard::new();
    exec_dash_c("echo $1".into(), vec!["s".into(), "$HOME".into()]).unwrap();
    assert_output!(g, "$HOME\n");
  }

  #[test]
  fn dash_c_args_with_globs_not_re_expanded() {
    let g = TestGuard::new();
    // `*.toml` matches Cargo.toml in the project root. Quoting `$1`
    // suppresses pathname expansion, so the literal pattern survives
    // even when a matching file exists — which is the actual property
    // we want to assert (the unquoted version would correctly glob).
    exec_dash_c("echo \"$1\"".into(), vec!["s".into(), "*.toml".into()]).unwrap();
    assert_output!(g, "*.toml\n");
  }

  #[test]
  fn dash_c_shift_advances_positional() {
    let g = TestGuard::new();
    exec_dash_c(
      "shift; echo $1 $#".into(),
      vec!["s".into(), "a".into(), "b".into(), "c".into()],
    )
    .unwrap();
    assert_output!(g, "b 2\n");
  }

  #[test]
  fn script_dollar_zero_is_path() {
    let g = TestGuard::new();
    let mut file = tempfile::NamedTempFile::new().unwrap();
    writeln!(file, "echo $0").unwrap();
    let path = file.path().to_path_buf();
    input::run_script(&path, vec![]).unwrap();
    assert_output!(g, "{}\n", path.display());
  }

  #[test]
  fn script_arg_count() {
    let g = TestGuard::new();
    let mut file = tempfile::NamedTempFile::new().unwrap();
    writeln!(file, "echo $#").unwrap();
    input::run_script(file.path(), vec!["a".into(), "b".into(), "c".into()]).unwrap();
    assert_output!(g, "3\n");
  }

  #[test]
  fn script_at_expansion_quoted_preserves_arg_boundaries() {
    let g = TestGuard::new();
    let mut file = tempfile::NamedTempFile::new().unwrap();
    writeln!(file, "for x in \"$@\"; do echo $x; done").unwrap();
    input::run_script(file.path(), vec!["a b".into(), "c".into()]).unwrap();
    assert_output!(g, "a b\nc\n");
  }

  #[test]
  fn script_empty_arg_preserved() {
    let g = TestGuard::new();
    let mut file = tempfile::NamedTempFile::new().unwrap();
    writeln!(file, "echo $#:$1:$2:$3").unwrap();
    input::run_script(
      file.path(),
      vec!["first".into(), String::new(), "third".into()],
    )
    .unwrap();
    assert_output!(g, "3:first::third\n");
  }

  #[test]
  fn script_args_not_re_expanded() {
    let g = TestGuard::new();
    let mut file = tempfile::NamedTempFile::new().unwrap();
    writeln!(file, "echo $1").unwrap();
    input::run_script(file.path(), vec!["$HOME".into()]).unwrap();
    assert_output!(g, "$HOME\n");
  }

  #[test]
  fn script_shift_advances_positional() {
    let g = TestGuard::new();
    let mut file = tempfile::NamedTempFile::new().unwrap();
    writeln!(file, "shift; echo $1 $#").unwrap();
    input::run_script(file.path(), vec!["a".into(), "b".into(), "c".into()]).unwrap();
    assert_output!(g, "b 2\n");
  }

  // ─── exec_dash_c: parse / structure edge paths ────────────────

  #[test]
  fn dash_c_parse_error_returns_ok() {
    // Unbalanced quote → ParsedSrc::parse_src returns Err; exec_dash_c
    // prints the errors but returns Ok(()) regardless.
    let _g = TestGuard::new();
    let res = exec_dash_c("echo 'unterminated".into(), vec!["s".into()]);
    assert!(res.is_ok());
  }

  #[test]
  fn dash_c_multi_statement_input_runs_each() {
    let g = TestGuard::new();
    // Multiple statements: the single-cmd NO_FORK optimization should
    // be skipped (nodes.len() != 1 path), and both echos still run.
    exec_dash_c(
      "echo first; echo second; echo third".into(),
      vec!["s".into()],
    )
    .unwrap();
    assert_output!(g, "first\nsecond\nthird\n");
  }

  #[test]
  fn dash_c_empty_input_is_ok() {
    let _g = TestGuard::new();
    exec_dash_c(String::new(), vec!["s".into()]).unwrap();
  }
}

mod quoting_2_2 {
  /*
   * §2.2 Quoting
   *
   * Quoting is used to remove the special meaning of certain characters or words to the shell.
   * Quoting can be used to preserve the literal meaning of the special characters in the next paragraph,
   * prevent reserved words from being recognized as such,
   * and prevent parameter expansion and command substitution within here-document processing.
   */

  // The application shall quote the following characters if they are to represent themselves:
  // | & ; < > ( ) $ ` \ " ' <space> <tab> <newline>
  //
  // and the following might need to be quoted under certain circumstances.
  // That is, these characters are sometimes special depending on conditions described elsewhere.
  // * ? [ ] ^ - ! # ~ = % { , }

  mod _1_escape {
    /*
     * §2.2.1 Escape Character (Backslash)
     *
     * A <backslash> that is not quoted shall preserve the literal value of the following character,
     * with the exception of a <newline>. If a <newline> immediately follows the <backslash>, the shell shall
     * interpret this as line continuation. The <backslash> and <newline> shall be removed before splitting
     * the input into tokens. Since the escaped <newline> is removed entirely from the input and is not replaced
     * by any whitespace, it cannot serve as a token separator.
     */
    test_input! {
      backslash_pipe        : r"echo a\|b"   => "a|b\n";
      backslash_amp         : r"echo a\&b"   => "a&b\n";
      backslash_semi        : r"echo a\;b"   => "a;b\n";
      backslash_lt          : r"echo a\<b"   => "a<b\n";
      backslash_gt          : r"echo a\>b"   => "a>b\n";
      backslash_paren       : r"echo a\(b\)" => "a(b)\n";
      backslash_dollar      : r"echo a\$b"   => "a$b\n";
      backslash_backtick    : r"echo a\`b\`" => "a`b`\n";
      backslash_backslash   : r"echo a\\b"   => "a\\b\n";
      backslash_double_quote: r#"echo a\"b\""# => "a\"b\"\n";
      backslash_single_quote: r"echo a\'b\'" => "a'b'\n";
      backslash_space       : r"echo a\ b"   => "a b\n";
      backslash_tab         : r"echo a\ b"   => "a b\n";
      backslash_newline     : "echo a\\\nb"    => "ab\n";
      backslash_glob        : r"echo a\*b"   => "a*b\n";
    }
  }

  mod _2_single_quotes {
    /*
     * §2.2.2 Single Quotes
     * Enclosing characters in single-quotes ('') shall preserve the literal value of each character
     * within the single-quotes. A single-quote cannot occur within single-quotes.
     */

    test_input! {
      squote_pipe             : r"echo 'a|b'"        => "a|b\n";
      squote_idiom            : r"echo 'foo'\''bar'" => "foo'bar\n";
      squote_backslash        : r"echo 'a\b'"        => "a\\b\n";
      squote_double_backslash : r"echo 'a\\b'"       => "a\\\\b\n";
      squote_backslash_does_not_escape_quote :
                                 r"echo 'foo\' bar"  => "foo\\ bar\n";
      squote_amp              : r"echo 'a&b'"        => "a&b\n";
      squote_semi             : r"echo 'a;b'"        => "a;b\n";
      squote_lt               : r"echo 'a<b'"        => "a<b\n";
      squote_gt               : r"echo 'a>b'"        => "a>b\n";
      squote_paren            : r"echo 'a(b)'"       => "a(b)\n";
      squote_dollar           : r"echo 'a$b'"        => "a$b\n";
      squote_backtick         : r"echo 'a`b`'"       => "a`b`\n";
      squote_double_quote     : r#"echo 'a"b'"#        => "a\"b\n";
      squote_space            : r"echo 'a b'"        => "a b\n";
      squote_tab              : r"echo 'a	b'"       => "a	b\n";
      squote_newline          :   "echo 'a\nb'"        => "a\nb\n";
      squote_backslash_newline:   "echo 'a\\\nb'"      => "a\\\nb\n";
      squote_glob             : r"echo 'a*b'"        => "a*b\n";
    }
  }

  mod _3_double_quotes {
    /*
     * §2.2.3 Double Quotes
     * Enclosing characters in double-quotes ("") shall preserve the literal value of all characters within the double-quotes,
     * with the exception of the characters backquote, <dollar-sign>, and <backslash>, as follows:
     */

    // The <dollar-sign> shall retain its special meaning introducing parameter expansion,
    // a form of command substitution, and arithmetic expansion, but shall not retain
    // its special meaning introducing the dollar-single-quotes form of quoting.
    test_input! {
      setup: {
        crate::__test_setup_vars!([
          "foo" => "bar",
          "baz" => "bu*zz",
          "with_spaces" => "this has spaces",
        ]);
      },
      dquote_dollar_var      : r#"echo "a$foo""#         => "abar\n";
      dquote_var_braced      : r#"echo "a${foo}""#       => "abar\n";
      dquote_param_op        : r#"echo "a${baz%"*"zz}""# => "abu\n";
      dquote_dollar_paren_sub: r#"echo "a$(echo foo)""#  => "afoo\n";
      dquote_backtick_sub    : r#"echo "a`echo foo`""#   => "afoo\n";
      dquote_dollar_arith    : r#"echo "a$((1 + 2))""#   => "a3\n";
      dquote_var_with_spaces : r#"echo "a$with_spaces""# => "athis has spaces\n";
      dquote_empty_var       : r#"echo "a$empty""#       => "a\n";
      dquote_single_dollar   : r#"echo "a$""#            => "a$\n";
    }

    // Outside of "$(...)" and "${...}" the <backslash> shall retain its special meaning as an escape character
    // only when immediately followed by one of the following characters:
    // $ ` \ <newline> "
    test_input! {
      dquote_backslash_dollar   : r#"echo "a\$foo""#   => "a$foo\n";
      dquote_backslash_backtick : r#"echo "a\`foo\`""# => "a`foo`\n";
      dquote_backslash_backslash: r#"echo "a\\b""#     => "a\\b\n";
      dquote_backslash_dquote   : r#"echo "a\"b""#     => "a\"b\n";
      dquote_backslash_newline  : "echo \"a\\\nb\""    => "ab\n";

      dquote_backslash_other    : r#"echo "a\*b""#     => "a\\*b\n";
    }

    test_input! {
      dquote_pipe        : r#"echo "a|b""#   => "a|b\n";
      dquote_amp         : r#"echo "a&b""#   => "a&b\n";
      dquote_semi        : r#"echo "a;b""#   => "a;b\n";
      dquote_lt          : r#"echo "a<b""#   => "a<b\n";
      dquote_gt          : r#"echo "a>b""#   => "a>b\n";
      dquote_paren       : r#"echo "a(b)""#  => "a(b)\n";
      dquote_double_quote: r#"echo "a\"b""#  => "a\"b\n";
      dquote_single_quote: r#"echo "a'b""#   => "a'b\n";

      dquote_space       : r#"echo "a b""#   => "a b\n";
      dquote_tab         : r#"echo "a	b""#   => "a	b\n";
      dquote_newline     : "echo \"a\nb\""   => "a\nb\n";

      dquote_star        : r#"echo "a*b""#   => "a*b\n";
      dquote_question    : r#"echo "a?b""#   => "a?b\n";
      dquote_bracket     : r#"echo "a[b]c""# => "a[b]c\n";
      dquote_caret       : r#"echo "a^b""#   => "a^b\n";
      dquote_hash        : r#"echo "a#b""#   => "a#b\n";
      dquote_lbrace      : r#"echo "a{b""#   => "a{{b\n";
    }

    test_input! {
      dquote_dollar_squote_literal    : r#"echo "$'foo'""#  => "$'foo'\n";
      dquote_dollar_squote_escape_seq : r#"echo "$'a\nb'""# => "$'a\\nb'\n";
      dquote_dollar_squote_in_word    : r#"echo "x$'y'z""#  => "x$'y'z\n";
      dquote_dollar_squote_empty      : r#"echo "$''""#     => "$''\n";
    }
  }

  mod _4_dollar_quotes {
    /*
     * §2.2.4 Dollar-Single-Quotes
     * A sequence of characters starting with a <dollar-sign> immediately followed by a single quote ($')
     * shall preserve the literal value of all characters up to an unescaped terminating single-quote ('),
     * with the exception of certain <backslash>-escape sequences, as follows:
     *
     * \" => yields a double quote
     * \' => yields a single quote
     * \\ => yields a backslash
     * \a => yields an <alert> character
     * \b => yields a <backspace> character
     * \e => yields an <escape> character
     * \f => yields a <form-feed> character
     * \n => yields a <newline> character
     * \r => yields a <carriage-return> character
     * \t => yields a <tab> character
     * \v => yields a <vertical-tab> character
     *
     * \cX => yields the control character listed in the Value column of Values for cpio c_mode Field
     * in the OPERANDS section of the stty utility when X is one of the characters listed in the ^c column
     * of the same table, except that \c\\ yields the <FS> control character since the <backslash> character
     * has to be escaped
     * \xXX => yields the byte whose value is the hexadecimal value XX (one or two hex digits)
     * \ddd => yields the byte whose value is the octal value ddd (one, two, or three octal digits)
     */

    test_input! {
      dollar_squote_double_quote     : r#"echo $'a\"b'"#   => "a\"b\n";
      dollar_squote_single_quote     : r"echo $'a\'b'"   => "a'b\n";
      dollar_squote_backslash        : r"echo $'a\\b'"   => "a\\b\n";
      dollar_squote_alert            : r"echo $'a\ab'"   => "a\x07b\n";
      dollar_squote_backspace        : r"echo $'a\bb'"   => "a\x08b\n";
      dollar_squote_escape           : r"echo $'a\eb'"   => "a\x1bb\n";
      dollar_squote_formfeed         : r"echo $'a\fb'"   => "a\x0cb\n";
      dollar_squote_newline          : r"echo $'a\nb'"   => "a\nb\n";
      dollar_squote_carriage_return  : r"echo $'a\rb'"   => "a\rb\n";
      dollar_squote_tab              : r"echo $'a\tb'"   => "a\tb\n";
      dollar_squote_vertical_tab     : r"echo $'a\vb'"   => "a\x0bb\n";

      dollar_squote_c_a              : r"echo $'a\ca'"   => "a\x01\n";
      dollar_squote_c_z              : r"echo $'a\cz'"   => "a\x1a\n";
      dollar_squote_c_uppercase      : r"echo $'a\cA'"   => "a\x01\n";
      dollar_squote_c_underscore     : r"echo $'a\c_'"   => "a\x1f\n";
      dollar_squote_c_at             : r"echo $'a\c@b'"  => "a\x00b\n";
      dollar_squote_c_question       : r"echo $'a\c?b'"  => "a\x7fb\n";
      dollar_squote_c_lbracket       : r"echo $'a\c[b'"  => "a\x1bb\n";
      dollar_squote_c_backslash      : r"echo $'a\c\\'"  => "a\x1c\n";

      dollar_squote_c_backslash_other: r"echo $'a\c\b'"  => "a\\c\x08\n";
      dollar_squote_c_backslash_n    : r"echo $'a\c\nb'" => "a\\c\nb\n";
      dollar_squote_c_at_eof         : r"echo $'a\c'"    => "a\\c\n";
      dollar_squote_c_invalid_letter : r"echo $'a\c1b'"  => "a\\c1b\n";

      dollar_squote_hex              : r"echo $'a\x41b'" => "aAb\n";
      dollar_squote_octal            : r"echo $'a\101b'" => "aAb\n";

      dollar_squote_escape_other     : r"echo $'a\z'"    => "a\\z\n";
    }
  }
}

/*
 * §2.3 Token Recognition
 *
 * Our lexing/parsing strategy is pretty far removed from what posix specifies
 * so the behaviors it lays out aren't really worth testing here. Our existing
 * lexer/parser tests work for making sure that the logic there is sound.
 * Additionally, the subsection for alias substitution (§2.3.1 Alias Substitution)
 * also doesn't really apply. So we'll just skip this section.
 */

/*
 *
 */

mod reserved_words_2_4 {
  /*
   * §2.4 Reserved Words
   *
   * Reserved words shall be recognized only when they are unquoted and appear in:
   *   - the first word of a command
   *   - the first word following another reserved word (except case/for/in)
   *   - the third word in a `case` command (only `in`)
   *   - the third word in a `for` command (only `in` and `do`)
   *
   * shed's reserved word set extends POSIX with: [[, ]], time, function
   */

  // each reserved word in command position drives its construct.
  test_input! {
    kw_if_then_fi             : "if true; then echo a; fi"                                       => "a\n";
    kw_if_else                : "if false; then echo a; else echo b; fi"                         => "b\n";
    kw_if_elif_else           : "if false; then echo a; elif true; then echo b; else echo c; fi" => "b\n";
    kw_for_in_do_done         : "for x in 1 2 3; do echo $x; done"                               => "1\n2\n3\n";
    kw_while                  : "x=0; while [ $x -lt 2 ]; do echo $x; x=$((x+1)); done"          => "0\n1\n";
    kw_until                  : "x=0; until [ $x -ge 2 ]; do echo $x; x=$((x+1)); done"          => "0\n1\n";
    kw_case_esac              : "case foo in foo) echo m ;; *) echo o ;; esac"                   => "m\n";
    kw_case_default           : "case zzz in foo) echo m ;; *) echo o ;; esac"                   => "o\n";
    kw_bang_inverts_true      : "! true; echo $?"                                                => "1\n";
    kw_bang_inverts_false     : "! false; echo $?"                                               => "0\n";
    kw_dbracket               : "[[ a == a ]] && echo yes"                                       => "yes\n";
    kw_function               : "function f { echo hi; }; f"                                     => "hi\n";

    // reserved word recognized after another reserved word (rule 2)
    kw_if_following_then      : "if true; then if true; then echo nested; fi; fi"                => "nested\n";
    kw_while_following_if     : "if true; then while false; do :; done; echo a; fi"              => "a\n";
  }

  // mid-command, not recognized
  test_input! {
    arg_if_then_else_fi   : "echo if then else fi" => "if then else fi\n";
    arg_for_while_until   : "echo for while until" => "for while until\n";
    arg_do_done           : "echo do done"         => "do done\n";
    arg_case_esac_in      : "echo case esac in"    => "case esac in\n";
    arg_function_time     : "echo function time"   => "function time\n";
    arg_dbracket_tokens   : "echo [[ ]]"           => "[[ ]]\n";
    arg_bang_after_word   : "echo a !"             => "a !\n";
    arg_done_in_args      : "echo a done b"        => "a done b\n";
  }

  test_input! {
    sup_backslash_arg : r"echo \if"                 => "if\n";
    sup_squote_arg    : r"echo 'if'"                => "if\n";
    sup_dquote_arg    : r#"echo "then""#              => "then\n";

    sup_quoted_bang   : r"'!' 2>/dev/null; echo $?" => "127\n";
    sup_escaped_if    : r"\if 2>/dev/null; echo $?" => "127\n";
  }

  // Positional rules specific to `case` and `for`.
  test_input! {
    for_in_keyword       : "for x in a; do echo $x; done"    => "a\n";
    for_do_keyword       : "for x in a b; do echo $x; done"  => "a\nb\n";
    case_in_keyword      : "case foo in foo) echo y ;; esac" => "y\n";
    in_outside_position  : "echo in"                         => "in\n";

    // No-`in` form: `for x` (and `for x do`) iterate the positional parameters.
    for_no_in_semi       : "set -- a b c; for x; do echo $x; done"     => "a\nb\nc\n";
    for_no_in_bare_do    : "set -- a b; for x do echo got $x; done"    => "got a\ngot b\n";
    for_no_in_no_args    : "set --; for x; do echo $x; done; echo end" => "end\n";
    // Explicit empty `in` list still iterates zero times (distinct from no-`in`).
    for_empty_in_list    : "set -- a b; for x in; do echo $x; done; echo z" => "z\n";
  }
}

mod params_and_vars_2_5 {
  /*
   * §2.5 Parameters and Variables
   *
   * A parameter can be denoted by a name, a number, or one of the special characters
   * listed in 2.5.2 Special Parameters. A variable is a parameter denoted by a name.
   *
   * A parameter is set if it has an assigned value (null is a valid value).
   * Once a variable is set, it can only be unset by using the unset special builtin.
   *
   * Parameters can contain arbitrary byte sequences, except for the null byte.
   * The shell shall process their values as characters only when performing
   * operations that are described in this standard in terms of characters.
   */

  mod _1_pos_params {
    /*
     * §2.5.1 Positional Parameters
     *
     * A positional parameter is a parameter denoted by a decimal representation of
     * a positive integer. The digits denoting the positional parameters shall always be
     * interpreted as a decimal value, even if there is a leading zero. When a positional
     * parameter with more than one digit is specified, the application shall enclose the
     * digits in braces.
     *
     * Examples:
     * "$8", "${8}", "${08}" all expand to the eighth positional parameter
     * "${10}" expands to the tenth positional parameter
     * "$10" expands to the first positional parameter followed by a literal "0"
     */

    test_input! {
      setup: {
        __test_setup_params!(["foo", "bar", "biz", "bam"]);
      },
      positional_param_1_digit         : "echo $2"          => "bar\n";
      positional_param_first           : "echo $1"          => "foo\n";
      positional_param_last            : "echo $4"          => "bam\n";
      positional_param_unset           : "echo a$5b"        => "ab\n";
      positional_param_zero_is_name    : "echo $0"          => "test_input\n";
      positional_param_braced          : "echo ${1}"        => "foo\n";
      positional_param_unbraced_2digit : "echo $10"         => "foo0\n";
      positional_param_concat          : "echo $1$2$3"      => "foobarbiz\n";
      positional_param_quoted          : r#"echo "$1 $2""#  => "foo bar\n";
      positional_param_in_word         : "echo pre$2post"   => "prebarpost\n";
      positional_param_braced_then_lit : "echo ${1}0"       => "foo0\n";
    }

    test_input! {
      setup: {
        __test_setup_params!([
          "p1", "p2", "p3", "p4", "p5",
          "p6", "p7", "p8", "p9", "p10", "p11",
        ]);
      },
      positional_param_braced_2digit       : "echo ${10}"   => "p10\n";
      positional_param_braced_2digit_11    : "echo ${11}"   => "p11\n";
      positional_param_leading_zero_8      : "echo ${08}"   => "p8\n";
      positional_param_leading_zero_1      : "echo ${01}"   => "p1\n";
      positional_param_unbraced_2digit_11  : "echo $11"     => "p11\n";
    }

    test_input! {
      setup: {
        __test_setup_params!(["a", "b", "c"]);
      },
      pos_count_three   : "echo $#"                                => "3\n";
      pos_at_unquoted   : "echo $@"                                => "a b c\n";
      pos_star_unquoted : "echo $*"                                => "a b c\n";
      pos_at_quoted     : r#"for x in "$@"; do echo "[$x]"; done"# => "[a]\n[b]\n[c]\n";
      pos_star_quoted   : r#"echo "$*""#                           => "a b c\n";
    }

    test_input! {
      setup: {
        // empty params
        __test_setup_params!([]);
      },
      pos_count_zero       : "echo $#"                             => "0\n";
      pos_at_empty         : "echo a$@b"                           => "ab\n";
      pos_star_empty       : "echo a$*b"                           => "ab\n";
      pos_at_quoted_empty  : r#"for x in "$@"; do echo hit; done"# => "";
    }
  }

  mod _2_special_params {
    /*
     * Listed below are the special parameters and the values to which they shall expand.
     *
     * @ => Expands to the positional parameters, starting from $1.
     * * => Expands to the positional parameters, starting from $1. Produced token has one field.
     * # => Expands to the number of positional parameters. $0 is not counted
     * ? => Expands to the exit status (integer) of the previous command.
     * - => Expands to the current 'set' option flags.
     * $ => Expands to the process ID of the shell.
     * ! => Expands to the process ID of the most recently dispatched background job.
     * 0 => Expands to the name of the shell or shell script.
     */

    // $? — exit status of the previous pipeline.
    test_input! {
      dollar_q_after_true     : "true; echo $?"                        => "0\n";
      dollar_q_after_false    : "false; echo $?"                       => "1\n";
      dollar_q_explicit_exit  : "(exit 42); echo $?"                   => "42\n";
      dollar_q_only_last      : "false; true; echo $?"                 => "0\n";
      dollar_q_after_pipe     : "true | false; echo $?"                => "1\n";
      dollar_q_cmd_not_found  : "no_such_cmd_xyz 2>/dev/null; echo $?" => "127\n";
    }

    test_input! {
      setup: { __test_setup_params!(["a", "b", "c"]); },
      dollar_hash_count       : "echo $#"                            => "3\n";
      dollar_at_unquoted      : "echo $@"                            => "a b c\n";
      dollar_star_unquoted    : "echo $*"                            => "a b c\n";
      dollar_at_quoted_iter   : r#"for x in "$@"; do echo $x; done"# => "a\nb\nc\n";
      dollar_star_quoted_dflt : r#"echo "$*""#                       => "a b c\n";
    }

    test_input! {
      setup: { __test_setup_params!(["a", "b", "c"]); },
      dollar_star_ifs_colon   : r#"IFS=:; echo "$*""#            => "a:b:c\n";
      dollar_star_ifs_multi   : r#"IFS=:#; echo "$*""#           => "a:b:c\n";
      dollar_star_ifs_empty   : r#"IFS=; echo "$*""#             => "abc\n";
    }

    test_input! {
      hash_after_set_dashdash : "set -- x y; echo $#"              => "2\n";
      at_after_set_dashdash   : "set -- x y z; echo $@"            => "x y z\n";
      hash_after_shift        : "set -- a b c d; shift; echo $#"   => "3\n";
      hash_after_shift_n      : "set -- a b c d; shift 2; echo $#" => "2\n";
      hash_zero_after_clear   : "set --; echo $#"                  => "0\n";
    }

    test_input! {
      dash_after_set_e        : r"set -e; case $- in *e*) echo has_e ;; *) echo no_e ;; esac"       => "has_e\n";
      dash_after_set_u        : r"set -u; case $- in *u*) echo has_u ;; *) echo no_u ;; esac"       => "has_u\n";
      dash_after_set_plus_e   : r"set -e; set +e; case $- in *e*) echo has_e ;; *) echo no_e ;; esac" => "no_e\n";
    }

    test_input! {
      pid_is_numeric          : r#"case $$ in "") echo empty ;; *[!0-9]*) echo nonum ;; *) echo num ;; esac"# => "num\n";
      pid_stable_within_shell : r#"a=$$; b=$$; [ "$a" = "$b" ] && echo same"# => "same\n";
    }

    test_input! {
      setup: {
        __test_setup_params!([]); // reset pos params
      },
      dollar_zero_is_source   : "echo $0"                        => "test_input\n";
    }
  }
}

mod word_expansions_2_6 {
  /*
   * §2.6 Word Expansions
   *
   * This section describes the various expansions that are performed on words.
   * Not all expansions are performed on every word, as explained in the following sections and elsewhere in this chapter.
   * The expansions that are performed for a given word shall be performed in the following order:
   * 1. Tilde expansion, parameter expansion, command substitution, and arithmetic expansion shall be performed.
   * 2. Field splitting shall be performeed on the portions of the fields generated by step 1.
   * 3. Pathname expansion shall be performed, unless set -f is in effect.
   * 4. Quote removal, if performed, shall always be performed last.
   */

  mod _1_tilde {
    /*
     * §2.6.1 Tilde Expansion
     *
     * A tilde-prefix consists of an unquoted '~' at the beginning of a word,
     * followed by all characters preceding the first unquoted slash in the word
     * (or all characters in the word if there is no slash). The chars that make
     * up the login name must be unquoted; quoting any of them disables tilde
     * expansion. The result of tilde expansion is treated as if it were quoted
     * (it always produces a single field).
     *
     * In assignment context, tilde expansion is also performed after the '='
     * and after each unquoted ':' within the assigned value.
     */
    use crate::{
      Shed,
      state::vars::{VarFlags, VarKind},
    };

    test_input! {
      setup: {
        Shed::vars_mut(|v| v.set_var(
          "HOME",
          VarKind::string("/home/test"),
          VarFlags::EXPORT,
        )).unwrap();
      },
      tilde_bare           : "echo ~"         => "/home/test\n";
      tilde_slash_path     : "echo ~/foo/bar" => "/home/test/foo/bar\n";
      tilde_slash_only     : "echo ~/"        => "/home/test/\n";
      tilde_after_text     : "echo a~"        => "a~\n";
      tilde_in_middle      : "echo foo~bar"   => "foo~bar\n";

      tilde_squoted        : "echo '~'"       => "~\n";
      tilde_dquoted        : r#"echo "~""#    => "~\n";
      tilde_escaped        : r"echo \~"     => "~\n";
      tilde_quoted_user_sq : r"echo ~'foo'" => "~foo\n";
      tilde_quoted_user_dq : r#"echo ~"foo""# => "~foo\n";
    }

    test_input! {
      setup: {
        Shed::vars_mut(|v| v.set_var(
          "HOME",
          VarKind::string("/home with spaces"),
          VarFlags::EXPORT,
        )).unwrap();
      },
      tilde_single_word    : r#"for x in ~; do echo "[$x]"; done"#     => "[/home with spaces]\n";
      tilde_path_single    : r#"for x in ~/sub; do echo "[$x]"; done"# => "[/home with spaces/sub]\n";
    }

    test_input! {
      setup: {
        Shed::vars_mut(|v| v.set_var(
          "HOME",
          VarKind::string("/home/test"),
          VarFlags::EXPORT,
        )).unwrap();
      },
      tilde_in_assignment    : r#"x=~; echo "$x""#       => "/home/test\n";
      tilde_in_assign_path   : r#"x=~/foo; echo "$x""#   => "/home/test/foo\n";
      tilde_in_assign_colon  : r#"x=a:~/b:c; echo "$x""# => "a:/home/test/b:c\n";
    }

    test_input! {
      tilde_unknown_user   : "echo ~zzz_no_such_user_xyz" => "~zzz_no_such_user_xyz\n";
    }
  }

  mod _2_param_expansion {
    /*
     * §2.6.2 Parameter Expansion
     * The format for parameter expansion is as follows:
     *
     * ${expression}
     *
     * where expression consists of all characters until the matching '}'.
     * Any '}' escaped by a <backslash> or within a quoted string, and characters
     * in embedded arithmetic expansions, command substitutions, and variable expansions
     * shall not be examined in determining the matching '}'.
     */

    test_input! {
      setup: {
        crate::__test_setup_vars!([
          "foo" => "bar}",
          "boo" => "foo}bar",
          "spaces" => "has spaces",
        ]);
      },
      trim_brace_backslash: r"echo ${foo%\}}"  => "bar\n";
      trim_brace_quoted   : r#"echo ${foo%"}"}"# => "bar\n";
      trim_brace_glob     : r"echo ${boo%\}*}" => "foo\n";
    }

    /*
     * The simplest form of parameter expansion is:
     *
     * ${parameter}
     *
     * The value, if any, of parameter shall be substituted.
     *
     * The parameter name or symbol can be enclosed in braces, which are optional
     * except for positional parameters with more than one digit or when parameter
     * is a name and is followed by a character that could be interpreted as part of the name
     */

    // We already tested this quite thoroughly above

    /*
     * In addition, a parameter expansion can be modified by using one of the following formats.
     * In each case that a value of 'word' is needed, 'word' shall be subjected to
     * tilde expansion, parameter expansion, command substitution, arithmetic expansion, and quote removal.
     * If 'word' is not needed, it shall not be expanded. The '}' character that delimits the following parameter
     * expansion modifications shall be determined as described previously in this section. If 'parameter' is
     * '*' or '@', the result is unspecified.
     *
     * ${parameter:-[word]}
     *   Use Default Values. If parameter is unset or null, substitute with 'word'.
     * ${parameter:=[word]}
     *   Assign Default Values. If parameter is unset or null, substitute with 'word'.
     * ${parameter:?[word]}
     *   Indicate Error if Null or Unset. If parameter is unset or null, write 'word' to standard error and exit non-zero.
     * ${parameter:+[word]}
     *   Use Alternate Value. If parameter is unset or null, null shall be substituted; otherwise, substitute with 'word'.
     *
     * In the parameter expansions shown previously, use of the <colon> in the format shall result in a test for a parameter
     * that is unset or null; omission of the <colon> shall result in a test for a parameter that is only unset.
     *
     * ${#parameter}
     *   String Length. The shorted decimal representation of the length in characters shall be substituted.
     * ${parameter%[word]}
     *   Remove Shortest Suffix. The result of parameter expansion shall have the shortest suffix pattern removed.
     * ${parameter%%[word]}
     *   Remove Longest Suffix. The result of parameter expansion shall have the longest suffix pattern removed.
     * ${parameter#[word]}
     *   Remove Shortest Prefix. The result of parameter expansion shall have the shortest prefix pattern removed.
     * ${parameter##[word]}
     *   Remove Longest Prefix. The result of parameter expansion shall have the longest prefix pattern removed.
     */

    // next the chapter gives us some examples. following are test cases based on them.
    test_input! {
      /*
       * ${parameter}
       * In this example, the effects of omitting braces are demonstrated.
       *
       * a=1
       * set 2
       * echo ${a}b-$ab-${1}0-${10}-$10
       * 1b--20--20
       */
      setup: {
        crate::__test_setup_vars!([
          "a" => "1",
        ]);
        crate::__test_setup_params!(["2"]);
      },
      param_exp_example_1: r"echo ${a}b-$ab-${1}0-${10}-$10" => "1b--20--20\n";
    }
    test_input! {
      /*
       * ${parameter-word}
       * This example demonstrates the difference between unset and set to the empty string
       * as well as the rules for finding the delimiting close brace.
       *
       * foo=asdf
       * echo ${foo-bar}xyz}
       * asdfxyz}
       *
       * foo=
       * echo ${foo-bar}xyz}
       * xyz}
       *
       * unset foo
       * echo ${foo-bar}xyz}
       * barxyz}
       */
      setup: {
        crate::__test_setup_vars!([
          "foo" => "asdf",
        ]);
      },
      param_exp_example_2: r"echo ${foo-bar}xyz}" => "asdfxyz}}\n";
      param_exp_example_3: r"foo=; echo ${foo-bar}xyz}" => "xyz}}\n";
      param_exp_example_4: r"unset foo; echo ${foo-bar}xyz}" => "barxyz}}\n";
    }

    test_input! {
      /*
       * ${parameter:-word}
       * In this example, ls is executed only if x is null or unset.
       * ${x:-$(ls)}
       */
      // we are gonna use echo instead because the test env might not have ls
      setup: {
        crate::__test_setup_vars!([
          "x" => "",
        ]);
      },
      param_exp_example_5: r"echo ${x:-$(echo foo)}" => "foo\n";
    }

    test_input! {
      /*
       * ${parameter:=word}
       */
      setup: {
        crate::__test_setup_vars!([
          "x" => "foo",
        ]);
      },
      param_exp_example_6: r"unset x; echo ${x:=bar}; echo $x" => "bar\nbar\n";
    }

    test_input! {
      /*
       * ${parameter:+word}
       */
      setup: {
        crate::__test_setup_params!([ "a", "b", "c" ]);
      },
      param_exp_example_7: r"echo ${3:+posix}" => "posix\n";
    }

    test_input! {
      /*
       * ${#parameter}
       */
      setup: {
        crate::__test_setup_vars!([
          "HOME" => "/usr/posix",
        ]);
      },
      param_exp_example_8: r"echo ${#HOME}" => "10\n";
    }

    test_input! {
      /*
       * ${parameter%word}
       */
      setup: {
        crate::__test_setup_vars!([
          "x" => "file.c"
        ]);
      },
      param_exp_example_9: r"echo ${x%.c}.o" => "file.o\n";
    }

    test_input! {
      /*
       * ${parameter%%word}
       */
      setup: {
        crate::__test_setup_vars!([
          "x" => "posix/src/std"
        ]);
      },
      param_exp_example_10: r"echo ${x%%/*}" => "posix\n";
    }

    test_input! {
      /*
       * ${parameter#word}
       */
      setup: {
        crate::__test_setup_vars!([
          "x" => "posix/src/cmd"
        ]);
      },
      param_exp_example_11: r"echo ${x#posix}" => "/src/cmd\n";
    }

    test_input! {
      /*
       * ${parameter##word}
       */
      setup: {
        crate::__test_setup_vars!([
          "x" => "/one/two/three"
        ]);
      },
      param_exp_example_12: r"echo ${x##*/}" => "three\n";
    }

    test_input! {
      /*
       * The double-quoting of patterns is different depending on where the double-quotes are placed.
       * "${x#*.}"
       *   The <asterisk> is a pattern character.
       * ${x#"*".}
       *   The literal <asterisk> is quoted and not special.
       */
      setup: {
        crate::__test_setup_vars!([
          "x" => "file.txt",
        ]);
      },
      param_exp_example_13: r"echo ${x#*.}" => "txt\n";
      param_exp_example_14: r#"echo ${x#"*".}"# => "file.txt\n";
    }
  }

  mod _3_command_sub {
    /*
     * §2.6.3 Command Substitution
     *
     * Command substitution allows the output of one or more commands to be
     * substituted in place of the commands themselves. Command substitution
     * shall occur when command(s) are enclosed as follows:
     *
     * $(commands)
     * or
     * `commands`
     *
     * The shell shall expand the command substitution by executing commands in a subshell environment
     * If the output ends with a newline, it shall not be included.
     */

    test_input! {
      cmdsub_paren_basic     : "echo $(echo hi)"                   => "hi\n";
      cmdsub_backtick_basic  : "echo `echo hi`"                    => "hi\n";
      cmdsub_paren_in_word   : "echo pre$(echo mid)post"           => "premidpost\n";
      cmdsub_backtick_in_word: "echo pre`echo mid`post"            => "premidpost\n";
      cmdsub_empty           : "echo \"[$(:)]\""                   => "[]\n";
    }

    test_input! {
      cmdsub_strips_trailing_nl    : r#"echo "[$(printf 'foo\n\n\n')]""#  => "[foo]\n";
      cmdsub_preserves_internal_nl : r#"echo "$(printf 'a\nb\nc')""#      => "a\nb\nc\n";
      cmdsub_only_trailing_stripped: r#"echo "[$(printf '\nfoo\n')]""#    => "[\nfoo]\n";
    }

    test_input! {
      cmdsub_unquoted_splits        : r"for x in $(printf 'a\nb\nc'); do echo $x; done" => "a\nb\nc\n";
      cmdsub_quoted_one_field       : r#"for x in "$(printf 'a\nb\nc')"; do echo "[$x]"; done"# => "[a\nb\nc]\n";
      cmdsub_unquoted_splits_spaces : "echo $(echo a b c)"                                => "a b c\n";
    }

    test_input! {
      cmdsub_nested_paren        : "echo $(echo $(echo deep))"        => "deep\n";
      cmdsub_nested_in_dquote    : r#"echo "$(echo $(echo deep))""#   => "deep\n";
      cmdsub_paren_with_arith    : "echo $(echo $((1 + 2)))"          => "3\n";
      cmdsub_paren_with_var      : "x=hello; echo $(echo $x)"         => "hello\n";
    }

    test_input! {
      cmdsub_var_isolation : r#"x=outer; y=$(x=inner; echo $x); echo "$x:$y""# => "outer:inner\n";
      cmdsub_status_zero   : "$(true); echo $?"                                => "0\n";
      cmdsub_status_nonzero: "$(false); echo $?"                               => "1\n";
      cmdsub_multi_stmt_joined : r#"echo "$(echo a; echo b)""#                 => "a\nb\n";
    }
  }

  mod _4_arithmetic_sub {
    /*
     * §2.6.4 Arithmetic Expansion
     *
     * Arithmetic expansion provides a mechanism for evaluating an arithmetic
     * $((expression))
     * The expression shall be treated as if it were in double-quotes, except that a double-quote
     * inside the expression is not treated specially. The shell shall expand all tokens in the expression
     * for parameter expansion, command substitution, and quote removal.
     */

    test_input! {
      // based on a given example:
      setup: {
        crate::__test_setup_vars!([
          "x" => "100",
        ]);
      },
      arith_example: "while [[ $x -gt 0 ]]; do x=$(($x-1)); done; echo $x" => "0\n";
    }

    // basic operators, precedence, parentheses, unary.
    test_input! {
      arith_add            : "echo $((2 + 3))"           => "5\n";
      arith_sub            : "echo $((10 - 4))"          => "6\n";
      arith_mul            : "echo $((6 * 7))"           => "42\n";
      arith_div            : "echo $((20 / 4))"          => "5\n";
      arith_div_truncates  : "echo $((7 / 2))"           => "3\n";
      arith_mod            : "echo $((10 % 3))"          => "1\n";
      arith_unary_minus    : "echo $((-5))"              => "-5\n";
      arith_unary_plus     : "echo $((+5))"              => "5\n";
      arith_double_neg     : "echo $((- -5))"            => "5\n";
      arith_precedence     : "echo $((2 + 3 * 4))"       => "14\n";
      arith_parens         : "echo $(((2 + 3) * 4))"     => "20\n";
      arith_zero           : "echo $((0))"               => "0\n";
      arith_large          : "echo $((1000000 * 1000))"  => "1000000000\n";
    }

    // number formats: decimal, octal (0NNN), hex (0xNN).
    test_input! {
      arith_octal          : "echo $((010))"             => "8\n";
      arith_octal_arith    : "echo $((010 + 1))"         => "9\n";
      arith_hex_lower      : "echo $((0x1f))"            => "31\n";
      arith_hex_upper      : "echo $((0xFF))"            => "255\n";
      arith_hex_arith      : "echo $((0x10 + 0x10))"     => "32\n";

      // Leading zero on a single digit is still decimal 0.
      arith_just_zero      : "echo $((00 + 05))"           => "5\n";
    }

    // comparison + logical ops. Comparisons yield 1/0.
    test_input! {
      arith_lt_true        : "echo $((1 < 2))"           => "1\n";
      arith_lt_false       : "echo $((2 < 1))"           => "0\n";
      arith_le             : "echo $((2 <= 2))"          => "1\n";
      arith_gt             : "echo $((3 > 2))"           => "1\n";
      arith_ge             : "echo $((2 >= 2))"          => "1\n";
      arith_eq             : "echo $((5 == 5))"          => "1\n";
      arith_ne             : "echo $((5 != 4))"          => "1\n";
      arith_logical_and    : "echo $((1 && 1))"          => "1\n";
      arith_logical_or     : "echo $((0 || 1))"          => "1\n";
      arith_logical_not    : "echo $((!0))"              => "1\n";
      arith_not_nonzero    : "echo $((!5))"              => "0\n";
      // Short-circuit: 1/0 must not be evaluated when LHS makes result decided.
      arith_or_short_circuit  : "echo $((1 || 1/0))"     => "1\n";
      arith_and_short_circuit : "echo $((0 && 1/0))"     => "0\n";
    }

    // bitwise + ternary.
    test_input! {
      arith_bit_and        : "echo $((0xf & 0x3))"       => "3\n";
      arith_bit_or         : "echo $((0x4 | 0x1))"       => "5\n";
      arith_bit_xor        : "echo $((0x5 ^ 0x3))"       => "6\n";
      arith_bit_not        : "echo $((~0))"              => "-1\n";
      arith_shift_left     : "echo $((1 << 4))"          => "16\n";
      arith_shift_right    : "echo $((16 >> 2))"         => "4\n";
      arith_ternary_true   : "echo $((1 ? 10 : 20))"     => "10\n";
      arith_ternary_false  : "echo $((0 ? 10 : 20))"     => "20\n";
      arith_ternary_expr   : "echo $((5 > 3 ? 100 : 200))" => "100\n";
    }

    // variable interaction: bare names, $-prefixed, assignment.
    test_input! {
      setup: { crate::__test_setup_vars!(["a" => "10", "b" => "3"]); },
      arith_var_bare         : "echo $((a + b))"             => "13\n";
      arith_var_dollar       : "echo $(($a + $b))"           => "13\n";
      arith_var_mixed        : "echo $((a + $b))"            => "13\n";
      arith_var_unset_is_zero: "echo $((unset_var + 5))"     => "5\n";
      arith_assign_in_expr   : "y=$((x = 5)); echo \"$x:$y\""=> "5:5\n";
      arith_compound_assign  : "x=10; x=$((x += 3)); echo $x"=> "13\n";
      arith_nested           : "echo $(( $((2 + 3)) * 4 ))"  => "20\n";
    }
  }

  mod _5_field_splitting {
    /*
     * §2.6.5 Field Splitting
     *
     * After parameter expansion, command substitution, and arithmetic expansion, if the shell variable
     * IFS is set and its value is not empty, or if IFS is unset, the shell shall scan each field containing
     * results of expansions and substitutions that did not occur in double quotes for field splitting.
     * Zero, one, or multiple fields can result.
     */

    test_input! {
      setup: { crate::__test_setup_vars!(["v" => "a b c"]); },
      ifs_default_three_fields : r#"for x in $v; do echo "[$x]"; done"# => "[a]\n[b]\n[c]\n";
    }
    test_input! {
      setup: { crate::__test_setup_vars!(["v" => "  a   b  "]); },
      ifs_default_collapses_ws : r#"for x in $v; do echo "[$x]"; done"# => "[a]\n[b]\n";
    }
    test_input! {
      setup: { crate::__test_setup_vars!(["v" => "single"]); },
      ifs_default_one_field : r#"for x in $v; do echo "[$x]"; done"# => "[single]\n";
    }

    test_input! {
      setup: { crate::__test_setup_vars!(["v" => "a:b:c"]); },
      ifs_colon_three_fields : r#"IFS=:; for x in $v; do echo "[$x]"; done"# => "[a]\n[b]\n[c]\n";
    }
    test_input! {
      setup: { crate::__test_setup_vars!(["v" => "a::b"]); },
      ifs_colon_empty_between : r#"IFS=:; for x in $v; do echo "[$x]"; done"# => "[a]\n[]\n[b]\n";
    }
    test_input! {
      setup: { crate::__test_setup_vars!(["v" => ":a:b:"]); },
      ifs_colon_leading_trailing : r#"IFS=:; for x in $v; do echo "[$x]"; done"# => "[]\n[a]\n[b]\n";
    }

    test_input! {
      setup: { crate::__test_setup_vars!(["v" => "a : b : c"]); },
      ifs_mixed_absorbs_ws : r#"IFS=': '; for x in $v; do echo "[$x]"; done"# => "[a]\n[b]\n[c]\n";
    }
    test_input! {
      setup: { crate::__test_setup_vars!(["v" => " a b "]); },
      ifs_mixed_trims_ws_only : r#"IFS=': '; for x in $v; do echo "[$x]"; done"# => "[a]\n[b]\n";
    }

    test_input! {
      setup: { crate::__test_setup_vars!(["v" => "a b c"]); },
      ifs_empty_no_split : r#"IFS=""; for x in $v; do echo "[$x]"; done"# => "[a b c]\n";
    }

    test_input! {
      setup: { crate::__test_setup_vars!(["v" => "a b c"]); },
      ifs_quoted_no_split : r#"for x in "$v"; do echo "[$x]"; done"# => "[a b c]\n";
    }

    test_input! {
      setup: { crate::__test_setup_vars!(["a" => "x y", "b" => "z w"]); },
      ifs_adjacent_glue : r#"for v in $a$b; do echo "[$v]"; done"# => "[x]\n[yz]\n[w]\n";
    }

    test_input! {
      ifs_unset_var_zero_fields  : r#"for x in pre $unset_var post; do echo "[$x]"; done"#   => "[pre]\n[post]\n";
      ifs_quoted_empty_one_field : r#"for x in "$unset_var"; do echo "[$x]"; done"#          => "[]\n";
      ifs_cmdsub_splits          : r#"for x in $(printf 'a\nb\nc'); do echo "[$x]"; done"#   => "[a]\n[b]\n[c]\n";
      ifs_cmdsub_quoted_no_split : r#"for x in "$(printf 'a\nb\nc')"; do echo "[$x]"; done"# => "[a\nb\nc]\n";
    }
  }

  mod _6_path_expansion {
    /*
     * §2.6.6 Pathname Expansion
     * After field splitting, if set -f is not in effect, each field in the resulting command line
     * shall be expanded using the algorithm described in §2.14 Pattern Matching Notation, qualified
     * by the rules in §2.14.3 Patterns Used for Filename Expansion.
     */

    // TODO: actually figure out a way to test with filesystem I/O
    // so that we can glob on actual files
  }

  mod _7_quote_removal {
    /*
     * The quote character sequence <dollar-sign> single-quote and the single-character quotes
     * (<backslash>, single-quote, and double-quote) that were present in the original word shall
     * be removed unless they have themselves been quoted. Note that the single-quote character
     * that terminates a <dollar-sign> single-quote sequence is itself a single-character quote character.
     *
     * Note: After quote removal the shell shall remember which characters were quoted. This is necessary for
     * purposes such as matching patterns in a case conditional construct.
     */

    test_input! {
      qrm_squote_removed     : "echo 'foo'"             => "foo\n";
      qrm_dquote_removed     : r#"echo "foo""#          => "foo\n";
      qrm_dollar_squote_remov: r"echo $'foo'"         => "foo\n";
      qrm_concat_quotes      : r#"echo a'b'c"d"e"#      => "abcde\n";
      qrm_empty_quotes_join  : "echo a''b"              => "ab\n";
      qrm_empty_dquotes_join : r#"echo a""b"#           => "ab\n";
    }

    test_input! {
      qrm_dquote_in_squote   : r#"echo '"'"#            => "\"\n";
      qrm_squote_in_dquote   : r#"echo "'""#            => "'\n";
      qrm_backslash_escape   : r"echo \\"             => "\\\n";
      qrm_backslash_in_dq    : r#"echo "\\""#           => "\\\n";
      qrm_dollar_in_squote   : r"echo '$foo'"         => "$foo\n";
    }

    // The shell "remembers" which characters were quoted: glob/case meta-chars
    // in a quoted pattern are treated as literal at match time.
    test_input! {
      qrm_quoted_pattern_literal_star : r"case foo in '*') echo glob ;; foo) echo lit ;; esac" => "lit\n";
      qrm_unquoted_pattern_star       : r"case foo in *) echo glob ;; esac"                    => "glob\n";
      qrm_quoted_pattern_matches_lit  : r"case '*' in '*') echo lit ;; *) echo any ;; esac"    => "lit\n";
      qrm_dquote_pattern_literal      : r#"case foo in "*") echo glob ;; foo) echo lit ;; esac"# => "lit\n";
      qrm_escaped_pattern_meta        : r"case foo in \*) echo glob ;; foo) echo lit ;; esac"  => "lit\n";
    }
  }
}

mod redirection_2_7 {
  /*
   * §2.7 Redirection
   *
   * Redirection is used to open and close files for the current shell execution environment
   * or for any command. Redirection operators can be used with numbers representing file descriptors.
   */

  macro_rules! test_files {
    ($($name:ident: $input:expr => $file:expr => $content:expr;)*) => {
      $(#[test]
      fn $name() {
        let dir_path;
        let file_path;
        {
          let mut g = $crate::tests::testutil::TestGuard::new();
          let dir = g.in_temp_dir();
          dir_path = dir.clone();
          $crate::eval::execute::exec_nonint(
            $input.into(),
            Some("test_input".into())
          ).unwrap();
          let filename = dir.join($file);
          file_path = filename.clone();
          let file_content = ::std::fs::read_to_string(filename).unwrap();
          assert_eq!(file_content, $content);
        }
        assert!(!dir_path.exists(), "temp dir should have been cleaned up");
        assert!(!file_path.exists(), "temp file should have been cleaned up");
      })*
    };
  }

  mod _1_redirect_input {
    /*
     * §2.7.1 Redirecting Input
     *
     * Input redirection shall cause the file whose name results from the expansion of word to be
     * opened for reading on the designated file descriptor, or standard input if the file
     * descriptor is not specified.
     *
     * [n]<word
     *
     * If the number is omitted, the redirection shall refer to standard input (file descriptor 0).
     */

    macro_rules! with_file {
      ($input:literal) => {
        concat!("{ defer rm file.txt; ", $input, "; }")
      };
    }
    test_input! {
      // < with no n defaults to fd 0 (standard input)
      redirect_input_basic:
        with_file!("echo foo > file.txt; read var < file.txt; echo $var")
        => "foo\n";

      // 0< is explicitly fd 0, identical to <
      redirect_explicit_fd0:
        with_file!("echo foo > file.txt; read var 0< file.txt; echo $var")
        => "foo\n";

      // word undergoes expansion, variable in the filename
      redirect_word_expansion:
        with_file!("echo hello > file.txt; f=file.txt; read var < $f; echo $var")
        => "hello\n";

      // [n]<word with n>0 opens the file on that descriptor
      redirect_named_fd:
        with_file!("echo bar > file.txt; read var 4< file.txt <&4; echo $var")
        => "bar\n";
    }
  }

  mod _2_redirect_output {
    /*
     * §2.7.2 Redirecting Output
     *
     * [n]>word
     * [n]>|word
     *
     * If the number is omitted, the redirection shall refer to standard output (file descriptor 1).
     * Output redirection using the '>' format shall fail if the noclobber option is set and the
     * file named by the expansion of word exists and is a regular file. Otherwise, redirection
     * using the '>' or ">|" formats shall cause the file to be created and opened for output on
     * the designated file descriptor. If the file does not exist, it shall be created; otherwise,
     * it shall be truncated to be an empty file after being opened.
     */

    test_files! {
      // > with no n defaults to fd 1 (standard output)
      redirect_output_basic:
        "echo foo > out.txt"
        => "out.txt" => "foo\n";

      // 1> is explicitly fd 1, identical to >
      redirect_explicit_fd1:
        "echo foo 1> out.txt"
        => "out.txt" => "foo\n";

      // file is created if it does not exist
      redirect_creates_file:
        "echo hello > new.txt"
        => "new.txt" => "hello\n";

      // existing file is truncated to empty before writing
      redirect_truncates_existing:
        "echo original > f.txt; echo new > f.txt"
        => "f.txt" => "new\n";

      // word undergoes expansion
      redirect_word_expansion:
        "f=out.txt; echo content > $f"
        => "out.txt" => "content\n";

      // noclobber: > still creates a new file
      noclobber_allows_new_file:
        "set -C; echo hello > new.txt"
        => "new.txt" => "hello\n";

      // >| overrides noclobber
      force_clobber_overrides_noclobber:
        "echo original > f.txt; set -C; echo new >| f.txt"
        => "f.txt" => "new\n";

      // >| works when noclobber is not set
      force_clobber_without_noclobber:
        "echo original > f.txt; echo new >| f.txt"
        => "f.txt" => "new\n";
    }

    #[test]
    fn noclobber_prevents_overwrite() {
      // noclobber causes > to return a Rust-level Err, so we can't use test_files! here
      let mut g = crate::tests::testutil::TestGuard::new();
      let dir = g.in_temp_dir();
      crate::eval::execute::exec_nonint(
        "echo original > protected.txt; set -C; echo new > protected.txt".into(),
        Some("test_input".into()),
      )
      .ok();
      let content = ::std::fs::read_to_string(dir.join("protected.txt")).unwrap();
      assert_eq!(content, "original\n");
    }
  }

  mod _3_redirect_output_append {
    /*
     * §2.7.3 Appending Redirected Output
     *
     * Appended output redirection shall cause the file whose name results from the expansion of
     * word to be opened for output on the designated file descriptor. The file is opened as if the
     * open() function was called with the O_APPEND flag. If the file does not exist, it shall be
     * created.
     *
     * [n]>>word
     *
     * If the number is omitted, the redirection refers to standard output (file descriptor 1).
     */

    test_files! {
      // >> with no n defaults to fd 1 (standard output)
      append_output_basic:
        "echo first > f.txt; echo second >> f.txt"
        => "f.txt" => "first\nsecond\n";

      // 1>> is explicitly fd 1, identical to >>
      append_explicit_fd1:
        "echo first > f.txt; echo second 1>> f.txt"
        => "f.txt" => "first\nsecond\n";

      // file is created if it does not exist
      append_creates_file:
        "echo hello >> new.txt"
        => "new.txt" => "hello\n";

      // existing content is preserved (O_APPEND semantics)
      append_preserves_existing:
        "echo line1 > f.txt; echo line2 >> f.txt; echo line3 >> f.txt"
        => "f.txt" => "line1\nline2\nline3\n";

      // word undergoes expansion
      append_word_expansion:
        "f=out.txt; echo first > $f; echo second >> $f"
        => "out.txt" => "first\nsecond\n";
    }
  }

  mod _4_here_document {
    /*
     * §2.7.4 Here-Document
     *
     * [n]<<word        - expand body (parameter, command sub, arithmetic)
     * [n]<<-word       - same, but strip leading <tab> from each body line and delimiter
     *
     * If any part of word is quoted, the delimiter is formed by quote-removal and the
     * body is not expanded. Otherwise all lines are expanded as if inside double-quotes
     * (backslash is special before $, `, \, and <newline>; double-quote is NOT special).
     *
     * If the number n is omitted the here-document is fed to standard input (fd 0).
     */

    test_input! {
      heredoc_basic:
        "read var <<EOF\nhello\nEOF\necho $var"
        => "hello\n";

      heredoc_param_expansion:
        "X=42\nread var <<EOF\nval$X\nEOF\necho $var"
        => "val42\n";

      heredoc_command_substitution:
        "read var <<EOF\n$(echo hi)\nEOF\necho $var"
        => "hi\n";

      heredoc_arithmetic_expansion:
        "read var <<EOF\n$((1+1))\nEOF\necho $var"
        => "2\n";

      heredoc_single_quoted_no_expansion:
        "X=42\nread var <<'EOF'\n$X\nEOF\necho $var"
        => "$X\n";

      heredoc_double_quoted_no_expansion:
        "X=42\nread var <<\"EOF\"\n$X\nEOF\necho $var"
        => "$X\n";

      heredoc_backslash_suppresses_dollar:
        "X=42\nread var <<EOF\n\\$X\nEOF\necho $var"
        => "$X\n";

      heredoc_backslash_backslash:
        "read -r var <<EOF\n\\\\\nEOF\necho $var"
        => "\\\n";

      heredoc_multiline:
        "{ read a; read b; } <<EOF\nfoo\nbar\nEOF\necho $a $b"
        => "foo bar\n";

      heredoc_strip_tabs:
        "read var <<-EOF\n\thello\n\tEOF\necho $var"
        => "hello\n";

      heredoc_strip_tabs_multiple:
        "{ read a; read b; } <<-EOF\n\tfoo\n\t\tbar\n\tEOF\necho $a $b"
        => "foo bar\n";

      heredoc_strip_tabs_preserves_spaces:
        "export IFS=''; read var <<-EOF\n\t  spaced\n\tEOF\necho $var"
        => "  spaced\n";

      heredoc_multiple:
        "cat <<A; cat <<B\nfoo\nA\nbar\nB"
        => "foo\nbar\n";
    }
  }
}

mod exit_status_2_8 {
  /*
   * §2.8 Exit Status
   *
   * Certain errors shall cause the shell to write a diagnostic message to
   * standard error and exit.
   */

  use crate::{eval::execute::exec_dash_c, state::Shed, tests::testutil::TestGuard};

  /*
   * §2.8.1 Consequences of Shell Errors
   *
   * "A redirection error [on a command that is not a special built-in] shall
   * not cause the shell to abort." It sets a non-zero exit status and the shell
   * continues with the next command; the command whose redirection failed is
   * not executed.
   */

  #[test]
  fn redirect_error_on_simple_command_is_nonfatal() {
    let g = TestGuard::new();
    exec_dash_c(
      "echo before; echo SKIP > /nonexistent_dir_xyzq/f; echo after".into(),
      vec![],
    )
    .unwrap();
    let out = g.read_output();
    assert!(out.contains("before"), "got: {out:?}");
    assert!(
      out.contains("after"),
      "later command must still run: {out:?}"
    );
    assert!(
      !out.contains("SKIP"),
      "the command whose redirection failed must not run: {out:?}"
    );
  }

  #[test]
  fn redirect_error_sets_nonzero_status() {
    let _g = TestGuard::new();
    exec_dash_c("echo x > /nonexistent_dir_xyzq/f".into(), vec![]).unwrap();
    assert_ne!(Shed::get_status(), 0);
  }

  #[test]
  fn redirect_error_on_nonspecial_builtin_is_nonfatal() {
    // `true` is a builtin but not a *special* built-in, so its redirection
    // error is non-fatal like any ordinary command.
    let g = TestGuard::new();
    exec_dash_c(
      "true > /nonexistent_dir_xyzq/f; echo reached".into(),
      vec![],
    )
    .unwrap();
    assert!(
      g.read_output().contains("reached"),
      "must continue past the failure"
    );
  }

  #[test]
  fn redirect_error_on_function_is_nonfatal() {
    // A redirection error on a function invocation skips the body but must not
    // abort the shell.
    let g = TestGuard::new();
    exec_dash_c(
      "f(){ echo FROMFUNC; }; f > /nonexistent_dir_xyzq/f; echo after".into(),
      vec![],
    )
    .unwrap();
    let out = g.read_output();
    assert!(out.contains("after"), "must continue: {out:?}");
    assert!(
      !out.contains("FROMFUNC"),
      "function body must be skipped: {out:?}"
    );
  }

  #[test]
  fn redirect_error_on_compound_is_nonfatal() {
    // Same for compound commands (brace group / loop / if).
    let g = TestGuard::new();
    exec_dash_c(
      "{ echo FROMGRP; } > /nonexistent_dir_xyzq/f; echo after".into(),
      vec![],
    )
    .unwrap();
    let out = g.read_output();
    assert!(out.contains("after"), "must continue: {out:?}");
    assert!(
      !out.contains("FROMGRP"),
      "compound body must be skipped: {out:?}"
    );
  }

  #[test]
  fn redirect_error_on_special_builtin_is_fatal() {
    // `:` is a special built-in: a redirection error is fatal (non-interactive),
    // so the following command must not run.
    let g = TestGuard::new();
    exec_dash_c(": > /nonexistent_dir_xyzq/f; echo AFTER".into(), vec![]).ok();
    assert!(
      !g.read_output().contains("AFTER"),
      "special-built-in redirection error must abort the input"
    );
  }
}
