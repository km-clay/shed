use super::super::*;

#[test]
fn parse_simple() {
	let input = "echo hello world";
	let tk_stream: Vec<_> = LexStream::new(input, LexFlags::empty()).collect();
	let nodes: Vec<_> = ParseStream::new(tk_stream).collect();

	insta::assert_debug_snapshot!(nodes)
}

#[test]
fn parse_pipeline() {
	let input = "echo foo | sed s/foo/bar";
	let tk_stream: Vec<_> = LexStream::new(input, LexFlags::empty()).collect();
	let nodes: Vec<_> = ParseStream::new(tk_stream).collect();

	insta::assert_debug_snapshot!(nodes)
}

#[test]
fn parse_conjunction() {
	let input = "echo foo && echo bar";
	let tk_stream: Vec<_> = LexStream::new(input, LexFlags::empty()).collect();
	let nodes: Vec<_> = ParseStream::new(tk_stream).collect();

	insta::assert_debug_snapshot!(nodes)
}

#[test]
fn parse_conjunction_and_pipeline() {
	let input = "echo foo | sed s/foo/bar/ && echo bar | sed s/bar/foo/ || echo foo bar | sed s/foo bar/bar foo/";
	let tk_stream: Vec<_> = LexStream::new(input, LexFlags::empty()).collect();
	let nodes: Vec<_> = ParseStream::new(tk_stream).collect();

	insta::assert_debug_snapshot!(nodes)
}

#[test]
fn parse_multiline() {
	let input = "
echo hello world
echo foo bar
echo boo biz";
	let tk_stream: Vec<_> = LexStream::new(input, LexFlags::empty()).collect();
	let nodes: Vec<_> = ParseStream::new(tk_stream).collect();

	insta::assert_debug_snapshot!(nodes)
}
