use pretty_assertions::assert_eq;

use super::{NdRule, node::NdKind};
use crate::tests::testutil::get_ast;

#[test]
fn parse_hello_world() {
  let input = "echo hello world";
  let expected = &mut [
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
  ]
  .into_iter();
  let ast = get_ast(input).unwrap();
  if let Err(e) = ast.assert_structure(expected) {
    panic!("{}", e);
  }
}

#[test]
fn parse_if_statement() {
  let input = "if echo foo; then echo bar; fi";
  let expected = &mut [
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::IfNode,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
  ]
  .into_iter();
  let ast = get_ast(input).unwrap();
  if let Err(e) = ast.assert_structure(expected) {
    panic!("{}", e);
  }
}

#[test]
fn parse_pipeline() {
  let input = "ls | grep foo | wc -l";
  let expected = &mut [
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
    NdKind::Command,
    NdKind::Command,
  ]
  .into_iter();
  let ast = get_ast(input).unwrap();
  if let Err(e) = ast.assert_structure(expected) {
    panic!("{}", e);
  }
}

#[test]
fn parse_conjunction_and() {
  let input = "echo foo && echo bar";
  let expected = &mut [
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
    NdKind::Pipeline,
    NdKind::Command,
  ]
  .into_iter();
  let ast = get_ast(input).unwrap();
  if let Err(e) = ast.assert_structure(expected) {
    panic!("{}", e);
  }
}

#[test]
fn parse_while_loop() {
  let input = "while true; do echo hello; done";
  let expected = &mut [
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::LoopNode,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
  ]
  .into_iter();
  let ast = get_ast(input).unwrap();
  if let Err(e) = ast.assert_structure(expected) {
    panic!("{}", e);
  }
}

#[test]
fn parse_for_loop() {
  let input = "for i in a b c; do echo $i; done";
  let expected = &mut [
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::ForNode,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
  ]
  .into_iter();
  let ast = get_ast(input).unwrap();
  if let Err(e) = ast.assert_structure(expected) {
    panic!("{}", e);
  }
}

#[test]
fn parse_case_statement() {
  let input = "case foo in bar) echo bar;; baz) echo baz;; esac";
  let expected = &mut [
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::CaseNode,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
  ]
  .into_iter();
  let ast = get_ast(input).unwrap();
  if let Err(e) = ast.assert_structure(expected) {
    panic!("{}", e);
  }
}

#[test]
fn parse_func_def() {
  let input = "foo() { echo hello; }";
  let expected = &mut [
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::FuncDef,
    NdKind::BraceGrp,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
  ]
  .into_iter();
  let ast = get_ast(input).unwrap();
  if let Err(e) = ast.assert_structure(expected) {
    panic!("{}", e);
  }
}

#[test]
fn parse_assignment() {
  let input = "FOO=bar";
  let expected = &mut [
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
    NdKind::Assignment,
  ]
  .into_iter();
  let ast = get_ast(input).unwrap();
  if let Err(e) = ast.assert_structure(expected) {
    panic!("{}", e);
  }
}

#[test]
fn parse_assignment_with_command() {
  let input = "FOO=bar echo hello";
  let expected = &mut [
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
    NdKind::Assignment,
  ]
  .into_iter();
  let ast = get_ast(input).unwrap();
  if let Err(e) = ast.assert_structure(expected) {
    panic!("{}", e);
  }
}

#[test]
fn parse_if_elif_else() {
  let input = "if true; then echo a; elif false; then echo b; else echo c; fi";
  let expected = &mut [
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::IfNode,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
  ]
  .into_iter();
  let ast = get_ast(input).unwrap();
  if let Err(e) = ast.assert_structure(expected) {
    panic!("{}", e);
  }
}

#[test]
fn parse_brace_group() {
  let input = "{ echo hello; echo world; }";
  let expected = &mut [
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::BraceGrp,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
  ]
  .into_iter();
  let ast = get_ast(input).unwrap();
  if let Err(e) = ast.assert_structure(expected) {
    panic!("{}", e);
  }
}

#[test]
fn parse_nested_if_in_while() {
  let input = "while true; do if false; then echo no; fi; done";
  let expected = &mut [
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::LoopNode,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::IfNode,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
  ]
  .into_iter();
  let ast = get_ast(input).unwrap();
  if let Err(e) = ast.assert_structure(expected) {
    panic!("{}", e);
  }
}

#[test]
fn parse_test_bracket() {
  let input = "[[ -n hello ]]";
  let expected = &mut [
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
  ]
  .into_iter();
  let ast = get_ast(input).unwrap();
  if let Err(e) = ast.assert_structure(expected) {
    panic!("{}", e);
  }
}

#[test]
fn parse_nested_func_with_if_and_loop() {
  let input = "setup() {
			for f in a b c; do
				if [[ -n $f ]]; then
					echo $f
				fi
			done
		}";
  let expected = &mut [
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::FuncDef,
    NdKind::BraceGrp,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::ForNode,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::IfNode,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
  ]
  .into_iter();
  let ast = get_ast(input).unwrap();
  if let Err(e) = ast.assert_structure(expected) {
    panic!("{}", e);
  }
}

#[test]
fn parse_pipeline_with_brace_groups() {
  let input = "{ echo foo; echo bar; } | { grep foo; wc -l; }";
  let expected = &mut [
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::BraceGrp,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
    NdKind::BraceGrp,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
  ]
  .into_iter();
  let ast = get_ast(input).unwrap();
  if let Err(e) = ast.assert_structure(expected) {
    panic!("{}", e);
  }
}

#[test]
fn parse_deeply_nested_if() {
  let input = "if true; then
			if false; then
				if true; then
					echo deep
				fi
			fi
		fi";
  let expected = &mut [
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::IfNode,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::IfNode,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::IfNode,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
  ]
  .into_iter();
  let ast = get_ast(input).unwrap();
  if let Err(e) = ast.assert_structure(expected) {
    panic!("{}", e);
  }
}

#[test]
fn parse_case_with_multiple_commands() {
  let input = "case $1 in
			start)
				echo starting
				run_server
			;;
			stop)
				echo stopping
				kill_server
			;;
			*)
				echo unknown
			;;
		esac";
  let expected = &mut [
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::CaseNode,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
  ]
  .into_iter();
  let ast = get_ast(input).unwrap();
  if let Err(e) = ast.assert_structure(expected) {
    panic!("{}", e);
  }
}

#[test]
fn parse_func_with_case_and_conjunction() {
  let input = "dispatch() {
			case $1 in
				build)
					make clean && make all
				;;
				test)
					make test || echo failed
				;;
			esac
		}";
  let expected = &mut [
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::FuncDef,
    NdKind::BraceGrp,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::CaseNode,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
    NdKind::Pipeline,
    NdKind::Command,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
    NdKind::Pipeline,
    NdKind::Command,
  ]
  .into_iter();
  let ast = get_ast(input).unwrap();
  if let Err(e) = ast.assert_structure(expected) {
    panic!("{}", e);
  }
}

#[test]
fn parse_while_with_pipeline_and_assignment() {
  let input = "while read line; do
			FOO=bar echo $line | grep pattern | wc -l
		done";
  let expected = &mut [
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::LoopNode,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
    NdKind::Assignment,
    NdKind::Command,
    NdKind::Command,
  ]
  .into_iter();
  let ast = get_ast(input).unwrap();
  if let Err(e) = ast.assert_structure(expected) {
    panic!("{}", e);
  }
}

#[test]
fn parse_nested_loops() {
  let input = "for i in 1 2 3; do
			for j in a b c; do
				while true; do
					echo $i $j
				done
			done
		done";
  let expected = &mut [
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::ForNode,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::ForNode,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::LoopNode,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
  ]
  .into_iter();
  let ast = get_ast(input).unwrap();
  if let Err(e) = ast.assert_structure(expected) {
    panic!("{}", e);
  }
}

#[test]
fn parse_complex_conjunction_chain() {
  let input = "mkdir -p dir && cd dir && touch file || echo failed && echo done";
  let expected = &mut [
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
    NdKind::Pipeline,
    NdKind::Command,
    NdKind::Pipeline,
    NdKind::Command,
    NdKind::Pipeline,
    NdKind::Command,
    NdKind::Pipeline,
    NdKind::Command,
  ]
  .into_iter();
  let ast = get_ast(input).unwrap();
  if let Err(e) = ast.assert_structure(expected) {
    panic!("{}", e);
  }
}

#[test]
fn parse_func_defining_inner_func() {
  let input = "outer() {
			inner() {
				echo hello from inner
			}
			inner
		}";
  let expected = &mut [
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::FuncDef,
    NdKind::BraceGrp,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::FuncDef,
    NdKind::BraceGrp,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
  ]
  .into_iter();
  let ast = get_ast(input).unwrap();
  if let Err(e) = ast.assert_structure(expected) {
    panic!("{}", e);
  }
}

