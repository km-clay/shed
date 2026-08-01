<h1 align="center">
      <a href="https://en.wikipedia.org/wiki/Bourne_shell">sh</a><a href="https://en.wikipedia.org/wiki/Ed_(text_editor)">ed</a>
</h1>
<p align="center">
	<img alt="Coveralls" src="https://img.shields.io/coverallsCoverage/github/km-clay/shed?color=%23178f2d">
	<img alt="Test Suite Status" src="https://img.shields.io/github/actions/workflow/status/km-clay/shed/ci.yml?label=tests">
	<img alt="GitHub last commit" src="https://img.shields.io/github/last-commit/km-clay/shed">
</p>
<h6 align="center">
  A modern POSIX shell focusing on smooth line editing and rich interactive features.
</h6>
<img width="1925" height="1026" alt="shed" src="https://github.com/user-attachments/assets/c7d7d410-6cf5-4a04-a2be-ef3b6146e155" />

<!-- TOC markers (for autoscript) -->
<!--tocbeg-->
## Table of Contents
- [Why shed?](#why-shed)
- [Features](#features)
  - [Line Editor](#line-editor)
    - [Emacs mode](#emacs-mode)
    - [Vi mode](#vi-mode)
    - [Ex mode](#ex-mode)
  - [Interactive Documentation](#interactive-documentation)
  - [Keymaps](#keymaps)
  - [Autocmds](#autocmds)
  - [Command History](#command-history)
  - [Alias Expansion](#alias-expansion)
  - [Syntax Highlighting](#syntax-highlighting)
  - [Status Line](#status-line)
  - [IPC Socket](#ipc-socket)
  - [Shell Language](#shell-language)
  - [Job Control](#job-control)
  - [Configuration](#configuration)
  - [Prompt](#prompt)
- [Building](#building)
  - [Arch Linux (AUR)](#arch-linux-aur)
  - [Cargo](#cargo)
  - [Nix](#nix)
- [Known issues](#known-issues)
- [AI Usage](#ai-usage)
- [Notes](#notes)
<!--tocend-->


## Why shed?

I started working on `shed` because I have yet to find an unopinionated shell with genuinely smooth out-of-the-box line editing features. `bash` and `zsh` are both POSIX compliant in their syntax, but bash's readline and zsh's zle are both really clunky to work with (in my opinion). `fish` has pretty decent line editing, but wants me to learn their scripting language instead of the one that everyone else uses. There just wasn't a perfect solution. `shed` is the answer I came up with for this problem.

## Features

### Line Editor

`shed` includes a built-in modal line editor, written from scratch. It supports both **vim** and **emacs** editing modes. `shed`'s line editor distinguishes itself by treating multi-line operations as first class. It's effectively a terminal-embedded text editor, rather than a traditional shell line editor.

#### Emacs mode
The default. Supports the usual readline-style bindings (`Ctrl+A`/`Ctrl+E`/`Ctrl+W`, transposition, kill ring, etc.), with multi-line awareness layered on top.

`Alt+;` provides access to `ex mode`

#### Vi mode
Enable with `shopt set.vi=true` or `set -o vi`. Implements normal, insert, visual, and replace modes with wide coverage of the standard vi motions and operators, including several vim-specific extensions like `g?`, and also implements registers and macro recording.

`:` from normal mode enters `ex mode`.

#### Ex mode
A secondary command prompt that has many tools for both controlling the line editor, and executing shell commands without losing the one you are currently working on.

| Command               | Description                                                                                                       |
|-----------------------|-------------------------------------------------------------------------------------------------------------------|
| `:!cmd`               | Executes `cmd` as though it had been submitted from the main prompt. Triggers pre-cmd and post-cmd autocmds.      |
| `:h topic`            | Opens the help page relevant to the given topic, e.g. `:h variables`.                                             |
| `:stash`              | Stores the main editor's buffer, then clears the main editor. `:stash pop` restores the buffer.                   |
| `:normal <keys>`      | Runs a normal-mode key sequence. Useful for `keymaps`.                                                            |
| `:w file` / `:w !cmd` | Write the current buffer to a file, or pipe to a shell command. e.g. `:w !wl-copy` to pipe it to your clipboard.  |
| `:r file` / `:r !cmd` | Read a file into the buffer, or read shell output, e.g. `:r !wl-paste` to read clipboard contents into the buffer.|

For a comprehensive list, as well as more in-depth stuff like line-addressing, check out the [ex mode docs](./include/help/ex.txt)

---

### Interactive Documentation

`shed` ships with documentation for all of its builtin utilities, unique features, common commands, and POSIX stuff that's easy to forget like parameter expansion. This documentation is accessible via the `help` builtin.

The help topics are opened in a custom interactive pager that uses a basic hypertext markup language. The content contains links to other topics that can be clicked on, or navigated to by using Tab to show link hints.

<img width="1925" height="1028" alt="shed_help" src="https://github.com/user-attachments/assets/a6cf2031-5f01-4260-9104-bcf488ef1778" />

Examples:
```bash
help param     # opens the parameter expansion page
help cd         # opens the builtins page and jumps to the 'cd' entry
```

Additionally, the help pages can be reached using `ex mode` like `:h some-topic`. This allows you to open the pager without losing the command that you're currently editing. Really convenient for when you forget what the `tar` flags do.

---

### Keymaps

The `keymap` builtin lets you bind key sequences to actions in any editor mode:

```sh
keymap -i 'jk' '<Esc>'                           # exit insert mode with jk
keymap -n '<leader>a' 'ggVG'                     # select all with leader + a in normal mode
keymap -n '<C-L>' '<CMD>clear<CR>'               # Ctrl+L runs clear in normal mode
keymap -e '<C-O>' '<CMD>my_function<CR>'         # Ctrl+O runs a shell function in emacs mode
keymap -n 'ys' '<CMD>function1<CR><CMD>function2<CR>' # Chain two functions together
keymap -v '<C-x>' '<CMD>!wl-copy<CR>'            # Ctrl+X cuts the current visual selection to the clipboard
keymap -v '<C-c>' '<CMD>!tee >(wl-copy)<CR>'     # Ctrl+C copies the current visual selection to the clipboard
keymap -i '<C-v>' '<CMD>r!wl-paste<CR>'          # Ctrl+V in insert mode pastes from the clipboard
```

Mode flags: `-n` normal, `-i` insert, `-v` visual, `-x` ex, `-o` operator-pending, `-r` replace, `-e` emacs. Flags can be combined (`-ni` binds in both normal and insert).
The leader key can be defined using `shopt prompt.leader=<some_key>`.

Keys use vim-style notation: `<C-X>` (Ctrl), `<A-X>` (Alt), `<S-X>` (Shift), `<CR>`, `<Esc>`, `<Tab>`, `<Space>`, `<BS>`, arrow keys, etc. `<CMD>...<CR>` executes a shell command inline.

Use `keymap --remove <keys>` to remove a binding that matches the given key sequence.

Similar to `zsh`'s line editor widgets, shell commands run via keymaps have read-write access to the line editor state through special variables:
* `$BUFFER` - Current line contents
* `$CURSOR` - Cursor position, can be written back as either a raw byte index or as 'row:col'
* `$ANCHOR` - Visual selection anchor
* `$KEYS` - Keys that the line editor will execute upon returning. This can be used to script arbitrary input.

Modifying these variables from within the command updates the editor when it returns.

---

### Autocmds

The `autocmd` builtin registers shell commands to run on specific events. Many events expose context variables that autocmds can use for conditional logic:

```sh
autocmd post-change-dir 'echo "moved to $NEW_DIR"'
autocmd on-exit 'echo goodbye'
autocmd on-time-report 'echo "$TIME_CMD took $TIME_REAL_FMT"'
```

Available events:

| Event                                                                 | When it fires                     |
|-----------------------------------------------------------------------|-----------------------------------|
| `pre-cmd`, `post-cmd`                                                 | Before/after command execution    |
| `pre-change-dir`, `post-change-dir`                                   | Before/after cwd changes          |
| `pre-prompt`, `post-prompt`                                           | Before/after prompt display       |
| `pre-mode-change`, `post-mode-change`                                 | Before/after editor mode switch   |
| `on-history-open`, `on-history-close`, `on-history-select`            | History search events             |
| `on-completion-start`, `on-completion-cancel`, `on-completion-select` | Tab completion events             |
| `on-job-finish`                                                       | Background job completes          |
| `on-time-report`                                                      | `time`-prefixed command completes |
| `on-command-not-found`                                                | Command name can't be resolved    |
| `on-idle-timeout`                                                     | Prompt idle past the idle timeout |
| `on-exit`                                                             | Shell is exiting                  |

Use `-c` to clear all autocmds for an event. Context variables (e.g. `$NEW_DIR`, `$TIME_REAL_MS`) are scoped to the autocmd execution and documented in `help autocmd`.

---

### Command History

`shed` uses an `sqlite` database to store your command history. While this is slightly heavier than the usual flat text file approach used by shells like `bash` and `zsh`, it has some advantages:
* Shared across sessions: All open `shed` instances read from and write to the same history in real time - commands entered in one terminal are immediately available in all the others via `hist --pull` or `Ctrl + R` history searching.
* Queryable: Power users can query the database directly with any SQLite tool for custom analysis, rather than needing a custom history file parser
* Richer metadata: Each entry stores timestamp, working directory, runtime duration in milliseconds, and exit code.
* Safe writes: SQLite's transaction model means a hard kill mid-write won't leave your history file in a broken state.
* Direct access via `hist`: The `hist` builtin allows you to interact with the database directly, and exposes flags that can be composed to create pseudo-SQL queries, e.g. `hist --starts-with 'echo' --after '10 minutes ago' --delete` will delete all commands starting with echo that were entered within the last 10 minutes.

Additionally, `shed` implements a unique feature for interacting with your history. Consecutive commands can be concatenated with `;` or `&&` as separators if you scroll with `Ctrl` or `Shift` respectively. Useful if you need to re-run a batch of commands.

---

### Alias Expansion

`shed` supports fish-style alias expansion on the prompt. When enabled (`shopt prompt.expand_aliases=true`, the default), aliases expand visually as you type. Press space or enter after an alias and the real command appears in the buffer before execution. This lets you see and edit the expanded form before running it. Useful for keeping your command history readable.

Expansion only applies to words in command position (not arguments).

---

### Syntax Highlighting

`shed`'s syntax highlighter is fully configurable through `shopt highlight.*`:

```sh
shopt highlight.valid_command="bold green"
shopt highlight.string="yellow"
shopt highlight.variable="#89b4fa"
shopt highlight.comment="italic bright black on white"
shopt highlight.operator="bold magenta"
```

Style descriptions support named colors, `bright` variants, modifiers (`bold`, `italic`, `underline`, `dim`, `strikethrough`), hex colors (`#rrggbb`), and backgrounds with `on`. Raw ANSI escapes are also accepted.

---

### Status Line

`shed` provides an API for implementing your own status line, using the `shopt statline.*` options:

```sh
shopt statline.enable=true # enables the status line, anchors the prompt to it
shopt statline.left_string="\u@\h"
shopt statline.middle_string='$?'
shopt statline.right_string=""
```

The status line strings expand prompt escape sequences.

An example implementation of a `shed` status line can be found [here](./examples/status_line.sh).

---

### IPC Socket

`shed` exposes a Unix socket that other processes can use to interact with it. The path to this socket is held per-instance in the `$SHED_SOCK` environment variable. Subscribing to the socket gives you a stream of event data, and there are several requests that can be written to the socket to control `shed` in various ways.

Among other things, it's possible to read from and write to the line editor directly via the socket. This enables total extensibility of the editor by anything that can interact with a Unix socket. The `remote` editing mode causes input keys to be broadcast over the socket, to be consumed by subscribers that can use those inputs to control the editor remotely.

More info can be found in [the socket help page](./include/help/socket.txt).

---

### Shell Language

`shed`'s scripting language follows the specification laid out by [IEEE Std 1003.1-2024 Shell & Utilities](https://pubs.opengroup.org/onlinepubs/9799919799/).
It is capable of sourcing any POSIX-portable shell script, or I'll eat my hat. <sub>(hats eaten so far: 2)</sub>

---

### Job Control

`shed` implements the usual shell job control utilities.

- Background execution with `&`
- Suspend foreground jobs with Ctrl+Z
- `fg`, `bg`, `jobs`, `disown` with flags (`-l`, `-p`, `-r`, `-s`, `-h`, `-a`)

---

### Configuration

Shell options are managed through `shopt`:

```sh
shopt core.autocd=true                        # cd by typing a directory path
shopt core.dotglob=true                       # include hidden files in globs
shopt line.highlight=false                    # toggle syntax highlighting
shopt set.vi=true                             # editor mode
shopt core.max_hist=5000                      # history size
shopt highlight.valid_command="bold green"    # customize highlight colors
```

Shed searches for an rc file in these paths, in decreasing order of priority:

1. $SHED_RC
2. $XDG_CONFIG_HOME/shedrc
3. $HOME/.config/shed/shedrc
4. $HOME/.shedrc

---

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
| `\c{desc}` | Named color styling (e.g. `\c{bold green}`, `\c{#ff5733}`, `\c{reset}`) |
| `\@name` | Execute a shell function and embed its output |

The `\@` escape is particularly useful. It lets you embed the output of any shell function directly in your prompt. Define a function that prints something, then reference it in your prompt string:

```sh
gitbranch() { git branch --show-current 2>/dev/null; }
export PS1='\u@\h \W \@gitbranch \$ '
```

If `shed` receives `SIGUSR1` while in interactive mode, it will refresh and redraw the prompt. This can be used to create asynchronous, dynamic prompt content.

Additionally, `echo` has a `-p` flag that expands prompt escape sequences. This provides an interface for accessing the information provided by these escape sequences from any context. For instance, using \c{...} instead of raw ANSI codes:

```sh
echo -p '\c{bold green on black}Build succeeded\c{reset} in \T'
```

`shed` also provides a `PSR` for expanding content that is justified to the right side of the prompt.

---


## Building

### Arch Linux (AUR)

```sh
yay -S shed-sh
```

Or your favorite AUR helper (`paru -S shed-sh`, etc).

### Cargo

Requires Rust (edition 2024).

```sh
git clone https://github.com/km-clay/shed.git
cd shed
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

## Known issues

* The expanded content from the `PSR` variable doesn't work well with multi-line content
* Aliases can't be used in the same script that defines them.

## AI Usage

AI has been used to assist with development in some areas of this codebase.
Full disclosure can be found here: [AI_POLICY.md](./AI_POLICY.md).

## Notes

`shed` is experimental software and is currently under active development. Using an experimental shell is inherently risky business, there is no guarantee that your computer will not explode when you run this. That being said, I've been daily driving it for 8 months at the time of writing and my computer has not exploded yet. Use it at your own risk, the software is provided as-is.
