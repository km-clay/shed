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
          ])) (list: list != []);
          description = "The events that trigger this autocmd";
        };
        pattern = lib.mkOption {
          type = lib.types.nullOr lib.types.str;
          default = null;
          description = "A regex pattern to use in the hook to determine whether it runs or not. What it's compared to differs by hook, for instance 'pre-change-dir' compares it to the new directory, pre-cmd compares it to the command, etc";
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

  viewportHeight = {
    type = lib.types.either lib.types.int lib.types.str;
    default = "50%";
    description = "Maximum viewport height for the line editor buffer";
  };

  scrollOffset = {
    type = lib.types.int;
    default = "1";
    description = "The minimum number of lines to keep visible above and below the cursor when scrolling (i.e. the 'scrolloff' option in vim)";
  };

  environmentVars = lib.mkOption {
    type = lib.types.attrsOf lib.types.str;
    default = {};
    description = "Environment variables to set when shed starts";
  };

  settings = {
    dotGlob = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Whether to include hidden files in glob patterns";
    };
    autocd = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Whether to automatically change into directories when they are entered as commands";
    };
    historyIgnoresDupes = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Whether to ignore duplicate entries in the command history";
    };
    maxHistoryEntries = lib.mkOption {
      type = lib.types.int;
      default = 10000;
      description = "The maximum number of entries to keep in the command history";
    };
    interactiveComments = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Whether to allow comments in interactive mode";
    };
    autoHistory = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Whether to automatically add commands to the history as they are executed";
    };
    bellEnabled = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Whether to allow shed to ring the terminal bell on certain events (e.g. command completion, errors, etc.)";
    };
    maxRecurseDepth = lib.mkOption {
      type = lib.types.int;
      default = 1000;
      description = "The maximum depth to allow when recursively executing shell functions";
    };
    echoExpandsEscapes = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Whether to have the 'echo' builtin expand escape sequences like \\n and \\t (if false, it will print them verbatim)";
    };
    noClobber = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Whether to prevent redirection from overwriting existing files by default (i.e. behave as if 'set -o noclobber' is always in effect)";
    };

    leaderKey = lib.mkOption {
      type = lib.types.str;
      default = "\\\\";
      description = "The leader key to use for custom keymaps (e.g. if set to '\\\\', then a keymap with keys='x' would be triggered by '\\x')";
    };
    promptPathSegments = lib.mkOption {
      type = lib.types.int;
      default = 4;
      description = "The maximum number of path segments to show in the prompt";
    };
    completionLimit = lib.mkOption {
      type = lib.types.int;
      default = 1000;
      description = "The maximum number of completion candidates to show before truncating the list";
    };
    syntaxHighlighting = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Whether to enable syntax highlighting in the shell";
    };
    linebreakOnIncomplete = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Whether to automatically insert a newline when the input is incomplete";
    };
    lineNumbers = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Whether to show line numbers in the prompt";
    };
    screensaverCmd = lib.mkOption {
      type = lib.types.str;
      default = "";
      description = "A shell command to execute after a period of inactivity (i.e. a custom screensaver)";
    };
    screensaverIdleTime = lib.mkOption {
      type = lib.types.int;
      default = 0;
      description = "The amount of inactivity time in seconds before the screensaver command is executed";
    };
    completionIgnoreCase = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Whether to ignore case when completing commands and file names";
    };
    autoIndent = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Whether to automatically indent new lines based on the previous line";
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
  };
}
