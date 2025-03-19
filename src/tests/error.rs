use libsh::error::{ShErr, ShErrKind};

use super::super::*;

#[test]
fn cmd_not_found() {
	let input = "foo";
	let token = LexStream::new(Rc::new(input.into()), LexFlags::empty()).next().unwrap().unwrap();
	let err = ShErr::full(ShErrKind::CmdNotFound("foo".into()), "", token.span);

	let err_fmt = format!("{err}");
	insta::assert_snapshot!(err_fmt)
}

#[test]
fn if_no_fi() {
	let input = "if foo; then bar;";
	let tokens = LexStream::new(Rc::new(input.into()), LexFlags::empty())
		.map(|tk| tk.unwrap())
		.collect::<Vec<_>>();

	let node = ParseStream::new(tokens).next().unwrap();
	let Err(e) = node else { panic!() };

	let err_fmt = format!("{e}");
	insta::assert_snapshot!(err_fmt)
}

#[test]
fn if_no_then() {
	let input = "if foo; bar; fi";
	let tokens = LexStream::new(Rc::new(input.into()), LexFlags::empty())
		.map(|tk| tk.unwrap())
		.collect::<Vec<_>>();

	let node = ParseStream::new(tokens).next().unwrap();
	let Err(e) = node else { panic!() };

	let err_fmt = format!("{e}");
	insta::assert_snapshot!(err_fmt)
}

#[test]
fn loop_no_done() {
	let input = "while true; do echo foo;";
	let tokens = LexStream::new(Rc::new(input.into()), LexFlags::empty())
		.map(|tk| tk.unwrap())
		.collect::<Vec<_>>();

	let node = ParseStream::new(tokens).next().unwrap();
	let Err(e) = node else { panic!() };

	let err_fmt = format!("{e}");
	insta::assert_snapshot!(err_fmt)
}

#[test]
fn loop_no_do() {
	let input = "while true; echo foo; done";
	let tokens = LexStream::new(Rc::new(input.into()), LexFlags::empty())
		.map(|tk| tk.unwrap())
		.collect::<Vec<_>>();

	let node = ParseStream::new(tokens).next().unwrap();
	let Err(e) = node else { panic!() };

	let err_fmt = format!("{e}");
	insta::assert_snapshot!(err_fmt)
}

#[test]
fn case_no_esac() {
	let input = "case foo in foo) bar;; bar) foo;;";
	let tokens = LexStream::new(Rc::new(input.into()), LexFlags::empty())
		.map(|tk| tk.unwrap())
		.collect::<Vec<_>>();

	let node = ParseStream::new(tokens).next().unwrap();
	let Err(e) = node else { panic!() };

	let err_fmt = format!("{e}");
	insta::assert_snapshot!(err_fmt)
}

#[test]
fn case_no_in() {
	let input = "case foo foo) bar;; bar) foo;; esac";
	let tokens = LexStream::new(Rc::new(input.into()), LexFlags::empty())
		.map(|tk| tk.unwrap())
		.collect::<Vec<_>>();

	let node = ParseStream::new(tokens).next().unwrap();
	let Err(e) = node else { panic!() };

	let err_fmt = format!("{e}");
	insta::assert_snapshot!(err_fmt)
}
