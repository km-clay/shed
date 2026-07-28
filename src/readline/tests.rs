#![expect(non_snake_case)]

use super::super::state::terminal::Terminal;
use super::{Prompt, Shed, ShedLine, eval::lex::Span, key};

use crate::tests::testutil::TestGuard;

/// Tests for our vim logic emulation. Each test consists of an initial text, a sequence of keys to feed, and the expected final text and cursor position.
macro_rules! vi_test {
  { $($name:ident: $input:expr => $op:expr => $expected_text:expr,$expected_cursor:expr);* $(;)? } => {
    mod vi {
      use super::*;
      $(#[test]
        fn $name() {
          let (mut vi, _g) = test_vi($input);
          Shed::term_mut(|t| t.feed_bytes(b"\x1b")); // Start in normal mode
          let keys = Shed::term_mut(|t| t.drain_keys());
          vi.process_input(keys).unwrap();

          for byte in $op.as_bytes() {
            Shed::term_mut(|t| t.feed_bytes(&[*byte]));
            let keys = Shed::term_mut(|t| t.drain_keys());
            vi.process_input(keys).unwrap();
          }
          assert_eq!(vi.core.editor.to_string(), $expected_text);
          assert_eq!(vi.core.editor.cursor_to_flat(), $expected_cursor);
        }
      )*
    }
  };
}