#[test]
fn parse_multiline_if_elif_with_pipelines() {
  let input = "if cat /etc/passwd | grep root; then
			echo found root
		elif ls /tmp | wc -l; then
			echo tmp has files
		else
			echo fallback | tee log.txt
		fi";
  let expected = &mut [
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::IfNode,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
    NdKind::Command,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
    NdKind::Command,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
    NdKind::Command,
  ]
  .into_iter();
  let ast = get_ast(input).unwrap();
  if let Err(e) = ast.assert_structure(expected) {
    panic!("{}", e);
  }
}

#[test]
fn parse_cursed_input() {
  // valid shell syntax btw
  // your editor might not enjoy this
  let input = "if if while if if until :; do :; done; then case foo in; foo) :;; bar) :;; biz) :;; esac; elif case foo in; foo) :;; bar) :;; biz) :;; esac; then if :; then :; elif :; then :; elif :; then :; else :; fi; elif if :; then :; elif :; then :; elif :; then :; else :; fi; then while :; do :; done; else while :; do :; done; fi; then if until :; do :; done; then until :; do :; done; elif case foo in; foo) :;; bar) :;; biz) :;; esac; then case foo in; foo) :;; bar) :;; biz) :;; esac; elif if :; then :; elif :; then :; elif :; then :; else :; fi; then if :; then :; elif :; then :; elif :; then :; else :; fi; else while :; do :; done; fi; elif while while :; do :; done; do until :; do :; done; done; then while until :; do :; done; do case foo in; foo) :;; bar) :;; biz) :;; esac; done; elif until case foo in; foo) :;; bar) :;; biz) :;; esac; do if :; then :; elif :; then :; elif :; then :; else :; fi; done; then until if :; then :; elif :; then :; elif :; then :; else :; fi; do while :; do :; done; done; else case foo in; foo) while :; do :; done;; bar) until :; do :; done;; biz) until :; do :; done;; esac; fi; do while case foo in; foo) case foo in; foo) :;; bar) :;; biz) :;; esac;; bar) case foo in; foo) :;; bar) :;; biz) :;; esac;; biz) :;; esac; do if :; then :; elif :; then :; elif :; then :; else :; fi; done; done; then until while if :; then :; elif :; then :; elif :; then :; else :; fi; do while :; do :; done; done; do until while :; do :; done; do until :; do :; done; done; done; elif until until until :; do :; done; do case foo in; foo) :;; bar) :;; biz) :;; esac; done; do case foo in; foo) case foo in; foo) :;; bar) :;; biz) :;; esac;; bar) if :; then :; elif :; then :; elif :; then :; else :; fi;; biz) if :; then :; elif :; then :; elif :; then :; else :; fi;; esac; done; then case foo in; foo) case foo in; foo) while :; do :; done;; bar) while :; do :; done;; biz) until :; do :; done;; esac;; bar) if until :; do :; done; then case foo in; foo) :;; bar) :;; biz) :;; esac; elif case foo in; foo) :;; bar) :;; biz) :;; esac; then if :; then :; elif :; then :; elif :; then :; else :; fi; elif if :; then :; elif :; then :; elif :; then :; else :; fi; then while :; do :; done; else while :; do :; done; fi;; biz) if until :; do :; done; then until :; do :; done; elif case foo in; foo) :;; bar) :;; biz) :;; esac; then case foo in; foo) :;; bar) :;; biz) :;; esac; elif if :; then :; elif :; then :; elif :; then :; else :; fi; then if :; then :; elif :; then :; elif :; then :; else :; fi; else while :; do :; done; fi;; esac; elif case foo in; foo) while while :; do :; done; do until :; do :; done; done;; bar) while until :; do :; done; do case foo in; foo) :;; bar) :;; biz) :;; esac; done;; biz) until case foo in; foo) :;; bar) :;; biz) :;; esac; do if :; then :; elif :; then :; elif :; then :; else :; fi; done;; esac; then if until if :; then :; elif :; then :; elif :; then :; else :; fi; do while :; do :; done; done; then case foo in; foo) while :; do :; done;; bar) until :; do :; done;; biz) until :; do :; done;; esac; elif case foo in; foo) case foo in; foo) :;; bar) :;; biz) :;; esac;; bar) case foo in; foo) :;; bar) :;; biz) :;; esac;; biz) if :; then :; elif :; then :; elif :; then :; else :; fi;; esac; then if if :; then :; elif :; then :; elif :; then :; else :; fi; then while :; do :; done; elif while :; do :; done; then until :; do :; done; elif until :; do :; done; then case foo in; foo) :;; bar) :;; biz) :;; esac; else case foo in; foo) :;; bar) :;; biz) :;; esac; fi; elif if if :; then :; elif :; then :; elif :; then :; else :; fi; then if :; then :; elif :; then :; elif :; then :; else :; fi; elif while :; do :; done; then while :; do :; done; elif until :; do :; done; then until :; do :; done; else case foo in; foo) :;; bar) :;; biz) :;; esac; fi; then while case foo in; foo) :;; bar) :;; biz) :;; esac; do if :; then :; elif :; then :; elif :; then :; else :; fi; done; else while if :; then :; elif :; then :; elif :; then :; else :; fi; do while :; do :; done; done; fi; else if until while :; do :; done; do until :; do :; done; done; then until until :; do :; done; do case foo in; foo) :;; bar) :;; biz) :;; esac; done; elif case foo in; foo) case foo in; foo) :;; bar) :;; biz) :;; esac;; bar) if :; then :; elif :; then :; elif :; then :; else :; fi;; biz) if :; then :; elif :; then :; elif :; then :; else :; fi;; esac; then case foo in; foo) while :; do :; done;; bar) while :; do :; done;; biz) until :; do :; done;; esac; elif if until :; do :; done; then case foo in; foo) :;; bar) :;; biz) :;; esac; elif case foo in; foo) :;; bar) :;; biz) :;; esac; then if :; then :; elif :; then :; elif :; then :; else :; fi; elif if :; then :; elif :; then :; elif :; then :; else :; fi; then while :; do :; done; else while :; do :; done; fi; then if until :; do :; done; then until :; do :; done; elif case foo in; foo) :;; bar) :;; biz) :;; esac; then case foo in; foo) :;; bar) :;; biz) :;; esac; elif if :; then :; elif :; then :; elif :; then :; else :; fi; then if :; then :; elif :; then :; elif :; then :; else :; fi; else while :; do :; done; fi; else while while :; do :; done; do until :; do :; done; done; fi; fi; then while while while until :; do :; done; do case foo in; foo) :;; bar) :;; biz) :;; esac; done; do until case foo in; foo) :;; bar) :;; biz) :;; esac; do if :; then :; elif :; then :; elif :; then :; else :; fi; done; done; do while until if :; then :; elif :; then :; elif :; then :; else :; fi; do while :; do :; done; done; do case foo in; foo) while :; do :; done;; bar) until :; do :; done;; biz) until :; do :; done;; esac; done; done; elif until until case foo in; foo) case foo in; foo) :;; bar) :;; biz) :;; esac;; bar) case foo in; foo) :;; bar) :;; biz) :;; esac;; biz) if :; then :; elif :; then :; elif :; then :; else :; fi;; esac; do if if :; then :; elif :; then :; elif :; then :; else :; fi; then while :; do :; done; elif while :; do :; done; then until :; do :; done; elif until :; do :; done; then case foo in; foo) :;; bar) :;; biz) :;; esac; else case foo in; foo) :;; bar) :;; biz) :;; esac; fi; done; do until if if :; then :; elif :; then :; elif :; then :; else :; fi; then if :; then :; elif :; then :; elif :; then :; else :; fi; elif while :; do :; done; then while :; do :; done; elif until :; do :; done; then until :; do :; done; else case foo in; foo) :;; bar) :;; biz) :;; esac; fi; do while case foo in; foo) :;; bar) :;; biz) :;; esac; do if :; then :; elif :; then :; elif :; then :; else :; fi; done; done; done; then case foo in; foo) case foo in; foo) while if :; then :; elif :; then :; elif :; then :; else :; fi; do while :; do :; done; done;; bar) until while :; do :; done; do until :; do :; done; done;; biz) until until :; do :; done; do case foo in; foo) :;; bar) :;; biz) :;; esac; done;; esac;; bar) case foo in; foo) case foo in; foo) case foo in; foo) :;; bar) :;; biz) :;; esac;; bar) if :; then :; elif :; then :; elif :; then :; else :; fi;; biz) if :; then :; elif :; then :; elif :; then :; else :; fi;; esac;; bar) case foo in; foo) while :; do :; done;; bar) while :; do :; done;; biz) until :; do :; done;; esac;; biz) if until :; do :; done; then case foo in; foo) :;; bar) :;; biz) :;; esac; elif case foo in; foo) :;; bar) :;; biz) :;; esac; then if :; then :; elif :; then :; elif :; then :; else :; fi; elif if :; then :; elif :; then :; elif :; then :; else :; fi; then while :; do :; done; else while :; do :; done; fi;; esac;; biz) if if until :; do :; done; then until :; do :; done; elif case foo in; foo) :;; bar) :;; biz) :;; esac; then case foo in; foo) :;; bar) :;; biz) :;; esac; elif if :; then :; elif :; then :; elif :; then :; else :; fi; then if :; then :; elif :; then :; elif :; then :; else :; fi; else while :; do :; done; fi; then while while :; do :; done; do until :; do :; done; done; elif while until :; do :; done; do case foo in; foo) :;; bar) :;; biz) :;; esac; done; then until case foo in; foo) :;; bar) :;; biz) :;; esac; do if :; then :; elif :; then :; elif :; then :; else :; fi; done; elif until if :; then :; elif :; then :; elif :; then :; else :; fi; do while :; do :; done; done; then case foo in; foo) while :; do :; done;; bar) until :; do :; done;; biz) until :; do :; done;; esac; else case foo in; foo) case foo in; foo) :;; bar) :;; biz) :;; esac;; bar) case foo in; foo) :;; bar) :;; biz) :;; esac;; biz) if :; then :; elif :; then :; elif :; then :; else :; fi;; esac; fi;; esac; elif if if if if :; then :; elif :; then :; elif :; then :; else :; fi; then while :; do :; done; elif while :; do :; done; then until :; do :; done; elif until :; do :; done; then case foo in; foo) :;; bar) :;; biz) :;; esac; else case foo in; foo) :;; bar) :;; biz) :;; esac; fi; then if if :; then :; elif :; then :; elif :; then :; else :; fi; then if :; then :; elif :; then :; elif :; then :; else :; fi; elif while :; do :; done; then while :; do :; done; elif until :; do :; done; then until :; do :; done; else case foo in; foo) :;; bar) :;; biz) :;; esac; fi; elif while case foo in; foo) :;; bar) :;; biz) :;; esac; do if :; then :; elif :; then :; elif :; then :; else :; fi; done; then while if :; then :; elif :; then :; elif :; then :; else :; fi; do while :; do :; done; done; elif until while :; do :; done; do until :; do :; done; done; then until until :; do :; done; do case foo in; foo) :;; bar) :;; biz) :;; esac; done; else case foo in; foo) case foo in; foo) :;; bar) :;; biz) :;; esac;; bar) if :; then :; elif :; then :; elif :; then :; else :; fi;; biz) if :; then :; elif :; then :; elif :; then :; else :; fi;; esac; fi; then while case foo in; foo) while :; do :; done;; bar) while :; do :; done;; biz) until :; do :; done;; esac; do if until :; do :; done; then case foo in; foo) :;; bar) :;; biz) :;; esac; elif case foo in; foo) :;; bar) :;; biz) :;; esac; then if :; then :; elif :; then :; elif :; then :; else :; fi; elif if :; then :; elif :; then :; elif :; then :; else :; fi; then while :; do :; done; else while :; do :; done; fi; done; elif while if until :; do :; done; then until :; do :; done; elif case foo in; foo) :;; bar) :;; biz) :;; esac; then case foo in; foo) :;; bar) :;; biz) :;; esac; elif if :; then :; elif :; then :; elif :; then :; else :; fi; then if :; then :; elif :; then :; elif :; then :; else :; fi; else while :; do :; done; fi; do while while :; do :; done; do until :; do :; done; done; done; then until while until :; do :; done; do case foo in; foo) :;; bar) :;; biz) :;; esac; done; do until case foo in; foo) :;; bar) :;; biz) :;; esac; do if :; then :; elif :; then :; elif :; then :; else :; fi; done; done; elif until until if :; then :; elif :; then :; elif :; then :; else :; fi; do while :; do :; done; done; do case foo in; foo) while :; do :; done;; bar) until :; do :; done;; biz) until :; do :; done;; esac; done; then case foo in; foo) case foo in; foo) case foo in; foo) :;; bar) :;; biz) :;; esac;; bar) case foo in; foo) :;; bar) :;; biz) :;; esac;; biz) if :; then :; elif :; then :; elif :; then :; else :; fi;; esac;; bar) if if :; then :; elif :; then :; elif :; then :; else :; fi; then while :; do :; done; elif while :; do :; done; then until :; do :; done; elif until :; do :; done; then case foo in; foo) :;; bar) :;; biz) :;; esac; else case foo in; foo) :;; bar) :;; biz) :;; esac; fi;; biz) if if :; then :; elif :; then :; elif :; then :; else :; fi; then if :; then :; elif :; then :; elif :; then :; else :; fi; elif while :; do :; done; then while :; do :; done; elif until :; do :; done; then until :; do :; done; else case foo in; foo) :;; bar) :;; biz) :;; esac; fi;; esac; else case foo in; foo) while case foo in; foo) :;; bar) :;; biz) :;; esac; do if :; then :; elif :; then :; elif :; then :; else :; fi; done;; bar) while if :; then :; elif :; then :; elif :; then :; else :; fi; do while :; do :; done; done;; biz) until while :; do :; done; do until :; do :; done; done;; esac; fi; then if if until until :; do :; done; do case foo in; foo) :;; bar) :;; biz) :;; esac; done; then case foo in; foo) case foo in; foo) :;; bar) :;; biz) :;; esac;; bar) if :; then :; elif :; then :; elif :; then :; else :; fi;; biz) if :; then :; elif :; then :; elif :; then :; else :; fi;; esac; elif case foo in; foo) while :; do :; done;; bar) while :; do :; done;; biz) until :; do :; done;; esac; then if until :; do :; done; then case foo in; foo) :;; bar) :;; biz) :;; esac; elif case foo in; foo) :;; bar) :;; biz) :;; esac; then if :; then :; elif :; then :; elif :; then :; else :; fi; elif if :; then :; elif :; then :; elif :; then :; else :; fi; then while :; do :; done; else while :; do :; done; fi; elif if until :; do :; done; then until :; do :; done; elif case foo in; foo) :;; bar) :;; biz) :;; esac; then case foo in; foo) :;; bar) :;; biz) :;; esac; elif if :; then :; elif :; then :; elif :; then :; else :; fi; then if :; then :; elif :; then :; elif :; then :; else :; fi; else while :; do :; done; fi; then while while :; do :; done; do until :; do :; done; done; else while until :; do :; done; do case foo in; foo) :;; bar) :;; biz) :;; esac; done; fi; then if until case foo in; foo) :;; bar) :;; biz) :;; esac; do if :; then :; elif :; then :; elif :; then :; else :; fi; done; then until if :; then :; elif :; then :; elif :; then :; else :; fi; do while :; do :; done; done; elif case foo in; foo) while :; do :; done;; bar) until :; do :; done;; biz) until :; do :; done;; esac; then case foo in; foo) case foo in; foo) :;; bar) :;; biz) :;; esac;; bar) case foo in; foo) :;; bar) :;; biz) :;; esac;; biz) if :; then :; elif :; then :; elif :; then :; else :; fi;; esac; elif if if :; then :; elif :; then :; elif :; then :; else :; fi; then while :; do :; done; elif while :; do :; done; then until :; do :; done; elif until :; do :; done; then case foo in; foo) :;; bar) :;; biz) :;; esac; else case foo in; foo) :;; bar) :;; biz) :;; esac; fi; then if if :; then :; elif :; then :; elif :; then :; else :; fi; then if :; then :; elif :; then :; elif :; then :; else :; fi; elif while :; do :; done; then while :; do :; done; elif until :; do :; done; then until :; do :; done; else case foo in; foo) :;; bar) :;; biz) :;; esac; fi; else while case foo in; foo) :;; bar) :;; biz) :;; esac; do if :; then :; elif :; then :; elif :; then :; else :; fi; done; fi; elif while while if :; then :; elif :; then :; elif :; then :; else :; fi; do while :; do :; done; done; do until while :; do :; done; do until :; do :; done; done; done; then while until until :; do :; done; do case foo in; foo) :;; bar) :;; biz) :;; esac; done; do case foo in; foo) case foo in; foo) :;; bar) :;; biz) :;; esac;; bar) if :; then :; elif :; then :; elif :; then :; else :; fi;; biz) if :; then :; elif :; then :; elif :; then :; else :; fi;; esac; done; elif until case foo in; foo) while :; do :; done;; bar) while :; do :; done;; biz) until :; do :; done;; esac; do if until :; do :; done; then case foo in; foo) :;; bar) :;; biz) :;; esac; elif case foo in; foo) :;; bar) :;; biz) :;; esac; then if :; then :; elif :; then :; elif :; then :; else :; fi; elif if :; then :; elif :; then :; elif :; then :; else :; fi; then while :; do :; done; else while :; do :; done; fi; done; then until if until :; do :; done; then until :; do :; done; elif case foo in; foo) :;; bar) :;; biz) :;; esac; then case foo in; foo) :;; bar) :;; biz) :;; esac; elif if :; then :; elif :; then :; elif :; then :; else :; fi; then if :; then :; elif :; then :; elif :; then :; else :; fi; else while :; do :; done; fi; do while while :; do :; done; do until :; do :; done; done; done; else case foo in; foo) while until :; do :; done; do case foo in; foo) :;; bar) :;; biz) :;; esac; done;; bar) until case foo in; foo) :;; bar) :;; biz) :;; esac; do if :; then :; elif :; then :; elif :; then :; else :; fi; done;; biz) until if :; then :; elif :; then :; elif :; then :; else :; fi; do while :; do :; done; done;; esac; fi; else while case foo in; foo) case foo in; foo) while :; do :; done;; bar) until :; do :; done;; biz) until :; do :; done;; esac;; bar) case foo in; foo) case foo in; foo) :;; bar) :;; biz) :;; esac;; bar) case foo in; foo) :;; bar) :;; biz) :;; esac;; biz) if :; then :; elif :; then :; elif :; then :; else :; fi;; esac;; biz) if if :; then :; elif :; then :; elif :; then :; else :; fi; then while :; do :; done; elif while :; do :; done; then until :; do :; done; elif until :; do :; done; then case foo in; foo) :;; bar) :;; biz) :;; esac; else case foo in; foo) :;; bar) :;; biz) :;; esac; fi;; esac; do if if if :; then :; elif :; then :; elif :; then :; else :; fi; then if :; then :; elif :; then :; elif :; then :; else :; fi; elif while :; do :; done; then while :; do :; done; elif until :; do :; done; then until :; do :; done; else case foo in; foo) :;; bar) :;; biz) :;; esac; fi; then while case foo in; foo) :;; bar) :;; biz) :;; esac; do if :; then :; elif :; then :; elif :; then :; else :; fi; done; elif while if :; then :; elif :; then :; elif :; then :; else :; fi; do while :; do :; done; done; then until while :; do :; done; do until :; do :; done; done; elif until until :; do :; done; do case foo in; foo) :;; bar) :;; biz) :;; esac; done; then case foo in; foo) case foo in; foo) :;; bar) :;; biz) :;; esac;; bar) if :; then :; elif :; then :; elif :; then :; else :; fi;; biz) if :; then :; elif :; then :; elif :; then :; else :; fi;; esac; else case foo in; foo) while :; do :; done;; bar) while :; do :; done;; biz) until :; do :; done;; esac; fi; done; fi";
  assert!(get_ast(input).is_ok()); // lets spare our sanity and just say that "ok" means "it parsed correctly"
}
#[test]
fn parse_stray_keyword_in_brace_group() {
  let input = "{ echo bar case foo in bar) echo fizz ;; buzz) echo buzz ;; esac }";
  assert!(get_ast(input).is_err());
}

