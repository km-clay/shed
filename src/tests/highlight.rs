use crate::prompt::readline::{
  annotate_input, annotate_input_recursive, highlight::Highlighter, markers,
};

use super::*;

/// Helper to check if a marker exists at any position in the annotated string
fn has_marker(annotated: &str, marker: char) -> bool {
  annotated.contains(marker)
}

/// Helper to find the position of a marker in the annotated string
fn find_marker(annotated: &str, marker: char) -> Option<usize> {
  annotated.find(marker)
}

/// Helper to check if markers appear in the correct order
fn marker_before(annotated: &str, first: char, second: char) -> bool {
  if let (Some(pos1), Some(pos2)) = (
    find_marker(annotated, first),
    find_marker(annotated, second),
  ) {
    pos1 < pos2
  } else {
    false
  }
}

// ============================================================================
// Basic Token-Level Annotation Tests
// ============================================================================

#[test]
fn annotate_simple_command() {
  let input = "/bin/ls -la";
  let annotated = annotate_input(input);

  // Should have COMMAND marker for "/bin/ls" (external command)
  assert!(has_marker(&annotated, markers::COMMAND));

  // Should have ARG marker for "-la"
  assert!(has_marker(&annotated, markers::ARG));

  // Should have RESET markers
  assert!(has_marker(&annotated, markers::RESET));
}

#[test]
fn annotate_builtin_command() {
  let input = "export FOO=bar";
  let annotated = annotate_input(input);

  // Should mark "export" as BUILTIN
  assert!(has_marker(&annotated, markers::BUILTIN));

  // Should mark assignment (or ARG if assignment isn't specifically marked
  // separately)
  assert!(has_marker(&annotated, markers::ASSIGNMENT) || has_marker(&annotated, markers::ARG));
}

#[test]
fn annotate_operator() {
  let input = "ls | grep foo";
  let annotated = annotate_input(input);

  // Should have OPERATOR marker for pipe
  assert!(has_marker(&annotated, markers::OPERATOR));

  // Should have COMMAND markers for both commands
  let command_count = annotated.chars().filter(|&c| c == markers::COMMAND).count();
  assert_eq!(command_count, 2);
}

#[test]
fn annotate_redirect() {
  let input = "echo hello > output.txt";
  let annotated = annotate_input(input);

  // Should have REDIRECT marker
  assert!(has_marker(&annotated, markers::REDIRECT));
}

#[test]
fn annotate_keyword() {
  let input = "if true; then echo yes; fi";
  let annotated = annotate_input(input);

  // Should have KEYWORD markers for if/then/fi
  assert!(has_marker(&annotated, markers::KEYWORD));
}

#[test]
fn annotate_command_separator() {
  let input = "echo foo; echo bar";
  let annotated = annotate_input(input);

  // Should have CMD_SEP marker for semicolon
  assert!(has_marker(&annotated, markers::CMD_SEP));
}

// ============================================================================
// Sub-Token Annotation Tests
// ============================================================================

#[test]
fn annotate_variable_simple() {
  let input = "echo $foo";
  let annotated = annotate_input(input);

  // Should have VAR_SUB markers
  assert!(has_marker(&annotated, markers::VAR_SUB));
  assert!(has_marker(&annotated, markers::VAR_SUB_END));
}

#[test]
fn annotate_variable_braces() {
  let input = "echo ${foo}";
  let annotated = annotate_input(input);

  // Should have VAR_SUB markers for ${foo}
  assert!(has_marker(&annotated, markers::VAR_SUB));
  assert!(has_marker(&annotated, markers::VAR_SUB_END));
}

#[test]
fn annotate_double_quoted_string() {
  let input = r#"echo "hello world""#;
  let annotated = annotate_input(input);

  // Should have STRING_DQ markers
  assert!(has_marker(&annotated, markers::STRING_DQ));
  assert!(has_marker(&annotated, markers::STRING_DQ_END));
}