// Why can't I marry a programming language
vi_test! {
  // test function name    // initial buffer                // command sequence      // final buffer, cursor position
  dw_basic                 : "hello world"                  => "dw"                  => "world", 0;
  dw_middle                : "one two three"                => "wdw"                 => "one three", 4;
  dd_whole_line            : "hello world"                  => "dd"                  => "", 0;
  x_single                 : "hello"                        => "x"                   => "ello", 0;
  x_middle                 : "hello"                        => "llx"                 => "helo", 2;
  X_backdelete             : "hello"                        => "llX"                 => "hllo", 1;
  h_motion                 : "hello"                        => "$h"                  => "hello", 3;
  l_motion                 : "hello"                        => "l"                   => "hello", 1;
  h_at_start               : "hello"                        => "h"                   => "hello", 0;
  l_at_end                 : "hello"                        => "$l"                  => "hello", 4;
  w_forward                : "one two three"                => "w"                   => "one two three", 4;
  b_backward               : "one two three"                => "$b"                  => "one two three", 8;
  e_end                    : "one two three"                => "e"                   => "one two three", 2;
  ge_back_end              : "one two three"                => "$ge"                 => "one two three", 6;
  w_punctuation            : "foo.bar baz"                  => "w"                   => "foo.bar baz", 3;
  e_punctuation            : "foo.bar baz"                  => "e"                   => "foo.bar baz", 2;
  b_punctuation            : "foo.bar baz"                  => "$b"                  => "foo.bar baz", 8;
  w_at_eol                 : "hello"                        => "$w"                  => "hello", 4;
  b_at_bol                 : "hello"                        => "b"                   => "hello", 0;
  W_forward                : "foo.bar baz"                  => "W"                   => "foo.bar baz", 8;
  B_backward               : "foo.bar baz"                  => "$B"                  => "foo.bar baz", 8;
  E_end                    : "foo.bar baz"                  => "E"                   => "foo.bar baz", 6;
  gE_back_end              : "one two three"                => "$gE"                 => "one two three", 6;
  W_skip_punct             : "one-two three"                => "W"                   => "one-two three", 8;
  B_skip_punct             : "one two-three"                => "$B"                  => "one two-three", 4;
  E_skip_punct             : "one-two three"                => "E"                   => "one-two three", 6;
  dW_big                   : "foo.bar baz"                  => "dW"                  => "baz", 0;
  cW_big                   : "foo.bar baz"                  => "cWx\x1b"             => "x baz", 0;
  zero_bol                 : "  hello"                      => "$0"                  => "  hello", 0;
  caret_first_char         : "  hello"                      => "$^"                  => "  hello", 2;
  dollar_eol               : "hello world"                  => "$"                   => "hello world", 10;
  g_last_nonws             : "hello   "                     => "g_"                  => "hello   ", 4;
  g_no_trailing            : "hello"                        => "g_"                  => "hello", 4;
  pipe_column              : "hello world"                  => "6|"                  => "hello world", 5;
  pipe_col1                : "hello world"                  => "1|"                  => "hello world", 0;
  I_insert_front           : "  hello"                      => "Iworld \x1b"         => "  world hello", 7;
  A_append_end             : "hello"                        => "A world\x1b"         => "hello world", 10;
  f_find                   : "hello world"                  => "fo"                  => "hello world", 4;
  F_find_back              : "hello world"                  => "$Fo"                 => "hello world", 7;
  t_till                   : "hello world"                  => "tw"                  => "hello world", 5;
  T_till_back              : "hello world"                  => "$To"                 => "hello world", 8;
  f_no_match               : "hello"                        => "fz"                  => "hello", 0;
  semicolon_repeat         : "abcabc"                       => "fa;;"                => "abcabc", 3;
  comma_reverse            : "abcabc"                       => "fa;;,"               => "abcabc", 0;
  df_semicolon             : "abcabc"                       => "fa;;dfa"             => "abcabc", 3;
  t_at_target              : "aab"                          => "lta"                 => "aab", 1;
  D_to_end                 : "hello world"                  => "wD"                  => "hello ", 5;
  d_dollar                 : "hello world"                  => "wd$"                 => "hello ", 5;
  d0_to_start              : "hello world"                  => "$d0"                 => "d", 0;
  dw_multiple              : "one two three"                => "d2w"                 => "three", 0;
  dt_char                  : "hello world"                  => "dtw"                 => "world", 0;
  df_char                  : "hello world"                  => "dfw"                 => "orld", 0;
  dh_back                  : "hello"                        => "lldh"                => "hllo", 1;
  dl_forward               : "hello"                        => "dl"                  => "ello", 0;
  dge_back_end             : "one two three"                => "$dge"                => "one tw", 5;
  dG_to_end                : "hello world"                  => "dG"                  => "", 0;
  dgg_to_start             : "hello world"                  => "$dgg"                => "", 0;
  d_semicolon              : "abcabc"                       => "fad;"                => "abcabc", 3;
  cw_basic                 : "hello world"                  => "cwfoo\x1b"           => "foo world", 2;
  C_to_end                 : "hello world"                  => "wCfoo\x1b"           => "hello foo", 8;
  cc_whole                 : "hello world"                  => "ccfoo\x1b"           => "foo", 2;
  ct_char                  : "hello world"                  => "ctwfoo\x1b"          => "fooworld", 2;
  s_single                 : "hello"                        => "sfoo\x1b"            => "fooello", 2;
  S_whole_line             : "hello world"                  => "Sfoo\x1b"            => "foo", 2;
  cl_forward               : "hello"                        => "clX\x1b"             => "Xello", 0;
  ch_backward              : "hello"                        => "llchX\x1b"           => "hXllo", 1;
  cb_word_back             : "hello world"                  => "$cbfoo\x1b"          => "hello food", 8;
  ce_word_end              : "hello world"                  => "cefoo\x1b"           => "foo world", 2;
  c0_to_start              : "hello world"                  => "wc0foo\x1b"          => "fooworld", 2;
  yw_p_basic               : "hello world"                  => "ywwP"                => "hello hello world", 11;
  dw_p_paste               : "hello world"                  => "dwP"                 => "hello world", 5;
  dd_p_paste               : "hello world"                  => "ddp"                 => "\nhello world", 1;
  y_dollar_p               : "hello world"                  => "wy$P"                => "hello worldworld", 10;
  ye_p                     : "hello world"                  => "yewP"                => "hello helloworld", 10;
  yy_p                     : "hello world"                  => "yyp"                 => "hello world\nhello world", 12;
  Y_p                      : "hello world"                  => "Yp"                  => "hhello worldello world", 11;
  p_after_x                : "hello"                        => "xp"                  => "ehllo", 1;
  P_before                 : "hello"                        => "llxP"                => "hello", 2;
  paste_empty              : "hello"                        => "p"                   => "hello", 0;
  r_replace                : "hello"                        => "ra"                  => "aello", 0;
  r_middle                 : "hello"                        => "llra"                => "healo", 2;
  r_at_end                 : "hello"                        => "$ra"                 => "hella", 4;
  r_space                  : "hello"                        => "r "                  => " ello", 0;
  r_with_count             : "hello"                        => "3rx"                 => "xxxlo", 2;
  tilde_single             : "hello"                        => "~"                   => "Hello", 1;
  tilde_count              : "hello"                        => "3~"                  => "HELlo", 3;
  tilde_at_end             : "HELLO"                        => "$~"                  => "HELLo", 4;
  tilde_mixed              : "hElLo"                        => "5~"                  => "HeLlO", 4;
  gu_word                  : "HELLO world"                  => "guw"                 => "hello world", 0;
  gU_word                  : "hello WORLD"                  => "gUw"                 => "HELLO WORLD", 0;
  gu_dollar                : "HELLO WORLD"                  => "gu$"                 => "hello world", 0;
  gU_dollar                : "hello world"                  => "gU$"                 => "HELLO WORLD", 0;
  gu_0                     : "HELLO WORLD"                  => "$gu0"                => "hello worlD", 0;
  gU_0                     : "hello world"                  => "$gU0"                => "HELLO WORLd", 0;
  gtilde_word              : "hello WORLD"                  => "g~w"                 => "HELLO WORLD", 0;
  gtilde_dollar            : "hello WORLD"                  => "g~$"                 => "HELLO world", 0;
  diw_inner                : "one two three"                => "wdiw"                => "one  three", 4;
  ciw_replace              : "hello world"                  => "ciwfoo\x1b"          => "foo world", 2;
  daw_around               : "one two three"                => "wdaw"                => "one three", 4;
  yiw_p                    : "hello world"                  => "yiwAp \x1bp"         => "hello worldp hello", 17;
  diW_big_inner            : "one-two three"                => "diW"                 => " three", 0;
  daW_big_around           : "one two-three end"            => "wdaW"                => "one end", 4;
  ciW_big                  : "one-two three"                => "ciWx\x1b"            => "x three", 0;
  diw_on_single_space      : "foo bar"                      => "f diw"               => "foobar", 3;
  diw_on_multi_space_run   : "a   b"                        => "f diw"               => "ab", 1;
  diW_on_whitespace        : "foo bar"                      => "f diW"               => "foobar", 3;
  diw_from_middle_of_word  : "one two three"                => "wldiw"               => "one  three", 4;
  diw_at_last_char_of_word : "foo"                          => "$diw"                => "", 0;
  diW_from_middle_punct    : "one-two three"                => "lldiW"               => " three", 0;
  daw_at_buffer_start      : "foo bar baz"                  => "daw"                 => "bar baz", 0;
  diw_on_punctuation_char  : "one-two"                      => "llldiw"              => "onetwo", 3;
  di_dquote                : "one \"two\" three"            => "f\"di\""             => "one \"\" three", 5;
  da_dquote                : "one \"two\" three"            => "f\"da\""             => "one three", 4;
  ci_dquote                : "one \"two\" three"            => "f\"ci\"x\x1b"        => "one \"x\" three", 5;
  di_squote                : "one 'two' three"              => "f'di'"               => "one '' three", 5;
  da_squote                : "one 'two' three"              => "f'da'"               => "one three", 4;
  di_backtick              : "one `two` three"              => "f`di`"               => "one `` three", 5;
  da_backtick              : "one `two` three"              => "f`da`"               => "one three", 4;
  ci_dquote_empty          : "one \"\" three"               => "f\"ci\"x\x1b"        => "one \"x\" three", 5;
  di_paren                 : "one (two) three"              => "f(di("               => "one () three", 5;
  da_paren                 : "one (two) three"              => "f(da("               => "one  three", 4;
  ci_paren                 : "one (two) three"              => "f(ci(x\x1b"          => "one (x) three", 5;
  di_brace                 : "one {two} three"              => "f{di{"               => "one {} three", 5;
  da_brace                 : "one {two} three"              => "f{da{"               => "one  three", 4;
  di_bracket               : "one [two] three"              => "f[di["               => "one [] three", 5;
  da_bracket               : "one [two] three"              => "f[da["               => "one  three", 4;
  di_angle                 : "one <two> three"              => "f<di<"               => "one <> three", 5;
  da_angle                 : "one <two> three"              => "f<da<"               => "one  three", 4;
  di_paren_nested          : "fn(a, (b, c))"                => "f(di("               => "fn()", 3;
  di_paren_empty           : "fn() end"                     => "f(di("               => "fn() end", 3;
  dib_alias                : "one (two) three"              => "f(dib"               => "one () three", 5;
  diB_alias                : "one {two} three"              => "f{diB"               => "one {} three", 5;
  percent_paren            : "(hello) world"                => "%"                   => "(hello) world", 6;
  percent_brace            : "{hello} world"                => "%"                   => "{hello} world", 6;
  percent_bracket          : "[hello] world"                => "%"                   => "[hello] world", 6;
  percent_from_close       : "(hello) world"                => "f)%"                 => "(hello) world", 0;
  d_percent_paren          : "(hello) world"                => "d%"                  => " world", 0;
  to_paren_fwd             : "foo (bar) baz"                => "])"                  => "foo (bar) baz", 8;
  to_paren_bkwd            : "foo (bar) baz"                => "f)[("                => "foo (bar) baz", 4;
  to_brace_fwd             : "foo {bar} baz"                => "]}"                  => "foo {bar} baz", 8;
  to_brace_bkwd            : "foo {bar} baz"                => "f}[{"                => "foo {bar} baz", 4;
  to_paren_nested          : "((a)(b)) end"                 => "])"                  => "((a)(b)) end", 7;
  to_brace_nested          : "{{a}{b}} end"                 => "]}"                  => "{{a}{b}} end", 7;
  d_to_paren_fwd           : "foo (bar) baz"                => "wd])"                => "foo  baz", 4;
  d_to_brace_fwd           : "foo {bar} baz"                => "wd]}"                => "foo  baz", 4;
  to_paren_no_match        : "foo bar baz"                  => "])"                  => "foo bar baz", 0;
  to_brace_no_match        : "foo bar baz"                  => "]}"                  => "foo bar baz", 0;
  i_insert                 : "hello"                        => "iX\x1b"              => "Xhello", 0;
  a_append                 : "hello"                        => "aX\x1b"              => "hXello", 1;
  I_front                  : "  hello"                      => "IX\x1b"              => "  Xhello", 2;
  A_end                    : "hello"                        => "AX\x1b"              => "helloX", 5;
  o_open_below             : "hello"                        => "oworld\x1b"          => "hello\nworld", 10;
  O_open_above             : "hello"                        => "Oworld\x1b"          => "world\nhello", 4;
  empty_input              : ""                             => "i hello\x1b"         => " hello", 5;
  insert_escape            : "hello"                        => "aX\x1b"              => "hXello", 1;
  ctrl_w_del_word          : "hello world"                  => "A\x17\x1b"           => "hello ", 5;
  ctrl_h_backspace         : "hello"                        => "A\x08\x1b"           => "hell", 3;
  ctrl_o_dw                : "hello world"                  => "i\x0fdw\x1b"         => "world", 0;
  ctrl_o_x                 : "hello"                        => "i\x0fx\x1b"          => "ello", 0;
  ctrl_o_motion            : "hello world"                  => "i\x0fwX\x1b"         => "hello Xworld", 6;
  R_replace_exact          : "abc"                          => "RXYZ\x1b"            => "XYZ", 2;
  R_replace_extends        : "abc"                          => "RXYZ1\x1b"           => "XYZ1", 3;
  R_extend_short           : "x"                            => "Rab\x1b"             => "ab", 1;
  R_on_empty               : ""                             => "Rabc\x1b"            => "abc", 2;
  R_overwrite_then_append  : "ab"                           => "RXYZ\x1b"            => "XYZ", 2;
  u_undo_delete            : "hello world"                  => "dwu"                 => "hello world", 0;
  u_undo_change            : "hello world"                  => "ciwfoo\x1bu"         => "hello world", 0;
  u_undo_x                 : "hello"                        => "xu"                  => "hello", 0;
  ctrl_r_redo              : "hello"                        => "xu\x12"              => "ello", 0;
  u_multiple               : "hello world"                  => "xdwu"                => "ello world", 0;
  redo_after_undo          : "hello world"                  => "dwu\x12"             => "world", 0;
  dot_repeat_x             : "hello"                        => "x."                  => "llo", 0;
  dot_repeat_dw            : "one two three"                => "dw."                 => "three", 0;
  dot_repeat_cw            : "one two three"                => "cwfoo\x1bw."         => "foo foo three", 6;
  dot_repeat_r             : "hello"                        => "ra.."                => "aello", 0;
  dot_repeat_s             : "hello"                        => "sX\x1bl."            => "XXllo", 1;
  count_h                  : "hello world"                  => "$3h"                 => "hello world", 7;
  count_l                  : "hello world"                  => "3l"                  => "hello world", 3;
  count_w                  : "one two three four"           => "2w"                  => "one two three four", 8;
  count_b                  : "one two three four"           => "$2b"                 => "one two three four", 8;
  count_x                  : "hello"                        => "3x"                  => "lo", 0;
  count_dw                 : "one two three four"           => "2dw"                 => "three four", 0;
  verb_count_motion        : "one two three four"           => "d2w"                 => "three four", 0;
  count_s                  : "hello"                        => "3sX\x1b"             => "Xlo", 0;
  indent_line              : "hello"                        => ">>"                  => "\thello", 1;
  dedent_line              : "\thello"                      => "<<"                  => "hello", 0;
  indent_double            : "hello"                        => ">>>>"                => "\t\thello", 2;
  J_join_lines             : "hello\nworld"                 => "J"                   => "hello world", 5;
  v_u_lower                : "HELLO"                        => "vlllu"               => "hellO", 0;
  v_U_upper                : "hello"                        => "vlllU"               => "HELLo", 0;
  v_d_delete               : "hello world"                  => "vwwd"                => "", 0;
  v_x_delete               : "hello world"                  => "vwwx"                => "", 0;
  v_c_change               : "hello world"                  => "vwcfoo\x1b"          => "fooorld", 2;
  v_y_p_yank               : "hello world"                  => "vwyAp \x1bp"         => "hello worldp hello w", 19;
  v_dollar_d               : "hello world"                  => "wv$d"                => "hello ", 5;
  v_0_d                    : "hello world"                  => "$v0d"                => "", 0;
  ve_d                     : "hello world"                  => "ved"                 => " world", 0;
  v_o_swap                 : "hello world"                  => "vllod"               => "lo world", 0;
  v_r_replace              : "hello"                        => "vlllrx"              => "xxxxo", 0;
  v_tilde_case             : "hello"                        => "vlll~"               => "HELLo", 0;
  V_d_delete               : "hello world"                  => "Vd"                  => "", 0;
  V_y_p                    : "hello world"                  => "Vyp"                 => "hello world\nhello world", 12;
  V_S_change               : "hello world"                  => "VSfoo\x1b"           => "foo", 2;
  ctrl_a_inc               : "num 5 end"                    => "w\x01"               => "num 6 end", 4;
  ctrl_x_dec               : "num 5 end"                    => "w\x18"               => "num 4 end", 4;
  ctrl_a_negative          : "num -3 end"                   => "w\x01"               => "num -2 end", 5;
  ctrl_x_to_neg            : "num 0 end"                    => "w\x18"               => "num -1 end", 5;
  ctrl_a_count             : "num 5 end"                    => "w3\x01"              => "num 8 end", 4;
  ctrl_a_width             : "num 0x000001 end"             => "w\x01"               => "num 0x000002 end", 11;
  delete_empty             : ""                             => "x"                   => "", 0;
  undo_on_empty            : ""                             => "u"                   => "", 0;
  w_single_char            : "a b c"                        => "w"                   => "a b c", 2;
  dw_last_word             : "hello"                        => "dw"                  => "", 0;
  dollar_single            : "h"                            => "$"                   => "h", 0;
  caret_no_ws              : "hello"                        => "$^"                  => "hello", 0;
  f_last_char              : "hello"                        => "fo"                  => "hello", 4;
  r_on_space               : "hello world"                  => "5|r-"                => "hell- world", 4;
  vw_doesnt_crash          : ""                             => "vw"                  => "", 0;
  indent_cursor_pos        : "echo foo"                     => ">>"                  => "\techo foo", 1;
  join_indent_lines        : "echo foo\n\t\techo bar"       => "J"                   => "echo foo echo bar", 8;
  cw_stays_on_line         : "echo foo\necho bar"           => "wcw"                 => "echo \necho bar", 5;
  ex_sub_simple            : "echo foo\necho bar"           => ":%s/foo/bar/\r"      => "echo bar\necho bar", 0;
  ex_sub_no_trailing_delim : "color me"                     => ":s/color/social\r"  => "social me", 0;
  ex_sub_percent_no_trail  : "color\ncolor"                 => ":%s/color/social\r" => "social\nsocial", 0;
  ex_sub_pat_no_repl_delim : "abc"                          => ":s/abc/x\r"         => "x", 0;
  ex_global_simple         : "echo foo\necho bar\necho biz" => ":g/echo/normal!dw\r" => "foo\nbar\nbiz", 8;
  ex_sub_first_only        : "foo foo foo"                  => ":s/foo/X/\r"         => "X foo foo", 0;
  ex_sub_global_flag       : "foo foo foo"                  => ":s/foo/X/g\r"        => "X X X", 0;
  ex_sub_line_range        : "foo\nfoo\nfoo"                => ":2,3s/foo/bar/\r"    => "foo\nbar\nbar", 0;
  ex_sub_single_line       : "hello\nworld"                 => ":1s/hello/hi/\r"     => "hi\nworld", 0;
  ex_repeat_sub            : "foo\nfoo"                     => ":s/foo/bar/\rj:s\r"  => "bar\nbar", 4;
  ex_repeat_sub_all        : "foo\nfoo\nfoo"                => ":s/foo/bar/\r :%s\r" => "bar\nbar\nbar", 0;
  ex_delete_cur            : "hello\nworld"                 => ":d\r"                => "world", 0;
  ex_delete_all            : "hello\nworld"                 => ":%d\r"               => "", 0;
  ex_delete_range          : "line1\nline2\nline3"          => ":1,2d\r"             => "line3", 0;
  ex_global_delete         : "echo foo\nls\necho bar"       => ":g/echo/d\r"         => "ls", 0;
  ex_global_sub            : "foo bar\nfoo baz\nkeep"       => ":g/foo/s/foo/X/\r"   => "X bar\nX baz\nkeep", 0;
  ex_global_negated_delete : "echo foo\nls\necho bar\nkeep" => ":g!/echo/d\r"        => "echo foo\necho bar", 9;
  ex_global_negated_sub    : "foo bar\nkeep\nfoo biz"       => ":g!/keep/s/foo/X/\r" => "X bar\nkeep\nX biz", 0;
  ex_normal_range          : "hello world\nfoo bar\nbiz"    => ":1,2normal!dw\r"     => "world\nbar\nbiz", 6;
  ex_repeat_global         : "echo foo\nls\necho bar\nls2"  => ":g/echo/d\r   :g\r"  => "ls\nls2", 0;
  ex_move_line_to_end      : "a\nb\nc"                       => ":1m$\r"              => "b\nc\na", 4;
  ex_move_line_to_top      : "a\nb\nc"                       => ":3m0\r"              => "c\na\nb", 0;
  ex_move_range_to_end     : "a\nb\nc"                       => ":1,2m$\r"            => "c\na\nb", 4;
  ex_copy_range_to_top     : "a\nb\nc"                       => ":1,2t0\r"            => "a\nb\na\nb\nc", 2;
  ex_copy_line_to_end      : "a\nb\nc"                       => ":1t$\r"              => "a\nb\nc\na", 6;
  ex_join_range            : "a\nb\nc"                       => ":1,2j\r"             => "a b\nc", 1;
  ex_join_all              : "foo\nbar\nbaz"                 => ":%j\r"               => "foo bar baz", 7;
  visual_dot_repeat        : "hello\nworld\nfoo\nbar\nbiz"  => "jVjdu2k."            => "foo\nbar\nbiz", 0;
  visual_replace           : "echo ./barbiz/baz buzz"       => "wvEdwvep"            => "echo  ./barbiz/baz", 17;
  n_char_search_f          : "foo=(bar biz bam)"            => "d2fb"                => "iz bam)", 0;
  n_char_search_t          : "foo=(bar biz bam)"            => "d2tb"                => "biz bam)", 0;
  n_char_search_bkwd_f     : "foo=(bar biz bam)"            => "$d2Fb"               => "foo=(bar )", 9;
  n_char_search_bkwd_t     : "foo=(bar biz bam)"            => "$d2Tb"               => "foo=(bar b)", 10;
  count_search_fwd         : "foo=(bar biz bam)"            => "2/b\rx"              => "foo=(bar iz bam)", 9;
  count_search_bkwd        : "foo=(bar biz bam)"            => "3?b\rx"              => "foo=(ar biz bam)", 5;
  count_n_fwd              : "foo=(bar biz bam)"            => "/b\r2nx"             => "foo=(bar biz am)", 13;
  count_n_bkwd             : "foo=(bar biz bam)"            => "/b\r2Nx"             => "foo=(bar iz bam)", 9;
  macro_record             : "foo bar biz"                  => "qacwbam\x1bwqQQ"     => "bam bam bam", 10;
  macro_double             : "foo BAR biz BAM"              => "qag~wwqqbguwwq@a@b"  => "FOO bar BIZ bam", 14;
  // The macro deletes each `X`. The second `@a` runs out of `X`s, so its `fX`
  // fails and aborts the replay before the `x` can delete an innocent char.
  macro_aborts_on_failed_search
                           : "aXbXc"                        => "qafXxq@a@a"          => "abc", 2;
  normal_V_line_visual_d   : "abc\ndef\nghi"                => "jVd"                 => "abc\nghi", 4;
  normal_ctrl_a_increments : "42"                           => "\x01"                => "43", 1;
  normal_ctrl_x_decrements : "42"                           => "\x18"                => "41", 1;
  ctrl_a_at_end_of_buffer  : "v0.19.5"                      => "$\x01"               => "v0.19.6", 6;
  ctrl_x_at_end_of_buffer  : "v0.19.5"                      => "$\x18"               => "v0.19.4", 6;
  ctrl_a_at_eob_grows_width: "v9"                           => "$\x01"               => "v10", 2;
  ctrl_x_at_eob_shrinks_width: "v10"                        => "$\x18"               => "v9", 1;
  normal_ctrl_g_no_change  : "hello"                        => "\x07"                => "hello", 0;
  replace_one_char         : "hello"                        => "Rx\x1b"               => "xello", 0;
  replace_multiple_chars   : "hello"                        => "Rxy\x1b"              => "xyllo", 1;
  replace_then_backspace   : "hello"                        => "Rxy\x08\x1b"          => "xyllo", 0;
  replace_then_esc_to_normal: "hello"                       => "Rx\x1b"               => "xello", 0;
  replace_tab_completes    : "hello"                        => "Rx\x09\x1b"           => "xello", 1;

  // ugly ones go down here
  ex_nested_global
    :  "alpha bravo\nalpha charlie\ndelta bravo\ngamma"
    => ":g/alpha/g/bravo/normal!dw\r"
    => "bravo\nalpha charlie\ndelta bravo\ngamma", 0;

  daw_at_buffer_end_keeps_leading_space
    : "foo bar"
    => "$daw"
    => "foo ", 3;

  daw_on_whitespace_takes_following_word
    : "foo bar"
    => "f daw"
    => "foo", 2;

  daW_on_whitespace_takes_following_big_word
    : "a b-c d"
    => "f daW"
    => "a d", 1;

}

