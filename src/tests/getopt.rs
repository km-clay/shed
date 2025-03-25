use getopt::{get_opts, get_opts_from_tokens};
use parse::NdRule;
use tests::get_nodes;

use super::super::*;

#[test]
fn getopt_from_argv() {
	let node = get_nodes("echo -n -e foo", |node| matches!(node.class, NdRule::Command {..}))
		.pop()
		.unwrap();
	let NdRule::Command { assignments, argv } = node.class else {
		panic!()
	};

	let (words,opts) = get_opts_from_tokens(argv);
	insta::assert_debug_snapshot!(words);
	insta::assert_debug_snapshot!(opts)
}

#[test]
fn getopt_simple() {
	let raw = "echo -n foo".split_whitespace().map(|s| s.to_string()).collect::<Vec<_>>();

	let (words,opts) = get_opts(raw);
	insta::assert_debug_snapshot!(words);
	insta::assert_debug_snapshot!(opts);
}

#[test]
fn getopt_multiple_short() {
	let raw = "echo -nre foo".split_whitespace().map(|s| s.to_string()).collect::<Vec<_>>();

	let (words,opts) = get_opts(raw);
	insta::assert_debug_snapshot!(words);
	insta::assert_debug_snapshot!(opts);
}
