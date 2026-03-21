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
      cfg.extraPreConfig
      (lib.concatLines (lib.mapAttrsToList (name: value: "export ${name}=\"${value}\"") cfg.environmentVars))
      (lib.concatLines (lib.mapAttrsToList (name: value: "alias ${name}=\"${value}\"") cfg.aliases))
      (lib.concatLines [
        "shopt line.viewport_height=${toString cfg.shopts.line.viewport_height}"
        "shopt line.scroll_offset=${toString cfg.shopts.line.scroll_offset}"

        "shopt core.dotglob=${boolToString cfg.shopts.core.dotglob}"
        "shopt core.autocd=${boolToString cfg.shopts.core.autocd}"
        "shopt core.hist_ignore_dupes=${boolToString cfg.shopts.core.hist_ignore_dupes}"
        "shopt core.max_hist=${toString cfg.shopts.core.max_hist}"
        "shopt core.interactive_comments=${boolToString cfg.shopts.core.interactive_comments}"
        "shopt core.auto_hist=${boolToString cfg.shopts.core.auto_hist}"
        "shopt core.bell_enabled=${boolToString cfg.shopts.core.bell_enabled}"
        "shopt core.max_recurse_depth=${toString cfg.shopts.core.max_recurse_depth}"
        "shopt core.xpg_echo=${boolToString cfg.shopts.core.xpg_echo}"
        "shopt core.noclobber=${boolToString cfg.shopts.core.noclobber}"

        "shopt prompt.leader='${cfg.shopts.prompt.leader}'"
        "shopt prompt.trunc_prompt_path=${toString cfg.shopts.prompt.trunc_prompt_path}"
        "shopt prompt.comp_limit=${toString cfg.shopts.prompt.comp_limit}"
        "shopt prompt.highlight=${boolToString cfg.shopts.prompt.highlight}"
        "shopt prompt.linebreak_on_incomplete=${boolToString cfg.shopts.prompt.linebreak_on_incomplete}"
        "shopt prompt.line_numbers=${boolToString cfg.shopts.prompt.line_numbers}"
        "shopt prompt.screensaver_idle_time=${toString cfg.shopts.prompt.screensaver_idle_time}"
        "shopt prompt.screensaver_cmd='${cfg.shopts.prompt.screensaver_cmd}'"
        "shopt prompt.completion_ignore_case=${boolToString cfg.shopts.prompt.completion_ignore_case}"
        "shopt prompt.auto_indent=${boolToString cfg.shopts.prompt.auto_indent}"
        functionLines
        completeLines
        keymapLines
        autocmdLines
      ])
      cfg.extraPostConfig
    ]
