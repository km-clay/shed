
use insta::assert_snapshot;

use crate::prompt::highlight::FernHighlighter;

use super::super::*;

#[test]
fn highlight_simple() {
	let line = "echo foo bar";
	let styled = FernHighlighter::new(line.to_string()).hl_input();
	assert_snapshot!(styled)
}

#[test]
fn highlight_cmd_sub() {
	let line = "echo foo $(echo bar)";
	let styled = FernHighlighter::new(line.to_string()).hl_input();
	assert_snapshot!(styled)
}

#[test]
fn highlight_cmd_sub_in_dquotes() {
	let line = "echo \"foo $(echo bar) biz\"";
	let styled = FernHighlighter::new(line.to_string()).hl_input();
	assert_snapshot!(styled)
}