// ===================== Heredocs =====================

#[test]
fn parse_basic_heredoc() {
  let input = "cat <<EOF\nhello world\nEOF";
  let expected = &mut [
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
  ]
  .into_iter();
  let ast = get_ast(input).unwrap();
  if let Err(e) = ast.assert_structure(expected) {
    panic!("{}", e);
  }
}

#[test]
fn parse_heredoc_with_tab_strip() {
  let input = "cat <<-EOF\n\t\thello\n\t\tworld\nEOF";
  let expected = &mut [
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
  ]
  .into_iter();
  let ast = get_ast(input).unwrap();
  if let Err(e) = ast.assert_structure(expected) {
    panic!("{}", e);
  }
}

#[test]
fn parse_literal_heredoc() {
  let input = "cat <<'EOF'\nhello $world\nEOF";
  let expected = &mut [
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
  ]
  .into_iter();
  let ast = get_ast(input).unwrap();
  if let Err(e) = ast.assert_structure(expected) {
    panic!("{}", e);
  }
}

#[test]
fn parse_herestring() {
  let input = "cat <<< \"hello world\"";
  let expected = &mut [
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
  ]
  .into_iter();
  let ast = get_ast(input).unwrap();
  if let Err(e) = ast.assert_structure(expected) {
    panic!("{}", e);
  }
}

