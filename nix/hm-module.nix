{ config, lib, pkgs, ... }:

let
  cfg = config.programs.shed;
  boolToString = b:
  if b then "true" else "false";

  mkAutoCmd = cfg:
    lib.concatLines (map (hook: "autocmd ${hook} ${lib.optionalString (cfg.pattern != null) "-p \"${cfg.pattern}\""} '${cfg.command}'") cfg.hooks);


  mkFunctionDef = name: body:
  let
    indented = "\t" + lib.concatStringsSep "\n\t" (lib.splitString "\n" body);
  in
    ''
${name}() {
${indented}
}'';

  mkKeymapCmd = cfg: let
    flags = "-${lib.concatStrings cfg.modes}";
    keys = "'${cfg.keys}'";
    action = "'${cfg.command}'";
  in
    "keymap ${flags} ${keys} ${action}";


  mkCompleteCmd = name: cfg: let
    flags = lib.concatStrings [
      (lib.optionalString cfg.files " -f")
      (lib.optionalString cfg.dirs " -d")
      (lib.optionalString cfg.commands " -c")
      (lib.optionalString cfg.variables " -v")
      (lib.optionalString cfg.users " -u")
      (lib.optionalString cfg.jobs " -j")
      (lib.optionalString cfg.aliases " -a")
      (lib.optionalString cfg.signals " -S")
      (lib.optionalString cfg.noSpace " -n")
      (lib.optionalString (cfg.function != null) " -F ${cfg.function}")
      (lib.optionalString (cfg.fallback != "no") " -o ${cfg.fallback}")
      (lib.optionalString (cfg.wordList != []) " -W '${lib.concatStringsSep " " cfg.wordList}'")

    ];
  in "complete${flags} ${name}";
in
{
  options.programs.shed = {
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
      default = {};
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
        default = 1000;
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
  };

  config =
  let
    completeLines = lib.concatLines (lib.mapAttrsToList mkCompleteCmd cfg.extraCompletion);
    keymapLines = lib.concatLines (map mkKeymapCmd cfg.keymaps);
    functionLines = lib.concatLines (lib.mapAttrsToList mkFunctionDef cfg.functions);
    autocmdLines = lib.concatLines (map mkAutoCmd cfg.autocmds);
  in
  lib.mkIf cfg.enable {
    home.packages = [ cfg.package ];

    home.file.".shedrc".text = lib.concatLines [
      cfg.settings.extraPreConfig
      (lib.concatLines (lib.mapAttrsToList (name: value: "export ${name}=\"${value}\"") cfg.environmentVars))
      (lib.concatLines (lib.mapAttrsToList (name: value: "alias ${name}=\"${value}\"") cfg.aliases))
      (lib.concatLines [
        "shopt core.dotglob=${boolToString cfg.settings.dotGlob}"
        "shopt core.autocd=${boolToString cfg.settings.autocd}"
        "shopt core.hist_ignore_dupes=${boolToString cfg.settings.historyIgnoresDupes}"
        "shopt core.max_hist=${toString cfg.settings.maxHistoryEntries}"
        "shopt core.interactive_comments=${boolToString cfg.settings.interactiveComments}"
        "shopt core.auto_hist=${boolToString cfg.settings.autoHistory}"
        "shopt core.bell_enabled=${boolToString cfg.settings.bellEnabled}"
        "shopt core.max_recurse_depth=${toString cfg.settings.maxRecurseDepth}"

        "shopt prompt.leader='${cfg.settings.leaderKey}'"
        "shopt prompt.trunc_prompt_path=${toString cfg.settings.promptPathSegments}"
        "shopt prompt.comp_limit=${toString cfg.settings.completionLimit}"
        "shopt prompt.highlight=${boolToString cfg.settings.syntaxHighlighting}"
        "shopt prompt.linebreak_on_incomplete=${boolToString cfg.settings.linebreakOnIncomplete}"
        functionLines
        completeLines
        keymapLines
        autocmdLines
      ])
      cfg.settings.extraPostConfig
    ];
  };
}