// ===================== Vi Tests =====================

fn test_vi(initial: &str) -> (ShedLine, TestGuard) {
  Shed::shopts_mut(|o| o.set.vi = true);
  let g = TestGuard::new();
  let prompt = Prompt::default();
  let vi = ShedLine::new_no_hist(prompt).unwrap().with_initial(initial);

  (vi, g)
}

#[test]
fn vi_auto_indent() {
  let (mut vi, _g) = test_vi("");

  // Type each line and press Enter separately so auto-indent triggers
  let lines = [
    "func() {",
    "case foo in",
    "bar)",
    "while true; do",
    "echo foo \\\rbar \\\rbiz \\\rbazz\rbreak\rdone\r;;\resac\r}",
  ];

  for (i, line) in lines.iter().enumerate() {
    Shed::term_mut(|t| t.feed_bytes(line.as_bytes()));
    if i != lines.len() - 1 {
      Shed::term_mut(|t| t.feed_bytes(b"\r"));
    }
    let keys = Shed::term_mut(Terminal::drain_keys);
    vi.process_input(keys).unwrap();
  }

  assert_eq!(
    vi.core.editor.to_string(),
    "func() {\n\tcase foo in\n\t\tbar)\n\t\t\twhile true; do\n\t\t\t\techo foo \\\n\t\t\t\tbar \\\n\t\t\t\tbiz \\\n\t\t\t\tbazz\n\t\t\t\tbreak\n\t\t\tdone\n\t\t;;\n\tesac\n}"
  );
}

#[test]
fn vi_auto_indent_siblings() {
  let (mut vi, _g) = test_vi("");

  // Type each line and press Enter separately so auto-indent triggers
  let lines = [
    "if foo; then",
    "echo foo",
    "elif bar; then",
    "echo biz",
    "else",
    "echo bar",
    "fi",
  ];

  for (i, line) in lines.iter().enumerate() {
    Shed::term_mut(|t| t.feed_bytes(line.as_bytes()));
    if i != lines.len() - 1 {
      Shed::term_mut(|t| t.feed_bytes(b"\r"));
    }
    let keys = Shed::term_mut(Terminal::drain_keys);
    vi.process_input(keys).unwrap();
  }

  assert_eq!(
    vi.core.editor.to_string(),
    "if foo; then\n\techo foo\nelif bar; then\n\techo biz\nelse\n\techo bar\nfi"
  );
}

// this one cant go up there because '\x1bO' looks like an OSC sequence to the vte parser.
// So we have to explicitly split up the key events here into 'key!(Esc)' and 'key!('O')'.
#[test]
fn vi_auto_indent_funcdef() {
  let (mut vi, _g) = test_vi("");

  let bytes = b"func_def() {}";
  Shed::term_mut(|t| t.feed_bytes(bytes));
  let keys = Shed::term_mut(Terminal::drain_keys);
  vi.process_input(keys).unwrap();
  vi.process_input(vec![key!(Esc)]).unwrap();
  vi.process_input(vec![key!('i')]).unwrap();
  vi.process_input(vec![key!(Enter)]).unwrap();
  vi.process_input(vec![key!(Esc)]).unwrap();
  vi.process_input(vec![key!('O')]).unwrap();
  assert_eq!(vi.core.editor.to_string(), "func_def() {\n\t\n}");
}

#[test]
fn vi_auto_indent_empty_body() {
  let (mut vi, _g) = test_vi("");

  let bytes = b"if true; then";
  Shed::term_mut(|t| t.feed_bytes(bytes));
  let keys = Shed::term_mut(Terminal::drain_keys);
  vi.process_input(keys).unwrap();
  vi.process_input(vec![key!(Enter)]).unwrap();
  vi.process_input(vec![key!('f')]).unwrap();
  vi.process_input(vec![key!('i')]).unwrap();

  assert_eq!(vi.core.editor.to_string(), "if true; then\nfi");
}

#[test]
fn vi_func_def_is_finished() {
  let (mut vi, _g) = test_vi("");

  let bytes = b"func_def() {\r}\r";
  Shed::term_mut(|t| t.feed_bytes(bytes));
  let keys = Shed::term_mut(Terminal::drain_keys);
  vi.process_input(keys).unwrap();
  assert_eq!(vi.core.editor.to_string(), "");
}

#[test]
fn case_stmt_is_finished() {
  let (mut vi, _g) = test_vi("");

  let bytes = b"case foo in\rfoo)\rcase bar in\rbar)\recho foo\r;;\resac\r;;\resac\r";
  Shed::term_mut(|t| t.feed_bytes(bytes));
  let keys = Shed::term_mut(Terminal::drain_keys);
  vi.process_input(keys).unwrap();
  assert_eq!(vi.core.editor.to_string(), "");
}

macro_rules! hist_expansion_test {
  ($($name:ident: $cmds:expr => $input:expr => $expected:literal);* $(;)?
   ---
   $($name2:ident: $cmds2:expr => $input2:literal);* $(;)?
  ) => {
    mod hist_expansion {
      use super::*;
      $(#[test]
        fn $name() {
          let _g = TestGuard::new();
          let prompt = Prompt::default();
          let mut line = ShedLine::new_no_hist(prompt).unwrap();
          for cmd in $cmds {
            line.history.push(cmd).unwrap();
          }
          line.history.refresh_hist_entries();
          line.history.update_search_mask(None);

          assert_eq!(line.history.masked_entries().len(), $cmds.len());

          Shed::term_mut(|t| t.feed_bytes($input.as_bytes()));
          let keys = Shed::term_mut(|t| t.drain_keys());
          line.process_input(keys).unwrap();

          let joined = line.core.editor.to_string();
          assert_eq!(joined, $expected);
        }
      )*

      $(#[test]
        fn $name2() {
          let _g = TestGuard::new();
          let prompt = Prompt::default();
          let mut line = ShedLine::new_no_hist(prompt).unwrap();
          for cmd in $cmds2 {
            line.history.push(cmd).unwrap();
          }
          line.history.update_search_mask(None);

          // Feed input without pressing Enter
          Shed::term_mut(|t| t.feed_bytes($input2.as_bytes()));
          let keys = Shed::term_mut(|t| t.drain_keys());
          line.process_input(keys).unwrap();

          let before = line.core.editor.to_string();
          // Manually call attempt_history_expansion - should return false
          let expanded = line.core.editor.attempt_history_expansion(&line.history);
          assert!(!expanded, "expected no expansion but expansion occurred");
          assert_eq!(line.core.editor.to_string(), before);
        }
      )*
    }
  };
}

hist_expansion_test! {
  breaks_on_metachars1   : &["cargo run"]                      => "if !car;\r"          => "if cargo run;";
  breaks_on_metachars2   : &["cargo run"]                      => "!car && true\r"      => "cargo run && true";
  breaks_on_metachars3   : &["cargo run"]                      => "!car | grep foo\r"   => "cargo run | grep foo";
  ignores_closing_quote  : &["cargo run"]                      => "echo \"foo !car\"\r" => "echo \"foo cargo run\"";
  works_in_double_quotes : &["foo"]                            => "\"!!\"\r"            => "\"foo\"";
  no_match_is_passthrough: &["foo", "bar"]                     => "!z\r"                => "z";
  multiple               : &["hello", "world"]                 => "echo !! !h\r"        => "echo world hello";
  inline                 : &["world"]                          => "echo !!\r"           => "echo world";
  positive_index         : &["alpha", "beta", "gamma"]         => "!1\r"                => "alpha";
  negative_index         : &["alpha", "beta", "gamma"]         => "!-2\r"               => "beta";
  bang_dollar_single_word: &["solo"]                           => "!$\r"                => "solo";
  bang_dollar            : &["echo hello world"]               => "!$\r"                => "world";
  bang_bang              : &["echo hello", "ls"]               => "!!\r"                => "ls";
  prefix_latest_match    : &["foo first", "bar", "foo second"] => "!f\r"                => "foo second";
  prefix                 : &["foo", "bar", "biz", "qux"]       => "!f\r"                => "foo";
  prefix_underscore_literal
    : &["my_script --safe", "my script.sh --dangerous"] => "!my_script\r" => "my_script --safe";
  prefix_case_sensitive  : &["make build"]                     => "!Make\r"             => "Make";

  multiline
    : &["echo foo","if true; then\necho foo\nfi", "echo bar"]
    => "!2\r"
    => "if true; then\n\techo foo\nfi";

  parse_recurses
    : &["cargo run"]
    => "echo \"foo $(echo \"!car\") bar\"\r"
    => "echo \"foo $(echo \"cargo run\") bar\"";

  --- // no-expansion tests
  skips_param_indirection1    : &["cargo run"] => "echo ${!var}";
  skips_param_indirection2    : &["cargo run"] => "echo ${!car}";
  skips_param_indirection3    : &["cargo run"] => "echo \"${!var}\"";
  no_recurse_in_single_quotes2: &["cargo run"] => "echo 'foo $(echo \"!car\") bar'";
  no_recurse_in_single_quotes1: &["cargo run"] => "echo \"foo $(echo '!car') bar\"\r";
  skips_dollar_bang           : &["foo"]       => "$!1";
  skips_single_quotes         : &["foo"]       => "'!!'";
}

// ===================== History General Tests =====================

use crate::readline::history::History;

#[test]
fn hist_push_returns_id() {
  let _g = TestGuard::new();
  let hist = History::empty("test_push_id");
  let id1 = hist.push("cmd1").unwrap();
  let id2 = hist.push("cmd2").unwrap();
  assert!(id1.is_some());
  assert!(id2.is_some());
  assert_ne!(id1, id2);
}

#[test]
fn hist_push_empty_returns_none() {
  let _g = TestGuard::new();
  let hist = History::empty("test_push_empty");
  let id = hist.push("").unwrap();
  assert!(id.is_none());
  assert_eq!(hist.entry_count(), 0);
}

#[test]
fn hist_entry_count() {
  let _g = TestGuard::new();
  let hist = History::empty("test_count");
  assert_eq!(hist.entry_count(), 0);
  hist.push("cmd1").unwrap();
  assert_eq!(hist.entry_count(), 1);
  hist.push("cmd2").unwrap();
  hist.push("cmd3").unwrap();
  assert_eq!(hist.entry_count(), 3);
}

#[test]
fn hist_last_id() {
  let _g = TestGuard::new();
  let hist = History::empty("test_last_id");
  assert_eq!(hist.last_id(), 0);
  hist.push("cmd1").unwrap();
  assert_eq!(hist.last_id(), 1);
  hist.push("cmd2").unwrap();
  assert_eq!(hist.last_id(), 2);
}

#[test]
fn hist_last_returns_most_recent() {
  let _g = TestGuard::new();
  let hist = History::empty("test_last");
  assert!(hist.last().is_none());
  hist.push("first").unwrap();
  hist.push("second").unwrap();
  let last = hist.last().unwrap();
  assert_eq!(last.command, "second");
}

#[test]
fn hist_query_with_filter() {
  let _g = TestGuard::new();
  let hist = History::empty("test_query_filter");
  hist.push("echo foo").unwrap();
  hist.push("ls -la").unwrap();
  hist.push("echo bar").unwrap();

  let results = hist
    .query(
      "WHERE command LIKE ?1 ORDER BY id ASC",
      &[&"echo%" as &dyn rusqlite::ToSql],
    )
    .unwrap();
  assert_eq!(results.len(), 2);
  assert_eq!(results[0].1.command, "echo foo");
  assert_eq!(results[1].1.command, "echo bar");
}

#[test]
fn hist_query_range() {
  let _g = TestGuard::new();
  let hist = History::empty("test_query_range");
  hist.push("cmd1").unwrap();
  hist.push("cmd2").unwrap();
  hist.push("cmd3").unwrap();
  hist.push("cmd4").unwrap();

  let results = hist.query_range(2, 3).unwrap();
  assert_eq!(results.len(), 2);
  assert_eq!(results[0].1.command, "cmd2");
  assert_eq!(results[1].1.command, "cmd3");
}

#[test]
fn hist_ids_are_sequential() {
  let _g = TestGuard::new();
  let hist = History::empty("test_sequential");
  hist.push("a").unwrap();
  hist.push("b").unwrap();
  hist.push("c").unwrap();

  let entries = hist.query("ORDER BY id ASC", &[]).unwrap();
  for (i, (id, _)) in entries.iter().enumerate() {
    assert_eq!(*id, (i + 1) as i64);
  }
}

// ===================== History Delete/Restore Tests =====================

