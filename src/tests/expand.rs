use expand::unescape_str;
use parse::lex::{Tk, TkFlags, TkRule};
use state::write_vars;
use super::super::*;

#[test]
fn simple_expansion() {
	let varsub = "$foo";
	write_vars(|v| v.new_var("foo", "this is the value of the variable".into()));

	let mut tokens: Vec<Tk> = LexStream::new(varsub, LexFlags::empty())
		.filter(|tk| !matches!(tk.class, TkRule::EOI | TkRule::SOI))
		.collect();
	let var_tk = tokens.pop().unwrap();

	let var_span = var_tk.span.clone();
	let exp_tk = var_tk.expand(var_span, TkFlags::empty());
	write_vars(|v| v.vars_mut().clear());
	insta::assert_debug_snapshot!(exp_tk.get_words())
}

#[test]
fn unescape_string() {
	let string = "echo $foo \\$bar";
	let unescaped = unescape_str(string);

	insta::assert_snapshot!(unescaped)
}
