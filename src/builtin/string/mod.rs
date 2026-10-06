use crate::{opt, sub_command, util};

use super::{Builtin, BuiltinArgs, BuiltinRouter, ShResult, SubCommand, argv, opt as mod_opt};

mod clip;
mod split;
mod trim;

pub(super) struct Str;
impl BuiltinRouter for Str {
  fn name(&self) -> &'static str {
    "str"
  }
  fn sub_commands(&self) -> &'static [SubCommand] {
    const SUB_COMMANDS: &[SubCommand] = &[
      sub_command!(
        &trim::Trim,
        "trim",
        "<string>",
        "trim whitespace or charsets from a string"
      ),
      sub_command!(
        &clip::Clip,
        "clip",
        "<width> [string]",
        "clip a string to a given width"
      ),
      sub_command!(
        &split::Split,
        "split",
        "[options] [string]",
        "split a string into parts"
      ),
    ];
    SUB_COMMANDS
  }
}

#[cfg(test)]
mod tests {
  use crate::state::Shed;
  use crate::tests::testutil::{TestGuard, test_input};
  use pretty_assertions::assert_eq;

  fn out_of(cmd: &str) -> String {
    let g = TestGuard::new();
    test_input(cmd).unwrap();
    g.read_output()
  }

  fn status_of(cmd: &str) -> i32 {
    let _g = TestGuard::new();
    test_input(cmd).ok();
    Shed::get_status()
  }