#[test]
fn parse_heredoc_in_pipeline() {
  let input = "cat <<EOF | grep hello\nhello world\ngoodbye world\nEOF";
  let expected = &mut [
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
    NdKind::Command,
  ]
  .into_iter();
  let ast = get_ast(input).unwrap();
  if let Err(e) = ast.assert_structure(expected) {
    panic!("{}", e);
  }
}

#[test]
fn parse_heredoc_in_conjunction() {
  let input = "cat <<EOF && echo done\nhello\nEOF";
  let expected = &mut [
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
    NdKind::Pipeline,
    NdKind::Command,
  ]
  .into_iter();
  let ast = get_ast(input).unwrap();
  if let Err(e) = ast.assert_structure(expected) {
    panic!("{}", e);
  }
}

#[test]
fn parse_heredoc_double_quoted_delimiter() {
  let input = "cat <<\"EOF\"\nhello $world\nEOF";
  let expected = &mut [
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
  ]
  .into_iter();
  let ast = get_ast(input).unwrap();
  if let Err(e) = ast.assert_structure(expected) {
    panic!("{}", e);
  }
}

#[test]
fn parse_heredoc_empty_body() {
  let input = "cat <<EOF\nEOF";
  let expected = &mut [
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
  ]
  .into_iter();
  let ast = get_ast(input).unwrap();
  if let Err(e) = ast.assert_structure(expected) {
    panic!("{}", e);
  }
}

#[test]
fn parse_heredoc_multiword_delimiter() {
  // delimiter should only be the first word
  let input = "cat <<DELIM\nsome content\nDELIM";
  let expected = &mut [
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
  ]
  .into_iter();
  let ast = get_ast(input).unwrap();
  if let Err(e) = ast.assert_structure(expected) {
    panic!("{}", e);
  }
}

#[test]
fn parse_two_heredocs_on_one_line() {
  let input = "cat <<A; cat <<B\nfoo\nA\nbar\nB";
  let ast = get_ast(input).unwrap();
  assert_eq!(ast.roots().len(), 1);
  let root = ast.get_root().unwrap();
  let NdRule::List { commands } = &ast[root].class else {
    panic!(
      "expected top-level List, got {:?}",
      ast[root].class.as_nd_kind()
    );
  };
  assert_eq!(commands.len(), 2);
}

// ===================== Heredoc Execution =====================

use crate::state::{Shed, vars::VarFlags, vars::VarKind};
use crate::tests::testutil::{TestGuard, test_input};

#[test]
fn heredoc_basic_output() {
  let guard = TestGuard::new();
  test_input("cat <<EOF\nhello world\nEOF".to_string()).unwrap();
  let out = guard.read_output();
  assert_eq!(out, "hello world\n");
}

#[test]
fn heredoc_multiline_output() {
  let guard = TestGuard::new();
  test_input("cat <<EOF\nline one\nline two\nline three\nEOF".to_string()).unwrap();
  let out = guard.read_output();
  assert_eq!(out, "line one\nline two\nline three\n");
}

#[test]
fn heredoc_variable_expansion() {
  let guard = TestGuard::new();
  Shed::vars_mut(|v| v.set_var("NAME", VarKind::Str("world".into()), VarFlags::empty())).unwrap();
  test_input("cat <<EOF\nhello $NAME\nEOF".to_string()).unwrap();
  let out = guard.read_output();
  assert_eq!(out, "hello world\n");
}

#[test]
fn heredoc_literal_no_expansion() {
  let guard = TestGuard::new();
  Shed::vars_mut(|v| v.set_var("NAME", VarKind::Str("world".into()), VarFlags::empty())).unwrap();
  test_input("cat <<'EOF'\nhello $NAME\nEOF".to_string()).unwrap();
  let out = guard.read_output();
  assert_eq!(out, "hello $NAME\n");
}

#[test]
fn heredoc_tab_stripping() {
  let guard = TestGuard::new();
  test_input("cat <<-EOF\n\t\thello\n\t\tworld\nEOF".to_string()).unwrap();
  let out = guard.read_output();
  assert_eq!(out, "hello\nworld\n");
}

#[test]
fn heredoc_tab_stripping_uneven() {
  let guard = TestGuard::new();
  test_input("cat <<-EOF\n\t\t\thello\n\tworld\nEOF".to_string()).unwrap();
  let out = guard.read_output();
  assert_eq!(out, "hello\nworld\n");
}

