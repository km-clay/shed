use super::super::*;
#[test]
fn lex_simple() {
	let input = "echo hello world";
	let tokens: Vec<_> = LexStream::new(input, LexFlags::empty()).collect();

	insta::assert_debug_snapshot!(tokens)
}
#[test]
fn lex_redir() {
	let input = "echo foo > bar.txt";
	let tokens: Vec<_> = LexStream::new(input, LexFlags::empty()).collect();

	insta::assert_debug_snapshot!(tokens)
}
#[test]
fn lex_redir_fds() {
	let input = "echo foo 1>&2";
	let tokens: Vec<_> = LexStream::new(input, LexFlags::empty()).collect();

	insta::assert_debug_snapshot!(tokens)
}
#[test]
fn lex_quote_str() {
	let input = "echo \"foo bar\" biz baz";
	let tokens: Vec<_> = LexStream::new(input, LexFlags::empty()).collect();

	insta::assert_debug_snapshot!(tokens)
}
#[test]
fn lex_with_keywords() {
	let input = "if true; then echo foo; fi";
	let tokens: Vec<_> = LexStream::new(input, LexFlags::empty()).collect();

	insta::assert_debug_snapshot!(tokens)
}

#[test]
fn lex_multiline() {
	let input = "echo hello world\necho foo bar\necho boo biz";
	let tokens: Vec<_> = LexStream::new(input, LexFlags::empty()).collect();

	insta::assert_debug_snapshot!(tokens)
}
