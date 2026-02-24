use std::process::{self, Output};

use pretty_assertions::assert_eq;

use super::super::*;
fn get_script_output(name: &str, args: &[&str]) -> Output {
  // Resolve the path to the shed binary.
  // Do not question me.
  let mut shed_path = env::current_exe().expect("Failed to get test executable"); // The path to the test executable
  shed_path.pop(); // Hocus pocus
  shed_path.pop();
  shed_path.push("shed"); // Abra Kadabra

  if !shed_path.is_file() {
    shed_path.pop();
    shed_path.pop();
    shed_path.push("release");
    shed_path.push("shed");
  }

  if !shed_path.is_file() {
    panic!("where the hell is the binary")
  }

  process::Command::new(shed_path) // Alakazam
    .arg(name)
    .args(args)
    .output()
    .expect("Failed to run script")
}
#[test]
fn script_hello_world() {
  let output = get_script_output("./test_scripts/hello.sh", &[]);
  assert!(output.status.success());
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert_eq!(stdout.trim(), "Hello, World!")
}
#[test]
fn script_cmdsub() {
  let output = get_script_output("./test_scripts/cmdsub.sh", &[]);
  assert!(output.status.success());
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert_eq!(stdout.trim(), "foo Hello bar")
}
#[test]
fn script_multiline() {
  let output = get_script_output("./test_scripts/multiline.sh", &[]);
  assert!(output.status.success());
  let stdout = String::from_utf8_lossy(&output.stdout);
  assert_eq!(stdout.trim(), "foo\nbar\nbiz\nbuzz")
}