#[test]
fn plain_heredoc_tab_indented_delim_is_body_not_closer() {
  // A plain `<<EOF` matches the closer with no leading whitespace; a
  // tab-indented `EOF` is body text, so reading continues to the real closer.
  let guard = TestGuard::new();
  test_input("cat <<EOF\nbody line\n\t\tEOF\nafter\nEOF".to_string()).unwrap();
  let out = guard.read_output();
  assert_eq!(out, "body line\n\t\tEOF\nafter\n");
}

#[test]
fn dash_heredoc_tab_indented_closer_terminates() {
  // `<<-` strips leading tabs, so a tab-indented `EOF` still closes it.
  let guard = TestGuard::new();
  test_input("cat <<-EOF\n\tbody\n\t\tEOF".to_string()).unwrap();
  let out = guard.read_output();
  assert_eq!(out, "body\n");
}

// Regressions: whitespace between `<<` and the delimiter word used to
// be eaten by a consume-then-test loop that overran by one char,
// producing errors like "Heredoc delimiter 'OF' not found".

#[test]
fn heredoc_space_before_unquoted_delim() {
  let guard = TestGuard::new();
  test_input("cat << EOF\nhello\nEOF".to_string()).unwrap();
  let out = guard.read_output();
  assert_eq!(out, "hello\n");
}

#[test]
fn heredoc_space_before_single_quoted_delim() {
  let guard = TestGuard::new();
  Shed::vars_mut(|v| v.set_var("NAME", VarKind::Str("world".into()), VarFlags::empty())).unwrap();
  test_input("cat << 'EOF'\nhello $NAME\nEOF".to_string()).unwrap();
  let out = guard.read_output();
  // Single-quoted delim → literal heredoc, no expansion.
  assert_eq!(out, "hello $NAME\n");
}

#[test]
fn heredoc_tab_before_delim() {
  let guard = TestGuard::new();
  test_input("cat <<\tEOF\nhello\nEOF".to_string()).unwrap();
  let out = guard.read_output();
  assert_eq!(out, "hello\n");
}

#[test]
fn heredoc_multiple_spaces_before_delim() {
  let guard = TestGuard::new();
  test_input("cat <<   EOF\nhello\nEOF".to_string()).unwrap();
  let out = guard.read_output();
  assert_eq!(out, "hello\n");
}

#[test]
fn heredoc_dash_with_space_before_delim() {
  let guard = TestGuard::new();
  test_input("cat <<- EOF\n\thello\nEOF".to_string()).unwrap();
  let out = guard.read_output();
  assert_eq!(out, "hello\n");
}

#[test]
fn heredoc_empty_body() {
  let guard = TestGuard::new();
  test_input("cat <<EOF\nEOF".to_string()).unwrap();
  let out = guard.read_output();
  assert_eq!(out, "");
}

#[test]
fn heredoc_in_pipeline() {
  let guard = TestGuard::new();
  test_input("cat <<EOF | grep hello\nhello world\ngoodbye world\nEOF".to_string()).unwrap();
  let out = guard.read_output();
  assert_eq!(out, "hello world\n");
}

#[test]
fn herestring_basic() {
  let guard = TestGuard::new();
  test_input("cat <<< \"hello world\"".to_string()).unwrap();
  let out = guard.read_output();
  assert_eq!(out, "hello world\n");
}

#[test]
fn herestring_variable_expansion() {
  let guard = TestGuard::new();
  Shed::vars_mut(|v| v.set_var("MSG", VarKind::Str("hi there".into()), VarFlags::empty())).unwrap();
  test_input("cat <<< $MSG".to_string()).unwrap();
  let out = guard.read_output();
  assert_eq!(out, "hi there\n");
}

#[test]
fn herestring_in_func_body_defers_expansion() {
  // Defining a function must not expand a herestring word in its body;
  // the command substitution should only run when the function is called.
  let guard = TestGuard::new();
  test_input("f() { cat <<< \"$(echo expanded)\"; }".to_string()).unwrap();
  assert_eq!(guard.read_output(), "");
  test_input("f".to_string()).unwrap();
  assert_eq!(guard.read_output(), "expanded\n");
}

#[test]
fn heredoc_double_quoted_delimiter_is_literal() {
  let guard = TestGuard::new();
  Shed::vars_mut(|v| v.set_var("X", VarKind::Str("val".into()), VarFlags::empty())).unwrap();
  test_input("cat <<\"EOF\"\nhello $X\nEOF".to_string()).unwrap();
  let out = guard.read_output();
  assert_eq!(out, "hello $X\n");
}

#[test]
fn heredoc_preserves_blank_lines() {
  let guard = TestGuard::new();
  test_input("cat <<EOF\nfirst\n\nsecond\nEOF".to_string()).unwrap();
  let out = guard.read_output();
  assert_eq!(out, "first\n\nsecond\n");
}

#[test]
fn heredoc_tab_strip_preserves_empty_lines() {
  let guard = TestGuard::new();
  test_input("cat <<-EOF\n\thello\n\n\tworld\nEOF".to_string()).unwrap();
  let out = guard.read_output();
  assert_eq!(out, "hello\n\nworld\n");
}

#[test]
fn heredoc_two_on_one_line() {
  let guard = TestGuard::new();
  test_input("cat <<A; cat <<B\nfoo\nA\nbar\nB".to_string()).unwrap();
  let out = guard.read_output();
  assert_eq!(out, "foo\nbar\n");
}

// ===================== Try Block =====================

#[test]
fn parse_try_basic() {
  let input = r#"try false; catch "msg""#;
  let expected = &mut [
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::TryNode,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
  ]
  .into_iter();
  let ast = get_ast(input).unwrap();
  if let Err(e) = ast.assert_structure(expected) {
    panic!("{}", e);
  }
}

#[test]
fn parse_try_multiple_body_commands() {
  let input = r#"try false; true; catch "msg""#;
  let expected = &mut [
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::TryNode,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
  ]
  .into_iter();
  let ast = get_ast(input).unwrap();
  if let Err(e) = ast.assert_structure(expected) {
    panic!("{}", e);
  }
}

#[test]
fn parse_try_multi_line() {
  let input = "try\n  false\ncatch \"msg\"";
  let expected = &mut [
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::TryNode,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
  ]
  .into_iter();
  let ast = get_ast(input).unwrap();
  if let Err(e) = ast.assert_structure(expected) {
    panic!("{}", e);
  }
}

#[test]
fn parse_try_missing_catch() {
  let input = "try false; done";
  assert!(get_ast(input).is_err());
}

#[test]
fn parse_try_missing_done_after_do() {
  // When `do` opens a catch body, `done` is required to close it.
  // The terminator-less form (`try X; catch "msg"`) is now valid and
  // tested elsewhere.
  let input = r#"try false; catch "msg"; do echo x"#;
  assert!(get_ast(input).is_err());
}

#[test]
fn parse_try_empty_body() {
  let input = r#"try; catch "msg"; done"#;
  assert!(get_ast(input).is_err());
}

