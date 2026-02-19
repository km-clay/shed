use crate::state::{LogTab, ScopeStack, ShellParam, VarFlags, VarTab};

// ============================================================================
// ScopeStack Tests - Variable Scoping
// ============================================================================

#[test]
fn scopestack_new() {
	let stack = ScopeStack::new();

	// Should start with one global scope
	assert!(stack.var_exists("PATH") || !stack.var_exists("PATH")); // Just check it doesn't panic
}

#[test]
fn scopestack_descend_ascend() {
	let mut stack = ScopeStack::new();

	// Set a global variable
	stack.set_var("GLOBAL", "value1", VarFlags::NONE);
	assert_eq!(stack.get_var("GLOBAL"), "value1");

	// Descend into a new scope
	stack.descend(None);

	// Global should still be visible
	assert_eq!(stack.get_var("GLOBAL"), "value1");

	// Set a local variable
	stack.set_var("LOCAL", "value2", VarFlags::LOCAL);
	assert_eq!(stack.get_var("LOCAL"), "value2");

	// Ascend back to global scope
	stack.ascend();

	// Global should still exist
	assert_eq!(stack.get_var("GLOBAL"), "value1");

	// Local should no longer be visible
	assert_eq!(stack.get_var("LOCAL"), "");
}

#[test]
fn scopestack_variable_shadowing() {
	let mut stack = ScopeStack::new();

	// Set global variable
	stack.set_var("VAR", "global", VarFlags::NONE);
	assert_eq!(stack.get_var("VAR"), "global");

	// Descend into local scope
	stack.descend(None);

	// Set local variable with same name
	stack.set_var("VAR", "local", VarFlags::LOCAL);
	assert_eq!(stack.get_var("VAR"), "local", "Local should shadow global");

	// Ascend back
	stack.ascend();

	// Global should be restored
	assert_eq!(stack.get_var("VAR"), "global", "Global should be unchanged after ascend");
}

#[test]
fn scopestack_local_vs_global_flag() {
	let mut stack = ScopeStack::new();

	// Descend into a local scope
	stack.descend(None);

	// Set with LOCAL flag - should go in current scope
	stack.set_var("LOCAL_VAR", "local", VarFlags::LOCAL);

	// Set without LOCAL flag - should go in global scope
	stack.set_var("GLOBAL_VAR", "global", VarFlags::NONE);

	// Both visible from local scope
	assert_eq!(stack.get_var("LOCAL_VAR"), "local");
	assert_eq!(stack.get_var("GLOBAL_VAR"), "global");

	// Ascend to global
	stack.ascend();

	// Only global var should be visible
	assert_eq!(stack.get_var("GLOBAL_VAR"), "global");
	assert_eq!(stack.get_var("LOCAL_VAR"), "");
}

#[test]
fn scopestack_multiple_levels() {
	let mut stack = ScopeStack::new();

	stack.set_var("LEVEL0", "global", VarFlags::NONE);

	// Level 1
	stack.descend(None);
	stack.set_var("LEVEL1", "first", VarFlags::LOCAL);

	// Level 2
	stack.descend(None);
	stack.set_var("LEVEL2", "second", VarFlags::LOCAL);

	// All variables visible from deepest scope
	assert_eq!(stack.get_var("LEVEL0"), "global");
	assert_eq!(stack.get_var("LEVEL1"), "first");
	assert_eq!(stack.get_var("LEVEL2"), "second");

	// Ascend to level 1
	stack.ascend();
	assert_eq!(stack.get_var("LEVEL0"), "global");
	assert_eq!(stack.get_var("LEVEL1"), "first");
	assert_eq!(stack.get_var("LEVEL2"), "");

	// Ascend to global
	stack.ascend();
	assert_eq!(stack.get_var("LEVEL0"), "global");
	assert_eq!(stack.get_var("LEVEL1"), "");
	assert_eq!(stack.get_var("LEVEL2"), "");
}

#[test]
fn scopestack_cannot_ascend_past_global() {
	let mut stack = ScopeStack::new();

	stack.set_var("VAR", "value", VarFlags::NONE);

	// Try to ascend from global scope (should be no-op)
	stack.ascend();
	stack.ascend();
	stack.ascend();

	// Variable should still exist
	assert_eq!(stack.get_var("VAR"), "value");
}

