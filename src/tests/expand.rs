use std::collections::HashSet;

use super::*;

#[test]
fn simple_expansion() {
	let varsub = "$foo";
	write_vars(|v| v.set_var("foo", "this is the value of the variable".into(), false));

	let mut tokens: Vec<Tk> = LexStream::new(Arc::new(varsub.to_string()), LexFlags::empty())
		.map(|tk| tk.unwrap())
		.filter(|tk| !matches!(tk.class, TkRule::EOI | TkRule::SOI))
		.collect();
	let var_tk = tokens.pop().unwrap();

	let var_span = var_tk.span.clone();
	let exp_tk = var_tk.expand().unwrap();
	write_vars(|v| v.vars_mut().clear());
	insta::assert_debug_snapshot!(exp_tk.get_words())
}

#[test]
fn unescape_string() {
	let string = "echo $foo \\$bar";
	let unescaped = unescape_str(string);

	insta::assert_snapshot!(unescaped)
}

#[test]
fn expand_alias_simple() {
	write_logic(|l| {
		l.insert_alias("foo", "echo foo");
		let input = String::from("foo");

		let result = expand_aliases(input, HashSet::new(), &l);
		assert_eq!(result.as_str(),"echo foo");
		l.clear_aliases();
	});
}

#[test]
fn expand_alias_in_if() {
	write_logic(|l| {
		l.insert_alias("foo", "echo foo");
		let input = String::from("if foo; then echo bar; fi");

		let result = expand_aliases(input, HashSet::new(), &l);
		assert_eq!(result.as_str(),"if echo foo; then echo bar; fi");
		l.clear_aliases();
	});
}

#[test]
fn expand_alias_multiline() {
	write_logic(|l| {
		l.insert_alias("foo", "echo foo");
		l.insert_alias("bar", "echo bar");
		let input = String::from("
			foo
			if true; then
				bar
			fi
		");
		let expected = String::from("
			echo foo
			if true; then
				echo bar
			fi
		");

		let result = expand_aliases(input, HashSet::new(), &l);
		assert_eq!(result,expected)
	});
}

#[test]
fn expand_multiple_aliases() {
	write_logic(|l| {
		l.insert_alias("foo", "echo foo");
		l.insert_alias("bar", "echo bar");
		l.insert_alias("biz", "echo biz");
		let input = String::from("foo; bar; biz");

		let result = expand_aliases(input, HashSet::new(), &l);
		assert_eq!(result.as_str(),"echo foo; echo bar; echo biz");
	});
}

#[test]
fn alias_in_arg_position() {
	write_logic(|l| {
		l.insert_alias("foo", "echo foo");
		let input = String::from("echo foo");

		let result = expand_aliases(input.clone(), HashSet::new(), &l);
		assert_eq!(input,result);
		l.clear_aliases();
	});
}

#[test]
fn expand_recursive_alias() {
	write_logic(|l| {
		l.insert_alias("foo", "echo foo");
		l.insert_alias("bar", "foo bar");

		let input = String::from("bar");
		let result = expand_aliases(input, HashSet::new(), &l);
		assert_eq!(result.as_str(),"echo foo bar");
	});
}

#[test]
fn test_infinite_recursive_alias() {
	write_logic(|l| {
		l.insert_alias("foo", "foo bar");

		let input = String::from("foo");
		let result = expand_aliases(input, HashSet::new(), &l);
		assert_eq!(result.as_str(),"foo bar");
		l.clear_aliases();
	});

}