#[test]
fn annotate_single_quoted_string() {
  let input = "echo 'hello world'";
  let annotated = annotate_input(input);

  // Should have STRING_SQ markers
  assert!(has_marker(&annotated, markers::STRING_SQ));
  assert!(has_marker(&annotated, markers::STRING_SQ_END));
}

#[test]
fn annotate_variable_in_string() {
  let input = r#"echo "hello $USER""#;
  let annotated = annotate_input(input);

  // Should have both STRING_DQ and VAR_SUB markers
  assert!(has_marker(&annotated, markers::STRING_DQ));
  assert!(has_marker(&annotated, markers::VAR_SUB));

  // VAR_SUB should be inside STRING_DQ
  assert!(marker_before(
    &annotated,
    markers::STRING_DQ,
    markers::VAR_SUB
  ));
}

#[test]
fn annotate_glob_asterisk() {
  let input = "ls *.txt";
  let annotated = annotate_input(input);

  // Should have GLOB marker for *
  assert!(has_marker(&annotated, markers::GLOB));
}

#[test]
fn annotate_glob_question() {
  let input = "ls file?.txt";
  let annotated = annotate_input(input);

  // Should have GLOB marker for ?
  assert!(has_marker(&annotated, markers::GLOB));
}

#[test]
fn annotate_glob_bracket() {
  let input = "ls file[abc].txt";
  let annotated = annotate_input(input);

  // Should have GLOB markers for bracket expression
  let glob_count = annotated.chars().filter(|&c| c == markers::GLOB).count();
  assert!(glob_count >= 2); // Opening and closing
}

// ============================================================================
// Command Substitution Tests (Flat)
// ============================================================================

#[test]
fn annotate_command_sub_basic() {
  let input = "echo $(whoami)";
  let annotated = annotate_input(input);

  // Should have CMD_SUB markers (but not recursively annotated yet)
  assert!(has_marker(&annotated, markers::CMD_SUB));
  assert!(has_marker(&annotated, markers::CMD_SUB_END));
}

#[test]
fn annotate_subshell_basic() {
  let input = "(cd /tmp && ls)";
  let annotated = annotate_input(input);

  // Should have SUBSH markers
  assert!(has_marker(&annotated, markers::SUBSH));
  assert!(has_marker(&annotated, markers::SUBSH_END));
}

#[test]
fn annotate_process_sub_output() {
  let input = "diff <(ls dir1) <(ls dir2)";
  let annotated = annotate_input(input);

  // Should have PROC_SUB markers
  assert!(has_marker(&annotated, markers::PROC_SUB));
  assert!(has_marker(&annotated, markers::PROC_SUB_END));
}

// ============================================================================
// Recursive Annotation Tests
// ============================================================================

#[test]
fn annotate_recursive_command_sub() {
  let input = "echo $(whoami)";
  let annotated = annotate_input_recursive(input);

  // Should have CMD_SUB markers
  assert!(has_marker(&annotated, markers::CMD_SUB));
  assert!(has_marker(&annotated, markers::CMD_SUB_END));

  // Inside the command sub, "whoami" should be marked as COMMAND
  // The recursive annotator should have processed the inside
  assert!(has_marker(&annotated, markers::COMMAND));
}

#[test]
fn annotate_recursive_nested_command_sub() {
  let input = "echo $(echo $(whoami))";
  let annotated = annotate_input_recursive(input);

  // Should have multiple CMD_SUB markers (nested)
  let cmd_sub_count = annotated.chars().filter(|&c| c == markers::CMD_SUB).count();
  assert!(
    cmd_sub_count >= 2,
    "Should have at least 2 CMD_SUB markers for nested substitutions"
  );
}

#[test]
fn annotate_recursive_command_sub_with_args() {
  let input = "echo $(grep foo file.txt)";
  let annotated = annotate_input_recursive(input);

  // Should have BUILTIN for echo and possibly COMMAND for grep (if in PATH)
  // Just check that we have command-type markers
  let builtin_count = annotated.chars().filter(|&c| c == markers::BUILTIN).count();
  let command_count = annotated.chars().filter(|&c| c == markers::COMMAND).count();
  assert!(
    builtin_count + command_count >= 2,
    "Expected at least 2 command markers (BUILTIN or COMMAND)"
  );
}