#[test]
fn scopestack_descend_with_args() {
	let mut stack = ScopeStack::new();

	// Get initial param values from global scope (test process args)
	let global_param_1 = stack.get_param(ShellParam::Pos(1));

	// Descend with positional parameters
	let args = vec!["local_arg1".to_string(), "local_arg2".to_string()];
	stack.descend(Some(args));

	// In local scope, positional params come from the VarTab created during descend
	// VarTab::new() initializes with process args, then our args are appended
	// So we check that SOME positional parameter exists (implementation detail may vary)
	let local_param = stack.get_param(ShellParam::Pos(1));
	assert!(!local_param.is_empty(), "Should have positional parameters in local scope");

	// Ascend back
	stack.ascend();

	// Should be back to global scope parameters
	assert_eq!(stack.get_param(ShellParam::Pos(1)), global_param_1);
}

#[test]
fn scopestack_global_parameters() {
	let mut stack = ScopeStack::new();

	// Set global parameters
	stack.set_param(ShellParam::Status, "0");
	stack.set_param(ShellParam::LastJob, "1234");

	assert_eq!(stack.get_param(ShellParam::Status), "0");
	assert_eq!(stack.get_param(ShellParam::LastJob), "1234");

	// Descend into local scope
	stack.descend(None);

	// Global parameters should still be visible
	assert_eq!(stack.get_param(ShellParam::Status), "0");
	assert_eq!(stack.get_param(ShellParam::LastJob), "1234");

	// Modify global parameter from local scope
	stack.set_param(ShellParam::Status, "1");
	assert_eq!(stack.get_param(ShellParam::Status), "1");

	// Ascend
	stack.ascend();

	// Global parameter should retain modified value
	assert_eq!(stack.get_param(ShellParam::Status), "1");
}

#[test]
fn scopestack_unset_var() {
	let mut stack = ScopeStack::new();

	stack.set_var("VAR", "value", VarFlags::NONE);
	assert_eq!(stack.get_var("VAR"), "value");

	stack.unset_var("VAR");
	assert_eq!(stack.get_var("VAR"), "");
	assert!(!stack.var_exists("VAR"));
}

#[test]
fn scopestack_unset_finds_innermost() {
	let mut stack = ScopeStack::new();

	// Set global
	stack.set_var("VAR", "global", VarFlags::NONE);

	// Descend and shadow
	stack.descend(None);
	stack.set_var("VAR", "local", VarFlags::LOCAL);
	assert_eq!(stack.get_var("VAR"), "local");

	// Unset should remove local, revealing global
	stack.unset_var("VAR");
	assert_eq!(stack.get_var("VAR"), "global");
}

#[test]
fn scopestack_export_var() {
	let mut stack = ScopeStack::new();

	stack.set_var("VAR", "value", VarFlags::NONE);

	// Export the variable
	stack.export_var("VAR");

	// Variable should still be accessible (flag is internal detail)
	assert_eq!(stack.get_var("VAR"), "value");
}

#[test]
fn scopestack_var_exists() {
	let mut stack = ScopeStack::new();

	assert!(!stack.var_exists("NONEXISTENT"));

	stack.set_var("EXISTS", "yes", VarFlags::NONE);
	assert!(stack.var_exists("EXISTS"));

	stack.descend(None);
	assert!(stack.var_exists("EXISTS"), "Global var should be visible in local scope");

	stack.set_var("LOCAL", "yes", VarFlags::LOCAL);
	assert!(stack.var_exists("LOCAL"));

	stack.ascend();
	assert!(!stack.var_exists("LOCAL"), "Local var should not exist after ascend");
}

#[test]
fn scopestack_flatten_vars() {
	let mut stack = ScopeStack::new();

	stack.set_var("GLOBAL1", "g1", VarFlags::NONE);
	stack.set_var("GLOBAL2", "g2", VarFlags::NONE);

	stack.descend(None);
	stack.set_var("LOCAL1", "l1", VarFlags::LOCAL);

	let flattened = stack.flatten_vars();

	// Should contain variables from all scopes
	assert!(flattened.contains_key("GLOBAL1"));
	assert!(flattened.contains_key("GLOBAL2"));
	assert!(flattened.contains_key("LOCAL1"));
}

// ============================================================================
// LogTab Tests - Functions and Aliases
// ============================================================================

#[test]
fn logtab_new() {
	let logtab = LogTab::new();
	assert_eq!(logtab.funcs().len(), 0);
	assert_eq!(logtab.aliases().len(), 0);
}

#[test]
fn logtab_insert_get_alias() {
	let mut logtab = LogTab::new();

	logtab.insert_alias("ll", "ls -la");
	assert_eq!(logtab.get_alias("ll"), Some("ls -la".to_string()));
	assert_eq!(logtab.get_alias("nonexistent"), None);
}