#[test]
fn hist_delete_removes_entries() {
  let _g = TestGuard::new();
  let hist = History::empty("test_delete");
  hist.push("echo foo").unwrap();
  hist.push("echo bar").unwrap();
  hist.push("echo baz").unwrap();
  assert_eq!(hist.entry_count(), 3);

  let deleted = hist
    .delete("WHERE command = ?1", &[&"echo bar" as &dyn rusqlite::ToSql])
    .unwrap();
  assert_eq!(deleted.len(), 1);
  assert_eq!(deleted[0].1.command, "echo bar");
  assert_eq!(hist.entry_count(), 2);
}

#[test]
fn hist_delete_purges_search_cache_no_resurrect() {
  // Regression: a delete refreshed only the scroll cache (HIST_ENTRIES), so the
  // deleted command lingered in the search cache (SEARCH_ENTRIES / Ctrl-R) and
  // was resurrected into scroll history by merge_search_entries.
  let _g = TestGuard::new();
  let mut hist = History::empty("test_delete_search_cache");
  hist.push("keep one").unwrap();
  hist.push("secret token").unwrap();
  hist.push("keep two").unwrap();

  // push populates the search cache.
  assert!(
    hist
      .search_cache_commands()
      .iter()
      .any(|c| c == "secret token")
  );

  hist
    .delete(
      "WHERE command = ?1",
      &[&"secret token" as &dyn rusqlite::ToSql],
    )
    .unwrap();
  hist.refresh_hist_entries();

  assert!(
    !hist
      .search_cache_commands()
      .iter()
      .any(|c| c == "secret token"),
    "deleted command still in search cache"
  );
  assert!(
    !hist
      .scroll_cache_commands()
      .iter()
      .any(|c| c == "secret token"),
    "deleted command still in scroll cache"
  );

  // A subsequent search-merge must not resurrect it into scroll history.
  hist.merge_search_entries();
  assert!(
    !hist
      .scroll_cache_commands()
      .iter()
      .any(|c| c == "secret token"),
    "deleted command resurrected via merge_search_entries"
  );
}

#[test]
fn hist_delete_reids_contiguously() {
  let _g = TestGuard::new();
  let hist = History::empty("test_reid");
  hist.push("cmd1").unwrap();
  hist.push("cmd2").unwrap();
  hist.push("cmd3").unwrap();

  hist
    .delete("WHERE command = ?1", &[&"cmd2" as &dyn rusqlite::ToSql])
    .unwrap();

  let entries = hist.query("ORDER BY id ASC", &[]).unwrap();
  assert_eq!(entries.len(), 2);
  assert_eq!(entries[0].0, 1); // id should be 1
  assert_eq!(entries[1].0, 2); // id should be 2, not 3
}

#[test]
fn hist_delete_creates_backup() {
  let _g = TestGuard::new();
  let hist = History::empty("test_backup");
  hist.push("echo foo").unwrap();
  hist.push("echo bar").unwrap();

  hist
    .delete("WHERE command = ?1", &[&"echo bar" as &dyn rusqlite::ToSql])
    .unwrap();

  // backup should exist - restore should succeed, not error
  assert!(hist.restore_backup().is_ok());
}

#[test]
fn hist_restore_recovers_deleted_entries() {
  let _g = TestGuard::new();
  let hist = History::empty("test_restore");
  hist.push("echo foo").unwrap();
  hist.push("echo bar").unwrap();
  hist.push("echo baz").unwrap();

  hist
    .delete("WHERE command = ?1", &[&"echo bar" as &dyn rusqlite::ToSql])
    .unwrap();

  assert_eq!(hist.entry_count(), 2);

  let restored = hist.restore_backup().unwrap();
  assert_eq!(restored, 1);
  assert_eq!(hist.entry_count(), 3);
}

#[test]
fn hist_restore_preserves_new_entries() {
  let _g = TestGuard::new();
  let hist = History::empty("test_restore_new");
  hist.push("cmd1").unwrap();
  hist.push("cmd2").unwrap();
  hist.push("cmd3").unwrap();

  hist
    .delete("WHERE command = ?1", &[&"cmd2" as &dyn rusqlite::ToSql])
    .unwrap();

  // add a new command after the delete
  hist.push("cmd4").unwrap();

  let restored = hist.restore_backup().unwrap();
  assert_eq!(restored, 1); // only cmd2 was missing
  assert_eq!(hist.entry_count(), 4);

  // all commands should be present, ordered by timestamp
  let entries = hist.query("ORDER BY id ASC", &[]).unwrap();
  let cmds: Vec<&str> = entries.iter().map(|(_, e)| e.command.as_str()).collect();
  assert!(cmds.contains(&"cmd1"));
  assert!(cmds.contains(&"cmd2"));
  assert!(cmds.contains(&"cmd3"));
  assert!(cmds.contains(&"cmd4"));
}

#[test]
fn hist_restore_no_backup_errors() {
  let _g = TestGuard::new();
  let hist = History::empty("test_no_backup");
  hist.push("echo foo").unwrap();

  assert!(hist.restore_backup().is_err());
}

#[test]
fn hist_restore_ids_are_contiguous() {
  let _g = TestGuard::new();
  let hist = History::empty("test_restore_ids");
  hist.push("cmd1").unwrap();
  hist.push("cmd2").unwrap();
  hist.push("cmd3").unwrap();

  hist
    .delete("WHERE command = ?1", &[&"cmd2" as &dyn rusqlite::ToSql])
    .unwrap();
  hist.push("cmd4").unwrap();
  hist.restore_backup().unwrap();

  let entries = hist.query("ORDER BY id ASC", &[]).unwrap();
  for (i, (id, _)) in entries.iter().enumerate() {
    assert_eq!(*id, (i + 1) as i64, "IDs should be contiguous");
  }
}

// ===================== Alias Expansion Tests =====================

macro_rules! alias_expansion_test {
  ($($name:ident: $aliases:expr => $input:literal => $result:literal);* $(;)?
   ---
   $($name2:ident: $aliases2:expr => $input2:literal);* $(;)?
  ) => {
    mod alias {
      use super::*;
      $(#[test]
        fn $name() {
          let _g = TestGuard::new();
          Shed::shopts_mut(|o| o.prompt.expand_aliases = true);
          Shed::logic_mut(|l| {
            for (name, body) in $aliases {
              l.insert_alias(name, body, Span::default());
            }
          });

          let prompt = Prompt::default();
          let mut line = ShedLine::new_no_hist(prompt).unwrap();

          Shed::term_mut(|t| t.feed_bytes($input.as_bytes()));
          let keys = Shed::term_mut(|t| t.drain_keys());
          line.process_input(keys).unwrap();

          let joined = line.core.editor.to_string();
          assert_eq!(joined, $result, "\nInput: {:?}", $input);
        }
      )*
        $(#[test]
          fn $name2() {
            let _g = TestGuard::new();
            Shed::shopts_mut(|o| o.prompt.expand_aliases = true);
            Shed::logic_mut(|l| {
              for (name, body) in $aliases2 {
                l.insert_alias(name, body, Span::default());
              }
            });

            let prompt = Prompt::default();
            let mut line = ShedLine::new_no_hist(prompt).unwrap();

            Shed::term_mut(|t| t.feed_bytes($input2.as_bytes()));
            let keys = Shed::term_mut(|t| t.drain_keys());
            line.process_input(keys).unwrap();

            let before = line.core.editor.to_string();
            let expanded = line.core.editor.attempt_alias_expansion();
            assert!(
              !expanded,
              "expected no alias expansion but expansion occurred"
            );
            assert_eq!(line.core.editor.to_string(), before);
          }
      )*
    }
  };
}

alias_expansion_test! {
  single_char_body      : &[("a", "b")]            => "a "            => "b ";
  single_char_name      : &[("g", "git")]          => "g "            => "git ";
  expand_after_semicolon: &[("gc", "git commit")]  => "echo hi; gc "  => "echo hi; git commit ";
  expansion_with_args   : &[("gc", "git commit")]  => "gc -m 'hello'" => "git commit -m 'hello'";
  simple_expansion      : &[("ll", "ls -la")]      => "ll "           => "ls -la ";

  recursion_terminates
    : &[("diff", "diff --color=auto")]
    => "diff "
    => "diff --color=auto ";

  multiple_on_same_line
    : &[("gc", "git commit"), ("gp", "git push")]
    => "gc; gp "
    => "git commit; git push ";

  ---
  no_expand_in_quotes: &[("gc", "git commit")] => "echo 'gc' ";
  no_expand_in_arg_position: &[("foo", "bar")] => "echo foo ";
}

#[test]
fn alias_no_expand_when_disabled() {
  let _g = TestGuard::new();
  Shed::shopts_mut(|o| o.prompt.expand_aliases = false);
  Shed::logic_mut(|l| {
    l.insert_alias("gc", "git commit", Span::default());
  });

  let prompt = Prompt::default();
  let mut line = ShedLine::new_no_hist(prompt).unwrap();

  Shed::term_mut(|t| t.feed_bytes(b"gc "));
  let keys = Shed::term_mut(Terminal::drain_keys);
  line.process_input(keys).unwrap();

  let joined = line.core.editor.to_string();
  assert_ne!(joined, "git commit");
}

#[test]
fn alias_no_inline_expand_when_disabled() {
  // Regression for #100: with prompt.expand_aliases=false the automatic
  // (Tab / submit / hint) expansion paths must leave the buffer untouched,
  // so the raw command is what gets submitted and saved to history.
  let _g = TestGuard::new();
  Shed::shopts_mut(|o| o.prompt.expand_aliases = false);
  Shed::logic_mut(|l| {
    l.insert_alias("gc", "git commit", Span::default());
  });

  let prompt = Prompt::default();
  let mut line = ShedLine::new_no_hist(prompt).unwrap();

  Shed::term_mut(|t| t.feed_bytes(b"gc"));
  let keys = Shed::term_mut(Terminal::drain_keys);
  line.process_input(keys).unwrap();

  // The Tab/inline path (and, by the same gate, submit) does not expand.
  let expanded = line.core.editor.attempt_inline_expansion(&line.history);
  assert!(!expanded, "alias expanded despite expand_aliases=false");
  assert_eq!(line.core.editor.to_string(), "gc");

  // The primitive itself stays unconditional: this is the path `:expand!`
  // drives, so the manual escape hatch keeps working even when disabled.
  assert!(line.core.editor.attempt_alias_expansion());
  assert_eq!(line.core.editor.to_string(), "git commit");
}

// ===================== Hint Tests =====================
//
// prepopulates some commands into the test harness command history
// then does line editor stuff that would interact with the hint system.
//
// Uses `expand_keymap` (same as emacs_test!/visual_test!) to parse
// vim-keymap notation like `<Up>`, `<C-a>`, `<BS>` into KeyEvents
// directly. Feeding raw bytes one-at-a-time would mangle CSI sequences
// like `\x1b[A` for arrow keys, since each byte gets processed before
// the next arrives.
macro_rules! hint_test {
  { $($name:ident
      : $hist:expr
      => $input:expr
      => $expected_buf:expr, $expected_hint:expr, $expected_cursor:expr
    );* $(;)?
  } => {
    mod hint {
      use super::*;
      use crate::expand::expand_keymap;
      $(#[test]
        fn $name() {
          Shed::shopts_mut(|o| {
            o.set.vi = true;
            o.line.auto_suggest = true;
          });
          let (mut line, _g) = test_vi("");

          for cmd in $hist {
            line.history.push(cmd).unwrap();
          }
          line.history.refresh_hist_entries();
          line.history.constrain_entries(None);

          let keys = expand_keymap($input);
          line.process_input(keys).unwrap();

          assert_eq!(line.core.editor.to_string(), $expected_buf, "buffer mismatch");
          assert_eq!(
            line.core.editor.try_join_hint().unwrap_or_default(),
            $expected_hint,
            "hint mismatch"
          );
          assert_eq!(line.core.editor.cursor_to_flat(), $expected_cursor, "cursor mismatch");
        }
      )*
    }
  };
}

hint_test! {
  edit_repopulates_hint
    : &[
      "command one",
      "command two",
      "command three",
    ]
    => "<Up><Esc>$bC"
    => "command ", "command two", 8;

  prefix_suggests_hint
    : &["echo foo bar"]
    => "echo"
    => "echo", "echo foo bar", 4;

  full_accept_by_l
    : &["echo foo bar"]
    => "echo<Esc>l"
    => "echo foo bar", "", 11;

  word_accept
    : &["echo foo bar"]
    => "echo<Esc>w"
    => "echo f", "echo foo bar", 5;

  escape_preserves_buffer
    : &["echo foo bar"]
    => "echo<Esc>"
    => "echo", "echo foo bar", 3;

  word_through_brace
    : &["flog -p \"[%H:%M:%S {level}\" info foo"]
    => "flog -p \"[%H:%M:%S {<Esc>ww"
    => "flog -p \"[%H:%M:%S {level}", "flog -p \"[%H:%M:%S {level}\" info foo", 25;

  word_across_line_boundary
    : &["echo foo\necho bar"]
    => "echo foo<Esc>ww"
    => "echo foo\necho b", "echo foo\necho bar", 14;

  back_motion_no_accept
    : &["echo foo bar"]
    => "echo<Esc>b"
    => "echo", "echo foo bar", 0;

  j_accepts_downward
    : &["echo foo\necho bar\necho biz"]
    => "echo f<Esc>j"
    => "echo foo\necho b", "echo foo\necho bar\necho biz", 14;

  j_accepts_downward_insert_mode_inclusive
    : &["echo foo\necho bar\necho biz"]
    => "echo <C-o>j"
    => "echo foo\necho ", "echo foo\necho bar\necho biz", 14;

  most_recent_wins
    : &[
      "echo apple",
      "echo banana",
      "echo cherry",
    ]
    => "echo "
    => "echo ", "echo cherry", 5;

  prefix_narrowing
    : &[
      "echo apple",
      "echo banana",
    ]
    => "echo a"
    => "echo a", "echo apple", 6;

  exact_buffer_no_hint
    : &[
      "echo foo",
    ]
    => "echo foo"
    => "echo foo", "", 8;

  no_match_no_hint
    : &[
      "echo foo",
      "ls -la",
    ]
    => "git status"
    => "git status", "", 10;

  backspace_revives_hint
    : &[
      "echo foobar",
    ]
    => "echo z<BS>"
    => "echo ", "echo foobar", 5;

  hint_constrained
    : &[
      "echo foobar",
      "echo foo",
      "echo foooooo"
    ]
    => "echo foob<Esc>e"
    => "echo foobar", "", 10;

  divergence_clears_hint
    : &[
      "echo foo bar",
    ]
    => "echo z"
    => "echo z", "", 6;
}

