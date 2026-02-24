use std::sync::Arc;

use crate::parse::{
  lex::{LexFlags, LexStream},
  NdRule, Node, ParseStream, Redir, RedirType,
};
use crate::procio::{IoFrame, IoMode, IoStack};

// ============================================================================
// Parser Tests - Redirection Syntax
// ============================================================================

fn parse_command(input: &str) -> Node {
  let source = Arc::new(input.to_string());
  let tokens = LexStream::new(source, LexFlags::empty())
    .flatten()
    .collect::<Vec<_>>();

  let mut nodes = ParseStream::new(tokens).flatten().collect::<Vec<_>>();

  assert_eq!(nodes.len(), 1, "Expected exactly one node");
  let top_node = nodes.remove(0);

  // Navigate to the actual Command node within the AST structure
  // Structure is typically: Conjunction -> Pipeline -> Command
  match top_node.class {
    NdRule::Conjunction { elements } => {
      let first_element = elements
        .into_iter()
        .next()
        .expect("Expected at least one conjunction element");
      match first_element.cmd.class {
        NdRule::Pipeline { cmds, .. } => {
          let mut commands = cmds;
          assert_eq!(
            commands.len(),
            1,
            "Expected exactly one command in pipeline"
          );
          commands.remove(0)
        }
        NdRule::Command { .. } => *first_element.cmd,
        _ => panic!(
          "Expected Command or Pipeline node, got {:?}",
          first_element.cmd.class
        ),
      }
    }
    NdRule::Pipeline { cmds, .. } => {
      let mut commands = cmds;
      assert_eq!(
        commands.len(),
        1,
        "Expected exactly one command in pipeline"
      );
      commands.remove(0)
    }
    NdRule::Command { .. } => top_node,
    _ => panic!(
      "Expected Conjunction, Pipeline, or Command node, got {:?}",
      top_node.class
    ),
  }
}

#[test]
fn parse_output_redirect() {
  let node = parse_command("echo hello > output.txt");

  assert_eq!(node.redirs.len(), 1);
  let redir = &node.redirs[0];

  assert!(matches!(redir.class, RedirType::Output));
  assert!(matches!(redir.io_mode, IoMode::File { tgt_fd: 1, .. }));
}

#[test]
fn parse_append_redirect() {
  let node = parse_command("echo hello >> output.txt");

  assert_eq!(node.redirs.len(), 1);
  let redir = &node.redirs[0];

  assert!(matches!(redir.class, RedirType::Append));
  assert!(matches!(redir.io_mode, IoMode::File { tgt_fd: 1, .. }));
}

#[test]
fn parse_input_redirect() {
  let node = parse_command("cat < input.txt");

  assert_eq!(node.redirs.len(), 1);
  let redir = &node.redirs[0];

  assert!(matches!(redir.class, RedirType::Input));
  assert!(matches!(redir.io_mode, IoMode::File { tgt_fd: 0, .. }));
}

#[test]
fn parse_stderr_redirect() {
  let node = parse_command("ls 2> errors.txt");

  assert_eq!(node.redirs.len(), 1);
  let redir = &node.redirs[0];

  assert!(matches!(redir.class, RedirType::Output));
  assert!(matches!(redir.io_mode, IoMode::File { tgt_fd: 2, .. }));
}

#[test]
fn parse_stderr_to_stdout() {
  let node = parse_command("ls 2>&1");

  assert_eq!(node.redirs.len(), 1);
  let redir = &node.redirs[0];

  assert!(matches!(
    redir.io_mode,
    IoMode::Fd {
      tgt_fd: 2,
      src_fd: 1
    }
  ));
}

#[test]
fn parse_stdout_to_stderr() {
  let node = parse_command("echo test 1>&2");

  assert_eq!(node.redirs.len(), 1);
  let redir = &node.redirs[0];

  assert!(matches!(
    redir.io_mode,
    IoMode::Fd {
      tgt_fd: 1,
      src_fd: 2
    }
  ));
}

