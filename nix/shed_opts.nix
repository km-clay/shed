{ pkgs, lib }:

{
  enable = lib.mkEnableOption "shed shell";

  package = lib.mkOption {
    type = lib.types.package;
    default = pkgs.shed;
    description = "The shed package to use";
  };

  aliases = lib.mkOption {
    type = lib.types.attrsOf lib.types.str;
    default = {};
    description = "Aliases to set when shed starts";
  };

  functions = lib.mkOption {
    type = lib.types.attrsOf lib.types.str;
    default = {};
    description = "Shell functions to set when shed starts";
  };

  autocmds = lib.mkOption {
    type = lib.types.listOf (lib.types.submodule {
      options = {
        hooks = lib.mkOption {
          type = lib.types.addCheck (lib.types.listOf (lib.types.enum [
            "pre-cmd"
            "post-cmd"
            "pre-change-dir"
            "post-change-dir"
            "on-job-finish"
            "pre-prompt"
            "post-prompt"
            "pre-mode-change"
            "post-mode-change"
            "on-exit"
            "on-history-open"
            "on-history-close"
            "on-history-select"
            "on-completion-start"
            "on-completion-cancel"
            "on-completion-select"
            "on-screensaver-exec"
            "on-screensaver-return"
            "on-time-report"
          ])) (list: list != []);
          description = "The events that trigger this autocmd";
        };
        command = lib.mkOption {
          type = lib.types.addCheck lib.types.str (cmd: cmd != "");
          description = "The shell command to execute when the hook is triggered and the pattern (if provided) matches";
        };
      };

    });
    default = [];
    description = "Custom autocmds to set when shed starts";
  };

  keymaps = lib.mkOption {
    type = lib.types.listOf (lib.types.submodule {
      options = {
        modes = lib.mkOption {
          type = lib.types.listOf (lib.types.enum [ "n" "i" "x" "v" "o" "r" ]);
          default = [];
          description = "The editing modes this keymap can be used in";
        };
        keys = lib.mkOption {
          type = lib.types.str;
          default = "";
          description = "The sequence of keys that trigger this keymap";
        };
        command = lib.mkOption {
          type = lib.types.str;
          default = "";
          description = "The sequence of characters to send to the line editor when the keymap is triggered.";
        };
      };
    });
    default = [];
    description = "Custom keymaps to set when shed starts";
  };

  extraCompletion = lib.mkOption {
    type = lib.types.attrsOf (lib.types.submodule {
      options = {
        files = lib.mkOption {
          type = lib.types.bool;
          default = false;
          description = "Complete file names in the current directory";
        };
        dirs = lib.mkOption {
          type = lib.types.bool;
          default = false;
          description = "Complete directory names in the current directory";
        };
        commands = lib.mkOption {
          type = lib.types.bool;
          default = false;
          description = "Complete executable commands in the PATH";
        };
        variables = lib.mkOption {
          type = lib.types.bool;
          default = false;
          description = "Complete variable names";
        };
        users = lib.mkOption {
          type = lib.types.bool;
          default = false;
          description = "Complete user names from /etc/passwd";
        };
        jobs = lib.mkOption {
          type = lib.types.bool;
          default = false;
          description = "Complete job names or pids from the current shell session";
        };
        aliases = lib.mkOption {
          type = lib.types.bool;
          default = false;
          description = "Complete alias names defined in the current shell session";
        };
        signals = lib.mkOption {
          type = lib.types.bool;
          default = false;
          description = "Complete signal names for commands like kill";
        };
        wordList = lib.mkOption {
          type = lib.types.listOf lib.types.str;
          default = [];
          description = "Complete from a custom list of words";
        };
        function = lib.mkOption {
          type = lib.types.nullOr lib.types.str;
          default = null;
          description = "Complete using a custom shell function (should be defined in extraCompletionPreConfig)";
        };
        noSpace = lib.mkOption {
          type = lib.types.bool;
          default = false;
          description = "Don't append a space after completion";
        };
        fallback = lib.mkOption {
          type = lib.types.enum [ "no" "default" "dirnames" ];
          default = "no";
          description = "Fallback behavior when no matches are found: 'no' means no fallback, 'default' means fall back to the default shell completion behavior, and 'directories' means fall back to completing directory names";
        };

      };
    });
    default = {};
    description = "Additional completion scripts to source when shed starts (e.g. for custom tools or functions)";
  };

  environmentVars = lib.mkOption {
    type = lib.types.attrsOf lib.types.str;
    default = {};
    description = "Environment variables to set when shed starts";
  };

  shopts = lib.mkOption {
    type = lib.types.submodule {
      options = {
        line = lib.mkOption {
          type = lib.types.submodule {
            options = {
              viewport_height = lib.mkOption {
                type = lib.types.either lib.types.int lib.types.str;
                default = "50%";
                description = "Maximum viewport height for the line editor buffer";
              };
              scroll_offset = lib.mkOption {
                type = lib.types.int;
                default = 1;
                description = "The minimum number of lines to keep visible above and below the cursor when scrolling (i.e. the 'scrolloff' option in vim)";
              };
              tab_width = lib.mkOption {
                type = lib.types.int;
                default = 4;
                description = "The number of spaces a tab character represents in the line editor";
              };
              highlight = lib.mkOption {
                type = lib.types.bool;
                default = true;
                description = "Whether to enable syntax highlighting in the shell";
              };
              linebreak_on_incomplete = lib.mkOption {
                type = lib.types.bool;
                default = true;
                description = "Whether to automatically insert a newline when the input is incomplete";
              };
              line_numbers = lib.mkOption {
                type = lib.types.bool;
                default = true;
                description = "Whether to show line numbers in multiline input";
              };
              auto_indent = lib.mkOption {
                type = lib.types.bool;
                default = true;
                description = "Whether to automatically indent new lines in multiline commands";
              };
            };
          };
          default = {};
          description = "Settings related to the line editor (i.e. the 'shopt line.*' options)";
        };
        core = lib.mkOption {
          type = lib.types.submodule {
            options = {
              dotglob = lib.mkOption {
                type = lib.types.bool;
                default = false;
                description = "Whether to include hidden files in glob patterns";
              };
              autocd = lib.mkOption {
                type = lib.types.bool;
                default = false;
                description = "Whether to automatically change into directories when they are entered as commands";
              };
              hist_ignore_dupes = lib.mkOption {
                type = lib.types.bool;
                default = false;
                description = "Whether to ignore duplicate entries in the command history";
              };
              max_hist = lib.mkOption {
                type = lib.types.int;
                default = 10000;
                description = "The maximum number of entries to keep in the command history";
              };
              interactive_comments = lib.mkOption {
                type = lib.types.bool;
                default = true;
                description = "Whether to allow comments in interactive mode";
              };
              auto_hist = lib.mkOption {
                type = lib.types.bool;
                default = true;
                description = "Whether to automatically add commands to the history as they are executed";
              };
              bell_enabled = lib.mkOption {
                type = lib.types.bool;
                default = true;
                description = "Whether to allow shed to ring the terminal bell on certain events (e.g. command completion, errors, etc.)";
              };
              max_recurse_depth = lib.mkOption {
                type = lib.types.int;
                default = 1000;
                description = "The maximum depth to allow when recursively executing shell functions";
              };
              xpg_echo = lib.mkOption {
                type = lib.types.bool;
                default = false;
                description = "Whether to have the 'echo' builtin expand escape sequences like \\n and \\t (if false, it will print them verbatim)";
              };
            };
          };
          default = {};
          description = "Core settings (i.e. the 'shopt core.*' options)";
        };
        set = lib.mkOption {
          type = lib.types.submodule {
            options = {
              hashall = lib.mkOption {
                type = lib.types.bool;
                default = true;
                description = "Remember the full path of commands to speed up command lookup";
              };
              vi = lib.mkOption {
                type = lib.types.bool;
                default = false;
                description = "Enable vi editing mode (currently the only editing mode)";
              };
              allexport = lib.mkOption {
                type = lib.types.bool;
                default = false;
                description = "Automatically export all variables that are assigned";
              };
              errexit = lib.mkOption {
                type = lib.types.bool;
                default = false;
                description = "Exit immediately if any command exits with a non-zero status (equivalent to set -e)";
              };
              noclobber = lib.mkOption {
                type = lib.types.bool;
                default = false;
                description = "Prevent '>' redirections from overwriting existing files (equivalent to set -C)";
              };
              monitor = lib.mkOption {
                type = lib.types.bool;
                default = true;
                description = "Run jobs in their own process groups and report status before the next prompt (equivalent to set -m)";
              };
              noglob = lib.mkOption {
                type = lib.types.bool;
                default = false;
                description = "Disable filename expansion (globbing) (equivalent to set -f)";
              };
              noexec = lib.mkOption {
                type = lib.types.bool;
                default = false;
                description = "Read commands but do not execute them; useful for syntax checking (equivalent to set -n)";
              };
              nolog = lib.mkOption {
                type = lib.types.bool;
                default = false;
                description = "Do not write function definitions to command history";
              };
              notify = lib.mkOption {
                type = lib.types.bool;
                default = false;
                description = "Print job status info asynchronously when jobs exit or are stopped (equivalent to set -b)";
              };
              nounset = lib.mkOption {
                type = lib.types.bool;
                default = false;
                description = "Treat expansion of unset variables as an error (equivalent to set -u)";
              };
              verbose = lib.mkOption {
                type = lib.types.bool;
                default = false;
                description = "Write shell input to stderr as it is read (equivalent to set -v)";
              };
              xtrace = lib.mkOption {
                type = lib.types.bool;
                default = false;
                description = "Write a trace for each command after expansion but before execution (equivalent to set -x)";
              };
            };
          };
          default = {};
          description = "POSIX set flags (i.e. the 'shopt set.*' options, equivalent to 'set -o')";
        };
        prompt = lib.mkOption {
          type = lib.types.submodule {
            options = {
              leader = lib.mkOption {
                type = lib.types.str;
                default = "\\\\";
                description = "The leader key to use for custom keymaps (e.g. if set to '\\\\', then a keymap with keys='x' would be triggered by '\\x')";
              };
              trunc_prompt_path = lib.mkOption {
                type = lib.types.int;
                default = 4;
                description = "The maximum number of path segments to show in the prompt";
              };
              comp_limit = lib.mkOption {
                type = lib.types.int;
                default = 1000;
                description = "The maximum number of completion candidates to show before truncating the list";
              };
              screensaver_cmd = lib.mkOption {
                type = lib.types.str;
                default = "";
                description = "A shell command to execute after a period of inactivity (i.e. a custom screensaver)";
              };
              screensaver_idle_time = lib.mkOption {
                type = lib.types.int;
                default = 0;
                description = "The amount of inactivity time in seconds before the screensaver command is executed";
              };
              completion_ignore_case = lib.mkOption {
                type = lib.types.bool;
                default = false;
                description = "Whether to ignore case when completing commands and file names";
              };
              hist_cat = lib.mkOption {
                type = lib.types.bool;
                default = true;
                description = "Whether to enable the history concatenation feature. Ctrl/Shift+Up/Down joins sequential commands in history.";
              };
            };
          };
          default = {};
          description = "Settings related to the prompt (i.e. the 'shopt prompt.*' options)";
        };
      };
    };
    default = {};
  };

  extraPostConfig = lib.mkOption {
    type = lib.types.str;
    default = "";
    description = "Additional configuration to append to the shed configuration file";
  };
  extraPreConfig = lib.mkOption {
    type = lib.types.str;
    default = "";
    description = "Additional configuration to prepend to the shed configuration file";
  };
}