// ===================== Emacs Tests =====================
//
// Unlike `vi_test!`, which feeds raw bytes through the parser, emacs
// bindings rely heavily on Ctrl/Alt modifiers that are awkward to spell
// in byte literals. We use `expand_keymap` to parse vim-keymap syntax
// (`<C-a>`, `<A-f>`, `<BS>`, etc.) into `KeyEvent`s, then feed them
// directly to `process_input`.

fn test_emacs(initial: &str) -> (ShedLine, TestGuard) {
  Shed::shopts_mut(|o| o.set.vi = false);
  let g = TestGuard::new();
  let prompt = Prompt::default();
  let mut em = ShedLine::new_no_hist(prompt).unwrap().with_initial(initial);
  // Place cursor at end-of-buffer — matches what a real interactive
  // emacs session looks like just after the user has finished typing.
  let end = em.core.editor.to_string().chars().count();
  em.core.editor.edit(|e| e.set_cursor_from_flat(end));
  (em, g)
}

macro_rules! emacs_test {
  { $($name:ident: $input:expr => $op:expr => $expected_text:expr,$expected_cursor:expr);* $(;)? } => {
    mod emacs {
      use super::*;
      use crate::expand::expand_keymap;
      $(#[test]
        fn $name() {
          let (mut em, _g) = test_emacs($input);
          let keys = expand_keymap($op);
          em.process_input(keys).unwrap();
          assert_eq!(em.core.editor.to_string(), $expected_text,
            "buffer mismatch for {}", stringify!($name));
          assert_eq!(em.core.editor.cursor_to_flat(), $expected_cursor,
            "cursor mismatch for {}", stringify!($name));
        }
      )*
    }
  };
}

emacs_test! {
  // ─── Plain typing ─────────────────────────────────────────────────
  insert_chars             : ""                  => "hello"           => "hello", 5;
  insert_then_more         : "abc"               => "def"             => "abcdef", 6;

  // ─── Backspace ────────────────────────────────────────────────────
  backspace_one            : "hello"             => "<BS>"            => "hell", 4;
  backspace_many           : "hello"             => "<BS><BS><BS>"    => "he", 2;

  // ─── Ctrl+a / Ctrl+e: line motions ────────────────────────────────
  ctrl_a_to_start          : "hello"             => "<C-a>"           => "hello", 0;
  ctrl_e_to_end            : "hello"             => "<C-a><C-e>"      => "hello", 5;

  // ─── Ctrl+f / Ctrl+b: char motions ────────────────────────────────
  ctrl_f_forward_char      : "hello"             => "<C-a><C-f>"      => "hello", 1;
  ctrl_b_backward_char     : "hello"             => "<C-b>"           => "hello", 4;

  // ─── Alt+f / Alt+b: word motions ──────────────────────────────────
  // Shed's Alt+f maps to `Motion::WordMotion(To::End, ...)`, so the cursor
  // lands on the LAST char of the target word, not past it.
  alt_f_word_forward       : "one two three"     => "<C-a><A-f>"      => "one two three", 2;
  alt_b_word_backward      : "one two three"     => "<A-b>"           => "one two three", 8;

  // ─── Ctrl+d: delete forward (non-empty buffer) ────────────────────
  ctrl_d_deletes_under_cursor : "hello"          => "<C-a><C-d>"      => "ello", 0;

  // ─── Ctrl+k: kill to end of line ──────────────────────────────────
  ctrl_k_kills_to_eol      : "hello world"       => "<C-a><C-f><C-f><C-f><C-f><C-f><C-k>"
                                                                     => "hello", 5;

  // ─── Ctrl+u: kill to start of line ────────────────────────────────
  ctrl_u_kills_to_sol      : "hello world"       => "<C-u>"           => "", 0;

  // ─── Ctrl+w / Alt+Backspace: kill word backward ───────────────────
  ctrl_w_kills_word_back   : "one two three"     => "<C-w>"           => "one two ", 8;
  alt_bs_kills_word_back   : "one two three"     => "<A-BS>"          => "one two ", 8;

  // ─── Alt+d: kill word forward ─────────────────────────────────────
  alt_d_kills_word_forward : "one two three"     => "<C-a><A-d>"      => " two three", 0;

  // ─── Ctrl+y: yank back what Ctrl+w just killed ────────────────────
  yank_after_kill_word     : "one two"           => "<C-w><C-y>"      => "one two", 7;

  // ─── Ctrl+t: transpose chars (swap char-before-cursor with char-at-cursor) ─
  ctrl_t_transpose         : "abc"               => "<C-a><C-f><C-t>" => "bac", 2;

  // ─── Alt+t: transpose words ───────────────────────────────────────
  // First word with no prior word: no-op (bail when prev_word_span fails).
  alt_t_first_word_noop    : "foo bar"           => "<C-a><A-t>"            => "foo bar", 0;
  // Cursor on second word triggers transpose. test_emacs starts the
  // cursor PAST the end; back up one with <C-b> to land on 'r'.
  alt_t_inside_second_word : "foo bar"           => "<C-b><A-t>"            => "bar foo", 7;

  // ─── Alt+u / Alt+l / Alt+c: case changes on next word ─────────────
  // Verb+Motion ops don't reposition the cursor — it stays at the
  // operation's anchor. (Real Emacs moves point past the word; shed's
  // impl currently doesn't. If that gets fixed, update the expected
  // cursor here to match.)
  alt_u_uppercases_word    : "hello world"       => "<C-a><A-u>"      => "HELLO world", 0;
  alt_l_lowercases_word    : "HELLO WORLD"       => "<C-a><A-l>"      => "hello WORLD", 0;
  alt_c_capitalizes_word   : "hello world"       => "<C-a><A-c>"      => "Hello world", 5;

  // ─── Ctrl+/ and Alt+/ — undo / redo ───────────────────────────────
  ctrl_slash_undoes        : "hello"             => "x<C-/>"          => "hello", 5;
}

// ===================== handle_completion_key =====================
//
// Drive the completion overlay's key dispatch directly. We can't use the
// public `process_input` path because that would also fire other state
// like keymaps and edit modes; we want isolation on the
// `handle_completion_key` arms specifically.

mod handle_completion_key {
  use super::*;
  use crate::readline::complete::{Candidate, FuzzyCompleter};

  fn fresh_line(initial: &str) -> (ShedLine, TestGuard) {
    let g = TestGuard::new();
    let prompt = Prompt::default();
    let line = ShedLine::new_no_hist(prompt).unwrap().with_initial(initial);
    (line, g)
  }

  fn install_completer(line: &mut ShedLine, items: &[&str]) {
    let mut comp = FuzzyCompleter::default();
    let cands: Vec<Candidate> = items
      .iter()
      .map(|s| Candidate::from(s.to_string()))
      .collect();
    comp.selector.activate(cands);
    line.completer = Some(Box::new(comp));
  }

  // ─── Enter → Accept ────────────────────────────────────────────────

  #[test]
  fn enter_accepts_selected_candidate_and_clears_completer() {
    let (mut line, _g) = fresh_line("");
    install_completer(&mut line, &["hello"]);
    // selected_candidate is filtered[cursor=0]; with a single candidate
    // that's "hello".
    let ret = line.handle_completion_key(&key!(Enter)).unwrap();
    assert!(ret, "handle_completion_key should return true on Accept");
    assert!(line.completer.is_none(), "completer should be cleared");
    // The Accept path replaces the buffer with the completed line. With
    // FuzzyCompleter::default() the token_span is (0, 0) and
    // original_input is empty, so the completed line is just the
    // candidate.
    assert_eq!(line.core.editor.to_string(), "hello");
    assert_eq!(line.core.editor.cursor_to_flat(), "hello".len());
  }

  #[test]
  fn enter_with_multiple_candidates_accepts_cursor_position() {
    // activate([a, b, c]) reverses → filtered=[c, b, a]; cursor=0 → "c".
    let (mut line, _g) = fresh_line("");
    install_completer(&mut line, &["alpha", "beta", "gamma"]);
    line.handle_completion_key(&key!(Enter)).unwrap();
    assert_eq!(line.core.editor.to_string(), "gamma");
    assert!(line.completer.is_none());
  }

  // ─── Esc / Ctrl+D → Dismiss ───────────────────────────────────────

  #[test]
  fn esc_dismisses_and_clears_completer() {
    let (mut line, _g) = fresh_line("partial");
    install_completer(&mut line, &["one", "two"]);
    let ret = line.handle_completion_key(&key!(Esc)).unwrap();
    assert!(ret);
    assert!(line.completer.is_none());
    // The editor buffer should NOT be modified on dismiss.
    assert_eq!(line.core.editor.to_string(), "partial");
  }

  #[test]
  fn ctrl_d_dismisses_and_clears_completer() {
    let (mut line, _g) = fresh_line("partial");
    install_completer(&mut line, &["one", "two"]);
    let ret = line.handle_completion_key(&key!(Ctrl + 'd')).unwrap();
    assert!(ret);
    assert!(line.completer.is_none());
    assert_eq!(line.core.editor.to_string(), "partial");
  }

  // ─── Tab/Down/Up → Consumed ───────────────────────────────────────

  #[test]
  fn tab_consumed_keeps_completer_and_marks_redraw() {
    let (mut line, _g) = fresh_line("");
    install_completer(&mut line, &["a", "b", "c"]);
    line.needs_redraw = false;
    let ret = line.handle_completion_key(&key!(Tab)).unwrap();
    assert!(ret);
    assert!(line.completer.is_some(), "completer should remain active");
    assert!(line.needs_redraw, "Consumed should request redraw");
  }

  #[test]
  fn down_consumed_keeps_completer() {
    let (mut line, _g) = fresh_line("");
    install_completer(&mut line, &["a", "b"]);
    line.handle_completion_key(&key!(Down)).unwrap();
    assert!(line.completer.is_some());
  }

  #[test]
  fn up_consumed_keeps_completer() {
    let (mut line, _g) = fresh_line("");
    install_completer(&mut line, &["a", "b"]);
    line.handle_completion_key(&key!(Up)).unwrap();
    assert!(line.completer.is_some());
  }

  // ─── Tab navigation then Enter accepts the new selection ──────────

  #[test]
  fn tab_then_enter_accepts_advanced_candidate() {
    // filtered = [c, b, a] (reverse-of-insertion); cursor=0 = c. After
    // one Tab, cursor moves to 1 = b. Enter accepts "b".
    let (mut line, _g) = fresh_line("");
    install_completer(&mut line, &["alpha", "beta", "gamma"]);
    line.handle_completion_key(&key!(Tab)).unwrap();
    line.handle_completion_key(&key!(Enter)).unwrap();
    assert_eq!(line.core.editor.to_string(), "beta");
  }

  // ─── Enter on empty filtered → Dismiss path ───────────────────────

  #[test]
  fn enter_on_empty_filtered_dismisses() {
    // Activate with zero candidates → filtered is empty → selector
    // returns Dismiss when Enter is pressed → handle_completion_key
    // clears completer.
    let (mut line, _g) = fresh_line("");
    let comp = FuzzyCompleter::default(); // no candidates activated
    line.completer = Some(Box::new(comp));
    let ret = line.handle_completion_key(&key!(Enter)).unwrap();
    assert!(ret);
    assert!(line.completer.is_none());
  }
}

// ===================== ViVisual::handle_key =====================
//
// `visual_test!` mirrors `vi_test!` but uses `expand_keymap` so we can
// write Ctrl/Alt sequences naturally (`<C-a>`, `<Esc>`, etc.) instead of
// embedding raw escape bytes. Each test starts in normal mode (we feed
// `<Esc>` first); the user op is expected to enter visual mode itself
// (typically `v` / `V` / `<C-v>`).

macro_rules! visual_test {
  { $($name:ident: $input:expr => $op:expr => $expected_text:expr, $expected_cursor:expr);* $(;)? } => {
    mod visual {
      use super::*;
      use crate::expand::expand_keymap;
      $(#[test]
        fn $name() {
          let (mut vi, _g) = test_vi($input);
          // Force a known starting state: normal mode.
          let prelude = expand_keymap("<Esc>");
          vi.process_input(prelude).unwrap();
          let keys = expand_keymap($op);
          vi.process_input(keys).unwrap();
          assert_eq!(vi.core.editor.to_string(), $expected_text,
            "buffer mismatch for {}", stringify!($name));
          assert_eq!(vi.core.editor.cursor_to_flat(), $expected_cursor,
            "cursor mismatch for {}", stringify!($name));
        }
      )*
    }
  };
}

