use super::super::*;

#[test]
fn parse_simple() {
	let input = "echo hello world";
	let tk_stream: Vec<_> = LexStream::new(Rc::new(input.to_string()), LexFlags::empty())
		.map(|tk| tk.unwrap())
		.collect();
	let nodes: Vec<_> = ParseStream::new(tk_stream).collect();

	insta::assert_debug_snapshot!(nodes)
}

#[test]
fn parse_pipeline() {
	let input = "echo foo | sed s/foo/bar";
	let tk_stream: Vec<_> = LexStream::new(Rc::new(input.to_string()), LexFlags::empty())
		.map(|tk| tk.unwrap())
		.collect();
	let nodes: Vec<_> = ParseStream::new(tk_stream).collect();

	insta::assert_debug_snapshot!(nodes)
}

#[test]
fn parse_conjunction() {
	let input = "echo foo && echo bar";
	let tk_stream: Vec<_> = LexStream::new(Rc::new(input.to_string()), LexFlags::empty())
		.map(|tk| tk.unwrap())
		.collect();
	let nodes: Vec<_> = ParseStream::new(tk_stream).collect();

	insta::assert_debug_snapshot!(nodes)
}

#[test]
fn parse_conjunction_and_pipeline() {
	let input = "echo foo | sed s/foo/bar/ && echo bar | sed s/bar/foo/ || echo foo bar | sed s/foo bar/bar foo/";
	let tk_stream: Vec<_> = LexStream::new(Rc::new(input.to_string()), LexFlags::empty())
		.map(|tk| tk.unwrap())
		.collect();
	let nodes: Vec<_> = ParseStream::new(tk_stream).collect();

	insta::assert_debug_snapshot!(nodes)
}

#[test]
fn parse_multiline() {
	let input = "
echo hello world
echo foo bar
echo boo biz";
	let tk_stream: Vec<_> = LexStream::new(Rc::new(input.to_string()), LexFlags::empty())
		.map(|tk| tk.unwrap())
		.collect();
	let nodes: Vec<_> = ParseStream::new(tk_stream).collect();

	insta::assert_debug_snapshot!(nodes)
}

#[test]
fn parse_if_simple() {
	let input = "if foo; then echo bar; fi";
	let tk_stream: Vec<_> = LexStream::new(Rc::new(input.to_string()), LexFlags::empty())
		.map(|tk| tk.unwrap())
		.collect();
	let nodes: Vec<_> = ParseStream::new(tk_stream).collect();

	insta::assert_debug_snapshot!(nodes)
}
#[test]
fn parse_if_with_elif() {
	let input = "if foo; then echo bar; elif bar; then echo foo; fi";
	let tk_stream: Vec<_> = LexStream::new(Rc::new(input.to_string()), LexFlags::empty())
		.map(|tk| tk.unwrap())
		.collect();
	let nodes: Vec<_> = ParseStream::new(tk_stream).collect();

	insta::assert_debug_snapshot!(nodes)
}
#[test]
fn parse_if_multiple_elif() {
	let input = "if foo; then echo bar; elif bar; then echo foo; elif biz; then echo baz; fi";
	let tk_stream: Vec<_> = LexStream::new(Rc::new(input.to_string()), LexFlags::empty())
		.map(|tk| tk.unwrap())
		.collect();
	let nodes: Vec<_> = ParseStream::new(tk_stream).collect();

	insta::assert_debug_snapshot!(nodes)
}
#[test]
fn parse_if_multiline() {
	let input = "
if foo; then
	echo bar
elif bar; then
	echo foo;
elif biz; then
	echo baz
fi";
	let tk_stream: Vec<_> = LexStream::new(Rc::new(input.to_string()), LexFlags::empty())
		.map(|tk| tk.unwrap())
		.collect();
	let nodes: Vec<_> = ParseStream::new(tk_stream).collect();

	insta::assert_debug_snapshot!(nodes)
}
#[test]
fn parse_loop_simple() {
	let input = "while foo; do bar; done";
	let tk_stream: Vec<_> = LexStream::new(Rc::new(input.to_string()), LexFlags::empty())
		.map(|tk| tk.unwrap())
		.collect();
	let nodes: Vec<_> = ParseStream::new(tk_stream).collect();

	insta::assert_debug_snapshot!(nodes)
}
#[test]
fn parse_loop_until() {
	let input = "until foo; do bar; done";
	let tk_stream: Vec<_> = LexStream::new(Rc::new(input.to_string()), LexFlags::empty())
		.map(|tk| tk.unwrap())
		.collect();
	let nodes: Vec<_> = ParseStream::new(tk_stream).collect();

	insta::assert_debug_snapshot!(nodes)
}
#[test]
fn parse_loop_multiline() {
	let input = "
until foo; do
	bar
done";
	let tk_stream: Vec<_> = LexStream::new(Rc::new(input.to_string()), LexFlags::empty())
		.map(|tk| tk.unwrap())
		.collect();
	let nodes: Vec<_> = ParseStream::new(tk_stream).collect();

	insta::assert_debug_snapshot!(nodes)
}
#[test]
fn parse_case_simple() {
	let input = "case foo in foo) bar;; bar) foo;; biz) baz;; esac";
	let tk_stream: Vec<_> = LexStream::new(Rc::new(input.to_string()), LexFlags::empty())
		.map(|tk| tk.unwrap())
		.collect();
	let nodes: Vec<_> = ParseStream::new(tk_stream).collect();

	insta::assert_debug_snapshot!(nodes)
}
#[test]
fn parse_case_multiline() {
	let input = "case foo in
	foo) bar
	;;
	bar) foo
	;;
	biz) baz
	;;
esac";
	let tk_stream: Vec<_> = LexStream::new(Rc::new(input.to_string()), LexFlags::empty())
		.map(|tk| tk.unwrap())
		.collect();
	let nodes: Vec<_> = ParseStream::new(tk_stream).collect();

	insta::assert_debug_snapshot!(nodes)
}
#[test]
fn parse_case_nested() {
	let input = "case foo in
	foo) if true; then
		echo foo
	fi
	;;
	bar) if false; then
		echo bar
	fi
	;;
esac";
	let tk_stream: Vec<_> = LexStream::new(Rc::new(input.to_string()), LexFlags::empty())
		.map(|tk| tk.unwrap())
		.collect();
	let nodes: Vec<_> = ParseStream::new(tk_stream).collect();

	insta::assert_debug_snapshot!(nodes)
}
