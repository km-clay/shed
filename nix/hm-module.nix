{ config, lib, pkgs, ... }:

let
  cfg = config.programs.shed;
  boolToString = b:
  if b then "true" else "false";
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
      description = "Aliases to set when shed starts (e.g. ls='ls --color=auto')";
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

  config = lib.mkIf cfg.enable {
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

        "shopt prompt.trunc_prompt_path=${toString cfg.settings.promptPathSegments}"
        "shopt prompt.comp_limit=${toString cfg.settings.completionLimit}"
        "shopt prompt.highlight=${boolToString cfg.settings.syntaxHighlighting}"
        "shopt prompt.linebreak_on_incomplete=${boolToString cfg.settings.linebreakOnIncomplete}"
      ])
      cfg.settings.extraPostConfig
    ];
  };
}