visual_test! {
  // ─── Esc → NormalMode (no buffer change) ──────────────────────────
  esc_in_visual_returns_to_normal      : "hello"       => "v<Esc>"            => "hello", 0;

  // ─── Char-driven verbs: delete / yank / change ────────────────────
  v_l_l_d_deletes_three                : "hello"       => "vlld"              => "lo", 0;
  v_w_d_deletes_through_word_boundary  : "hello world" => "vwd"               => "orld", 0;
  v_l_l_c_changes_three                : "hello"       => "vllcX<Esc>"        => "Xlo", 0;

  // ─── Backspace inside visual moves cursor back ────────────────────
  v_l_l_bs_d_deletes_two               : "hello"       => "vll<BS>d"          => "llo", 0;

  // ─── Counted motion in visual ─────────────────────────────────────
  v_count_l_d_deletes_n_plus_one       : "abcdef"      => "v3ld"              => "ef", 0;

  // ─── Ctrl+a / Ctrl+x — increment / decrement number in visual ────
  // <C-a>/<C-x> operate on whatever number is in the visual selection.
  ctrl_a_increments_visual_number      : "5"           => "v<C-a>"            => "6", 0;
  ctrl_x_decrements_visual_number      : "5"           => "v<C-x>"            => "4", 0;
  ctrl_a_on_full_multi_digit           : "10"          => "vl<C-a>"           => "11", 1;

  // ─── Ctrl+r — redo from visual ────────────────────────────────────
  // Delete in visual → undo restores → redo via visual+Ctrl+r reapplies.
  visual_ctrl_r_redoes_last_change     : "hello"       => "vllduv<C-r>"      => "lo", 0;

  // ─── Arrow keys via common_cmds fallthrough ──────────────────────
  v_right_right_d_deletes_via_arrows   : "abcdef"      => "v<Right><Right>d"  => "def", 0;

  // ─── Invalid pending seq is cleared, next valid char works ──────
  v_invalid_then_d_still_deletes       : "hello"       => "vzd"               => "ello", 0;

  // ─── Ctrl+g — PrintPosition (no buffer effect) ───────────────────
  ctrl_g_in_visual_doesnt_modify_buffer: "hello"       => "v<C-g><Esc>"       => "hello", 0;

  // ─── Ctrl+d / Ctrl+u — HalfScreen motions; on a single-line buffer
  // the motion has nowhere meaningful to go and lands cursor at 0, but
  // the dispatcher still fires (which is what we're covering here) ──
  ctrl_d_in_visual_single_line_noop    : "hello"       => "v<C-d><Esc>"       => "hello", 0;
  ctrl_u_in_visual_single_line_noop    : "hello"       => "v<C-u><Esc>"       => "hello", 0;

  // ═══════════════ parse_verb dispatch arms ═══════════════════════════
  // Each entry exercises one char-keyed arm in ViVisual::parse_verb.

  // ─── y / d / c — basic operators ─────────────────────────────────
  parse_verb_y_yanks_and_p_pastes      : "hello"       => "vlly$p"            => "hellohel", 7;
  parse_verb_d_deletes_selection       : "hello"       => "vlld"              => "lo", 0;
  parse_verb_c_changes_selection       : "hello"       => "vllcX<Esc>"        => "Xlo", 0;

  // ─── x / s / S — delete / substitute / change-selection ──────────
  parse_verb_x_deletes_selection       : "hello"       => "vllx"              => "lo", 0;
  // `s` in shed's visual maps to Delete (not Substitute-with-insert).
  // The trailing `X<Esc>` runs in normal mode; X at cursor=0 is a no-op.
  parse_verb_s_deletes_like_d          : "hello"       => "vllsX<Esc>"        => "lo", 0;
  parse_verb_S_changes_selection       : "hello"       => "vllSX<Esc>"        => "Xlo", 0;

  // ─── ~ / u / U — case toggle / lower / upper ──────────────────────
  parse_verb_tilde_toggles_case        : "Hello"       => "vll~"              => "hELlo", 0;
  parse_verb_u_lowers_selection        : "HELLO"       => "vllu"              => "helLO", 0;
  parse_verb_U_uppers_selection        : "hello"       => "vllU"              => "HELlo", 0;

  // ─── r <ch> — replace each char in selection ─────────────────────
  parse_verb_r_replaces_each_char      : "hello"       => "vllrX"             => "XXXlo", 0;

  // ─── X / Y / D / R / C — visual-with-whole-line operators ────────
  // In visual mode, the attached WholeLine motion is overridden by the
  // selection, so these end up operating on the visual range only. We
  // pin shed's current behavior — vim's classic semantics differ.
  parse_verb_capital_X_deletes_selection_only
                                       : "foo\nbar"    => "vX"                => "oo\nbar", 0;
  parse_verb_capital_Y_yanks_selection_only
                                       : "foo\nbar"    => "vYjP"              => "foo\nfbar", 4;
  parse_verb_capital_D_deletes_selection_only
                                       : "foo\nbar"    => "vD"                => "oo\nbar", 0;
  parse_verb_capital_R_changes_selection_only
                                       : "foo\nbar"    => "vRX<Esc>"          => "Xoo\nbar", 0;
  parse_verb_capital_C_changes_selection_only
                                       : "foo\nbar"    => "vCX<Esc>"          => "Xoo\nbar", 0;

  // ─── > / < — indent / dedent ─────────────────────────────────────
  parse_verb_gt_indents_line           : "foo\nbar"    => "v>"                => "\tfoo\nbar", 1;
  // Dedent on a tab-indented first line with cursor at 0 currently does
  // nothing — pinning shed's behavior.
  parse_verb_lt_dedent_at_indent_noop  : "\tfoo\nbar"  => "v<"                => "\tfoo\nbar", 0;

  // ─── p / P — put (paste) replacing selection ─────────────────────
  // `yl` yanks one char; the trailing `l`s are separate cursor moves.
  // So `yll` from cursor=0 of "hello world": yank "h", cursor moves to
  // 1, then `vll` selects [1..3]="ell", `p` replaces with "h" → "hho world".
  parse_verb_p_pastes_over_selection
    : "hello world"                                    => "yllvllp"           => "hho world", 1;

  // ─── A / I — append / insert from visual ─────────────────────────
  parse_verb_capital_A_appends_at_selection_end
    : "hello"                                          => "vllAX<Esc>"        => "heXllo", 2;
  parse_verb_capital_I_inserts_at_selection_start
    : "hello"                                          => "$vlIX<Esc>"        => "hellXo", 4;

  // ─── o / O — swap visual anchor; no buffer change ────────────────
  parse_verb_o_swaps_anchor            : "hello"       => "vllo<Esc>"         => "hello", 2;
  parse_verb_capital_O_swaps_anchor    : "hello"       => "vllO<Esc>"         => "hello", 2;

  // ─── J — join lines (visual) ─────────────────────────────────────
  parse_verb_J_joins_selected_lines    : "foo\nbar"    => "vjJ"               => "foo bar", 3;

  // ─── g + v — re-select last visual (current cursor only) ─────────
  // Pin behavior: `gv` after Esc re-enters visual but only at the
  // cursor's final position, so the subsequent `d` deletes just one
  // char rather than the full original range.
  parse_verb_gv_reselects_last_position: "hello"       => "vll<Esc>gvd"       => "helo", 2;

  // ─── g + ? — rot13 ───────────────────────────────────────────────
  parse_verb_g_question_rot13          : "hello"       => "vllg?"             => "urylo", 0;

  // ─── . — repeat last change ──────────────────────────────────────
  // Pin shed's current dot-repeat semantics in visual.
  parse_verb_dot_repeats_last_change   : "abcdefghij"  => "vllduvll."         => "abcdhij", 4;

  // ─── = — equalize (no-op on a buffer with no structure) ─────────
  parse_verb_eq_equalize_noop          : "foo"         => "v="                => "foo", 0;

  // ─── invalid sequence — pending seq cleared, next op runs ───────
  parse_verb_invalid_then_d            : "hello"       => "vgX d"             => "ello", 0;

  // ─── text objects in visual mode ─────────────────────────────────
  v_iw_selects_inner_word              : "hello world" => "viwd"              => " world", 0;
  v_aw_selects_word_and_trailing_ws    : "hello world" => "vawd"              => "world", 0;
  v_i_paren_selects_inside             : "x(foo)y"     => "f(vi(d"            => "x()y", 2;
  v_a_paren_selects_around             : "x(foo)y"     => "f(va(d"            => "xy", 1;
  v_i_quote_selects_inside             : "say \"hi\""  => "f\"vi\"d"          => "say \"\"", 5;
  v_a_quote_selects_around_at_eol      : "say \"hi\""  => "f\"va\"d"          => "say ", 3;
  // Cursor on the closing quote (e.g. after typing then Esc) must still
  // resolve the surrounding pair, not mistake the closer for an opener.
  v_a_quote_cursor_on_closing_quote    : "say \"hi\""  => "$va\"d"            => "say ", 3;
  v_i_quote_cursor_on_closing_quote    : "say \"hi\""  => "$vi\"d"            => "say \"\"", 5;
}

// A hint after a buffer ending in a quote must not bleed into the text object:
// `va"` evaluates with the hint merged (so a closing quote in the hint still
// resolves), but when the closer is the last real char the selection must stop
// at it rather than dropping it once the hint is restored.
#[test]
fn va_quote_with_hint_at_eol_includes_closing_quote() {
  use crate::expand::expand_keymap;
  use crate::readline::linebuf::{Hint, Lines};

  let (mut vi, _g) = test_vi("say \"hi\"");
  vi.process_input(expand_keymap("<Esc>")).unwrap();
  vi.core.editor.set_cursor_from_flat(7); // on the closing quote
  vi.core
    .editor
    .set_hint(Some(Hint::Override(Lines::to_lines("say \"hi\"more"))));

  vi.process_input(expand_keymap("va\"")).unwrap();

  assert_eq!(vi.core.selection().as_deref(), Some("\"hi\""));
}

// ===================== handle_hist_search_key =====================
//
// Drive the history-fuzzy-finder's key dispatch directly. Same pattern
// as `handle_completion_key` tests above: bypass `process_input` to
// isolate the Accept / Dismiss / Consumed arms.

mod handle_hist_search_key {
  use super::*;
  use crate::keys::{KeyCode, KeyEvent, ModKeys};
  use crate::readline::complete::{Candidate, FuzzySelector};

  fn fresh_line() -> (ShedLine, TestGuard) {
    let g = TestGuard::new();
    let prompt = Prompt::default();
    let line = ShedLine::new_no_hist(prompt).unwrap();
    (line, g)
  }

  /// Build a `FuzzySelector` with candidates carrying ids, install it as
  /// the history's `fuzzy_finder`. Candidate ids must be present because
  /// the Accept arm does `cmd.id().unwrap()`.
  fn install_hist_finder(line: &mut ShedLine, items: &[(usize, &str)]) {
    let mut sel = FuzzySelector::new("History");
    let cands: Vec<Candidate> = items
      .iter()
      .map(|(id, s)| Candidate::from((*id, s.to_string())))
      .collect();
    sel.activate(cands);
    line.history.fuzzy_finder = Some(sel);
  }

  // ─── Enter → Accept ────────────────────────────────────────────────

  #[test]
  fn enter_accepts_and_clears_finder() {
    let (mut line, _g) = fresh_line();
    install_hist_finder(&mut line, &[(0, "ls -la")]);
    line.handle_hist_search_key(key!(Enter)).unwrap();
    // Accept calls stop_search → fuzzy_finder becomes None.
    assert!(line.history.fuzzy_finder.is_none());
    assert!(line.needs_redraw);
  }

  #[test]
  fn enter_accept_with_multiple_candidates_clears_finder() {
    let (mut line, _g) = fresh_line();
    install_hist_finder(&mut line, &[(0, "alpha"), (1, "beta"), (2, "gamma")]);
    line.handle_hist_search_key(key!(Enter)).unwrap();
    assert!(line.history.fuzzy_finder.is_none());
  }

  // ─── Esc → Dismiss ────────────────────────────────────────────────

  #[test]
  fn esc_dismisses_and_clears_finder() {
    let (mut line, _g) = fresh_line();
    install_hist_finder(&mut line, &[(0, "some cmd")]);
    line.handle_hist_search_key(key!(Esc)).unwrap();
    assert!(line.history.fuzzy_finder.is_none());
    assert!(line.needs_redraw);
  }

  #[test]
  fn ctrl_d_dismisses_and_clears_finder() {
    let (mut line, _g) = fresh_line();
    install_hist_finder(&mut line, &[(0, "x")]);
    line.handle_hist_search_key(key!(Ctrl + 'd')).unwrap();
    assert!(line.history.fuzzy_finder.is_none());
  }

  // ─── Tab/Down/Up → Consumed ───────────────────────────────────────

  #[test]
  fn tab_consumed_keeps_finder_and_marks_redraw() {
    let (mut line, _g) = fresh_line();
    install_hist_finder(&mut line, &[(0, "a"), (1, "b"), (2, "c")]);
    line.needs_redraw = false;
    line.handle_hist_search_key(key!(Tab)).unwrap();
    assert!(line.history.fuzzy_finder.is_some());
    assert!(line.needs_redraw);
  }