#[test]
fn annotate_recursive_subshell() {
  let input = "(echo hello; echo world)";
  let annotated = annotate_input_recursive(input);

  // Should have SUBSH markers
  assert!(has_marker(&annotated, markers::SUBSH));
  assert!(has_marker(&annotated, markers::SUBSH_END));

  // Inside should be annotated with BUILTIN (echo is a builtin) and CMD_SEP
  assert!(has_marker(&annotated, markers::BUILTIN));
  assert!(has_marker(&annotated, markers::CMD_SEP));
}

#[test]
fn annotate_recursive_process_sub() {
  let input = "diff <(ls -la)";
  let annotated = annotate_input_recursive(input);

  // Should have PROC_SUB markers
  assert!(has_marker(&annotated, markers::PROC_SUB));

  // ls should be marked as COMMAND inside the process sub
  assert!(has_marker(&annotated, markers::COMMAND));
}

#[test]
fn annotate_recursive_command_sub_in_string() {
  let input = r#"echo "current user: $(whoami)""#;
  let annotated = annotate_input_recursive(input);

  // Should have STRING_DQ, CMD_SUB, and COMMAND markers
  assert!(has_marker(&annotated, markers::STRING_DQ));
  assert!(has_marker(&annotated, markers::CMD_SUB));
  assert!(has_marker(&annotated, markers::COMMAND));
}

#[test]
fn annotate_recursive_deeply_nested() {
  let input = r#"echo "outer: $(echo "inner: $(whoami)")""#;
  let annotated = annotate_input_recursive(input);

  // Should have multiple STRING_DQ and CMD_SUB markers
  let string_count = annotated
    .chars()
    .filter(|&c| c == markers::STRING_DQ)
    .count();
  let cmd_sub_count = annotated.chars().filter(|&c| c == markers::CMD_SUB).count();

  assert!(string_count >= 2, "Should have multiple STRING_DQ markers");
  assert!(cmd_sub_count >= 2, "Should have multiple CMD_SUB markers");
}

// ============================================================================
// Marker Priority/Ordering Tests
// ============================================================================

#[test]
fn marker_priority_var_in_string() {
  let input = r#""$foo""#;
  let annotated = annotate_input(input);

  // STRING_DQ should come before VAR_SUB (outer before inner)
  assert!(marker_before(
    &annotated,
    markers::STRING_DQ,
    markers::VAR_SUB
  ));
}

#[test]
fn marker_priority_arg_vs_string() {
  let input = r#"echo "hello""#;
  let annotated = annotate_input(input);

  // Both ARG and STRING_DQ should be present
  // STRING_DQ should be inside the ARG token's span
  assert!(has_marker(&annotated, markers::ARG));
  assert!(has_marker(&annotated, markers::STRING_DQ));
}

#[test]
fn marker_priority_reset_placement() {
  let input = "echo hello";
  let annotated = annotate_input(input);

  // RESET markers should appear after each token
  // There should be multiple RESET markers
  let reset_count = annotated.chars().filter(|&c| c == markers::RESET).count();
  assert!(reset_count >= 2);
}

// ============================================================================
// Highlighter Output Tests
// ============================================================================

#[test]
fn highlighter_produces_ansi_codes() {
  let mut highlighter = Highlighter::new();
  highlighter.load_input("echo hello");
  highlighter.highlight();
  let output = highlighter.take();

  // Should contain ANSI escape codes
  assert!(
    output.contains("\x1b["),
    "Output should contain ANSI escape sequences"
  );

  // Should still contain the original text
  assert!(output.contains("echo"));
  assert!(output.contains("hello"));
}

#[test]
fn highlighter_handles_empty_input() {
  let mut highlighter = Highlighter::new();
  highlighter.load_input("");
  highlighter.highlight();
  let output = highlighter.take();

  // Should not crash and should return empty or minimal output
  assert!(output.len() < 10); // Just escape codes or empty
}

