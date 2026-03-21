lib: cfg:

let
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

  completeLines = lib.concatLines (lib.mapAttrsToList mkCompleteCmd cfg.extraCompletion);
  keymapLines = lib.concatLines (map mkKeymapCmd cfg.keymaps);
  functionLines = lib.concatLines (lib.mapAttrsToList mkFunctionDef cfg.functions);
  autocmdLines = lib.concatLines (map mkAutoCmd cfg.autocmds);
in
lib.concatLines [
      cfg.settings.extraPreConfig
      (lib.concatLines (lib.mapAttrsToList (name: value: "export ${name}=\"${value}\"") cfg.environmentVars))
      (lib.concatLines (lib.mapAttrsToList (name: value: "alias ${name}=\"${value}\"") cfg.aliases))
      (lib.concatLines [
        "shopt line.viewport_height=${toString cfg.settings.viewportHeight}"
        "shopt line.scroll_offset=${toString cfg.settings.scrollOffset}"

        "shopt core.dotglob=${boolToString cfg.settings.dotGlob}"
        "shopt core.autocd=${boolToString cfg.settings.autocd}"
        "shopt core.hist_ignore_dupes=${boolToString cfg.settings.historyIgnoresDupes}"
        "shopt core.max_hist=${toString cfg.settings.maxHistoryEntries}"
        "shopt core.interactive_comments=${boolToString cfg.settings.interactiveComments}"
        "shopt core.auto_hist=${boolToString cfg.settings.autoHistory}"
        "shopt core.bell_enabled=${boolToString cfg.settings.bellEnabled}"
        "shopt core.max_recurse_depth=${toString cfg.settings.maxRecurseDepth}"
        "shopt core.xpg_echo=${boolToString cfg.settings.echoExpandsEscapes}"
        "shopt core.noclobber=${boolToString cfg.settings.noClobber}"

        "shopt prompt.leader='${cfg.settings.leaderKey}'"
        "shopt prompt.trunc_prompt_path=${toString cfg.settings.promptPathSegments}"
        "shopt prompt.comp_limit=${toString cfg.settings.completionLimit}"
        "shopt prompt.highlight=${boolToString cfg.settings.syntaxHighlighting}"
        "shopt prompt.linebreak_on_incomplete=${boolToString cfg.settings.linebreakOnIncomplete}"
        "shopt prompt.line_numbers=${boolToString cfg.settings.lineNumbers}"
        "shopt prompt.screensaver_idle_time=${toString cfg.settings.screensaverIdleTime}"
        "shopt prompt.screensaver_cmd='${cfg.settings.screensaverCmd}'"
        "shopt prompt.completion_ignore_case=${boolToString cfg.settings.completionIgnoreCase}"
        "shopt prompt.auto_indent=${boolToString cfg.settings.autoIndent}"
        functionLines
        completeLines
        keymapLines
        autocmdLines
      ])
      cfg.settings.extraPostConfig
    ]