#[test]
fn parse_multiple_redirects() {
  let node = parse_command("cmd < input.txt > output.txt 2> errors.txt");

  assert_eq!(node.redirs.len(), 3);

  // Input redirect
  assert!(matches!(node.redirs[0].class, RedirType::Input));
  assert!(matches!(
    node.redirs[0].io_mode,
    IoMode::File { tgt_fd: 0, .. }
  ));

  // Stdout redirect
  assert!(matches!(node.redirs[1].class, RedirType::Output));
  assert!(matches!(
    node.redirs[1].io_mode,
    IoMode::File { tgt_fd: 1, .. }
  ));

  // Stderr redirect
  assert!(matches!(node.redirs[2].class, RedirType::Output));
  assert!(matches!(
    node.redirs[2].io_mode,
    IoMode::File { tgt_fd: 2, .. }
  ));
}

#[test]
fn parse_custom_fd_redirect() {
  let node = parse_command("echo test 3> fd3.txt");

  assert_eq!(node.redirs.len(), 1);
  let redir = &node.redirs[0];

  assert!(matches!(redir.class, RedirType::Output));
  assert!(matches!(redir.io_mode, IoMode::File { tgt_fd: 3, .. }));
}

#[test]
fn parse_custom_fd_dup() {
  let node = parse_command("cmd 3>&4");

  assert_eq!(node.redirs.len(), 1);
  let redir = &node.redirs[0];

  assert!(matches!(
    redir.io_mode,
    IoMode::Fd {
      tgt_fd: 3,
      src_fd: 4
    }
  ));
}

#[test]
fn parse_heredoc() {
  let node = parse_command("cat << EOF");

  assert_eq!(node.redirs.len(), 1);
  let redir = &node.redirs[0];

  assert!(matches!(redir.class, RedirType::HereDoc));
}

#[test]
fn parse_herestring() {
  let node = parse_command("cat <<< 'hello world'");

  assert_eq!(node.redirs.len(), 1);
  let redir = &node.redirs[0];

  assert!(matches!(redir.class, RedirType::HereString));
}

#[test]
fn parse_redirect_with_no_space() {
  let node = parse_command("echo hello >output.txt");

  assert_eq!(node.redirs.len(), 1);
  assert!(matches!(node.redirs[0].class, RedirType::Output));
}

#[test]
fn parse_redirect_order_preserved() {
  let node = parse_command("cmd 2>&1 > file.txt");

  assert_eq!(node.redirs.len(), 2);

  // First redirect: 2>&1
  assert!(matches!(
    node.redirs[0].io_mode,
    IoMode::Fd {
      tgt_fd: 2,
      src_fd: 1
    }
  ));

  // Second redirect: > file.txt
  assert!(matches!(node.redirs[1].class, RedirType::Output));
  assert!(matches!(
    node.redirs[1].io_mode,
    IoMode::File { tgt_fd: 1, .. }
  ));
}

// ============================================================================
// IoStack Tests - Data Structure Logic
// ============================================================================

#[test]
fn iostack_new() {
  let stack = IoStack::new();

  assert_eq!(stack.len(), 1, "IoStack should start with one frame");
  assert_eq!(stack.curr_frame().len(), 0, "Initial frame should be empty");
}

#[test]
fn iostack_push_pop_frame() {
  let mut stack = IoStack::new();

  // Push a new frame
  stack.push_frame(IoFrame::new());
  assert_eq!(stack.len(), 2);

  // Pop it back
  let frame = stack.pop_frame();
  assert_eq!(frame.len(), 0);
  assert_eq!(stack.len(), 1);
}

#[test]
fn iostack_never_empties() {
  let mut stack = IoStack::new();

  // Try to pop the last frame
  let frame = stack.pop_frame();
  assert_eq!(frame.len(), 0);

  // Stack should still have one frame
  assert_eq!(stack.len(), 1);

  // Pop again - should still have one frame
  let frame = stack.pop_frame();
  assert_eq!(frame.len(), 0);
  assert_eq!(stack.len(), 1);
}

#[test]
fn iostack_push_to_frame() {
  let mut stack = IoStack::new();

  let redir = crate::parse::Redir::new(IoMode::fd(1, 2), RedirType::Output);

  stack.push_to_frame(redir);
  assert_eq!(stack.curr_frame().len(), 1);
}