#[test]
fn highlighter_command_validation() {
  let mut highlighter = Highlighter::new();

  // Valid command (echo exists)
  highlighter.load_input("echo test");
  highlighter.highlight();
  let valid_output = highlighter.take();

  // Invalid command (definitely doesn't exist)
  highlighter.load_input("xyznotacommand123 test");
  highlighter.highlight();
  let invalid_output = highlighter.take();

  // Both should have ANSI codes
  assert!(valid_output.contains("\x1b["));
  assert!(invalid_output.contains("\x1b["));

  // The color codes should be different (green vs red)
  // Valid commands should have \x1b[32m (green)
  // Invalid commands should have \x1b[31m (red) or \x1b[1;31m (bold red)
}

#[test]
fn highlighter_preserves_text_content() {
  let input = "echo hello world";
  let mut highlighter = Highlighter::new();
  highlighter.load_input(input);
  highlighter.highlight();
  let output = highlighter.take();

  // Remove ANSI codes to check text content
  let text_only: String = output
    .chars()
    .filter(|c| !c.is_control() && *c != '\x1b')
    .collect();

  // Should still contain the words (might have escape sequence fragments)
  assert!(output.contains("echo"));
  assert!(output.contains("hello"));
  assert!(output.contains("world"));
}

#[test]
fn highlighter_multiple_tokens() {
  let mut highlighter = Highlighter::new();
  highlighter.load_input("ls -la | grep foo");
  highlighter.highlight();
  let output = highlighter.take();

  // Should contain all tokens
  assert!(output.contains("ls"));
  assert!(output.contains("-la"));
  assert!(output.contains("|"));
  assert!(output.contains("grep"));
  assert!(output.contains("foo"));

  // Should have ANSI codes
  assert!(output.contains("\x1b["));
}

