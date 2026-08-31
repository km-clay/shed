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

<img width="1924" height="1086" alt="shed_preview" src="https://github.com/user-attachments/assets/83276eb5-92bf-4eb4-891f-c2dfeddced5d" />

<!-- TOC markers (for autoscript) -->
<!--tocbeg-->
## Table of Contents
- [Why shed?](#why-shed)
- [Features](#features)
- [Documentation](#documentation)
- [Building](#building)
  - [Arch Linux (AUR)](#arch-linux-aur)
  - [Cargo](#cargo)
  - [Nix](#nix)
- [Known issues](#known-issues)
- [AI Usage](#ai-usage)
- [Notes](#notes)
<!--tocend-->


## Why shed?

I started working on `shed` because in my experience, picking a shell meant making a decision between "portable syntax" and "smooth interactive UX". `bash` and `zsh` are both good POSIX options, but their interactive experience is somewhat clunky (in my opinion). `fish` has great interactive features, but it wants me to learn their scripting language instead of the one that everyone else uses. I wasn't able to find a satisfying compromise between the two, so I decided to take a crack at building a POSIX shell with a modern, unopinionated interactive experience.

## Features

`shed` is a POSIX shell at heart - it will source any POSIX-portable script, or I'll eat my hat <sub>(hats eaten so far: 2)</sub>

`shed` ships with a large set of interactive features layered on top. Each links to its full documentation in the [wiki](https://github.com/km-clay/shed/wiki):

- **[Modal line editor](https://github.com/km-clay/shed/wiki/Line-Editor)** - a from-scratch modal editor with `vim` and `emacs` modes that treats multi-line editing as first-class. It's closer to a terminal-embedded text editor than a traditional `readline`-style line editor.
- **[Ex mode](https://github.com/km-clay/shed/wiki/Ex-Mode)** - a secondary command line for controlling the editor and running commands without losing the one you're typing.
- **[Keymaps](https://github.com/km-clay/shed/wiki/Keymaps)** - bind key sequences to editor actions or shell commands, with `zsh`-widget-style read/write access to the line buffer.
- **[Interactive documentation](https://github.com/km-clay/shed/wiki/Interactive-Documentation)** - every builtin, feature, and easy-to-forget POSIX detail lives in a built-in hypertext pager, via the `help` builtin.
- **[Autocmds](https://github.com/km-clay/shed/wiki/Autocmds)** - run commands on shell events: directory changes, prompt draws, job completion, command-not-found, and more.
- **[SQLite command history](https://github.com/km-clay/shed/wiki/Command-History)** - shared across sessions in real time, queryable with any SQLite tool, with rich per-command metadata and command lookup via the `hist` builtin.
- **[Syntax highlighting](https://github.com/km-clay/shed/wiki/Syntax-Highlighting)** - the line is highlighted with `shed`'s own lexer as you type, so unknown commands light up before you ever run them.
- **[Prompt & status line](https://github.com/km-clay/shed/wiki/Prompt)** - familiar backslash escapes plus embedded function output, a right-hand prompt, and a fully configurable status line.
- **[IPC socket](https://github.com/km-clay/shed/wiki/IPC-Socket)** - a Unix socket other processes can use to subscribe to shell events, query state, or even drive the line editor remotely.
- **[Scripting extensions](https://github.com/km-clay/shed/wiki/shed-Scripting-Features)** - POSIX plus `try`/`catch`, Go-style `defer`, and shell-quoted records for passing structured data through pipelines.
- **[Configuration](https://github.com/km-clay/shed/wiki/Configuration)** - The `shopt` builtin, rc file sourcing and generation via `genrc`.

## Documentation

General documentation for `shed`-specific features and extensions can be found in the **[wiki](https://github.com/km-clay/shed/wiki)**. Everything there is also available inside the shell itself, via the `help` builtin's interactive pager.

<img width="1925" height="1028" alt="shed_help" src="https://github.com/user-attachments/assets/a6cf2031-5f01-4260-9104-bcf488ef1778" />

## Building

### Arch Linux (AUR)

```sh
yay -S shed-sh
```

Or your favorite AUR helper (`paru -S shed-sh`, etc).

### Cargo

`shed` is a published crate on [crates.io](https://crates.io/crates/shed-sh), so it can be installed directly using `cargo`:
```sh
cargo install shed-sh
```

To build from source:
```sh
git clone https://github.com/km-clay/shed.git
cd shed
cargo build --profile dist
```

The binary will be at `target/dist/shed`.

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

`shed` is experimental software and is currently under active development. Using an experimental shell is inherently risky business, there is no guarantee that your computer will not explode when you run this. That being said, I've been daily driving it for 11 months at the time of writing and my computer has not exploded yet. Use it at your own risk, the software is provided as-is.
