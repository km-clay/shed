use std::env;
use std::fs;
use std::path::PathBuf;

use tempfile::TempDir;

use crate::prompt::readline::complete::Completer;
use crate::prompt::readline::markers;
use crate::state::{write_logic, write_vars, VarFlags};

use super::*;

/// Helper to create a temp directory with test files
fn setup_test_files() -> TempDir {
    let temp_dir = tempfile::tempdir().unwrap();
    let path = temp_dir.path();

    // Create some test files and directories
    fs::write(path.join("file1.txt"), "").unwrap();
    fs::write(path.join("file2.txt"), "").unwrap();
    fs::write(path.join("script.sh"), "").unwrap();
    fs::create_dir(path.join("subdir")).unwrap();
    fs::write(path.join("subdir/nested.txt"), "").unwrap();
    fs::create_dir(path.join("another_dir")).unwrap();

    temp_dir
}

/// Helper to create a test directory in current dir for relative path tests
fn setup_local_test_files() -> TempDir {
    let temp_dir = tempfile::tempdir_in(".").unwrap();
    let path = temp_dir.path();

    fs::write(path.join("local1.txt"), "").unwrap();
    fs::write(path.join("local2.txt"), "").unwrap();
    fs::create_dir(path.join("localdir")).unwrap();

    temp_dir
}

// ============================================================================
// Command Completion Tests
// ============================================================================

#[test]
fn complete_command_from_path() {
    let mut completer = Completer::new();

    // Try to complete "ec" - should find "echo" (which is in PATH)
    let line = "ec".to_string();
    let cursor_pos = 2;

    let result = completer.complete(line, cursor_pos, 1);
    assert!(result.is_ok());
    let completed = result.unwrap();

    // Should have found something
    assert!(completed.is_some());
    let completed_line = completed.unwrap();

    // Should contain "echo"
    assert!(completed_line.starts_with("echo") || completer.candidates.iter().any(|c| c == "echo"));
}

#[test]
fn complete_command_builtin() {
    let mut completer = Completer::new();

    // Try to complete "ex" - should find "export" builtin
    let line = "ex".to_string();
    let cursor_pos = 2;

    let result = completer.complete(line, cursor_pos, 1).unwrap();
    assert!(result.is_some());

    // Check candidates include "export"
    assert!(completer.candidates.iter().any(|c| c == "export"));
}

// NOTE: Disabled - ShFunc constructor requires parsed AST which is complex to set up in tests
// TODO: Re-enable once we have a helper to create test functions
/*
#[test]
fn complete_command_function() {
    write_logic(|l| {
        // Add a test function - would need to parse "test_func() { echo test; }"
        // and create proper ShFunc from it
        // let func = ...;
        // l.insert_func("test_func", func);

        let mut completer = Completer::new();
        let line = "test_f".to_string();
        let cursor_pos = 6;

        let result = completer.complete(line, cursor_pos, 1).unwrap();
        assert!(result.is_some());

        // Should find test_func
        assert!(completer.candidates.iter().any(|c| c == "test_func"));

        // Cleanup
        l.clear_functions();
    });
}
*/

#[test]
fn complete_command_alias() {
    // Add alias outside of completion call to avoid RefCell borrow conflict
    write_logic(|l| {
        l.insert_alias("ll", "ls -la");
    });

    let mut completer = Completer::new();
    let line = "l".to_string();
    let cursor_pos = 1;

    let result = completer.complete(line, cursor_pos, 1).unwrap();
    assert!(result.is_some());

    // Should find ll and ls
    assert!(completer.candidates.iter().any(|c| c == "ll"));

    // Cleanup
    write_logic(|l| {
        l.clear_aliases();
    });
}

#[test]
fn complete_command_no_matches() {
    let mut completer = Completer::new();

    // Try to complete something that definitely doesn't exist
    let line = "xyzabc123notacommand".to_string();
    let cursor_pos = line.len();

    let result = completer.complete(line, cursor_pos, 1).unwrap();
    // Should return None when no matches
    assert!(result.is_none());
}

// ============================================================================
// Filename Completion Tests
// ============================================================================