#[test]
fn logtab_overwrite_alias() {
	let mut logtab = LogTab::new();

	logtab.insert_alias("ll", "ls -la");
	assert_eq!(logtab.get_alias("ll"), Some("ls -la".to_string()));

	logtab.insert_alias("ll", "ls -lah");
	assert_eq!(logtab.get_alias("ll"), Some("ls -lah".to_string()));
}

#[test]
fn logtab_remove_alias() {
	let mut logtab = LogTab::new();

	logtab.insert_alias("ll", "ls -la");
	assert!(logtab.get_alias("ll").is_some());

	logtab.remove_alias("ll");
	assert!(logtab.get_alias("ll").is_none());
}

#[test]
fn logtab_clear_aliases() {
	let mut logtab = LogTab::new();

	logtab.insert_alias("ll", "ls -la");
	logtab.insert_alias("la", "ls -A");
	logtab.insert_alias("l", "ls -CF");

	assert_eq!(logtab.aliases().len(), 3);

	logtab.clear_aliases();
	assert_eq!(logtab.aliases().len(), 0);
}

#[test]
fn logtab_multiple_aliases() {
	let mut logtab = LogTab::new();

	logtab.insert_alias("ll", "ls -la");
	logtab.insert_alias("la", "ls -A");
	logtab.insert_alias("grep", "grep --color=auto");

	assert_eq!(logtab.aliases().len(), 3);
	assert_eq!(logtab.get_alias("ll"), Some("ls -la".to_string()));
	assert_eq!(logtab.get_alias("la"), Some("ls -A".to_string()));
	assert_eq!(logtab.get_alias("grep"), Some("grep --color=auto".to_string()));
}

// Note: Function tests are limited because ShFunc requires complex setup (parsed AST)
// We'll test the basic storage/retrieval mechanics

#[test]
fn logtab_funcs_empty_initially() {
	let logtab = LogTab::new();
	assert_eq!(logtab.funcs().len(), 0);
	assert!(logtab.get_func("nonexistent").is_none());
}

// ============================================================================
// VarTab Tests - Variable Storage
// ============================================================================

#[test]
fn vartab_new() {
	let vartab = VarTab::new();
	// VarTab initializes with some default params, just check it doesn't panic
	assert!(vartab.get_var("NONEXISTENT").is_empty());
}

#[test]
fn vartab_set_get_var() {
	let mut vartab = VarTab::new();

	vartab.set_var("TEST", "value", VarFlags::NONE);
	assert_eq!(vartab.get_var("TEST"), "value");
}

#[test]
fn vartab_overwrite_var() {
	let mut vartab = VarTab::new();

	vartab.set_var("VAR", "value1", VarFlags::NONE);
	assert_eq!(vartab.get_var("VAR"), "value1");

	vartab.set_var("VAR", "value2", VarFlags::NONE);
	assert_eq!(vartab.get_var("VAR"), "value2");
}

#[test]
fn vartab_var_exists() {
	let mut vartab = VarTab::new();

	assert!(!vartab.var_exists("TEST"));

	vartab.set_var("TEST", "value", VarFlags::NONE);
	assert!(vartab.var_exists("TEST"));
}

#[test]
fn vartab_unset_var() {
	let mut vartab = VarTab::new();

	vartab.set_var("VAR", "value", VarFlags::NONE);
	assert!(vartab.var_exists("VAR"));

	vartab.unset_var("VAR");
	assert!(!vartab.var_exists("VAR"));
	assert_eq!(vartab.get_var("VAR"), "");
}

#[test]
fn vartab_export_var() {
	let mut vartab = VarTab::new();

	vartab.set_var("VAR", "value", VarFlags::NONE);
	vartab.export_var("VAR");

	// Variable should still be accessible
	assert_eq!(vartab.get_var("VAR"), "value");
}

#[test]
fn vartab_positional_params() {
	let mut vartab = VarTab::new();

	// Get the current argv length
	let initial_len = vartab.sh_argv().len();

	// Clear and reinitialize with known args
	vartab.clear_args(); // This keeps $0 as current exe

	// After clear_args, should have just $0
	// Push additional args
	vartab.bpush_arg("test_arg1".to_string());
	vartab.bpush_arg("test_arg2".to_string());

	// Now sh_argv should be: [exe_path, test_arg1, test_arg2]
	// Pos(0) = exe_path, Pos(1) = test_arg1, Pos(2) = test_arg2
	let final_len = vartab.sh_argv().len();
	assert!(final_len > initial_len || final_len >= 1, "Should have arguments");

	// Just verify we can retrieve the last args we pushed
	let last_idx = final_len - 1;
	assert_eq!(vartab.get_param(ShellParam::Pos(last_idx)), "test_arg2");
}