#[test]
fn iostack_append_to_frame() {
  let mut stack = IoStack::new();

  let redirs = vec![
    crate::parse::Redir::new(IoMode::fd(1, 2), RedirType::Output),
    crate::parse::Redir::new(IoMode::fd(2, 1), RedirType::Output),
  ];

  stack.append_to_frame(redirs);
  assert_eq!(stack.curr_frame().len(), 2);
}

#[test]
fn iostack_frame_isolation() {
  let mut stack = IoStack::new();

  // Add redir to first frame
  let redir1 = crate::parse::Redir::new(IoMode::fd(1, 2), RedirType::Output);
  stack.push_to_frame(redir1);
  assert_eq!(stack.curr_frame().len(), 1);

  // Push new frame
  stack.push_frame(IoFrame::new());
  assert_eq!(stack.curr_frame().len(), 0, "New frame should be empty");

  // Add redir to second frame
  let redir2 = crate::parse::Redir::new(IoMode::fd(2, 1), RedirType::Output);
  stack.push_to_frame(redir2);
  assert_eq!(stack.curr_frame().len(), 1);

  // Pop second frame
  let frame2 = stack.pop_frame();
  assert_eq!(frame2.len(), 1);

  // First frame should still have its redir
  assert_eq!(stack.curr_frame().len(), 1);
}

#[test]
fn iostack_flatten() {
  let mut stack = IoStack::new();

  // Add redir to first frame
  let redir1 = crate::parse::Redir::new(IoMode::fd(1, 2), RedirType::Output);
  stack.push_to_frame(redir1);

  // Push new frame with redir
  let mut frame2 = IoFrame::new();
  frame2.push(crate::parse::Redir::new(
    IoMode::fd(2, 1),
    RedirType::Output,
  ));
  stack.push_frame(frame2);

  // Push third frame with redir
  let mut frame3 = IoFrame::new();
  frame3.push(crate::parse::Redir::new(IoMode::fd(0, 3), RedirType::Input));
  stack.push_frame(frame3);

  assert_eq!(stack.len(), 3);

  // Flatten
  stack.flatten();

  // Should have one frame with all redirects
  assert_eq!(stack.len(), 1);
  assert_eq!(stack.curr_frame().len(), 3);
}

#[test]
fn ioframe_new() {
  let frame = IoFrame::new();
  assert_eq!(frame.len(), 0);
}

#[test]
fn ioframe_from_redirs() {
  let redirs = vec![
    crate::parse::Redir::new(IoMode::fd(1, 2), RedirType::Output),
    crate::parse::Redir::new(IoMode::fd(2, 1), RedirType::Output),
  ];

  let frame = IoFrame::from_redirs(redirs);
  assert_eq!(frame.len(), 2);
}

#[test]
fn ioframe_push() {
  let mut frame = IoFrame::new();

  let redir = crate::parse::Redir::new(IoMode::fd(1, 2), RedirType::Output);
  frame.push(redir);

  assert_eq!(frame.len(), 1);
}

// ============================================================================
// IoMode Tests - Construction Logic
// ============================================================================

#[test]
fn iomode_fd_construction() {
  let io_mode = IoMode::fd(2, 1);

  match io_mode {
    IoMode::Fd { tgt_fd, src_fd } => {
      assert_eq!(tgt_fd, 2);
      assert_eq!(src_fd, 1);
    }
    _ => panic!("Expected IoMode::Fd"),
  }
}

#[test]
fn iomode_tgt_fd() {
  let fd_mode = IoMode::fd(2, 1);
  assert_eq!(fd_mode.tgt_fd(), 2);

  let file_mode = IoMode::file(1, std::path::PathBuf::from("test.txt"), RedirType::Output);
  assert_eq!(file_mode.tgt_fd(), 1);
}

#[test]
fn iomode_src_fd() {
  let fd_mode = IoMode::fd(2, 1);
  assert_eq!(fd_mode.src_fd(), 1);
}