#[test]
fn try_block_success_no_error() {
  let guard = TestGuard::new();
  test_input(r#"try true; catch "should not appear"; echo after"#).unwrap();
  let out = guard.read_output();
  assert!(out.contains("after\n"), "got: {out:?}");
  assert!(!out.contains("should not appear"), "got: {out:?}");
}

#[test]
fn try_block_failure_prints_catch() {
  let guard = TestGuard::new();
  test_input(r#"try false; catch "body failed""#).unwrap();
  let out = guard.read_output();
  assert!(
    out.contains("Try Failed"),
    "expected report header; got: {out:?}"
  );
  assert!(
    out.contains("body failed"),
    "expected catch message; got: {out:?}"
  );
}

#[test]
fn try_block_continues_after_catch() {
  let guard = TestGuard::new();
  test_input(r#"try false; catch "err"; echo survived"#).unwrap();
  let out = guard.read_output();
  assert!(
    out.contains("survived\n"),
    "shell should continue past try; got: {out:?}"
  );
}

#[test]
fn try_block_catch_message_expansion() {
  let guard = TestGuard::new();
  Shed::vars_mut(|v| v.set_var("name", VarKind::Str("world".into()), VarFlags::empty())).unwrap();
  test_input(r#"try false; catch "hello $name""#).unwrap();
  let out = guard.read_output();
  assert!(
    out.contains("hello world"),
    "catch message should expand $name; got: {out:?}"
  );
}

#[test]
fn try_block_multi_arg_catch_joined_with_space() {
  let guard = TestGuard::new();
  test_input(r#"try false; catch "alpha" "beta" "gamma""#).unwrap();
  let out = guard.read_output();
  assert!(out.contains("alpha beta gamma"), "got: {out:?}");
}

#[test]
fn try_block_forces_errexit_on_body() {
  // Without errexit, a failing simple command in a list doesn't halt
  // execution. Inside the try block, errexit is forced on, so a failing
  // command should fire the catch arm even when set +e is otherwise active.
  let guard = TestGuard::new();
  test_input(r#"set +e; try false; echo body-continued; catch "caught""#).unwrap();
  let out = guard.read_output();
  assert!(out.contains("caught"), "catch should fire; got: {out:?}");
  assert!(
    !out.contains("body-continued"),
    "body should halt at first failure; got: {out:?}"
  );
}

#[test]
fn try_block_restores_errexit_after_catch() {
  // After the try block exits, the prior errexit state should be restored.
  // With set +e active before the block, set +e should still be active after.
  let guard = TestGuard::new();
  test_input(r#"set +e; try false; catch "x"; false; echo post"#).unwrap();
  let out = guard.read_output();
  assert!(
    out.contains("post\n"),
    "errexit should be restored to off; got: {out:?}"
  );
}

// ===================== try/catch four-form matrix =====================
//
//                 no body                with body
// no msg          silent swallow         override (body only)
// with msg        default report         report + post-hook

#[test]
fn try_bare_catch_swallows_silently() {
  // catch with no message, no body: swallow the failure, no report,
  // status goes to 0 as if nothing happened.
  let guard = TestGuard::new();
  test_input("try false; catch; echo $?").unwrap();
  let out = guard.read_output();
  assert!(
    out.contains("0\n"),
    "bare catch should set $? to 0 (silent swallow); got: {out:?}"
  );
  assert!(
    !out.contains("Try Failed"),
    "bare catch should not print an error report; got: {out:?}"
  );
}

#[test]
fn try_catch_body_runs_after_report() {
  // catch with message AND body: the styled report prints first, then
  // the body runs as a post-hook.
  let guard = TestGuard::new();
  test_input(r#"try false; catch "fail"; do echo recovered; done"#).unwrap();
  let out = guard.read_output();
  assert!(
    out.contains("Try Failed"),
    "report should print; got: {out:?}"
  );
  assert!(
    out.contains("fail"),
    "catch message should print; got: {out:?}"
  );
  assert!(
    out.contains("recovered\n"),
    "body should run after the report; got: {out:?}"
  );
}

#[test]
fn try_catch_body_only_skips_report() {
  // catch with body but no message: the user is overriding the default
  // reporting, no styled report appears.
  let guard = TestGuard::new();
  test_input("try false; catch; do echo recovered; done").unwrap();
  let out = guard.read_output();
  assert!(
    !out.contains("Try Failed"),
    "body-only catch should suppress the report; got: {out:?}"
  );
  assert!(
    out.contains("recovered\n"),
    "body should still run; got: {out:?}"
  );
}

#[test]
fn try_caught_failure_resets_status() {
  // A try block that catches a failure is structurally "successful" —
  // $? is 0 after, regardless of what the body or the original failure
  // set it to. This prevents outer errexit from refiring on the original
  // failure's status, and matches the "exception handled" mental model.
  let guard = TestGuard::new();
  test_input("try false; catch; do false; done; echo $?").unwrap();
  let out = guard.read_output();
  assert!(
    out.contains("0\n"),
    "$? should be 0 after a caught failure; got: {out:?}"
  );
}

#[test]
fn try_catch_body_failure_does_not_propagate() {
  // A failure inside the recovery body must not retrigger the surrounding
  // errexit. The `echo after` line proves outer execution continued.
  let guard = TestGuard::new();
  test_input("set -e; try false; catch; do false; done; echo after").unwrap();
  let out = guard.read_output();
  assert!(
    out.contains("after\n"),
    "failing recovery must not propagate; got: {out:?}"
  );
}

#[test]
fn try_catch_multiline_do_done_form() {
  // Verify the do/done form works with a newline-separated body.
  let guard = TestGuard::new();
  test_input("try\n  false\ncatch \"oops\"; do\n  echo line1\n  echo line2\ndone").unwrap();
  let out = guard.read_output();
  assert!(out.contains("Try Failed"), "got: {out:?}");
  assert!(
    out.contains("line1\nline2"),
    "body statements should run in order; got: {out:?}"
  );
}

#[test]
fn parse_try_empty_do_body_errors() {
  // `catch; do done` with nothing between `do` and `done` should error
  // at parse time. Empty do-blocks are essentially never intentional.
  let input = "try false; catch; do done";
  assert!(get_ast(input).is_err());
}

#[test]
fn try_nested_in_catch_body_catches_its_own_failures() {
  // Regression: NOT_ERR propagation on the outer catch body must NOT
  // reach into a nested try's body, or the inner try silently swallows
  // its own body's failure (errexit never fires, inner catch never
  // runs). The IS_ERR claim on the inner try body blocks the outer
  // NOT_ERR propagation.
  let guard = TestGuard::new();
  test_input("try false; catch; do\n  try false; catch; do echo inner; done\n  echo outer\ndone")
    .unwrap();
  let out = guard.read_output();
  assert!(
    out.contains("inner\n"),
    "inner catch should fire on inner try's failure; got: {out:?}"
  );
  assert!(
    out.contains("outer\n"),
    "outer catch body should continue past inner try; got: {out:?}"
  );
}

#[test]
fn try_inside_negation_catches_its_own_failures() {
  // Same IS_ERR claim should protect against `parse_negate`'s NOT_ERR
  // walk reaching into a nested try's body.
  let guard = TestGuard::new();
  test_input("! try false; catch; do echo caught; done").unwrap();
  let out = guard.read_output();
  assert!(
    out.contains("caught\n"),
    "inner catch should fire even when the try is negated; got: {out:?}"
  );
}

#[test]
fn try_inside_pipeline_catches_its_own_failures() {
  // Same IS_ERR claim should protect against `parse_pipeln`'s NOT_ERR
  // walk on non-final pipeline stages reaching into a nested try's body.
  let guard = TestGuard::new();
  test_input("try false; catch; do echo caught; done | cat").unwrap();
  let out = guard.read_output();
  assert!(
    out.contains("caught\n"),
    "inner catch should fire even when piped to another command; got: {out:?}"
  );
}

#[test]
fn try_inside_conjunction_catches_its_own_failures() {
  // Same IS_ERR claim should protect against `parse_conjunction`'s
  // NOT_ERR walk on `&&`/`||` chained commands reaching into a nested
  // try's body.
  let guard = TestGuard::new();
  test_input("try false; catch; do echo caught; done && echo after").unwrap();
  let out = guard.read_output();
  assert!(
    out.contains("caught\n"),
    "inner catch should fire even when joined with &&; got: {out:?}"
  );
  assert!(
    out.contains("after\n"),
    "chain should continue past the caught try; got: {out:?}"
  );
}

// ===================== `not` keyword =====================

#[test]
fn parse_not_basic() {
  let input = "not false";
  let expected = &mut [
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Negate,
    NdKind::Pipeline,
    NdKind::Command,
  ]
  .into_iter();
  let ast = get_ast(input).unwrap();
  if let Err(e) = ast.assert_structure(expected) {
    panic!("{}", e);
  }
}

#[test]
fn parse_not_matches_bang_structure() {
  // `not` and `!` should produce identical node structures.
  let bang_ast = get_ast("! false").unwrap();
  let not_ast = get_ast("not false").unwrap();

  let mut bang_kinds = vec![];
  let bang_root = bang_ast.get_root().unwrap();
  bang_ast.walk_tree(bang_root, &mut |id, tree| {
    bang_kinds.push(tree[id].class.as_nd_kind())
  });
  let mut not_kinds = vec![];
  let not_root = not_ast.get_root().unwrap();
  not_ast.walk_tree(not_root, &mut |id, tree| {
    not_kinds.push(tree[id].class.as_nd_kind())
  });

  assert_eq!(bang_kinds, not_kinds);
}

#[test]
fn parse_not_without_command_errors() {
  assert!(get_ast("not").is_err());
}

#[test]
fn not_inverts_false_to_zero() {
  let guard = TestGuard::new();
  test_input("not false; echo $?").unwrap();
  let out = guard.read_output();
  assert_eq!(out, "0\n");
}

#[test]
fn not_inverts_true_to_one() {
  let guard = TestGuard::new();
  test_input("not true; echo $?").unwrap();
  let out = guard.read_output();
  assert_eq!(out, "1\n");
}

#[test]
fn not_and_bang_equivalent_at_runtime() {
  let guard = TestGuard::new();
  test_input("not false; a=$?; ! false; b=$?; echo $a $b").unwrap();
  let out = guard.read_output();
  assert_eq!(out, "0 0\n");
}

#[test]
fn not_in_if_condition() {
  let guard = TestGuard::new();
  test_input("if not false; then echo taken; else echo skipped; fi").unwrap();
  let out = guard.read_output();
  assert_eq!(out, "taken\n");
}

#[test]
fn not_in_while_condition() {
  // Loop while the file does not exist (it never will), capped by a counter.
  let guard = TestGuard::new();
  test_input("i=0; while not [ \"$i\" = 3 ]; do i=$((i+1)); done; echo $i").unwrap();
  let out = guard.read_output();
  assert_eq!(out, "3\n");
}

#[test]
fn not_negates_pipeline() {
  // The body of `not` is a pipeline, so a multi-stage pipe negates as a whole.
  let guard = TestGuard::new();
  test_input("not echo foo | grep bar; echo $?").unwrap();
  let out = guard.read_output();
  assert_eq!(out, "0\n");
}

#[test]
fn not_zero_status_does_not_trigger_errexit() {
  // `not false` inverts a failing status to success, so the shell should
  // continue under set -e. (The complementary case — `not true` producing
  // status 1 — correctly triggers errexit and is not exercised here.)
  let guard = TestGuard::new();
  test_input("set -e; not false; echo survived").unwrap();
  let out = guard.read_output();
  assert!(
    out.contains("survived\n"),
    "set -e should not fire when inverted status is 0; got: {out:?}"
  );
}

// ===================== `set -o pipefail` =====================

#[test]
fn pipefail_off_last_stage_wins() {
  // Default POSIX behavior: pipeline status comes from the last stage.
  let guard = TestGuard::new();
  test_input("false | true; echo $?").unwrap();
  let out = guard.read_output();
  assert_eq!(out, "0\n");
}

#[test]
fn pipefail_on_propagates_intermediate_failure() {
  let guard = TestGuard::new();
  test_input("set -o pipefail; false | true; echo $?").unwrap();
  let out = guard.read_output();
  assert_eq!(out, "1\n");
}

#[test]
fn pipefail_on_all_zero_stays_zero() {
  let guard = TestGuard::new();
  test_input("set -o pipefail; true | true | true; echo $?").unwrap();
  let out = guard.read_output();
  assert_eq!(out, "0\n");
}

#[test]
fn pipefail_on_picks_last_nonzero_for_status() {
  // With pipefail, the pipeline's status is the LAST non-zero stage's status
  // (this is independent of which stage gets blamed in error reports).
  // Use distinguishable exit codes so we can tell which stage "won".
  let guard = TestGuard::new();
  test_input("set -o pipefail; (exit 3) | (exit 5) | true; echo $?").unwrap();
  let out = guard.read_output();
  assert_eq!(out, "5\n");
}

#[test]
fn pipefail_off_pipestatus_still_records_all_stages() {
  // PIPESTATUS is unaffected by pipefail — it always records per-stage codes.
  let guard = TestGuard::new();
  test_input("false | true; echo ${PIPESTATUS[0]} ${PIPESTATUS[1]}").unwrap();
  let out = guard.read_output();
  assert_eq!(out, "1 0\n");
}

#[test]
fn pipefail_can_be_disabled() {
  let guard = TestGuard::new();
  test_input("set -o pipefail; set +o pipefail; false | true; echo $?").unwrap();
  let out = guard.read_output();
  assert_eq!(out, "0\n");
}

#[test]
fn pipefail_triggers_errexit_on_intermediate_failure() {
  // With pipefail + errexit, an intermediate stage failure terminates
  // the shell at the pipeline boundary. The echo should never run.
  // test_input returns Err when errexit interrupts; that's expected here.
  let guard = TestGuard::new();
  let _ = test_input("set -e -o pipefail; false | true; echo should-not-print");
  let out = guard.read_output();
  assert!(
    !out.contains("should-not-print"),
    "errexit should fire on pipefail status; got: {out:?}"
  );
}

// ===================== pipefail × try =====================

#[test]
fn try_forces_pipefail_on_inside_body() {
  // try blocks force both errexit AND pipefail on inside the body, even
  // when both are off in the surrounding shell. A failing intermediate
  // stage that would normally be swallowed by POSIX pipeline semantics
  // should fire the catch arm here.
  let guard = TestGuard::new();
  test_input(r#"try echo foo | false | cat; catch "caught"; echo after"#).unwrap();
  let out = guard.read_output();
  assert!(
    out.contains("caught"),
    "catch should fire on intermediate failure; got: {out:?}"
  );
  assert!(
    out.contains("after\n"),
    "shell should continue past catch; got: {out:?}"
  );
}

#[test]
fn try_restores_pipefail_after_catch() {
  // The forced-on pipefail inside try must be restored to its prior
  // (off) state once the block exits, otherwise the outer shell would
  // see surprising behavior change.
  let guard = TestGuard::new();
  test_input(r#"try true; catch "x"; false | true; echo $?"#).unwrap();
  let out = guard.read_output();
  // After try exits, pipefail should be off again, so `false | true`
  // takes the last stage's status (0).
  assert!(
    out.contains("0\n"),
    "pipefail should be restored to off; got: {out:?}"
  );
}

// ===================== `defer` keyword =====================

#[test]
fn parse_defer_basic() {
  let input = "defer echo hi";
  let expected = &mut [
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::DeferNode,
    NdKind::Pipeline,
    NdKind::Command,
  ]
  .into_iter();
  let ast = get_ast(input).unwrap();
  if let Err(e) = ast.assert_structure(expected) {
    panic!("{}", e);
  }
}

#[test]
fn parse_defer_brace_group_body() {
  let input = "defer { echo a; echo b; }";
  let expected = &mut [
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::DeferNode,
    NdKind::Pipeline,
    NdKind::BraceGrp,
    NdKind::List,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
    NdKind::Conjunction,
    NdKind::Pipeline,
    NdKind::Command,
  ]
  .into_iter();
  let ast = get_ast(input).unwrap();
  if let Err(e) = ast.assert_structure(expected) {
    panic!("{}", e);
  }
}

#[test]
fn parse_defer_matches_time_structure() {
  // defer and time both take a single block as body and wrap it the
  // same way (no extra list-of-conjunctions layer like try has).
  let time_ast = get_ast("time echo x").unwrap();
  let defer_ast = get_ast("defer echo x").unwrap();

  let mut time_kinds = vec![];
  let time_root = time_ast.get_root().unwrap();
  time_ast.walk_tree(time_root, &mut |id, tree| {
    time_kinds.push(tree[id].class.as_nd_kind())
  });
  let mut defer_kinds = vec![];
  let defer_root = defer_ast.get_root().unwrap();
  defer_ast.walk_tree(defer_root, &mut |id, tree| {
    defer_kinds.push(tree[id].class.as_nd_kind())
  });

  // Replace Timed with DeferNode for the comparison.
  let normalized: Vec<NdKind> = time_kinds
    .into_iter()
    .map(|k| {
      if k == NdKind::Timed {
        NdKind::DeferNode
      } else {
        k
      }
    })
    .collect();
  assert_eq!(normalized, defer_kinds);
}

#[test]
fn parse_defer_missing_body_errors() {
  assert!(get_ast("defer").is_err());
}

#[test]
fn defer_fires_at_brace_group_exit() {
  let guard = TestGuard::new();
  test_input("{ defer echo bye; echo hi; }").unwrap();
  let out = guard.read_output();
  let lines: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
  assert_eq!(lines, vec!["hi", "bye"]);
}

#[test]
fn defer_lifo_in_brace_group() {
  let guard = TestGuard::new();
  test_input("{ defer echo a; defer echo b; defer echo c; }").unwrap();
  let out = guard.read_output();
  let lines: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
  assert_eq!(lines, vec!["c", "b", "a"]);
}

#[test]
fn defer_lifo_in_function() {
  let guard = TestGuard::new();
  test_input("foo() { defer echo a; defer echo b; defer echo c; }").unwrap();
  test_input("foo").unwrap();
  let out = guard.read_output();
  let lines: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
  assert_eq!(lines, vec!["c", "b", "a"]);
}

#[test]
fn defer_nested_scope_isolation() {
  // Inner defers fire when the inner brace group exits, not when the
  // outer one does.
  let guard = TestGuard::new();
  test_input("{ defer echo outer; { defer echo inner; }; echo middle; }").unwrap();
  let out = guard.read_output();
  let lines: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
  assert_eq!(lines, vec!["inner", "middle", "outer"]);
}

#[test]
fn defer_variable_snapshot_at_registration_time() {
  // Variables in the defer body are expanded at registration time, so
  // changing the variable after defer is registered does not affect what
  // the deferred body sees.
  let guard = TestGuard::new();
  test_input(r#"foo() { x=before; defer echo "$x"; x=after; }"#).unwrap();
  test_input("foo").unwrap();
  let out = guard.read_output();
  assert!(
    out.contains("before"),
    "defer body should snapshot $x at register time; got: {out:?}"
  );
}

#[test]
fn defer_command_sub_resolves_at_registration_time() {
  // Command substitution inside the defer body fires when the defer is
  // registered, not when it fires. This matches the canonical
  // save-now-restore-later defer pattern.
  let guard = TestGuard::new();
  test_input(r#"foo() { x=before; defer echo "$(echo $x)"; x=after; }"#).unwrap();
  test_input("foo").unwrap();
  let out = guard.read_output();
  assert!(
    out.contains("before"),
    "command sub should snapshot at register time; got: {out:?}"
  );
}

#[test]
fn defer_eager_capture_via_eval() {
  // To capture a value at registration time, route the defer through
  // eval. eval expands its argument string first, so the parsed defer
  // body sees the literal value baked in.
  let guard = TestGuard::new();
  test_input(r#"foo() { x=before; eval "defer echo $x"; x=after; }"#).unwrap();
  test_input("foo").unwrap();
  let out = guard.read_output();
  assert!(
    out.contains("before"),
    "eval should bake literal value into AST; got: {out:?}"
  );
}

#[test]
fn defer_brace_group_body_runs_in_order() {
  // Within a single defer's brace-group body, statements run top to
  // bottom (only the registration of defers themselves is LIFO).
  let guard = TestGuard::new();
  test_input("{ defer { echo a; echo b; echo c; }; }").unwrap();
  let out = guard.read_output();
  let lines: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
  assert_eq!(lines, vec!["a", "b", "c"]);
}

#[test]
fn defer_does_not_clobber_outer_status() {
  // Status set by the body before scope exit should survive the
  // defer firing. The defer body's `true` would otherwise overwrite
  // the `false` exit status.
  let guard = TestGuard::new();
  test_input("foo() { false; defer true; }; foo; echo $?").unwrap();
  let out = guard.read_output();
  assert!(
    out.contains("1\n"),
    "outer status should still be 1 from `false`; got: {out:?}"
  );
}

#[test]
fn deferred_exit_halts_remaining_defers_and_latches_code() {
  // `exit` from inside a deferred command is an intentional hard-stop: it
  // terminates immediately and skips the rest of the defer stack. Defers fire
  // LIFO, so `echo B` (registered last) runs, `exit 3` then halts the drain,
  // and `echo A` (registered first) is skipped. The exit code is latched into
  // QUIT_CODE so it survives the status save/restore around the defer drain.
  let guard = TestGuard::new();
  let _ = test_input("{ defer echo A; defer exit 3; defer echo B; }");
  let out = guard.read_output();
  let lines: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
  assert_eq!(
    lines,
    vec!["B"],
    "a deferred exit must halt the rest of the stack; got: {out:?}"
  );
  assert_eq!(
    crate::signal::QUIT_CODE.load(std::sync::atomic::Ordering::SeqCst),
    3,
    "a deferred exit's code must propagate via the quit latch"
  );
}

#[test]
fn deferred_exit_skips_outer_defers_but_plain_exit_runs_them() {
  // Design decision: a deferred `exit` hard-stops and does NOT run outer-scope
  // defers, whereas a plain `exit` unwinds cleanly and does run them. This
  // asymmetry is intentional; lock it in so a refactor can't silently flip it.
  {
    let guard = TestGuard::new();
    let _ = test_input("{ defer echo OUTER; { defer exit 0; echo inner; }; echo NOPE; }");
    let out = guard.read_output();
    let lines: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
      lines,
      vec!["inner"],
      "deferred exit hard-stops: the outer defer and rest of the body are skipped; got: {out:?}"
    );
  }
  {
    let guard = TestGuard::new();
    let _ = test_input("{ defer echo OUTER; { echo inner; exit 0; }; echo NOPE; }");
    let out = guard.read_output();
    let lines: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
      lines,
      vec!["inner", "OUTER"],
      "plain exit unwinds cleanly and runs outer defers; got: {out:?}"
    );
  }
}

/// Strip ANSI CSI (`ESC[…m`) sequences so traceback assertions match on the
/// rendered text, not the color codes ariadne wraps it in.
fn strip_ansi(s: &str) -> String {
  let mut out = String::new();
  let mut chars = s.chars();
  while let Some(c) = chars.next() {
    if c == '\x1b' {
      for c2 in chars.by_ref() {
        if c2 == 'm' {
          break;
        }
      }
    } else {
      out.push(c);
    }
  }
  out
}

// ─── Golden traceback tests ──────────────────────────────────────────────
// Lock in the error-context ("in call to function …") behavior so the lazy /
// on-the-way-up context refactor can be verified to preserve it. A
// command-not-found inside a function is printed-and-swallowed, so its
// traceback is captured from the terminal; errexit propagates as an Err, so
// its labels are inspected directly.

#[test]
fn traceback_function_error_shows_call_chain() {
  let guard = TestGuard::new();
  let _ = test_input("f() { nonexistent_xyz_123; }; f");
  let out = strip_ansi(&guard.read_output());
  for expect in [
    "command not found",
    "in function 'f' defined here",
    "in call to function 'f'",
  ] {
    assert!(
      out.contains(expect),
      "missing {expect:?} in traceback:\n{out}"
    );
  }
}

#[test]
fn traceback_nested_function_error_shows_full_chain() {
  let guard = TestGuard::new();
  let _ = test_input("g() { nonexistent_xyz_123; }; f() { g; }; f");
  let out = strip_ansi(&guard.read_output());
  for expect in [
    "command not found",
    "in function 'g' defined here",
    "in function 'f' defined here",
    "in call to function 'g'",
    "in call to function 'f'",
  ] {
    assert!(
      out.contains(expect),
      "missing {expect:?} in traceback:\n{out}"
    );
  }
}

#[test]
fn traceback_errexit_in_function_is_bare() {
  // `set -e` surfaces just the failing command; no function call-chain context.
  let _guard = TestGuard::new();
  let err = test_input("set -e; f() { false; echo NOPE; }; f").unwrap_err();
  let labels = err.label_messages();
  assert!(
    labels
      .iter()
      .any(|l| l.contains("Command returned non-zero exit status")),
    "labels: {labels:?}"
  );
  assert!(
    !labels.iter().any(|l| l.contains("in call to function")),
    "errexit should not carry call-chain labels; got: {labels:?}"
  );
}

#[test]
fn traceback_propagating_builtin_error_shows_call_chain() {
  // A builtin error (cd) propagates out of the function and is printed at the
  // top — it must still carry the call chain (unlike a forked command error,
  // it never dies in a child).
  let guard = TestGuard::new();
  let _ = test_input("f() { cd /nonexistent_dir_xyz_987; }; f");
  let out = strip_ansi(&guard.read_output());
  for expect in ["in function 'f' defined here", "in call to function 'f'"] {
    assert!(
      out.contains(expect),
      "missing {expect:?} in traceback:\n{out}"
    );
  }
}

#[test]
fn traceback_raise_in_function_surfaces_message() {
  // `raise` surfaces its message. (Its *collapse* to a single call-site label
  // at func_depth<=1 is depth-sensitive and not cleanly reproducible under the
  // test harness's frame depth; the collapsed rendering is verified
  // byte-for-byte against the eager baseline separately.)
  let _guard = TestGuard::new();
  let err = test_input("f() { raise MyErr \"boom\"; }; f").unwrap_err();
  let labels = err.label_messages();
  assert!(
    labels.iter().any(|l| l.contains("boom")),
    "expected raised message in labels; got: {labels:?}"
  );
}

#[test]
fn defer_failure_does_not_propagate_to_outer_errexit() {
  // A failing defer body must not trigger the surrounding errexit. The
  // `echo after` line proves the outer execution continued past the
  // brace group's defer-fire phase.
  let guard = TestGuard::new();
  test_input("set -e; { defer false; }; echo after").unwrap();
  let out = guard.read_output();
  assert!(
    out.contains("after\n"),
    "defer failure must not trigger outer errexit; got: {out:?}"
  );
}

#[test]
fn defer_failure_does_not_fire_try_catch() {
  // The defer-in-try regression: a failing defer body inside a try
  // block's scope should NOT propagate into the try's catch arm,
  // because defer failures are absorbed at the scope boundary.
  let guard = TestGuard::new();
  test_input(r#"{ try { defer false; }; catch "should not fire"; echo after; }"#).unwrap();
  let out = guard.read_output();
  assert!(
    !out.contains("should not fire"),
    "try's catch arm should not fire on defer failure; got: {out:?}"
  );
  assert!(
    out.contains("after\n"),
    "execution should continue past try; got: {out:?}"
  );
}

#[test]
fn defer_registration_returns_zero_status() {
  // The defer keyword itself (the registration) should succeed.
  let guard = TestGuard::new();
  test_input("defer echo unused; echo $?").unwrap();
  let out = guard.read_output();
  assert!(
    out.contains("0\n"),
    "defer registration should return 0; got: {out:?}"
  );
}
