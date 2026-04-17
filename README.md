# shed

A Linux shell written in Rust. The name is a nod to the original Unix utilities `sh` and `ed`. It's a shell with a heavy emphasis on smooth line editing and general interactive UX improvements over existing shells.

<img width="506" height="407" alt="shed" src="https://github.com/user-attachments/assets/3945f663-a361-4418-bf20-0c4eaa2a36d2" />

## Features

### Line Editor

`shed` includes a built-in modal line editor, written from scratch. It supports both **vim** and **emacs** editing modes. `shed`'s line editor distinguishes itself by treating multi-line operations as first class. It's effectively a terminal-embedded text editor, rather than a traditional shell line editor.

---

### Fuzzy Tab Completion/History Search

`shed` comes with fuzzy completion and history searching out of the box. It has it's own internal fuzzyfinder implementation, so `fzf` is not a dependency.

<img width="380" height="270" alt="shed_comp" src="https://github.com/user-attachments/assets/d317387e-4c33-406a-817f-1c183afab749" />
<img width="380" height="270" alt="shed_search" src="https://github.com/user-attachments/assets/5109eb14-5c33-46bb-ab39-33c60ca039a8" />

---

### Keymaps

The `keymap` builtin lets you bind key sequences to actions in any editor mode:

```sh
keymap -i 'jk' '<Esc>'                           # exit insert mode with jk
keymap -n '<C-L>' '<CMD>clear<CR>'               # Ctrl+L runs clear in normal mode
keymap -e '<C-O>' '<CMD>my_function<CR>'         # Ctrl+O runs a shell function in emacs mode
keymap -n 'ys' '<CMD>function1<CR><CMD>function2<CR>' # Chain two functions together
keymap -i '<C-P>' '<CMD>w!wl-copy<CR>'           # Ctrl+P pipes the buffer content to the clipboard
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
autocmd post-change-dir 'echo "moved to $_NEW_DIR"'
autocmd on-exit 'echo goodbye'
autocmd on-time-report 'echo "$_TIME_CMD took $_TIME_REAL_FMT"'
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
| `on-exit`                                                             | Shell is exiting                  |

Use `-c` to clear all autocmds for an event. Context variables (e.g. `$_NEW_DIR`, `$_TIME_REAL_MS`) are scoped to the autocmd execution and documented in `help autocmd`.

---

### Command History

`shed` uses an `sqlite` database to store your command history. While this is slightly heavier than the usual flat text file approach used by shells like `bash` and `zsh`, it has some advantages:
* Shared across sessions: All open `shed` instances read from and write to the same history in real time - commands entered in one terminal are immediately available in all the others.
* Queryable: Power users can query the database directly with any SQLite tool for custom analysis, rather than needing a custom history file parser
* Richer metadata: Each entry stores timestamp, working directory, runtime duration in milliseconds, and exit code.
* Safe writes: SQLite's transaction model means a hard kill mid-write won't leave your history file in a broken state.
* Direct access via `hist`: The `hist` builtin allows you to interact with the database directly, and exposes flags that can be composed to create pseudo-SQL queries, e.g. `hist --starts-with 'echo' --after '10 minutes ago' --delete` will delete all commands starting with echo that were entered within the last 10 minutes.

---

### Alias Expansion

`shed` supports fish-style alias expansion on the prompt. When enabled (`shopt prompt.expand_aliases=true`, the default), aliases expand visually as you type — press space or enter after an alias and the real command appears in the buffer before execution. This lets you see and edit the expanded form before running it.

Expansion only applies to words in command position (not arguments).

---

### Syntax Highlighting

`shed`'s syntax highlighter is fully configurable through `shopt highlight.*`:

```sh
shopt highlight.valid_command="bold green"
shopt highlight.string="yellow"
shopt highlight.variable="#89b4fa"
shopt highlight.comment="italic bright black"
shopt highlight.operator="bold magenta"
shopt highlight.selection="black on white"
```

Style descriptions support named colors, `bright` variants, modifiers (`bold`, `italic`, `underline`, `dim`, `strikethrough`), hex colors (`#rrggbb`), and backgrounds with `on`. Raw ANSI escapes are also accepted.

---

### IPC Socket

`shed` exposes a Unix socket that other processes can use to interact with it. The path to this socket is held per-instance in the `$SHED_SOCK` environment variable. Subscribing to the socket gives you a stream of event data, and there are several requests that can be written to the socket to control `shed` in various ways.

Among other things, it's possible to read from and write to the line editor directly via the socket. This enables total extensibility of the editor by anything that can interact with a Unix socket. The `remote` editing mode causes input keys to be broadcast over the socket, to be consumed by subscribers that can use those inputs to control the editor remotely.

More info can be found in [./doc/socket.txt](./doc/socket.txt).

---

### Shell Language

`shed`'s scripting language follows the specification laid out by [IEEE Std 1003.1-2024 Shell & Utilities](https://pubs.opengroup.org/onlinepubs/9799919799/).
It is capable of sourcing any POSIX-portable shell script.

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

The rc file is loaded from `~/.shedrc` on startup.

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

### Built-in Documentation

`shed` ships with documentation for all of its builtin commands, unique features, and POSIX stuff thats easy to forget like parameter expansion. This documentation is accessible via the `help` command. The help pages are modeled after `vim`'s manual pages, and are accessed in a similar fashion. `help` takes either a topic or a filename as an argument. If a topic is given, it will find it in any of the included help pages and jump straight to that entry.

Examples:
```bash
help params.txt # opens params.txt
help cd         # opens builtins.txt and jumps to the 'cd' entry
```

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
* `select` has not been implemented yet

## Quirks

* Single quotes can be escaped inside of single quoted strings. This is a slight deviation from the POSIX spec, which says that backslashes should always be literal in single quoted strings.

## Notes

`shed` is experimental software and is currently under active development. Using an experimental shell is inherently risky business, there is no guarantee that your computer will not explode when you run this. That being said, I've been daily driving it for 5 months at the time of writing and my computer has not exploded yet. Use it at your own risk, the software is provided as-is.

## Why shed?

This originally started as an educational hobby project, but over the course of about two years or so it's taken the form of an actual daily-drivable shell (I switched to using it over `zsh` in October 2025). I mainly wanted to create a shell where line editing and general interactive use is more fluid. With the notable exception of `fish`, existing shells are command-interpreters first and interactive programs second, which is fine for scripting, but it's my opinion that interactive user experience should be first class in programs like this.
