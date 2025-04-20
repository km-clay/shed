use libsh::term::{Style, StyleSet, Styled};

use super::super::*;
#[test]
fn styled_simple() {
	let input = "hello world";
	let styled = input.styled(Style::Green);

	insta::assert_snapshot!(styled)
}
#[test]
fn styled_multiple() {
	let input = "styled text";
	let styled = input.styled(Style::Red | Style::Bold | Style::Underline);
	insta::assert_snapshot!(styled);
}
#[test]
fn styled_rgb() {
	let input = "RGB styled text";
	let styled = input.styled(Style::RGB(255, 99, 71));  // Tomato color
	insta::assert_snapshot!(styled);
}
#[test]
fn styled_background() {
	let input = "text with background";
	let styled = input.styled(Style::BgBlue | Style::Bold);
	insta::assert_snapshot!(styled);
}
#[test]
fn styled_set() {
	let input = "multi-style text";
	let style_set = StyleSet::new().add_style(Style::Magenta).add_style(Style::Italic);
	let styled = input.styled(style_set);
	insta::assert_snapshot!(styled);
}
#[test]
fn styled_reset() {
	let input = "reset test";
	let styled = input.styled(Style::Bold | Style::Reset);
	insta::assert_snapshot!(styled);
}