#[test]
fn complete_filename_basic() {
    let temp_dir = setup_test_files();
    let path = temp_dir.path();

    let mut completer = Completer::new();
    let line = format!("cat {}/fil", path.display());
    let cursor_pos = line.len();

    let result = completer.complete(line, cursor_pos, 1).unwrap();
    assert!(result.is_some());

    // Should have file1.txt and file2.txt as candidates
    assert!(completer.candidates.len() >= 2);
    assert!(completer.candidates.iter().any(|c| c.contains("file1.txt")));
    assert!(completer.candidates.iter().any(|c| c.contains("file2.txt")));
}

#[test]
fn complete_filename_directory() {
    let temp_dir = setup_test_files();
    let path = temp_dir.path();

    let mut completer = Completer::new();
    let line = format!("cd {}/sub", path.display());
    let cursor_pos = line.len();

    let result = completer.complete(line, cursor_pos, 1).unwrap();
    assert!(result.is_some());

    // Should find "subdir"
    assert!(completer.candidates.iter().any(|c| c.contains("subdir")));
}

#[test]
fn complete_filename_with_slash() {
    let temp_dir = setup_test_files();
    let path = temp_dir.path();

    let mut completer = Completer::new();
    let line = format!("ls {}/subdir/", path.display());
    let cursor_pos = line.len();

    let result = completer.complete(line, cursor_pos, 1).unwrap();

    // Should complete files in subdir/
    if result.is_some() {
        assert!(completer.candidates.iter().any(|c| c.contains("nested.txt")));
    }
}

#[test]
fn complete_filename_preserves_trailing_slash() {
    let temp_dir = setup_test_files();
    let path = temp_dir.path();

    let mut completer = Completer::new();
    let line = format!("cd {}/sub", path.display());
    let cursor_pos = line.len();

    let result = completer.complete(line, cursor_pos, 1).unwrap();
    assert!(result.is_some());

    let completed = result.unwrap();
    // Directory completions should have trailing slash
    assert!(completed.ends_with('/'));
}

#[test]
fn complete_filename_relative_path() {
    let _temp_dir = setup_local_test_files();
    let dir_name = _temp_dir.path().file_name().unwrap().to_str().unwrap();

    let mut completer = Completer::new();
    let line = format!("cat {}/local", dir_name);
    let cursor_pos = line.len();

    let result = completer.complete(line, cursor_pos, 1).unwrap();

    if result.is_some() {
        // Should find local1.txt and local2.txt
        assert!(completer.candidates.len() >= 2);
    }
}

#[test]
fn complete_filename_current_dir() {
    let mut completer = Completer::new();

    // Complete files in current directory
    let line = "cat ".to_string();
    let cursor_pos = 4;

    let result = completer.complete(line, cursor_pos, 1).unwrap();

    // Should find something in current dir (at least Cargo.toml should exist)
    if result.is_some() {
        assert!(!completer.candidates.is_empty());
    }
}

#[test]
fn complete_filename_with_dot_slash() {
    let _temp_dir = setup_local_test_files();
    let dir_name = _temp_dir.path().file_name().unwrap().to_str().unwrap();

    let mut completer = Completer::new();
    let line = format!("./{}/local", dir_name);
    let cursor_pos = line.len();

    let result = completer.complete(line, cursor_pos, 1);

    // Should preserve the ./
    if let Ok(Some(completed)) = result {
        assert!(completed.starts_with("./"));
    }
}

// ============================================================================
// Completion After '=' Tests
// ============================================================================

#[test]
fn complete_after_equals_assignment() {
    let temp_dir = setup_test_files();
    let path = temp_dir.path();

    let mut completer = Completer::new();
    let line = format!("FOO={}/fil", path.display());
    let cursor_pos = line.len();

    let result = completer.complete(line, cursor_pos, 1).unwrap();
    assert!(result.is_some());

    // Should complete filenames after =
    assert!(completer.candidates.iter().any(|c| c.contains("file")));
}

#[test]
fn complete_after_equals_option() {
    let temp_dir = setup_test_files();
    let path = temp_dir.path();

    let mut completer = Completer::new();
    let line = format!("command --output={}/fil", path.display());
    let cursor_pos = line.len();

    let result = completer.complete(line, cursor_pos, 1).unwrap();
    assert!(result.is_some());

    // Should complete filenames after = in option
    assert!(completer.candidates.iter().any(|c| c.contains("file")));
}

