# Test Coverage Audit for Fern Shell

## Current Test Statistics
- **Total Tests**: 104 (all passing)
- **Test Files**: 8 modules

### Test Distribution
- `error.rs`: 13 tests - Error message formatting
- `expand.rs`: 26 tests - Variable/parameter expansion, aliases
- `getopt.rs`: 3 tests - Option parsing
- `lexer.rs`: 7 tests - Tokenization
- `parser.rs`: 17 tests - AST parsing (if/case/loop/for)
- `readline.rs`: 34 tests - Vi mode, linebuf operations, text objects
- `script.rs`: 3 tests - Script execution
- `term.rs`: 6 tests - Terminal operations

---

## Coverage Analysis

### ✅ Well-Covered Areas

#### Lexer (`lexer.rs` - 7 tests)
- Basic tokenization
- String handling
- Operators

#### Parser (`parser.rs` - 17 tests)
- Control structures (if/elif/else, case, while/until, for loops)
- Command parsing
- Nested structures

#### Variable Expansion (`expand.rs` - 26 tests)
- Parameter expansion (`${var}`)
- Default values (`${var:-default}`, `${var:=default}`)
- Alternative values (`${var:+alt}`)
- String operations (length, substring, prefix/suffix removal)
- Pattern replacement
- Alias expansion

#### Vi Mode (`readline.rs` - 34 tests)
- Insert mode commands
- Normal mode commands
- Cursor motions
- Text objects (quoted, delimited)
- Line operations
- Unicode/grapheme handling
- Delete/change operations

#### Error Handling (`error.rs` - 13 tests)
- Error message formatting
- Error types

---

## ⚠️ MISSING or INCOMPLETE Coverage

### Critical Missing Tests

#### 1. **Tab Completion** (`complete.rs`) - **0 tests**
**Recently implemented, NO TESTS!**
- ❌ Command completion (PATH, builtins, functions, aliases)
- ❌ Filename completion
- ❌ Completion after `=` (assignments/options)
- ❌ Context detection (command vs argument)
- ❌ Cycling behavior (Tab/Shift+Tab)
- ❌ Glob expansion preservation (trailing slash, leading `./`)
- ❌ Nested structure completion (`$(command` etc.)

**Priority**: **CRITICAL** - This is a major new feature with complex logic

#### 2. **Syntax Highlighting** (`highlight.rs`) - **0 tests**
**Recently implemented, NO TESTS!**
- ❌ Token-level highlighting (commands, args, operators, keywords)
- ❌ Sub-token highlighting (strings, variables, globs)
- ❌ Recursive annotation (command substitutions, subshells)
- ❌ Marker insertion/ordering
- ❌ Style stack behavior for nested constructs
- ❌ Command validation (green/red for valid/invalid)

**Priority**: **CRITICAL** - Complex recursive logic needs coverage

#### 3. **File Descriptor Redirections** (`parse/mod.rs`, `procio.rs`) - **0 tests**
**Recently fixed, NO TESTS!**
- ❌ `2>&1` style fd duplication
- ❌ `<&0` input duplication
- ❌ Multiple redirections in sequence
- ❌ Redirection with incomplete syntax (e.g., `2>&` with LEX_UNFINISHED)
- ❌ Redirection order matters (`2>&1 > file` vs `> file 2>&1`)

**Priority**: **HIGH** - Recently had bugs, needs regression tests

#### 4. **History** (`history.rs`) - **0 tests**
- ❌ History file I/O
- ❌ History navigation (up/down)
- ❌ Prefix matching
- ❌ Autosuggestions
- ❌ History persistence

**Priority**: **HIGH** - Core interactive feature

#### 5. **Job Control** (`jobs.rs`, `builtin/jobctl.rs`) - **0 tests**
- ❌ Background jobs (`&`)
- ❌ Job suspension (Ctrl+Z)
- ❌ `fg`/`bg` commands
- ❌ `jobs` listing
- ❌ Job status tracking
- ❌ Process group management

**Priority**: **HIGH** - Complex system interaction

#### 6. **Signal Handling** (`signal.rs`) - **0 tests**
- ❌ SIGINT (Ctrl+C)
- ❌ SIGTSTP (Ctrl+Z)
- ❌ SIGCHLD handling
- ❌ Signal delivery to process groups
- ❌ Signal masking

**Priority**: **MEDIUM** - Hard to test but important

#### 7. **I/O Redirection** (`procio.rs`) - **Partial**
- ✅ Basic redirect parsing (in parser tests)
- ❌ File opening modes (>, >>, <, <<<, <<)
- ❌ Pipe creation and management
- ❌ IoStack frame management
- ❌ Redirect restoration (RedirGuard drop)
- ❌ Error handling (file not found, permission denied)

**Priority**: **MEDIUM**