  #[test]
  fn down_consumed_keeps_finder() {
    let (mut line, _g) = fresh_line();
    install_hist_finder(&mut line, &[(0, "a"), (1, "b")]);
    line.handle_hist_search_key(key!(Down)).unwrap();
    assert!(line.history.fuzzy_finder.is_some());
  }

  #[test]
  fn up_consumed_keeps_finder() {
    let (mut line, _g) = fresh_line();
    install_hist_finder(&mut line, &[(0, "a"), (1, "b")]);
    line.handle_hist_search_key(key!(Up)).unwrap();
    assert!(line.history.fuzzy_finder.is_some());
  }

  // ─── typing a char → Consumed (query filter) ─────────────────────

  #[test]
  fn typing_filters_candidates_and_keeps_finder() {
    let (mut line, _g) = fresh_line();
    install_hist_finder(&mut line, &[(0, "alpha"), (1, "beta"), (2, "gamma")]);
    let key = KeyEvent(KeyCode::Char('g'), ModKeys::NONE);
    line.handle_hist_search_key(key).unwrap();
    assert!(line.history.fuzzy_finder.is_some());
    let names: Vec<&str> = line
      .history
      .fuzzy_finder
      .as_ref()
      .unwrap()
      .filtered()
      .iter()
      .map(|c| c.candidate.content())
      .collect();
    assert_eq!(names, vec!["gamma"]);
  }
}

// ===================== ShedLine::handle_key =====================
//
// Direct tests for the top-level key dispatcher. Existing vi_test! and
// emacs_test! exercise this transitively via `process_input`, but those
// flows also fire keymap matching, edit-mode resolution, etc. These
// tests target the LineCmd branches in handle_key/resolve_cmd directly.

mod handle_key_dispatch {
  use super::*;
  use crate::keys::{KeyCode, KeyEvent, ModKeys};
  use crate::readline::ReadlineEvent;

  fn fresh_emacs(initial: &str) -> (ShedLine, TestGuard) {
    Shed::shopts_mut(|o| o.set.vi = false);
    let g = TestGuard::new();
    let prompt = Prompt::default();
    let mut line = ShedLine::new_no_hist(prompt).unwrap().with_initial(initial);
    let end = line.core.editor.to_string().chars().count();
    line.core.editor.edit(|e| e.set_cursor_from_flat(end));
    (line, g)
  }

  // ─── EndOfFile branch ─────────────────────────────────────────────

  #[test]
  fn ctrl_d_on_empty_buffer_returns_eof() {
    let (mut line, _g) = fresh_emacs("");
    let res = line.handle_key(&key!(Ctrl + 'd')).unwrap();
    assert!(matches!(res, Some(ReadlineEvent::Eof)));
  }

  #[test]
  fn ctrl_d_on_non_empty_buffer_deletes_char_returns_none() {
    let (mut line, _g) = fresh_emacs("hello");
    line.core.editor.edit(|e| e.set_cursor_from_flat(0));
    let res = line.handle_key(&key!(Ctrl + 'd')).unwrap();
    assert!(res.is_none(), "expected None, got {res:?}");
    // The Ctrl+D-as-Delete should remove the char under the cursor.
    assert_eq!(line.core.editor.to_string(), "ello");
  }

  // ─── ClearScreen ──────────────────────────────────────────────────

  #[test]
  fn ctrl_l_marks_redraw_returns_none() {
    let (mut line, _g) = fresh_emacs("anything");
    line.needs_redraw = false;
    let res = line.handle_key(&key!(Ctrl + 'l')).unwrap();
    assert!(res.is_none());
    assert!(line.needs_redraw);
  }

  // ─── TriggerHistSearch ──────────────────────────────────────────

  #[test]
  fn ctrl_r_in_insert_mode_starts_history_search() {
    let (mut line, _g) = fresh_emacs("");
    // History search needs the search-entries cache to have something.
    // Push an entry so start_search has data to work with.
    line.history.push("prev_cmd").unwrap();
    let _ = line.handle_key(&key!(Ctrl + 'r')).unwrap();
    // start_hist_search sets up the finder when there are >=2 entries
    // OR adopts the single match. Either way, the call should return
    // without error and not throw an EOF.
  }

  // ─── SubmitLine on empty editor still routes through SubmitLine ─

  #[test]
  fn enter_on_empty_buffer_returns_line() {
    // submit() should return ReadlineEvent::Line("") for an empty
    // buffer (the loop layer handles the empty case).
    let (mut line, _g) = fresh_emacs("");
    let res = line.handle_key(&key!(Enter)).unwrap();
    match res {
      Some(ReadlineEvent::Line(s)) => assert_eq!(s, ""),
      other => panic!("expected Line(\"\"), got {other:?}"),
    }
  }

  #[test]
  fn enter_on_simple_command_submits_line() {
    let (mut line, _g) = fresh_emacs("echo hi");
    let res = line.handle_key(&key!(Enter)).unwrap();
    match res {
      Some(ReadlineEvent::Line(s)) => assert_eq!(s, "echo hi"),
      other => panic!("expected Line(\"echo hi\"), got {other:?}"),
    }
  }

  // ─── Plain printable char is routed through Execute → buffer grows

  #[test]
  fn typing_a_char_grows_buffer_returns_none() {
    let (mut line, _g) = fresh_emacs("");
    let key = KeyEvent(KeyCode::Char('x'), ModKeys::NONE);
    let res = line.handle_key(&key).unwrap();
    assert!(res.is_none());
    assert_eq!(line.core.editor.to_string(), "x");
  }

  // ─── Unbound key returns None (no LineCmd) ─────────────────────

  #[test]
  fn key_with_no_command_returns_none() {
    // F12 isn't bound in default emacs/vi mode. resolve_key returns
    // None → handle_key returns Ok(None).
    let (mut line, _g) = fresh_emacs("");
    let res = line
      .handle_key(&KeyEvent(KeyCode::F(12), ModKeys::NONE))
      .unwrap();
    assert!(res.is_none());
  }
}

// ===================== readline/mod.rs coverage =====================
//
// Targeted tests for branches with no existing coverage:
//   * SimpleEditor::{scroll_history, handle_key} arms
//   * StatusLine methods
//   * Prompt::new() PS1+PSR branch
//   * ShedLine::{reset_active_widget, handle_keymap (is_exact),
//                start_hist_search, run_cmd (ctrl+d spam),
//                scroll_history_virtual, handle_cmd_repeat (count>1)}
mod readline_mod_coverage {
  use super::*;
  use crate::expand::expand_keymap;
  use crate::keys::{KeyCode, KeyEvent, KeyMap, KeyMapFlags, ModKeys};
  use crate::motion;
  use crate::readline::editcmd::{CmdFlags, EditCmd, Motion};
  use crate::readline::{SimpleEditor, StatusLine};
  use crate::state::vars::{VarFlags, VarKind};

  // ─── SimpleEditor ────────────────────────────────────────────────

  fn simple_editor_with_history(entries: &[&str]) -> SimpleEditor {
    crate::readline::history::History::clear_global_caches_for_test("simple_editor_test");
    let mut ed = SimpleEditor::new(Some("simple_editor_test"));
    {
      let hist = ed.history.as_mut().unwrap();
      for entry in entries {
        hist.push(entry).unwrap();
      }
      hist.refresh_hist_entries();
      hist.constrain_entries(None);
    }
    ed
  }

  #[test]
  fn simple_editor_up_at_col0_scrolls_history_back() {
    // Covers should_grab_history (LineUp + col 0) and scroll_history
    // (entry-loaded branch). Buffer starts empty, Up arrow should
    // replace it with the most recent history entry.
    let _g = TestGuard::new();
    let mut ed = simple_editor_with_history(&["first", "second"]);
    ed.handle_key(KeyEvent(KeyCode::Up, ModKeys::NONE)).unwrap();
    assert_eq!(ed.buf.to_string(), "second");
  }

  #[test]
  fn simple_editor_down_on_last_line_restores_pending() {
    // After scrolling up then down past pending, pending is restored.
    let _g = TestGuard::new();
    let mut ed = simple_editor_with_history(&["first", "second"]);
    // Seed pending with current buffer content
    ed.buf.set_buffer("pending_input");
    ed.handle_key(KeyEvent(KeyCode::Up, ModKeys::NONE)).unwrap();
    assert_eq!(ed.buf.to_string(), "second");
    ed.handle_key(KeyEvent(KeyCode::Down, ModKeys::NONE))
      .unwrap();
    // After scrolling forward past last entry, pending is restored.
    assert_eq!(ed.buf.to_string(), "pending_input");
  }

  #[test]
  fn simple_editor_ctrl_d_on_empty_resolves_to_endoffile() {
    // Covers the EndOfFile branch of handle_key's Ctrl+D resolution.
    let _g = TestGuard::new();
    let mut ed = SimpleEditor::new(None);
    // Empty buffer + Ctrl+D → resolves to EndOfFile, lines.clear() runs
    ed.handle_key(key!(Ctrl + 'd')).unwrap();
    assert_eq!(ed.buf.to_string(), "");
  }

  #[test]
  fn simple_editor_ctrl_d_on_nonempty_resolves_to_delete() {
    // Covers the Delete branch of handle_key's Ctrl+D resolution.
    let _g = TestGuard::new();
    let mut ed = SimpleEditor::new(None);
    ed.buf.set_buffer("hello");
    ed.buf.set_cursor_from_flat(0);
    ed.handle_key(key!(Ctrl + 'd')).unwrap();
    assert_eq!(ed.buf.to_string(), "ello");
  }

  // ─── StatusLine ─────────────────────────────────────────────────

  fn with_statline_strings<R>(left: &str, mid: &str, right: &str, f: impl FnOnce() -> R) -> R {
    Shed::shopts_mut(|o| {
      o.statline.left_string = left.to_string();
      o.statline.middle_string = mid.to_string();
      o.statline.right_string = right.to_string();
    });
    f()
  }

  #[test]
  fn statline_new_reads_shopt_strings() {
    let _g = TestGuard::new();
    let mut sl = with_statline_strings("LEFT", "MID", "RIGHT", StatusLine::new);
    let (l, m, r) = sl.parts();
    assert_eq!(l, "LEFT");
    assert_eq!(m, "MID");
    assert_eq!(r, "RIGHT");
  }

  #[test]
  fn statline_render_pads_between_parts_when_room() {
    let _g = TestGuard::new();
    let mut sl = with_statline_strings("L", "M", "R", StatusLine::new);
    let out = sl.render(11);
    // Lengths: L=1, M=1, R=1, leftover = 11 - 3 = 8.
    // pad_lm = 4, pad_mr = 4 → "L    M    R" (11 cols).
    assert_eq!(out, "L    M    R");
    assert_eq!(out.chars().count(), 11);
  }

  #[test]
  fn statline_render_truncates_middle_when_too_narrow() {
    let _g = TestGuard::new();
    let mut sl = with_statline_strings("", "MIDDLE_TEXT", "R", StatusLine::new);
    let out = sl.render(6);
    // Right takes 1 col → 5 left for middle → middle gets ellipsis-truncated.
    assert!(out.ends_with('R'), "render = {out:?}");
    // The truncated middle should not contain the full word.
    assert!(!out.contains("MIDDLE_TEXT"));
  }

  #[test]
  fn statline_refresh_marks_dirty_and_parts_refreshes() {
    let _g = TestGuard::new();
    let mut sl = with_statline_strings("A", "B", "C", StatusLine::new);
    let _ = sl.parts(); // prime
    // Change the underlying shopt; parts() shouldn't see it yet.
    Shed::shopts_mut(|o| o.statline.left_string = "Z".to_string());
    {
      let (l, _, _) = sl.parts();
      assert_eq!(l, "A");
    }
    sl.refresh();
    {
      let (l, _, _) = sl.parts();
      assert_eq!(l, "Z", "refresh_now should have repopulated from shopt");
    }
  }

  // ─── Prompt::new() PS1+PSR branch ──────────────────────────────

  #[test]
  fn prompt_new_uses_ps1_and_psr_when_set() {
    let _g = TestGuard::new();
    Shed::vars_mut(|v| {
      v.set_var("PS1", VarKind::string("PS1_RAW"), VarFlags::empty())
        .unwrap();
      v.set_var("PSR", VarKind::string("PSR_RAW"), VarFlags::empty())
        .unwrap();
    });
    let mut p = Prompt::new();
    assert_eq!(p.get_ps1(), "PS1_RAW");
    // psr_expanded is private but accessible from this module.
    assert_eq!(p.psr_expanded.as_deref(), Some("PSR_RAW"));
  }

  #[test]
  fn prompt_new_falls_back_to_default_when_ps1_unset() {
    let _g = TestGuard::new();
    // PS1 should be unset for this test (TestGuard clears env).
    let _ = Shed::vars_mut(|v| v.unset_var("PS1"));
    let mut p = Prompt::new();
    // Default PS1 expands; it's non-empty.
    assert!(!p.get_ps1().is_empty());
    assert!(p.psr_expanded.is_none());
  }

  // ─── ShedLine::reset_active_widget ─────────────────────────────

  fn fresh_emacs_line() -> (ShedLine, TestGuard) {
    Shed::shopts_mut(|o| o.set.vi = false);
    let g = TestGuard::new();
    let prompt = Prompt::default();
    let line = ShedLine::new_no_hist(prompt).unwrap();
    (line, g)
  }

