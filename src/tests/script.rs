use std::process::{self, Output};

use pretty_assertions::assert_eq;

use super::super::*;
fn get_script_output(name: &str, args: &[&str]) -> Output {
	// Resolve the path to the fern binary.
	// Do not question me.
	let mut fern_path = env::current_exe()
		.expect("Failed to get test executable"); // The path to the test executable
	fern_path.pop(); // Hocus pocus
	fern_path.pop(); 
	fern_path.push("fern"); // Abra Kadabra

	if !fern_path.is_file() {
		fern_path.pop(); 
		fern_path.pop(); 
		fern_path.push("release"); 
		fern_path.push("fern");
	}

	if !fern_path.is_file() {
		panic!("where the hell is the binary")
	}

	process::Command::new(fern_path) // Alakazam
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