  #[test]
  fn split_defaults_to_ifs_fields() {
    assert_eq!(out_of(r#"str split "a b c""#), "a\nb\nc\n");
  }

  #[test]
  fn split_ifs_folds_whitespace_runs() {
    assert_eq!(out_of(r#"str split "a  b""#), "a\nb\n");
  }

  #[test]
  fn split_ifs_drops_leading_and_trailing_runs() {
    assert_eq!(out_of(r#"str split " a b ""#), "a\nb\n");
  }

  #[test]
  fn split_ifs_non_whitespace_does_not_fold() {
    assert_eq!(out_of(r#"IFS=: str split "a::b""#), "a\n\nb\n");
  }

  #[test]
  fn split_ifs_agrees_with_read() {
    assert_eq!(
      out_of(
        r#"IFS=:; printf "a::b" | { read -a r; }; str split -a s "a::b"; printf '%s:%s' "${#r[@]}" "${#s[@]}""#
      ),
      "3:3"
    );
  }

  #[test]
  fn split_empty_ifs_does_not_divide() {
    assert_eq!(
      out_of(r#"IFS= str split -a p "a b"; printf '%s' "${#p[@]}""#),
      "1"
    );
  }

  #[test]
  fn split_any_keeps_separator_semantics() {
    assert_eq!(out_of("str split --any $' \\t' \"a  b\""), "a\n\nb\n");
  }

  #[test]
  fn split_ifs_leaves_a_carriage_return_attached() {
    assert_eq!(
      out_of(r#"str split $'a\rb c' -a p; [ "${p[0]}" = $'a\rb' ] && printf attached"#),
      "attached"
    );
  }

  #[test]
  fn split_on_a_literal_delimiter() {
    assert_eq!(out_of(r#"str split -d , "a,b,c""#), "a\nb\nc\n");
  }

  #[test]
  fn split_on_a_multibyte_delimiter() {
    assert_eq!(out_of(r#"str split -d ", " "a, b, c""#), "a\nb\nc\n");
  }

  #[test]
  fn split_output_separator_replaces_the_newline() {
    assert_eq!(out_of(r#"str split -d , -s : "a,b,c""#), "a:b:c\n");
  }

  #[test]
  fn split_quoted_output_survives_unquote() {
    assert_eq!(
      out_of(r#"str split -q -d , "a b,c d" | unquote -a p; printf "[%s]" "${p[@]}""#),
      "[a b][c d]"
    );
  }

  #[test]
  fn split_fills_an_array() {
    assert_eq!(
      out_of(r#"str split -a p -d , "a,b,c"; printf "[%s]" "${p[@]}""#),
      "[a][b][c]"
    );
  }

  #[test]
  fn split_on_any_of_a_byte_set() {
    assert_eq!(out_of(r#"str split --any ",;" "a,b;c""#), "a\nb\nc\n");
  }

  #[test]
  fn split_keeps_empty_fields() {
    assert_eq!(out_of(r#"str split -d , "a,,b""#), "a\n\nb\n");
  }

  #[test]
  fn split_keeps_a_trailing_empty_field() {
    assert_eq!(out_of(r#"str split -d , "a,b,""#), "a\nb\n\n");
  }

  #[test]
  fn split_with_no_delimiter_present_is_one_field() {
    assert_eq!(out_of(r#"str split -d , "abc""#), "abc\n");
  }

  #[test]
  fn split_ignores_escapes_by_default() {
    assert_eq!(out_of(r#"str split -d , "a\,b,c""#), "a\\\nb\nc\n");
  }

  #[test]
  fn split_honours_escapes_with_e() {
    assert_eq!(out_of(r#"str split -E -d , "a\,b,c""#), "a\\,b\nc\n");
  }

  #[test]
  fn split_reads_stdin_without_an_operand() {
    assert_eq!(out_of(r#"printf "x y z" | str split"#), "x\ny\nz\n");
  }

  #[test]
  fn split_fields_are_byte_exact() {
    assert_eq!(
      out_of(
        r#"v=$'\xff\xfe,\x41\x42'; str split -d , -a p "$v"; [ "${p[0]}" = $'\xff\xfe' ] && [ "${p[1]}" = AB ] && printf exact"#
      ),
      "exact"
    );
  }

  #[test]
  fn split_terminated_drops_a_trailing_empty() {
    assert_eq!(
      out_of(r#"str split -t -d , -a p "a,b,"; printf '%s' "${#p[@]}""#),
      "2"
    );
  }

  #[test]
  fn split_terminated_leaves_unterminated_input_alone() {
    assert_eq!(
      out_of(r#"str split -t -d , -a p "a,b"; printf '%s' "${#p[@]}""#),
      "2"
    );
  }

  #[test]
  fn split_terminated_keeps_interior_empties() {
    assert_eq!(
      out_of(r#"str split -t -d , -a p "a,,b,"; printf '%s' "${#p[@]}""#),
      "3"
    );
  }

  #[test]
  fn split_terminated_on_empty_input_is_no_parts() {
    assert_eq!(
      out_of(r#"str split -t -d , -a p ""; printf '%s' "${#p[@]}""#),
      "0"
    );
  }

  #[test]
  fn split_without_terminated_keeps_the_trailing_empty() {
    assert_eq!(
      out_of(r#"str split -d , -a p "a,b,"; printf '%s' "${#p[@]}""#),
      "3"
    );
  }

  #[test]
  fn split_null_in_reads_terminated_records() {
    assert_eq!(
      out_of(r#"str split --0in -a p $'x\x00y\x00z\x00'; printf '%s' "${#p[@]}""#),
      "3"
    );
  }

  #[test]
  fn split_null_in_reads_unterminated_records() {
    assert_eq!(
      out_of(r#"str split --0in -a p $'x\x00y\x00z'; printf '%s' "${#p[@]}""#),
      "3"
    );
  }

  #[test]
  fn split_null_in_keeps_an_interior_empty_record() {
    assert_eq!(
      out_of(r#"str split --0in -a p $'x\x00\x00y'; printf '%s' "${#p[@]}""#),
      "3"
    );
  }

  #[test]
  fn split_null_in_on_empty_input_is_no_records() {
    assert_eq!(
      out_of(r#"str split --0in -a p ""; printf '%s' "${#p[@]}""#),
      "0"
    );
  }

  #[test]
  fn split_any_nul_is_a_separator_not_a_terminator() {
    assert_eq!(
      out_of(r#"str split --any $'\x00' -a p $'x\x00y\x00z\x00'; printf '%s' "${#p[@]}""#),
      "4"
    );
  }

  #[test]
  fn split_null_out_terminates_each_part() {
    assert_eq!(
      out_of(r#"str split --0out -d , "a,b,c" >@o; [ "$o" = $'a\x00b\x00c\x00' ] && printf exact"#),
      "exact"
    );
  }

  #[test]
  fn split_null_out_with_no_parts_writes_nothing() {
    assert_eq!(
      out_of(r#"str split --0in --0out "" >@o; printf '%s' "$(len -b "$o")""#),
      "0"
    );
  }

  #[test]
  fn split_null_out_part_may_hold_a_nul() {
    assert_eq!(
      out_of(r#"str split --0out -d , $'\x00,x' >@o; [ "$o" = $'\x00\x00x\x00' ] && printf exact"#),
      "exact"
    );
  }

  #[test]
  fn split_null_round_trip_is_exact() {
    assert_eq!(
      out_of(
        r#"str split --0out -d , "a,b,c" | str split --0in -a p; printf '%s:%s' "${#p[@]}" "${p[-1]}""#
      ),
      "3:c"
    );
  }

  #[test]
  fn split_null_in_conflicts_with_delim() {
    assert_ne!(status_of(r#"str split --0in -d , "a""#), 0);
    assert_ne!(status_of(r#"str split -d , --0in "a""#), 0);
  }

  #[test]
  fn split_null_out_conflicts_with_quoted() {
    assert_ne!(status_of(r#"str split --0out -q "a""#), 0);
    assert_ne!(status_of(r#"str split -q --0out "a""#), 0);
  }

  #[test]
  fn split_null_in_and_null_out_combine() {
    assert_eq!(status_of(r#"str split --0in --0out "a""#), 0);
  }

  #[test]
  fn split_input_options_are_exclusive() {
    assert_ne!(status_of(r#"str split -d , --any ";" "a,b""#), 0);
  }

  #[test]
  fn split_output_options_are_exclusive() {
    assert_ne!(status_of(r#"str split -q -a p "a b""#), 0);
  }

  #[test]
  fn clip_default_marker_is_empty() {
    assert_eq!(out_of("str clip 5 abcdefghij"), "abcde");
  }

  #[test]
  fn clip_passes_through_when_under_limit() {
    assert_eq!(out_of("str clip 20 abc"), "abc");
  }

  #[test]
  fn clip_width_unit_counts_columns() {
    assert_eq!(out_of("str clip -w 6 日本語テキスト"), "日本語");
  }

  #[test]
  fn clip_width_shorthand_is_reachable() {
    assert_eq!(status_of("str clip -w 6 abcdefghij"), 0);
  }

  #[test]
  fn clip_chars_marker_counted_in_chars() {
    assert_eq!(out_of("str clip -c 5 -m … abcdefghij"), "abcd…");
  }

  #[test]
  fn clip_chars_center_keeps_both_ends() {
    assert_eq!(
      out_of("str clip -c 6 -j center abcdefghijklmnopqrstuvwxyz"),
      "abcxyz"
    );
  }

  #[test]
  fn clip_right_keeps_the_tail() {
    assert_eq!(out_of("str clip -c 4 -j right abcdefghij"), "ghij");
  }

  #[test]
  fn clip_bytes_unit_is_byte_exact() {
    assert_eq!(
      out_of(
        r#"v=$'\xff\xfe\x41\x42\x43\x44'; str clip -b 4 "$v" >@o; [ "$o" = $'\xff\xfe\x41\x42' ] && printf exact"#
      ),
      "exact"
    );
  }

  #[test]
  fn clip_units_are_mutually_exclusive() {
    assert_ne!(status_of("str clip -c -b 4 abc"), 0);
  }

  #[test]
  fn clip_missing_limit_is_an_error() {
    assert_ne!(status_of("str clip abc"), 0);
  }

  #[test]
  fn clip_reads_stdin_without_an_operand() {
    assert_eq!(out_of("printf abcdefghij | str clip 4"), "abcd");
  }
}