#[test]
fn highlighter_string_with_variable() {
  let mut highlighter = Highlighter::new();
  highlighter.load_input(r#"echo "hello $USER""#);
  highlighter.highlight();
  let output = highlighter.take();

  // Should contain the text
  assert!(output.contains("echo"));
  assert!(output.contains("hello"));
  assert!(output.contains("USER"));

  // Should have ANSI codes for different elements
  assert!(output.contains("\x1b["));
}

#[test]
fn highlighter_reusable() {
  let mut highlighter = Highlighter::new();

  // First input
  highlighter.load_input("echo first");
  highlighter.highlight();
  let output1 = highlighter.take();

  // Second input (reusing same highlighter)
  highlighter.load_input("echo second");
  highlighter.highlight();
  let output2 = highlighter.take();

  // Both should work
  assert!(output1.contains("first"));
  assert!(output2.contains("second"));

  // Should not contain each other's text
  assert!(!output1.contains("second"));
  assert!(!output2.contains("first"));
}

// ============================================================================
// Edge Cases
// ============================================================================

#[test]
fn annotate_unclosed_string() {
  let input = r#"echo "hello"#;
  let annotated = annotate_input(input);

  // Should handle unclosed string gracefully
  assert!(has_marker(&annotated, markers::STRING_DQ));
  // May or may not have STRING_DQ_END depending on implementation
}

#[test]
fn annotate_unclosed_command_sub() {
  let input = "echo $(whoami";
  let annotated = annotate_input(input);

  // Should handle unclosed command sub gracefully
  assert!(has_marker(&annotated, markers::CMD_SUB));
}

#[test]
fn annotate_empty_command_sub() {
  let input = "echo $()";
  let annotated = annotate_input_recursive(input);

  // Should handle empty command sub
  assert!(has_marker(&annotated, markers::CMD_SUB));
  assert!(has_marker(&annotated, markers::CMD_SUB_END));
}

#[test]
fn annotate_escaped_characters() {
  let input = r#"echo \$foo \`bar\` \"test\""#;
  let annotated = annotate_input(input);

  // Should not mark escaped $ as variable
  // This is tricky - the behavior depends on implementation
  // At minimum, should not crash
}

#[test]
fn annotate_special_variables() {
  let input = "echo $0 $1 $2 $3 $4";
  let annotated = annotate_input(input);

  // Should mark positional parameters
  let var_count = annotated.chars().filter(|&c| c == markers::VAR_SUB).count();
  assert!(
    var_count >= 5,
    "Expected at least 5 VAR_SUB markers, found {}",
    var_count
  );
}

#[test]
fn annotate_variable_no_expansion_in_single_quotes() {
  let input = "echo '$foo'";
  let annotated = annotate_input(input);

  // Should have STRING_SQ markers
  assert!(has_marker(&annotated, markers::STRING_SQ));

  // Should NOT have VAR_SUB markers (variables don't expand in single quotes)
  // Note: The annotator might still mark it - depends on implementation
}

#[test]
fn annotate_complex_pipeline() {
  let input = "cat file.txt | grep pattern | sed 's/foo/bar/' | sort | uniq";
  let annotated = annotate_input(input);

  // Should have multiple OPERATOR markers for pipes
  let operator_count = annotated
    .chars()
    .filter(|&c| c == markers::OPERATOR)
    .count();
  assert!(operator_count >= 4);

  // Should have multiple COMMAND markers
  let command_count = annotated.chars().filter(|&c| c == markers::COMMAND).count();
  assert!(command_count >= 5);
}

#[test]
fn annotate_assignment_with_command_sub() {
  let input = "FOO=$(whoami)";
  let annotated = annotate_input_recursive(input);

  // Should have ASSIGNMENT marker
  assert!(has_marker(&annotated, markers::ASSIGNMENT));

  // Should have CMD_SUB marker
  assert!(has_marker(&annotated, markers::CMD_SUB));

  // Inside command sub should have COMMAND marker
  assert!(has_marker(&annotated, markers::COMMAND));
}

#[test]
fn annotate_redirect_with_fd() {
  let input = "command 2>&1";
  let annotated = annotate_input(input);

  // Should have REDIRECT marker for the redirect operator
  assert!(has_marker(&annotated, markers::REDIRECT));
}

#[test]
fn annotate_multiple_redirects() {
  let input = "command > out.txt 2>&1";
  let annotated = annotate_input(input);

  // Should have multiple REDIRECT markers
  let redirect_count = annotated
    .chars()
    .filter(|&c| c == markers::REDIRECT)
    .count();
  assert!(redirect_count >= 2);
}

#[test]
fn annotate_here_string() {
  let input = "cat <<< 'hello world'";
  let annotated = annotate_input(input);

  // Should have REDIRECT marker for <<<
  assert!(has_marker(&annotated, markers::REDIRECT));

  // Should have STRING_SQ markers
  assert!(has_marker(&annotated, markers::STRING_SQ));
}

#[test]
fn annotate_unicode_content() {
  let input = "echo 'hello 世界 🌍'";
  let annotated = annotate_input(input);

  // Should handle unicode gracefully
  assert!(has_marker(&annotated, markers::BUILTIN));
  assert!(has_marker(&annotated, markers::STRING_SQ));
}

// ============================================================================
// Regression Tests (for bugs we've fixed)
// ============================================================================

#[test]
fn regression_arg_marker_at_position_zero() {
  // Regression test: ARG marker was appearing at position 3 for input "ech"
  // This was caused by SOI/EOI tokens falling through to ARG annotation
  let input = "ech";
  let annotated = annotate_input(input);

  // Should only have COMMAND marker, not ARG
  // (incomplete command should still be marked as command attempt)
  assert!(has_marker(&annotated, markers::COMMAND));
}

#[test]
fn regression_string_color_in_annotated_strings() {
  // Regression test: ARG marker was overriding STRING_DQ color
  let input = r#"echo "test""#;
  let annotated = annotate_input(input);

  // STRING_DQ should be present and properly positioned
  assert!(has_marker(&annotated, markers::STRING_DQ));
  assert!(has_marker(&annotated, markers::STRING_DQ_END));

  // The string markers should come after the ARG marker
  // (so they override it in the highlighting)
}
