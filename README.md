# shed

A Linux shell written in Rust. The name is a nod to the original Unix utilities `sh` and `ed`. It's a shell with a heavy emphasis on smooth line editing.

<img width="506" height="407" alt="shed" src="https://github.com/user-attachments/assets/5333dd47-ae1b-45cd-8729-b623f586b10e" />

## Features

### Line Editor

`shed` includes a built-in `vim` emulator as its line editor, written from scratch. It aims to provide a more precise vim-like editing experience at the shell prompt.

- **Normal mode** - motions (`w`, `b`, `e`, `f`, `t`, `%`, `0`, `$`, etc.), verbs (`d`, `c`, `y`, `p`, `r`, `x`, `~`, etc.), text objects (`iw`, `aw`, `i"`, `a{`, `is`, etc.), registers, `.` repeat, `;`/`,` repeat, and counts
- **Insert mode** - insert, append, replace, with Ctrl+W word deletion and undo/redo
- **Visual mode** - character-wise and visual line selection with operator support
- **Real-time syntax highlighting** - commands, keywords, strings, variables, redirections, and operators are colored as you type
- **Tab completion** - context-aware completion for commands, file paths, and variables

### Prompt

The prompt string supports escape sequences for dynamic content:

| Escape | Description |
|--------|-------------|
| `\u` | Username |
| `\h`, `\H` | Hostname (short / full) |
| `\w`, `\W` | Working directory (full / basename, truncation configurable via `shopt`) |
| `\$` | `$` for normal users, `#` for root |
| `\t`, `\T` | Last command runtime (milliseconds / human-readable) |
| `\s` | Shell name |
| `\e[...` | ANSI escape sequences for colors and styling |
| `\!name` | Execute a shell function and embed its output |

The `\!` escape is particularly useful. It lets you embed the output of any shell function directly in your prompt. Define a function that prints something, then reference it in your prompt string:

```sh
gitbranch() { git branch --show-current 2>/dev/null; }
export PS1='\u@\h \W \!gitbranch \$ '
```

Additionally, `echo` now has a `-p` flag that expands prompt escape sequences, similar to how the `-e` flag expands conventional escape sequences.

### Shell Language

shed's scripting language contains all of the essentials.

- **Control flow** - `if`/`elif`/`else`, `for`, `while`, `until`, `case` with pattern matching and fallthrough
- **Functions** - user-defined with local variable scoping, recursion depth limits, and `return`
- **Pipes and redirections** - `|`, `|&` (pipe stderr), `<`, `>`, `>>`, `<<` (heredoc), `<<<` (herestring), fd duplication (`2>&1`)
- **Process substitution** - `<(...)` and `>(...)`
- **Command substitution** - `$(...)` and backticks
- **Arithmetic expansion** - `$((...))` with `+`, `-`, `*`, `/`, `%`, `**`
- **Parameter expansion** - `${var}`, `${var:-default}`, `${var:=default}`, `${var:+alt}`, `${var:?err}`, `${#var}`, `${var#pattern}`, `${var%pattern}`, `${var/pat/rep}`
- **Brace expansion** - `{a,b,c}`, `{1..10}`, `{1..10..2}`
- **Glob expansion** - `*`, `?`, `[...]` with optional dotglob
- **Tilde expansion** - `~` and `~user`
- **Logical operators** - `&&`, `||`, `&` (background)
- **Test expressions** - `[[ ... ]]` with file tests, string comparison, arithmetic comparison, and regex matching
- **Subshells** - `(...)` for isolated execution
- **Variable attributes** - `export`, `local`, `readonly`

### Job Control

- Background execution with `&`
- Suspend foreground jobs with Ctrl+Z
- `fg`, `bg`, `jobs`, `disown` with flags (`-l`, `-p`, `-r`, `-s`, `-h`, `-a`)
- Process group management and proper signal forwarding

### Configuration

Shell options are managed through `shopt`:

```sh
shopt core.autocd=true          # cd by typing a directory path
shopt core.dotglob=true         # include hidden files in globs
shopt prompt.highlight=false    # toggle syntax highlighting
shopt prompt.edit_mode=vi       # editor mode
shopt core.max_hist=5000        # history size
```

The rc file is loaded from `~/.shedrc` on startup.

## Building

### Cargo

Requires Rust (edition 2024).

```sh
git clone https://github.com/km-clay/shed.git
cargo build --release
```

The binary will be at `target/release/shed`.

### Nix

A flake is provided with a NixOS module, a Home Manager module, and a simple overlay that adds `pkgs.shed`.

```sh
# Build and run directly
nix run github:km-clay/shed

# Or add to your flake inputs
inputs.shed.url = "github:km-clay/shed";
```

To use the NixOS module:

```nix
# flake.nix outputs
nixosConfigurations.myhost = nixpkgs.lib.nixosSystem {
  modules = [
    shed.nixosModules.shed
    # ...
  ];
};
```

Or with Home Manager:

```nix
imports = [ shed.homeModules.shed ];
```

And the overlay:

```nix
pkgs = import nixpkgs {
	overlays = [
		shed.overlays.default
	];
};
```

## Status

`shed` is experimental software and is currently under active development. It covers most day-to-day interactive shell usage and a good portion of POSIX shell scripting, but it is not yet fully POSIX-compliant. There is no guarantee that your computer will not explode when you run this. Use it at your own risk, the software is provided as-is.

## Why shed?

This originally started as an educational hobby project, but over the course of about a year or so it's taken the form of an actual daily-drivable shell. I mainly wanted to create a shell where line editing is more frictionless than standard choices. I use vim a lot so I've built up a lot of muscle memory, and a fair amount of that muscle memory does not apply to vi modes in `bash`/`zsh`. For instance, the standard vi mode in `zsh` does not support selection via text objects. I wanted to create a line editor that actually feels like you're in an editor.