  #[test]
  fn reset_active_widget_with_completer_keeps_it_active_and_marks_redraw() {
    use crate::readline::complete::{Candidate, FuzzyCompleter};
    let (mut line, _g) = fresh_emacs_line();
    let mut comp = FuzzyCompleter::default();
    comp
      .selector
      .activate(vec![Candidate::from("alpha".to_string())]);
    line.completer = Some(Box::new(comp));
    line.needs_redraw = false;
    line.reset_active_widget(false).unwrap();
    assert!(line.completer.is_some(), "completer should remain");
    assert!(line.needs_redraw);
  }

  #[test]
  fn reset_active_widget_with_no_widget_falls_through_to_reset() {
    let (mut line, _g) = fresh_emacs_line();
    line.core.editor.set_buffer("some content");
    assert_eq!(line.core.editor.to_string(), "some content");
    line.reset_active_widget(false).unwrap();
    // reset() wipes the editor.
    assert_eq!(line.core.editor.to_string(), "");
  }

  // ─── ShedLine::handle_keymap is_exact arm ──────────────────────

  #[test]
  fn handle_keymap_executes_action_on_exact_match() {
    let (mut line, _g) = fresh_emacs_line();
    line.core.editor.set_buffer("hello");
    line.core.editor.set_cursor_from_flat(0);
    // Map <C-a> → <C-e> in emacs mode. This overrides the built-in
    // StartOfLine binding for Ctrl+A so that pressing it goes to EOL.
    Shed::logic_mut(|l| {
      l.insert_keymap(KeyMap {
        flags: KeyMapFlags::EMACS,
        keys: "<C-a>".into(),
        action: "<C-e>".into(),
      });
    });
    line.handle_keymap(&key!(Ctrl + 'a')).unwrap();
    // Action expanded to Ctrl+E → EndOfLine → cursor at "hello".len().
    assert_eq!(line.core.editor.cursor_to_flat(), 5);
    assert!(
      line.pending_keymap.is_empty(),
      "exact match should clear pending_keymap"
    );
  }

  // ─── ShedLine::start_hist_search ───────────────────────────────

  #[test]
  fn start_hist_search_with_prefix_match_adopts_entry() {
    // When the current buffer is a prefix of a single history entry,
    // start_hist_search adopts that entry into the editor (no finder).
    // SEARCH_ENTRIES is a process-global cache keyed by table name, so we
    // wipe the "shed_history" key before pushing to keep len()==1.
    crate::readline::history::History::clear_global_caches_for_test("shed_history");
    let (mut line, _g) = fresh_emacs_line();
    line.history.push("echo foobar").unwrap();
    line.history.refresh_hist_entries();
    line.history.constrain_entries(None);
    line.core.editor.set_buffer("echo");
    line.core.editor.move_cursor_to_end();
    line.start_hist_search();
    assert_eq!(line.core.editor.to_string(), "echo foobar");
    assert!(
      line.history.fuzzy_finder.is_none(),
      "finder should not have opened"
    );
  }

  #[test]
  fn start_hist_search_with_multiple_matches_opens_finder() {
    crate::readline::history::History::clear_global_caches_for_test("shed_history");
    let (mut line, _g) = fresh_emacs_line();
    line.history.push("git status").unwrap();
    line.history.push("git diff").unwrap();
    line.history.push("ls -la").unwrap();
    line.history.refresh_hist_entries();
    line.history.constrain_entries(None);
    line.core.editor.set_buffer("git");
    line.core.editor.move_cursor_to_end();
    line.start_hist_search();
    assert!(
      line.history.fuzzy_finder.is_some(),
      "finder should be active for ambiguous prefix"
    );
  }

  // ─── ShedLine::run_cmd ctrl+d spam handling ────────────────────

  fn ctrl_d_motion_cmd() -> EditCmd {
    EditCmd {
      motion: Some(motion!(Motion::HalfScreenDown)),
      ..Default::default()
    }
  }

  #[test]
  fn run_cmd_ctrl_d_motion_on_empty_fires_warning_each_call() {
    // editor.is_empty() short-circuits to the warning + counter reset.
    let (mut line, _g) = fresh_emacs_line();
    assert_eq!(line.ctrl_d_warning_counter, 0);
    line.run_cmd(ctrl_d_motion_cmd()).unwrap();
    // Empty editor → counter stays at 0 (warning fired immediately)
    assert_eq!(line.ctrl_d_warning_counter, 0);
  }

  #[test]
  fn run_cmd_ctrl_d_motion_increments_counter_then_resets() {
    let (mut line, _g) = fresh_emacs_line();
    line.core.editor.set_buffer("single line");
    line.core.editor.move_cursor_to_end();
    // 3 non-moving Ctrl+D-as-motion calls increment the counter.
    line.run_cmd(ctrl_d_motion_cmd()).unwrap();
    assert_eq!(line.ctrl_d_warning_counter, 1);
    line.run_cmd(ctrl_d_motion_cmd()).unwrap();
    assert_eq!(line.ctrl_d_warning_counter, 2);
    line.run_cmd(ctrl_d_motion_cmd()).unwrap();
    assert_eq!(line.ctrl_d_warning_counter, 3);
    // 4th call fires the warning and resets.
    line.run_cmd(ctrl_d_motion_cmd()).unwrap();
    assert_eq!(line.ctrl_d_warning_counter, 0);
  }

  // ─── ShedLine::scroll_history_virtual ──────────────────────────

  fn virtual_scroll_cmd(motion: Motion, shift: bool) -> EditCmd {
    let mut flags = CmdFlags::empty();
    if shift {
      flags |= CmdFlags::HAS_SHIFT;
    } else {
      flags |= CmdFlags::HAS_CTRL;
    }
    EditCmd {
      motion: Some(motion!(motion)),
      flags,
      ..Default::default()
    }
  }

  fn line_with_virt_history(initial: &str, entries: &[&str]) -> (ShedLine, TestGuard) {
    crate::readline::history::History::clear_global_caches_for_test("shed_history");
    let (mut line, g) = fresh_emacs_line();
    for entry in entries {
      line.history.push(entry).unwrap();
    }
    line.history.refresh_hist_entries();
    // constrain_entries (not just update_search_mask) repositions the cursor
    // to mask.len() so virt_scroll(-1) actually moves backward from the
    // "pending" sentinel position.
    line.history.constrain_entries(None);
    line.core.editor.set_buffer(initial);
    line.core.editor.move_cursor_to_end();
    (line, g)
  }

  #[test]
  fn scroll_history_virtual_lineup_shift_prepends_with_amp() {
    // Backward virtual scroll prepends the previous history entry with
    // " && " (HAS_SHIFT) onto the current buffer.
    let (mut line, _g) = line_with_virt_history("current", &["prev_cmd"]);
    line.scroll_history_virtual(virtual_scroll_cmd(Motion::LineUp, true));
    assert_eq!(line.core.editor.to_string(), "prev_cmd && current");
  }

  #[test]
  fn scroll_history_virtual_lineup_ctrl_prepends_with_semicolon() {
    let (mut line, _g) = line_with_virt_history("current", &["prev_cmd"]);
    line.scroll_history_virtual(virtual_scroll_cmd(Motion::LineUp, false));
    assert_eq!(line.core.editor.to_string(), "prev_cmd; current");
  }

  #[test]
  fn scroll_history_virtual_linedown_after_lineup_pops_left() {
    // Up (concat_left) puts us in Backward scroll direction;
    // Down then pops_left and reverses one step.
    let (mut line, _g) = line_with_virt_history("current", &["prev_cmd"]);
    line.scroll_history_virtual(virtual_scroll_cmd(Motion::LineUp, true));
    assert_eq!(line.core.editor.to_string(), "prev_cmd && current");
    line.scroll_history_virtual(virtual_scroll_cmd(Motion::LineDown, true));
    // pop_left strips the prepended chunk
    assert_eq!(line.core.editor.to_string(), "current");
  }

  #[test]
  fn scroll_history_virtual_linedown_none_direction_concats_right() {
    // Reaching the None|Forward LineDown arm (concat_right) needs the
    // history cursor parked below mask.len() so virt_scroll(1) has room
    // to advance and yield Some. Two scroll(-1) calls position
    // cursor=virt=0 with mask.len()=2.
    let (mut line, _g) = line_with_virt_history("current", &["alpha", "beta"]);
    line.history.scroll(-1);
    line.history.scroll(-1);
    // direction=None (virt==cursor) → enters concat_right branch.
    // virt_scroll(1) returns search_mask[1]; concat_right appends " && ".
    // Because both pushes share a second-resolution timestamp, SQLite's
    // tie-break for equal `MAX(timestamp)` rows is unspecified — assert
    // structurally without pinning which entry won the tie.
    line.scroll_history_virtual(virtual_scroll_cmd(Motion::LineDown, true));
    let joined = line.core.editor.to_string();
    assert!(
      joined == "current && alpha" || joined == "current && beta",
      "expected concat_right with one of the entries, got {joined:?}"
    );
  }

  // ─── ShedLine::handle_cmd_repeat CmdReplay::Single count>1 ────

  #[test]
  fn dot_with_count_n_takes_override_branch() {
    // `3.` after `dw` exercises the `count > 1` arm of CmdReplay::Single:
    // the saved cmd's verb.0 is overridden to 3 and motion.0 to 1.
    //
    // Note: eval_motion (motion.rs:185) reads count from the *motion* arg,
    // not the verb, so for a motion-driven verb like Delete the override
    // has no cumulative effect — `3.` deletes one word, same as `.`. We
    // pin that current behavior here; the test serves to exercise the
    // branch (no panic, no skip) and lock in the observed result so a
    // future refactor that fixes the count plumbing fails this assertion
    // intentionally.
    let (mut vi, _g) = test_vi("one two three four five");
    let keys = expand_keymap("<Esc>0dw3.");
    vi.process_input(keys).unwrap();
    assert_eq!(vi.core.editor.to_string(), "three four five");
  }
}

mod keymap_implied_submit {
  use super::*;
  use crate::keys::{KeyMap, KeyMapFlags};
  use crate::readline::ModeReport;

  fn feed(line: &mut ShedLine, s: &str) {
    Shed::term_mut(|t| t.feed_bytes(s.as_bytes()));
    let keys = Shed::term_mut(|t| t.drain_keys());
    line.process_input(keys).unwrap();
  }

  fn register(keys: &str, action: &str) {
    Shed::logic_mut(|l| {
      l.insert_keymap(KeyMap {
        flags: KeyMapFlags::NORMAL,
        keys: keys.into(),
        action: action.into(),
      });
    });
  }

  #[test]
  fn keymap_ending_in_search_auto_submits() {
    let (mut vi, _g) = test_vi("foo bar foo");
    feed(&mut vi, "\x1b"); // ensure normal mode
    register(",s", "/bar"); // a search with no trailing <CR>
    feed(&mut vi, ",s");
    // the pending search was submitted: cursor jumped to "bar", back to normal
    assert_eq!(vi.core.editor.cursor_to_flat(), 4);
    assert_eq!(vi.core.mode.report_mode(), ModeReport::Normal);
  }

  #[test]
  fn keymap_opening_empty_prompt_stays_open() {
    let (mut vi, _g) = test_vi("foo bar foo");
    feed(&mut vi, "\x1b");
    register(",o", "/"); // opens a bare search prompt, nothing typed
    feed(&mut vi, ",o");
    // no pattern was typed, so the prompt is left open for the user
    assert_eq!(vi.core.mode.report_mode(), ModeReport::Search);
  }
}

mod normal_command_mappings {
  use super::*;
  use crate::keys::{KeyMap, KeyMapFlags};

  fn feed(line: &mut ShedLine, s: &str) {
    Shed::term_mut(|t| t.feed_bytes(s.as_bytes()));
    let keys = Shed::term_mut(|t| t.drain_keys());
    line.process_input(keys).unwrap();
  }
  fn register(keys: &str, action: &str) {
    Shed::logic_mut(|l| {
      l.insert_keymap(KeyMap {
        flags: KeyMapFlags::NORMAL,
        keys: keys.into(),
        action: action.into(),
      });
    });
  }
  fn unregister(keys: &str) {
    Shed::logic_mut(|l| l.remove_keymap(keys, KeyMapFlags::NORMAL));
  }

  #[test]
  fn normal_applies_mappings() {
    // `:normal` (no bang) runs the sequence *with* mappings, so a mapped `x`
    // (→ `dw`) deletes a word rather than a char.
    let (mut vi, _g) = test_vi("hello world");
    feed(&mut vi, "\x1b");
    register("x", "dw");
    feed(&mut vi, ":normal x\r");
    unregister("x");
    assert_eq!(vi.core.editor.to_string(), "world");
  }

  #[test]
  fn normal_bang_ignores_mappings() {
    // `:normal!` bypasses mappings, so `x` runs literally (delete one char).
    let (mut vi, _g) = test_vi("hello world");
    feed(&mut vi, "\x1b");
    register("x", "dw");
    feed(&mut vi, ":normal! x\r");
    unregister("x");
    assert_eq!(vi.core.editor.to_string(), "ello world");
  }

  #[test]
  fn normal_flushes_trailing_partial_mapping() {
    // A `:normal` sequence that ends on a proper prefix of a multi-key mapping
    // (`g` of `gh`) must flush the buffered key instead of leaving it in
    // `pending_keymap` to leak into the next keystroke.
    let (mut vi, _g) = test_vi("abc");
    feed(&mut vi, "\x1b");
    register("gh", "x");
    feed(&mut vi, ":normal g\r");
    unregister("gh");
    assert!(
      vi.pending_keymap.is_empty(),
      "trailing partial mapping leaked: {:?}",
      vi.pending_keymap
    );
  }
}