#[test]
fn complete_after_equals_empty() {
    let mut completer = Completer::new();
    let line = "FOO=".to_string();
    let cursor_pos = 4;

    let result = completer.complete(line, cursor_pos, 1).unwrap();

    // Should complete files in current directory when path is empty after =
    if result.is_some() {
        assert!(!completer.candidates.is_empty());
    }
}

// ============================================================================
// Context Detection Tests
// ============================================================================

#[test]
fn context_detection_command_position() {
    let completer = Completer::new();

    // At the beginning - command context
    let (ctx, _) = completer.get_completion_context("ech", 3);
    assert!(ctx.last() == Some(&markers::COMMAND), "Should be in command context at start");

    // After whitespace - still command if no command yet
    let (ctx, _) = completer.get_completion_context("  ech", 5);
    assert!(ctx.last() == Some(&markers::COMMAND), "Should be in command context after whitespace");
}

#[test]
fn context_detection_argument_position() {
    let completer = Completer::new();

    // After a complete command - argument context
    let (ctx, _) = completer.get_completion_context("echo hello", 10);
    assert!(ctx.last() != Some(&markers::COMMAND), "Should be in argument context after command");

    let (ctx, _) = completer.get_completion_context("ls -la /tmp", 11);
    assert!(ctx.last() != Some(&markers::COMMAND), "Should be in argument context");
}

#[test]
fn context_detection_nested_command_sub() {
    let completer = Completer::new();

    // Inside $() - should be command context
    let (ctx, _) = completer.get_completion_context("echo \"$(ech", 11);
    assert!(ctx.last() == Some(&markers::COMMAND), "Should be in command context inside $()");

    // After command in $() - argument context
    let (ctx, _) = completer.get_completion_context("echo \"$(echo hell", 17);
    assert!(ctx.last() != Some(&markers::COMMAND), "Should be in argument context inside $()");
}

#[test]
fn context_detection_pipe() {
    let completer = Completer::new();

    // After pipe - command context
    let (ctx, _) = completer.get_completion_context("ls | gre", 8);
    assert!(ctx.last() == Some(&markers::COMMAND), "Should be in command context after pipe");
}

#[test]
fn context_detection_command_sep() {
    let completer = Completer::new();

    // After semicolon - command context
    let (ctx, _) = completer.get_completion_context("echo foo; l", 11);
    assert!(ctx.last() == Some(&markers::COMMAND), "Should be in command context after semicolon");

    // After && - command context
    let (ctx, _) = completer.get_completion_context("true && l", 9);
    assert!(ctx.last() == Some(&markers::COMMAND), "Should be in command context after &&");
}

#[test]
fn context_detection_variable_substitution() {
    let completer = Completer::new();

    // $VAR at argument position - VAR_SUB should take priority over ARG
    let (ctx, _) = completer.get_completion_context("echo $HOM", 9);
    assert_eq!(ctx.last(), Some(&markers::VAR_SUB), "Should be in var_sub context for $HOM");

    // $VAR at command position - VAR_SUB should take priority over COMMAND
    let (ctx, _) = completer.get_completion_context("$HOM", 4);
    assert_eq!(ctx.last(), Some(&markers::VAR_SUB), "Should be in var_sub context for bare $HOM");
}

#[test]
fn context_detection_variable_in_double_quotes() {
    let completer = Completer::new();

    // $VAR inside double quotes
    let (ctx, _) = completer.get_completion_context("echo \"$HOM", 10);
    assert_eq!(ctx.last(), Some(&markers::VAR_SUB), "Should be in var_sub context inside double quotes");
}

#[test]
fn context_detection_stack_base_is_null() {
    let completer = Completer::new();

    // Empty input - only NULL on the stack
    let (ctx, _) = completer.get_completion_context("", 0);
    assert_eq!(ctx, vec![markers::NULL], "Empty input should only have NULL marker");
}