#### 8. **Builtins** - **Minimal Coverage**
- ❌ `cd` - directory changing, OLDPWD, error cases
- ❌ `echo` - options (-n, -e), escape sequences
- ❌ `export` - variable export, listing
- ❌ `read` - reading into variables, IFS handling
- ❌ `alias` - alias management, recursive expansion
- ❌ `source` - sourcing files, error handling
- ❌ `shift` - argument shifting
- ❌ `shopt` - shell options
- ❌ `test`/`[` - conditional expressions
- ❌ Flow control (`break`, `continue`, `return`, `exit`)
- ❌ Job control (`fg`, `bg`, `jobs`)

**Priority**: **MEDIUM** - Each builtin should have basic tests

#### 9. **State Management** (`state.rs`) - **0 tests**
- ❌ Variable scoping (global vs local)
- ❌ Function storage/retrieval
- ❌ Alias storage/retrieval
- ❌ VarFlags (EXPORT, LOCAL, READONLY)
- ❌ Scope push/pop (descend/ascend)
- ❌ Shell parameters ($?, $$, $!, etc.)

**Priority**: **MEDIUM** - Core shell state

#### 10. **Glob Expansion** (`expand.rs`) - **Minimal**
- ✅ Basic variable expansion tested
- ❌ Glob patterns (*, ?, [...])
- ❌ Brace expansion ({a,b,c})
- ❌ Tilde expansion (~, ~user)
- ❌ Glob edge cases (no matches, multiple matches)
- ❌ Trailing slash preservation (recently fixed)

**Priority**: **MEDIUM**

#### 11. **Command Execution** (`parse/execute.rs`) - **Integration Only**
- ✅ Script execution tests exist
- ❌ Pipeline execution
- ❌ Command substitution execution
- ❌ Subshell execution
- ❌ Process substitution
- ❌ Exit status propagation
- ❌ Error handling in execution

**Priority**: **MEDIUM**

#### 12. **Lexer Edge Cases** - **Basic Coverage**
- ✅ Basic tokenization
- ❌ Incomplete tokens (unfinished strings, unclosed quotes)
- ❌ LEX_UNFINISHED mode behavior
- ❌ Escape sequences in various contexts
- ❌ Complex nesting (strings in command subs in strings)
- ❌ Comments
- ❌ Here documents/here strings

**Priority**: **LOW-MEDIUM**

---

## 📋 Recommended Test Additions

### Immediate Priority (Next Session)

1. **Tab Completion Tests** (`tests/complete.rs`)
   - Command completion from PATH
   - Builtin/function/alias completion
   - Filename completion
   - Completion after `=`
   - Context detection
   - Cycling behavior
   - Edge cases (empty input, no matches, nested structures)

2. **Syntax Highlighting Tests** (`tests/highlight.rs`)
   - Basic token highlighting
   - Recursive annotation
   - Marker priority/ordering
   - Nested constructs
   - Command validation colors

3. **Redirect Tests** (`tests/redirect.rs` or in `parser.rs`)
   - File descriptor duplication (`2>&1`, `<&0`)
   - Order-dependent behavior
   - Multiple redirects
   - Error cases

### High Priority

4. **History Tests** (in `tests/readline.rs` or separate file)
   - File I/O
   - Navigation
   - Prefix matching
   - Autosuggestions

5. **Builtin Tests** (`tests/builtins.rs`)
   - Test each builtin's core functionality
   - Error cases
   - Edge cases

6. **Job Control Tests** (`tests/jobs.rs`)
   - Background execution
   - Suspension/resumption
   - Status tracking

### Medium Priority

7. **State Management Tests** (`tests/state.rs`)
8. **I/O Stack Tests** (in `tests/redirect.rs`)
9. **Glob Expansion Tests** (extend `tests/expand.rs`)
10. **Execution Tests** (extend `tests/script.rs`)

---

## 📊 Coverage Metrics

**Rough Estimates**:
- **Core parsing/lexing**: 60% covered
- **Variable expansion**: 70% covered
- **Vi mode/linebuf**: 80% covered
- **Tab completion**: **0% covered** ⚠️
- **Syntax highlighting**: **0% covered** ⚠️
- **Redirections**: **20% covered** ⚠️
- **Job control**: **0% covered**
- **History**: **0% covered**
- **Builtins**: **10% covered**
- **State management**: **0% covered**

**Overall Estimated Coverage**: ~35-40%

---

## 🎯 Goal Coverage Targets

- **Critical path features**: 80%+ (parsing, execution, expansion)
- **Interactive features**: 70%+ (completion, highlighting, history)
- **System interaction**: 50%+ (jobs, signals, I/O)
- **Edge cases**: 40%+ (error handling, malformed input)

---

## Notes

- 5 integration tests in `readline.rs` are currently disabled (commented out)
- Script execution tests exist but are minimal (only 3 tests)
- No fuzzing or property-based tests
- No performance/benchmark tests
