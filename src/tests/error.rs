use super::*;

#[test]
fn cmd_not_found() {
  let input = "foo";
  let token = LexStream::new(Arc::new(input.into()), LexFlags::empty())
    .next()
    .unwrap()
    .unwrap();
  let err = ShErr::full(ShErrKind::CmdNotFound("foo".into()), "", token.span);

  let err_fmt = format!("{err}");
  insta::assert_snapshot!(err_fmt)
}

#[test]
fn unclosed_subsh() {
  let input = "(foo";
  let token = LexStream::new(Arc::new(input.into()), LexFlags::empty())
    .nth(1)
    .unwrap();
  let Err(err) = token else {
    panic!("{:?}", token);
  };

  let err_fmt = format!("{err}");
  insta::assert_snapshot!(err_fmt)
}

#[test]
fn unclosed_dquote() {
  let input = "\"foo bar";
  let token = LexStream::new(Arc::new(input.into()), LexFlags::empty())
    .nth(1)
    .unwrap();
  let Err(err) = token else {
    panic!();
  };

  let err_fmt = format!("{err}");
  insta::assert_snapshot!(err_fmt)
}

#[test]
fn unclosed_squote() {
  let input = "'foo bar";
  let token = LexStream::new(Arc::new(input.into()), LexFlags::empty())
    .nth(1)
    .unwrap();
  let Err(err) = token else {
    panic!();
  };

  let err_fmt = format!("{err}");
  insta::assert_snapshot!(err_fmt)
}

#[test]
fn unclosed_brc_grp() {
  let input = "{ foo bar";
  let tokens =
    LexStream::new(Arc::new(input.into()), LexFlags::empty()).collect::<ShResult<Vec<_>>>();

  let Err(err) = tokens else {
    panic!("Expected an error, got {:?}", tokens);
  };

  let err_fmt = format!("{err}");
  insta::assert_snapshot!(err_fmt)
}

#[test]
fn if_no_fi() {
  let input = "if foo; then bar;";
  let tokens = LexStream::new(Arc::new(input.into()), LexFlags::empty())
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
  let tokens = LexStream::new(Arc::new(input.into()), LexFlags::empty())
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
  let tokens = LexStream::new(Arc::new(input.into()), LexFlags::empty())
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
  let tokens = LexStream::new(Arc::new(input.into()), LexFlags::empty())
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
  let tokens = LexStream::new(Arc::new(input.into()), LexFlags::empty())
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
  let tokens = LexStream::new(Arc::new(input.into()), LexFlags::empty())
    .map(|tk| tk.unwrap())
    .collect::<Vec<_>>();

  let node = ParseStream::new(tokens).next().unwrap();
  let Err(e) = node else { panic!() };

  let err_fmt = format!("{e}");
  insta::assert_snapshot!(err_fmt)
}

#[test]
fn error_with_notes() {
  let err = ShErr::simple(ShErrKind::ExecFail, "Execution failed")
    .with_note(Note::new("Execution failed for this reason"))
    .with_note(Note::new("Here is how to fix it: blah blah blah"));

  let err_fmt = format!("{err}");
  insta::assert_snapshot!(err_fmt)
}

#[test]
fn error_with_notes_and_sub_notes() {
  let err = ShErr::simple(ShErrKind::ExecFail, "Execution failed")
    .with_note(Note::new("Execution failed for this reason"))
    .with_note(Note::new("Here is how to fix it:").with_sub_notes(vec!["blah", "blah", "blah"]));

  let err_fmt = format!("{err}");
  insta::assert_snapshot!(err_fmt)
}