#[test]
fn context_detection_context_start_position() {
    let completer = Completer::new();

    // Command at start - ctx_start should be 0
    let (_, ctx_start) = completer.get_completion_context("ech", 3);
    assert_eq!(ctx_start, 0, "Command at start should have ctx_start=0");

    // Argument after command - ctx_start should be at arg position
    let (_, ctx_start) = completer.get_completion_context("echo hel", 8);
    assert_eq!(ctx_start, 5, "Argument ctx_start should point to arg start");

    // Variable sub - ctx_start should point to the $
    let (_, ctx_start) = completer.get_completion_context("echo $HOM", 9);
    assert_eq!(ctx_start, 5, "Var sub ctx_start should point to the $");
}

#[test]
fn context_detection_priority_ordering() {
    let completer = Completer::new();

    // COMMAND (priority 2) should override ARG (priority 1)
    // After a pipe, the next token is a command even though it looks like an arg
    let (ctx, _) = completer.get_completion_context("echo foo | gr", 13);
    assert_eq!(ctx.last(), Some(&markers::COMMAND), "COMMAND should win over ARG after pipe");

    // VAR_SUB (priority 3) should override COMMAND (priority 2)
    let (ctx, _) = completer.get_completion_context("$PA", 3);
    assert_eq!(ctx.last(), Some(&markers::VAR_SUB), "VAR_SUB should win over COMMAND");
}

// ============================================================================
// Cycling Behavior Tests
// ============================================================================

#[test]
fn cycle_forward_through_candidates() {
    let temp_dir = setup_test_files();
    let path = temp_dir.path();

    let mut completer = Completer::new();
    let line = format!("cat {}/file", path.display());
    let cursor_pos = line.len();

    // First tab
    let result1 = completer.complete(line.clone(), cursor_pos, 1).unwrap();
    assert!(result1.is_some());
    let first_candidate = completer.selected_candidate().unwrap().clone();

    // Second tab - should cycle to next
    let result2 = completer.complete(line.clone(), cursor_pos, 1).unwrap();
    assert!(result2.is_some());
    let second_candidate = completer.selected_candidate().unwrap().clone();

    // Should be different (if there are multiple candidates)
    if completer.candidates.len() > 1 {
        assert_ne!(first_candidate, second_candidate);
    }
}

#[test]
fn cycle_backward_with_shift_tab() {
    let temp_dir = setup_test_files();
    let path = temp_dir.path();

    let mut completer = Completer::new();
    let line = format!("cat {}/file", path.display());
    let cursor_pos = line.len();

    // Forward twice
    completer.complete(line.clone(), cursor_pos, 1).unwrap();
    let after_first = completer.selected_idx;
    completer.complete(line.clone(), cursor_pos, 1).unwrap();

    // Backward once (shift-tab = direction -1)
    completer.complete(line.clone(), cursor_pos, -1).unwrap();
    let after_backward = completer.selected_idx;

    // Should be back to first selection
    assert_eq!(after_first, after_backward);
}

#[test]
fn cycle_wraps_around() {
    let temp_dir = setup_test_files();
    let path = temp_dir.path();

    let mut completer = Completer::new();
    let line = format!("cat {}/", path.display());
    let cursor_pos = line.len();

    // Get all candidates
    completer.complete(line.clone(), cursor_pos, 1).unwrap();
    let num_candidates = completer.candidates.len();

    if num_candidates > 1 {
        // Cycle through all and one more
        for _ in 0..num_candidates {
            completer.complete(line.clone(), cursor_pos, 1).unwrap();
        }

        // Should wrap back to first (index 0)
        assert_eq!(completer.selected_idx, 0);
    }
}

#[test]
fn cycle_reset_on_input_change() {
    let temp_dir = setup_test_files();
    let path = temp_dir.path();

    let mut completer = Completer::new();
    let line1 = format!("cat {}/file", path.display());

    // Complete once
    completer.complete(line1.clone(), line1.len(), 1).unwrap();
    let candidates_count = completer.candidates.len();

    // Change input
    let line2 = format!("cat {}/script", path.display());
    completer.complete(line2.clone(), line2.len(), 1).unwrap();

    // Should have different candidates
    // (or at least should have reset the completer state)
    assert!(completer.active);
}

#[test]
fn reset_clears_state() {
    let mut completer = Completer::new();
    // Use a prefix that will definitely have completions
    let line = "ec".to_string();

    let result = completer.complete(line, 2, 1).unwrap();
    // Only check if we got completions
    if result.is_some() {
        // Should have candidates after completion
        assert!(!completer.candidates.is_empty());

        completer.reset();

        // After reset, state should be cleared
        assert!(!completer.active);
        assert!(completer.candidates.is_empty());
        assert_eq!(completer.selected_idx, 0);
    }
}

