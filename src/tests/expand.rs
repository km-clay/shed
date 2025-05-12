use std::collections::HashSet;

use crate::expand::perform_param_expansion;

use super::*;

#[test]
fn simple_expansion() {
	let varsub = "$foo";
	write_vars(|v| v.set_var("foo", "this is the value of the variable", false));

	let mut tokens: Vec<Tk> = LexStream::new(Arc::new(varsub.to_string()), LexFlags::empty())
		.map(|tk| tk.unwrap())
		.filter(|tk| !matches!(tk.class, TkRule::EOI | TkRule::SOI))
		.collect();
	let var_tk = tokens.pop().unwrap();

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

		let result = expand_aliases(input, HashSet::new(), l);
		assert_eq!(result.as_str(),"echo foo");
		l.clear_aliases();
	});
}

#[test]
fn expand_alias_in_if() {
	write_logic(|l| {
		l.insert_alias("foo", "echo foo");
		let input = String::from("if foo; then echo bar; fi");

		let result = expand_aliases(input, HashSet::new(), l);
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

		let result = expand_aliases(input, HashSet::new(), l);
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

		let result = expand_aliases(input, HashSet::new(), l);
		assert_eq!(result.as_str(),"echo foo; echo bar; echo biz");
	});
}

#[test]
fn alias_in_arg_position() {
	write_logic(|l| {
		l.insert_alias("foo", "echo foo");
		let input = String::from("echo foo");

		let result = expand_aliases(input.clone(), HashSet::new(), l);
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
		let result = expand_aliases(input, HashSet::new(), l);
		assert_eq!(result.as_str(),"echo foo bar");
	});
}

#[test]
fn test_infinite_recursive_alias() {
	write_logic(|l| {
		l.insert_alias("foo", "foo bar");

		let input = String::from("foo");
		let result = expand_aliases(input, HashSet::new(), l);
		assert_eq!(result.as_str(),"foo bar");
		l.clear_aliases();
	});

}

#[test]
fn param_expansion_defaultunsetornull() {
    write_vars(|v| {
        v.set_var("foo", "foo", false);
        v.set_var("set_var", "value", false);
    });
    let result = perform_param_expansion("unset:-default").unwrap();
    assert_eq!(result, "default");
    write_vars(|v| v.vars_mut().clear());
}

#[test]
fn param_expansion_defaultunset() {
    write_vars(|v| {
        v.set_var("foo", "foo", false);
        v.set_var("set_var", "value", false);
    });
    let result = perform_param_expansion("unset-default").unwrap();
    assert_eq!(result, "default");
    write_vars(|v| v.vars_mut().clear());
}

#[test]
fn param_expansion_setdefaultunsetornull() {
    write_vars(|v| {
        v.set_var("foo", "foo", false);
        v.set_var("set_var", "value", false);
    });
    let result = perform_param_expansion("unset:=assigned").unwrap();
    assert_eq!(result, "assigned");
    write_vars(|v| v.vars_mut().clear());
}

#[test]
fn param_expansion_setdefaultunset() {
    write_vars(|v| {
        v.set_var("foo", "foo", false);
        v.set_var("set_var", "value", false);
    });
    let result = perform_param_expansion("unset=assigned").unwrap();
    assert_eq!(result, "assigned");
    write_vars(|v| v.vars_mut().clear());
}

#[test]
fn param_expansion_altsetnotnull() {
    write_vars(|v| {
        v.set_var("foo", "foo", false);
        v.set_var("set_var", "value", false);
    });
    let result = perform_param_expansion("set_var:+alt").unwrap();
    assert_eq!(result, "alt");
    write_vars(|v| v.vars_mut().clear());
}

#[test]
fn param_expansion_altnotnull() {
    write_vars(|v| {
        v.set_var("foo", "foo", false);
        v.set_var("set_var", "value", false);
    });
    let result = perform_param_expansion("set_var+alt").unwrap();
    assert_eq!(result, "alt");
    write_vars(|v| v.vars_mut().clear());
}

#[test]
fn param_expansion_len() {
    write_vars(|v| {
        v.set_var("foo", "foo", false);
    });
    let result = perform_param_expansion("#foo").unwrap();
    assert_eq!(result, "3");
    write_vars(|v| v.vars_mut().clear());
}

#[test]
fn param_expansion_substr() {
    write_vars(|v| {
        v.set_var("foo", "foo", false);
    });
    let result = perform_param_expansion("foo:1").unwrap();
    assert_eq!(result, "oo");
    write_vars(|v| v.vars_mut().clear());
}

#[test]
fn param_expansion_substrlen() {
    write_vars(|v| {
        v.set_var("foo", "foo", false);
    });
    let result = perform_param_expansion("foo:0:2").unwrap();
    assert_eq!(result, "fo");
    write_vars(|v| v.vars_mut().clear());
}

#[test]
fn param_expansion_remshortestprefix() {
    write_vars(|v| {
        v.set_var("foo", "foo", false);
    });
    let result = perform_param_expansion("foo#f*").unwrap();
    assert_eq!(result, "oo");
    write_vars(|v| v.vars_mut().clear());
}

#[test]
fn param_expansion_remlongestprefix() {
    write_vars(|v| {
        v.set_var("foo", "foo", false);
    });
    let result = perform_param_expansion("foo##f*").unwrap();
    assert_eq!(result, "");
    write_vars(|v| v.vars_mut().clear());
}

#[test]
fn param_expansion_remshortestsuffix() {
    write_vars(|v| {
        v.set_var("foo", "foo", false);
    });
    let result = perform_param_expansion("foo%*o").unwrap();
    assert_eq!(result, "fo");
    write_vars(|v| v.vars_mut().clear());
}

#[test]
fn param_expansion_remlongestsuffix() {
    write_vars(|v| {
        v.set_var("foo", "foo", false);
    });
    let result = perform_param_expansion("foo%%*o").unwrap();
    assert_eq!(result, "");
    write_vars(|v| v.vars_mut().clear());
}

#[test]
fn param_expansion_replacefirstmatch() {
    write_vars(|v| {
        v.set_var("foo", "foo", false);
    });
    let result = perform_param_expansion("foo/foo/X").unwrap();
    assert_eq!(result, "X");
    write_vars(|v| v.vars_mut().clear());
}

#[test]
fn param_expansion_replaceallmatches() {
    write_vars(|v| {
        v.set_var("foo", "foo", false);
    });
    let result = perform_param_expansion("foo//o/X").unwrap();
    assert_eq!(result, "fXX");
    write_vars(|v| v.vars_mut().clear());
}

#[test]
fn param_expansion_replaceprefix() {
    write_vars(|v| {
        v.set_var("foo", "foo", false);
    });
    let result = perform_param_expansion("foo/#f/X").unwrap();
    assert_eq!(result, "Xoo");
    write_vars(|v| v.vars_mut().clear());
}

#[test]
fn param_expansion_replacesuffix() {
    write_vars(|v| {
        v.set_var("foo", "foo", false);
    });
    let result = perform_param_expansion("foo/%o/X").unwrap();
    assert_eq!(result, "foX");
    write_vars(|v| v.vars_mut().clear());
}
