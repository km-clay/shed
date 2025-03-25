use super::*;

#[test]
fn parse_simple() {
	let input = "echo hello world";
	let tk_stream: Vec<_> = LexStream::new(Arc::new(input.to_string()), LexFlags::empty())
		.map(|tk| tk.unwrap())
		.collect();
	let nodes: Vec<_> = ParseStream::new(tk_stream).collect();

	insta::assert_debug_snapshot!(nodes)
}

#[test]
fn parse_pipeline() {
	let input = "echo foo | sed s/foo/bar";
	let tk_stream: Vec<_> = LexStream::new(Arc::new(input.to_string()), LexFlags::empty())
		.map(|tk| tk.unwrap())
		.collect();
	let nodes: Vec<_> = ParseStream::new(tk_stream).collect();

	insta::assert_debug_snapshot!(nodes)
}

#[test]
fn parse_conjunction() {
	let input = "echo foo && echo bar";
	let tk_stream: Vec<_> = LexStream::new(Arc::new(input.to_string()), LexFlags::empty())
		.map(|tk| tk.unwrap())
		.collect();
	let nodes: Vec<_> = ParseStream::new(tk_stream).collect();

	insta::assert_debug_snapshot!(nodes)
}

#[test]
fn parse_conjunction_and_pipeline() {
	let input = "echo foo | sed s/foo/bar/ && echo bar | sed s/bar/foo/ || echo foo bar | sed s/foo bar/bar foo/";
	let tk_stream: Vec<_> = LexStream::new(Arc::new(input.to_string()), LexFlags::empty())
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
	let tk_stream: Vec<_> = LexStream::new(Arc::new(input.to_string()), LexFlags::empty())
		.map(|tk| tk.unwrap())
		.collect();
	let nodes: Vec<_> = ParseStream::new(tk_stream).collect();

	insta::assert_debug_snapshot!(nodes)
}

#[test]
fn parse_if_simple() {
	let input = "if foo; then echo bar; fi";
	let tk_stream: Vec<_> = LexStream::new(Arc::new(input.to_string()), LexFlags::empty())
		.map(|tk| tk.unwrap())
		.collect();
	let nodes: Vec<_> = ParseStream::new(tk_stream).collect();

	insta::assert_debug_snapshot!(nodes)
}
#[test]
fn parse_if_with_elif() {
	let input = "if foo; then echo bar; elif bar; then echo foo; fi";
	let tk_stream: Vec<_> = LexStream::new(Arc::new(input.to_string()), LexFlags::empty())
		.map(|tk| tk.unwrap())
		.collect();
	let nodes: Vec<_> = ParseStream::new(tk_stream).collect();

	insta::assert_debug_snapshot!(nodes)
}
#[test]
fn parse_if_multiple_elif() {
	let input = "if foo; then echo bar; elif bar; then echo foo; elif biz; then echo baz; fi";
	let tk_stream: Vec<_> = LexStream::new(Arc::new(input.to_string()), LexFlags::empty())
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
	let tk_stream: Vec<_> = LexStream::new(Arc::new(input.to_string()), LexFlags::empty())
		.map(|tk| tk.unwrap())
		.collect();
	let nodes: Vec<_> = ParseStream::new(tk_stream).collect();

	insta::assert_debug_snapshot!(nodes)
}
#[test]
fn parse_loop_simple() {
	let input = "while foo; do bar; done";
	let tk_stream: Vec<_> = LexStream::new(Arc::new(input.to_string()), LexFlags::empty())
		.map(|tk| tk.unwrap())
		.collect();
	let nodes: Vec<_> = ParseStream::new(tk_stream).collect();

	insta::assert_debug_snapshot!(nodes)
}
#[test]
fn parse_loop_until() {
	let input = "until foo; do bar; done";
	let tk_stream: Vec<_> = LexStream::new(Arc::new(input.to_string()), LexFlags::empty())
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
	let tk_stream: Vec<_> = LexStream::new(Arc::new(input.to_string()), LexFlags::empty())
		.map(|tk| tk.unwrap())
		.collect();
	let nodes: Vec<_> = ParseStream::new(tk_stream).collect();

	insta::assert_debug_snapshot!(nodes)
}
#[test]
fn parse_case_simple() {
	let input = "case foo in foo) bar;; bar) foo;; biz) baz;; esac";
	let tk_stream: Vec<_> = LexStream::new(Arc::new(input.to_string()), LexFlags::empty())
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
	let tk_stream: Vec<_> = LexStream::new(Arc::new(input.to_string()), LexFlags::empty())
		.map(|tk| tk.unwrap())
		.collect();
	let nodes: Vec<_> = ParseStream::new(tk_stream).collect();

	insta::assert_debug_snapshot!(nodes)
}
#[test]
fn parse_case_nested() {
	let input = "case foo in
	foo)
		if true; then
			while true; do
				echo foo
			done
		fi
	;;
	bar)
		if false; then
			until false; do
				case foo in
					foo)
						if true; then
							echo foo
						fi
					;;
					bar)
						if false; then
							echo foo
						fi
					;;
				esac
			done
		fi
	;;
esac";
	let tk_stream: Vec<_> = LexStream::new(Arc::new(input.to_string()), LexFlags::empty())
		.map(|tk| tk.unwrap())
		.collect();
	let nodes: Vec<_> = ParseStream::new(tk_stream).collect();

	insta::assert_debug_snapshot!(nodes)
}
#[test]
fn parse_cursed() {
	let input = "if if if if case foo in foo) if true; then true; fi;; esac; then case foo in foo) until true; do true; done;; esac; fi; then until if case foo in foo) true;; esac; then if true; then true; fi; fi; do until until true; do true; done; do case foo in foo) true;; esac; done; done; fi; then until until case foo in foo) true;; esac; do if true; then true; fi; done; do until true; do true; done; done; fi; then until case foo in foo) case foo in foo) true;; esac;; esac; do if if true; then true; fi; then until true; do true; done; fi; done; elif until until case foo in foo) true;; esac; do if true; then true; fi; done; do case foo in foo) until true; do true; done;; esac; done; then case foo in foo) if case foo in foo) true;; esac; then if true; then true; fi; fi;; esac; else case foo in foo) until until true; do true; done; do case foo in foo) true;; esac; done;; esac; fi";

	let tk_stream: Vec<_> = LexStream::new(Arc::new(input.to_string()), LexFlags::empty())
		.map(|tk| tk.unwrap())
		.collect();
	let nodes: Vec<_> = ParseStream::new(tk_stream).collect();

	// 15,000 line snapshot file btw
	insta::assert_debug_snapshot!(nodes)
}
#[test]
fn test_node_operation() {
	let input = String::from("echo hello world; echo foo bar");
	let mut check_nodes = vec![];
	let mut tokens: Vec<Tk> = LexStream::new(input.into(), LexFlags::empty())
		.map(|tk| tk.unwrap())
		.collect();

	let nodes = ParseStream::new(tokens)
		.map(|nd| nd.unwrap());

	for mut node in nodes {
		node_operation(&mut node,
			&|node: &Node| matches!(node.class, NdRule::Command {..}),
			&mut |node: &mut Node| check_nodes.push(node.clone()),
		);
	}
	insta::assert_debug_snapshot!(check_nodes)
}