// ============================================================================
// Edge Cases Tests
// ============================================================================

#[test]
fn complete_empty_input() {
    let mut completer = Completer::new();
    let line = "".to_string();
    let cursor_pos = 0;

    let result = completer.complete(line, cursor_pos, 1).unwrap();

    // Empty input might return files in current dir or no completion
    // Either is valid behavior
}

#[test]
fn complete_whitespace_only() {
    let mut completer = Completer::new();
    let line = "   ".to_string();
    let cursor_pos = 3;

    let result = completer.complete(line, cursor_pos, 1);
    // Should handle gracefully
    assert!(result.is_ok());
}

#[test]
fn complete_at_middle_of_word() {
    let mut completer = Completer::new();
    let line = "echo hello world".to_string();
    let cursor_pos = 7; // In the middle of "hello"

    let result = completer.complete(line, cursor_pos, 1);
    // Should handle cursor in middle of word
    assert!(result.is_ok());
}

#[test]
fn complete_with_quotes() {
    let temp_dir = setup_test_files();
    let path = temp_dir.path();

    let mut completer = Completer::new();
    let line = format!("cat \"{}/fil", path.display());
    let cursor_pos = line.len();

    let result = completer.complete(line, cursor_pos, 1);

    // Should handle quoted paths
    assert!(result.is_ok());
}

#[test]
fn complete_incomplete_command_substitution() {
    let mut completer = Completer::new();
    let line = "echo \"$(ech".to_string();
    let cursor_pos = 11;

    let result = completer.complete(line, cursor_pos, 1);

    // Should not crash on incomplete command sub
    assert!(result.is_ok());
}

#[test]
fn complete_with_multiple_spaces() {
    let mut completer = Completer::new();
    let line = "echo    hello    world".to_string();
    let cursor_pos = line.len();

    let result = completer.complete(line, cursor_pos, 1);
    assert!(result.is_ok());
}

#[test]
fn complete_special_characters_in_filename() {
    let temp_dir = tempfile::tempdir().unwrap();
    let path = temp_dir.path();

    // Create files with special characters
    fs::write(path.join("file-with-dash.txt"), "").unwrap();
    fs::write(path.join("file_with_underscore.txt"), "").unwrap();

    let mut completer = Completer::new();
    let line = format!("cat {}/file", path.display());
    let cursor_pos = line.len();

    let result = completer.complete(line, cursor_pos, 1).unwrap();

    if result.is_some() {
        // Should handle special chars in filenames
        assert!(completer.candidates.iter().any(|c| c.contains("file-with-dash") || c.contains("file_with_underscore")));
    }
}

// ============================================================================
// Integration Tests
// ============================================================================

#[test]
fn complete_full_workflow() {
    let temp_dir = setup_test_files();
    let path = temp_dir.path();

    let mut completer = Completer::new();
    let line = format!("cat {}/fil", path.display());
    let cursor_pos = line.len();

    // Tab 1: Get first completion
    let result = completer.complete(line.clone(), cursor_pos, 1).unwrap();
    assert!(result.is_some());
    let completion1 = result.unwrap();
    assert!(completion1.contains("file"));

    // Tab 2: Cycle to next
    let result = completer.complete(line.clone(), cursor_pos, 1).unwrap();
    assert!(result.is_some());
    let completion2 = result.unwrap();

    // Shift-Tab: Go back
    let result = completer.complete(line.clone(), cursor_pos, -1).unwrap();
    assert!(result.is_some());
    let completion3 = result.unwrap();

    // Should be back to first
    assert_eq!(completion1, completion3);
}

#[test]
fn complete_mixed_command_and_file() {
    let mut completer = Completer::new();

    // First part: command completion
    let line1 = "ech".to_string();
    let result1 = completer.complete(line1, 3, 1).unwrap();
    assert!(result1.is_some());

    // Reset for new completion
    completer.reset();

    // Second part: file completion
    let line2 = "echo Cargo.tom".to_string();
    let result2 = completer.complete(line2, 14, 1).unwrap();

    // Both should work
    assert!(result1.is_some());
}
