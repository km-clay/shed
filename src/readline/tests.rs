#![allow(non_snake_case)]
use std::os::fd::AsRawFd;

use crate::{
  readline::{Prompt, ShedVi},
  testutil::TestGuard,
};

/// Tests for our vim logic emulation. Each test consists of an initial text, a sequence of keys to feed, and the expected final text and cursor position.
macro_rules! vi_test {
    { $($name:ident: $input:expr => $op:expr => $expected_text:expr,$expected_cursor:expr);* } => {
				$(
						#[test]
						fn $name() {
							let (mut vi, _g) = test_vi($input);
							vi.feed_bytes(b"\x1b"); // Start in normal mode
							vi.process_input().unwrap();

							vi.feed_bytes($op.as_bytes());
							vi.process_input().unwrap();
							assert_eq!(vi.editor.as_str(), $expected_text);
							assert_eq!(vi.editor.cursor.get(), $expected_cursor);
						}
				)*

    };
}

fn test_vi(initial: &str) -> (ShedVi, TestGuard) {
  let g = TestGuard::new();
  let prompt = Prompt::default();
  let vi = ShedVi::new_no_hist(prompt, g.pty_slave().as_raw_fd())
    .unwrap()
    .with_initial(initial);

  (vi, g)
}

// Why can't I marry a programming language
vi_test! {
  vi_dw_basic          : "hello world"            => "dw"           => "world", 0;
  vi_dw_middle         : "one two three"          => "wdw"          => "one three", 4;
  vi_dd_whole_line     : "hello world"            => "dd"           => "", 0;
  vi_x_single          : "hello"                  => "x"            => "ello", 0;
  vi_x_middle          : "hello"                  => "llx"          => "helo", 2;
  vi_X_backdelete      : "hello"                  => "llX"          => "hllo", 1;
  vi_h_motion          : "hello"                  => "$h"           => "hello", 3;
  vi_l_motion          : "hello"                  => "l"            => "hello", 1;
  vi_h_at_start        : "hello"                  => "h"            => "hello", 0;
  vi_l_at_end          : "hello"                  => "$l"           => "hello", 4;
  vi_w_forward         : "one two three"          => "w"            => "one two three", 4;
  vi_b_backward        : "one two three"          => "$b"           => "one two three", 8;
  vi_e_end             : "one two three"          => "e"            => "one two three", 2;
  vi_ge_back_end       : "one two three"          => "$ge"          => "one two three", 6;
  vi_w_punctuation     : "foo.bar baz"            => "w"            => "foo.bar baz", 3;
  vi_e_punctuation     : "foo.bar baz"            => "e"            => "foo.bar baz", 2;
  vi_b_punctuation     : "foo.bar baz"            => "$b"           => "foo.bar baz", 8;
  vi_w_at_eol          : "hello"                  => "$w"           => "hello", 4;
  vi_b_at_bol          : "hello"                  => "b"            => "hello", 0;
  vi_W_forward         : "foo.bar baz"            => "W"            => "foo.bar baz", 8;
  vi_B_backward        : "foo.bar baz"            => "$B"           => "foo.bar baz", 8;
  vi_E_end             : "foo.bar baz"            => "E"            => "foo.bar baz", 6;
  vi_gE_back_end       : "one two three"          => "$gE"          => "one two three", 6;
  vi_W_skip_punct      : "one-two three"          => "W"            => "one-two three", 8;
  vi_B_skip_punct      : "one two-three"          => "$B"           => "one two-three", 4;
  vi_E_skip_punct      : "one-two three"          => "E"            => "one-two three", 6;
  vi_dW_big            : "foo.bar baz"            => "dW"           => "baz", 0;
  vi_cW_big            : "foo.bar baz"            => "cWx\x1b"      => "x baz", 0;
  vi_zero_bol          : "  hello"                => "$0"           => "  hello", 0;
  vi_caret_first_char  : "  hello"                => "$^"           => "  hello", 2;
  vi_dollar_eol        : "hello world"            => "$"            => "hello world", 10;
  vi_g_last_nonws      : "hello   "               => "g_"           => "hello   ", 4;
  vi_g_no_trailing     : "hello"                  => "g_"           => "hello", 4;
  vi_pipe_column       : "hello world"            => "6|"           => "hello world", 5;
  vi_pipe_col1         : "hello world"            => "1|"           => "hello world", 0;
  vi_I_insert_front    : "  hello"                => "Iworld \x1b"  => "  world hello", 7;
  vi_A_append_end      : "hello"                  => "A world\x1b"  => "hello world", 10;
  vi_f_find            : "hello world"            => "fo"           => "hello world", 4;
  vi_F_find_back       : "hello world"            => "$Fo"          => "hello world", 7;
  vi_t_till            : "hello world"            => "tw"           => "hello world", 5;
  vi_T_till_back       : "hello world"            => "$To"          => "hello world", 8;
  vi_f_no_match        : "hello"                  => "fz"           => "hello", 0;
  vi_semicolon_repeat  : "abcabc"                 => "fa;;"         => "abcabc", 3;
  vi_comma_reverse     : "abcabc"                 => "fa;;,"        => "abcabc", 0;
  vi_df_semicolon      : "abcabc"                 => "fa;;dfa"      => "abcabc", 3;
  vi_t_at_target       : "aab"                    => "lta"          => "aab", 1;
  vi_D_to_end          : "hello world"            => "wD"           => "hello ", 5;
  vi_d_dollar          : "hello world"            => "wd$"          => "hello ", 5;
  vi_d0_to_start       : "hello world"            => "$d0"          => "d", 0;
  vi_dw_multiple       : "one two three"          => "d2w"          => "three", 0;
  vi_dt_char           : "hello world"            => "dtw"          => "world", 0;
  vi_df_char           : "hello world"            => "dfw"          => "orld", 0;
  vi_dh_back           : "hello"                  => "lldh"         => "hllo", 1;
  vi_dl_forward        : "hello"                  => "dl"           => "ello", 0;
  vi_dge_back_end      : "one two three"          => "$dge"         => "one tw", 5;
  vi_dG_to_end         : "hello world"            => "dG"           => "", 0;
  vi_dgg_to_start      : "hello world"            => "$dgg"         => "", 0;
  vi_d_semicolon       : "abcabc"                 => "fad;"         => "abcabc", 3;
  vi_cw_basic          : "hello world"            => "cwfoo\x1b"    => "foo world", 2;
  vi_C_to_end          : "hello world"            => "wCfoo\x1b"    => "hello foo", 8;
  vi_cc_whole          : "hello world"            => "ccfoo\x1b"    => "foo", 2;
  vi_ct_char           : "hello world"            => "ctwfoo\x1b"   => "fooworld", 2;
  vi_s_single          : "hello"                  => "sfoo\x1b"     => "fooello", 2;
  vi_S_whole_line      : "hello world"            => "Sfoo\x1b"     => "foo", 2;
  vi_cl_forward        : "hello"                  => "clX\x1b"      => "Xello", 0;
  vi_ch_backward       : "hello"                  => "llchX\x1b"    => "hXllo", 1;
  vi_cb_word_back      : "hello world"            => "$cbfoo\x1b"   => "hello food", 8;
  vi_ce_word_end       : "hello world"            => "cefoo\x1b"    => "foo world", 2;
  vi_c0_to_start       : "hello world"            => "wc0foo\x1b"   => "fooworld", 2;
  vi_yw_p_basic        : "hello world"            => "ywwP"         => "hello hello world", 11;
  vi_dw_p_paste        : "hello world"            => "dwP"          => "hello world", 5;
  vi_dd_p_paste        : "hello world"            => "ddp"          => "\nhello world", 1;
  vi_y_dollar_p        : "hello world"            => "wy$P"         => "hello worldworld", 10;
  vi_ye_p              : "hello world"            => "yewP"         => "hello helloworld", 10;
  vi_yy_p              : "hello world"            => "yyp"          => "hello world\nhello world", 12;
  vi_Y_p               : "hello world"            => "Yp"           => "hhello worldello world", 11;
  vi_p_after_x         : "hello"                  => "xp"           => "ehllo", 1;
  vi_P_before          : "hello"                  => "llxP"         => "hello", 2;
  vi_paste_empty       : "hello"                  => "p"            => "hello", 0;
  vi_r_replace         : "hello"                  => "ra"           => "aello", 0;
  vi_r_middle          : "hello"                  => "llra"         => "healo", 2;
  vi_r_at_end          : "hello"                  => "$ra"          => "hella", 4;
  vi_r_space           : "hello"                  => "r "           => " ello", 0;
  vi_r_with_count      : "hello"                  => "3rx"          => "xxxlo", 2;
  vi_tilde_single      : "hello"                  => "~"            => "Hello", 1;
  vi_tilde_count       : "hello"                  => "3~"           => "HELlo", 3;
  vi_tilde_at_end      : "HELLO"                  => "$~"           => "HELLo", 4;
  vi_tilde_mixed       : "hElLo"                  => "5~"           => "HeLlO", 4;
  vi_gu_word           : "HELLO world"            => "guw"          => "hello world", 0;
  vi_gU_word           : "hello WORLD"            => "gUw"          => "HELLO WORLD", 0;
  vi_gu_dollar         : "HELLO WORLD"            => "gu$"          => "hello world", 0;
  vi_gU_dollar         : "hello world"            => "gU$"          => "HELLO WORLD", 0;
  vi_gu_0              : "HELLO WORLD"            => "$gu0"         => "hello worlD", 0;
  vi_gU_0              : "hello world"            => "$gU0"         => "HELLO WORLd", 0;
  vi_gtilde_word       : "hello WORLD"            => "g~w"          => "HELLO WORLD", 0;
  vi_gtilde_dollar     : "hello WORLD"            => "g~$"          => "HELLO world", 0;
  vi_diw_inner         : "one two three"          => "wdiw"         => "one  three", 4;
  vi_ciw_replace       : "hello world"            => "ciwfoo\x1b"   => "foo world", 2;
  vi_daw_around        : "one two three"          => "wdaw"         => "one three", 4;
  vi_yiw_p             : "hello world"            => "yiwAp \x1bp"  => "hello worldp hello", 17;
  vi_diW_big_inner     : "one-two three"          => "diW"          => " three", 0;
  vi_daW_big_around    : "one two-three end"      => "wdaW"         => "one end", 4;
  vi_ciW_big           : "one-two three"          => "ciWx\x1b"     => "x three", 0;
  vi_di_dquote         : "one \"two\" three"      => "f\"di\""      => "one \"\" three", 5;
  vi_da_dquote         : "one \"two\" three"      => "f\"da\""      => "one three", 4;
  vi_ci_dquote         : "one \"two\" three"      => "f\"ci\"x\x1b" => "one \"x\" three", 5;
  vi_di_squote         : "one 'two' three"        => "f'di'"        => "one '' three", 5;
  vi_da_squote         : "one 'two' three"        => "f'da'"        => "one three", 4;
  vi_di_backtick       : "one `two` three"        => "f`di`"        => "one `` three", 5;
  vi_da_backtick       : "one `two` three"        => "f`da`"        => "one three", 4;
  vi_ci_dquote_empty   : "one \"\" three"         => "f\"ci\"x\x1b" => "one \"x\" three", 5;
  vi_di_paren          : "one (two) three"        => "f(di("        => "one () three", 5;
  vi_da_paren          : "one (two) three"        => "f(da("        => "one  three", 4;
  vi_ci_paren          : "one (two) three"        => "f(ci(x\x1b"   => "one (x) three", 5;
  vi_di_brace          : "one {two} three"        => "f{di{"        => "one {} three", 5;
  vi_da_brace          : "one {two} three"        => "f{da{"        => "one  three", 4;
  vi_di_bracket        : "one [two] three"        => "f[di["        => "one [] three", 5;
  vi_da_bracket        : "one [two] three"        => "f[da["        => "one  three", 4;
  vi_di_angle          : "one <two> three"        => "f<di<"        => "one <> three", 5;
  vi_da_angle          : "one <two> three"        => "f<da<"        => "one  three", 4;
  vi_di_paren_nested   : "fn(a, (b, c))"          => "f(di("        => "fn()", 3;
  vi_di_paren_empty    : "fn() end"               => "f(di("        => "fn() end", 3;
  vi_dib_alias         : "one (two) three"        => "f(dib"        => "one () three", 5;
  vi_diB_alias         : "one {two} three"        => "f{diB"        => "one {} three", 5;
  vi_percent_paren     : "(hello) world"          => "%"            => "(hello) world", 6;
  vi_percent_brace     : "{hello} world"          => "%"            => "{hello} world", 6;
  vi_percent_bracket   : "[hello] world"          => "%"            => "[hello] world", 6;
  vi_percent_from_close: "(hello) world"          => "f)%"          => "(hello) world", 0;
  vi_d_percent_paren   : "(hello) world"          => "d%"           => " world", 0;
  vi_i_insert          : "hello"                  => "iX\x1b"       => "Xhello", 0;
  vi_a_append          : "hello"                  => "aX\x1b"       => "hXello", 1;
  vi_I_front           : "  hello"                => "IX\x1b"       => "  Xhello", 2;
  vi_A_end             : "hello"                  => "AX\x1b"       => "helloX", 5;
  vi_o_open_below      : "hello"                  => "oworld\x1b"   => "hello\nworld", 10;
  vi_O_open_above      : "hello"                  => "Oworld\x1b"   => "world\nhello", 4;
  vi_empty_input       : ""                       => "i hello\x1b"  => " hello", 5;
  vi_insert_escape     : "hello"                  => "aX\x1b"       => "hXello", 1;
  vi_ctrl_w_del_word   : "hello world"            => "A\x17\x1b"    => "hello ", 5;
  vi_ctrl_h_backspace  : "hello"                  => "A\x08\x1b"    => "hell", 3;
  vi_u_undo_delete     : "hello world"            => "dwu"          => "hello world", 0;
  vi_u_undo_change     : "hello world"            => "ciwfoo\x1bu"  => "hello world", 0;
  vi_u_undo_x          : "hello"                  => "xu"           => "hello", 0;
  vi_ctrl_r_redo       : "hello"                  => "xu\x12"       => "ello", 0;
  vi_u_multiple        : "hello world"            => "xdwu"         => "ello world", 0;
  vi_redo_after_undo   : "hello world"            => "dwu\x12"      => "world", 0;
  vi_dot_repeat_x      : "hello"                  => "x."           => "llo", 0;
  vi_dot_repeat_dw     : "one two three"          => "dw."          => "three", 0;
  vi_dot_repeat_cw     : "one two three"          => "cwfoo\x1bw."  => "foo foo three", 6;
  vi_dot_repeat_r      : "hello"                  => "ra.."         => "aello", 0;
  vi_dot_repeat_s      : "hello"                  => "sX\x1bl."     => "XXllo", 1;
  vi_count_h           : "hello world"            => "$3h"          => "hello world", 7;
  vi_count_l           : "hello world"            => "3l"           => "hello world", 3;
  vi_count_w           : "one two three four"     => "2w"           => "one two three four", 8;
  vi_count_b           : "one two three four"     => "$2b"          => "one two three four", 8;
  vi_count_x           : "hello"                  => "3x"           => "lo", 0;
  vi_count_dw          : "one two three four"     => "2dw"          => "three four", 0;
  vi_verb_count_motion : "one two three four"     => "d2w"          => "three four", 0;
  vi_count_s           : "hello"                  => "3sX\x1b"      => "Xlo", 0;
  vi_indent_line       : "hello"                  => ">>"           => "\thello", 1;
  vi_dedent_line       : "\thello"                => "<<"           => "hello", 0;
  vi_indent_double     : "hello"                  => ">>>>"         => "\t\thello", 2;
  vi_J_join_lines      : "hello\nworld"           => "J"            => "hello world", 5;
  vi_v_u_lower         : "HELLO"                  => "vlllu"        => "hellO", 0;
  vi_v_U_upper         : "hello"                  => "vlllU"        => "HELLo", 0;
  vi_v_d_delete        : "hello world"            => "vwwd"         => "", 0;
  vi_v_x_delete        : "hello world"            => "vwwx"         => "", 0;
  vi_v_c_change        : "hello world"            => "vwcfoo\x1b"   => "fooorld", 2;
  vi_v_y_p_yank        : "hello world"            => "vwyAp \x1bp"  => "hello worldp hello w", 19;
  vi_v_dollar_d        : "hello world"            => "wv$d"         => "hello ", 5;
  vi_v_0_d             : "hello world"            => "$v0d"         => "", 0;
  vi_ve_d              : "hello world"            => "ved"          => " world", 0;
  vi_v_o_swap          : "hello world"            => "vllod"        => "lo world", 0;
  vi_v_r_replace       : "hello"                  => "vlllrx"       => "xxxxo", 0;
  vi_v_tilde_case      : "hello"                  => "vlll~"        => "HELLo", 0;
  vi_V_d_delete        : "hello world"            => "Vd"           => "", 0;
  vi_V_y_p             : "hello world"            => "Vyp"          => "hello world\nhello world", 12;
  vi_V_S_change        : "hello world"            => "VSfoo\x1b"    => "foo", 2;
  vi_ctrl_a_inc        : "num 5 end"              => "w\x01"        => "num 6 end", 4;
  vi_ctrl_x_dec        : "num 5 end"              => "w\x18"        => "num 4 end", 4;
  vi_ctrl_a_negative   : "num -3 end"             => "w\x01"        => "num -2 end", 4;
  vi_ctrl_x_to_neg     : "num 0 end"              => "w\x18"        => "num -1 end", 4;
  vi_ctrl_a_count      : "num 5 end"              => "w3\x01"       => "num 8 end", 4;
  vi_ctrl_a_width      : "num -00001 end"         => "w\x01"        => "num 00000 end", 4;
  vi_delete_empty      : ""                       => "x"            => "", 0;
  vi_undo_on_empty     : ""                       => "u"            => "", 0;
  vi_w_single_char     : "a b c"                  => "w"            => "a b c", 2;
  vi_dw_last_word      : "hello"                  => "dw"           => "", 0;
  vi_dollar_single     : "h"                      => "$"            => "h", 0;
  vi_caret_no_ws       : "hello"                  => "$^"           => "hello", 0;
  vi_f_last_char       : "hello"                  => "fo"           => "hello", 4;
  vi_r_on_space        : "hello world"            => "5|r-"         => "hell- world", 4;
  vi_vw_doesnt_crash   : ""        					      => "vw"           => "", 0;
  vi_indent_cursor_pos : "echo foo"               => ">>"           => "\techo foo", 1;
  vi_join_indent_lines : "echo foo\n\t\techo bar" => "J"        => "echo foo echo bar", 8
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
		"echo foo \\\rbar \\\rbiz \\\rbazz\rbreak\rdone\r;;\resac\r}"
	];

	for (i,line) in lines.iter().enumerate() {
		vi.feed_bytes(line.as_bytes());
		if i != lines.len() - 1 {
			vi.feed_bytes(b"\r");
		}
		vi.process_input().unwrap();
	}

	assert_eq!(
		vi.editor.as_str(),
		"func() {\n\tcase foo in\n\t\tbar)\n\t\t\twhile true; do\n\t\t\t\techo foo \\\n\t\t\t\t\tbar \\\n\t\t\t\t\tbiz \\\n\t\t\t\t\tbazz\n\t\t\t\tbreak\n\t\t\tdone\n\t\t;;\n\tesac\n}"
	);
}