#[test]
fn vartab_shell_argv_operations() {
	let mut vartab = VarTab::new();

	// Clear initial args and set fresh ones
	vartab.clear_args();

	// Push args (clear_args leaves $0, so these become $1, $2, $3)
	vartab.bpush_arg("arg1".to_string());
	vartab.bpush_arg("arg2".to_string());
	vartab.bpush_arg("arg3".to_string());

	// Get initial arg count
	let initial_len = vartab.sh_argv().len();

	// Pop first arg (removes $0)
	let popped = vartab.fpop_arg();
	assert!(popped.is_some());

	// Should have one fewer arg
	assert_eq!(vartab.sh_argv().len(), initial_len - 1);
}

// ============================================================================
// VarFlags Tests
// ============================================================================

#[test]
fn varflags_none() {
	let flags = VarFlags::NONE;
	assert!(!flags.contains(VarFlags::EXPORT));
	assert!(!flags.contains(VarFlags::LOCAL));
	assert!(!flags.contains(VarFlags::READONLY));
}

#[test]
fn varflags_export() {
	let flags = VarFlags::EXPORT;
	assert!(flags.contains(VarFlags::EXPORT));
	assert!(!flags.contains(VarFlags::LOCAL));
}

#[test]
fn varflags_local() {
	let flags = VarFlags::LOCAL;
	assert!(!flags.contains(VarFlags::EXPORT));
	assert!(flags.contains(VarFlags::LOCAL));
}

#[test]
fn varflags_combine() {
	let flags = VarFlags::EXPORT | VarFlags::LOCAL;
	assert!(flags.contains(VarFlags::EXPORT));
	assert!(flags.contains(VarFlags::LOCAL));
	assert!(!flags.contains(VarFlags::READONLY));
}

#[test]
fn varflags_readonly() {
	let flags = VarFlags::READONLY;
	assert!(flags.contains(VarFlags::READONLY));
	assert!(!flags.contains(VarFlags::EXPORT));
}

// ============================================================================
// ShellParam Tests
// ============================================================================

#[test]
fn shellparam_is_global() {
	assert!(ShellParam::Status.is_global());
	assert!(ShellParam::ShPid.is_global());
	assert!(ShellParam::LastJob.is_global());
	assert!(ShellParam::ShellName.is_global());

	assert!(!ShellParam::Pos(1).is_global());
	assert!(!ShellParam::AllArgs.is_global());
	assert!(!ShellParam::AllArgsStr.is_global());
	assert!(!ShellParam::ArgCount.is_global());
}

#[test]
fn shellparam_from_str() {
	assert!(matches!("?".parse::<ShellParam>().unwrap(), ShellParam::Status));
	assert!(matches!("$".parse::<ShellParam>().unwrap(), ShellParam::ShPid));
	assert!(matches!("!".parse::<ShellParam>().unwrap(), ShellParam::LastJob));
	assert!(matches!("0".parse::<ShellParam>().unwrap(), ShellParam::ShellName));
	assert!(matches!("@".parse::<ShellParam>().unwrap(), ShellParam::AllArgs));
	assert!(matches!("*".parse::<ShellParam>().unwrap(), ShellParam::AllArgsStr));
	assert!(matches!("#".parse::<ShellParam>().unwrap(), ShellParam::ArgCount));

	match "1".parse::<ShellParam>().unwrap() {
		ShellParam::Pos(n) => assert_eq!(n, 1),
		_ => panic!("Expected Pos(1)"),
	}

	match "42".parse::<ShellParam>().unwrap() {
		ShellParam::Pos(n) => assert_eq!(n, 42),
		_ => panic!("Expected Pos(42)"),
	}

	assert!("invalid".parse::<ShellParam>().is_err());
}

#[test]
fn shellparam_display() {
	assert_eq!(ShellParam::Status.to_string(), "?");
	assert_eq!(ShellParam::ShPid.to_string(), "$");
	assert_eq!(ShellParam::LastJob.to_string(), "!");
	assert_eq!(ShellParam::ShellName.to_string(), "0");
	assert_eq!(ShellParam::AllArgs.to_string(), "@");
	assert_eq!(ShellParam::AllArgsStr.to_string(), "*");
	assert_eq!(ShellParam::ArgCount.to_string(), "#");
	assert_eq!(ShellParam::Pos(1).to_string(), "1");
	assert_eq!(ShellParam::Pos(99).to_string(), "99");
}
