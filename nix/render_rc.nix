lib: cfg: let
  # replace embedded single quotes with '\''
  escape = str: lib.replaceStrings ["'"] ["'\\''"] str;

  boolToString = b:
    if b
    then "true"
    else "false";

  mkAutoCmd = cfg:
    lib.concatLines (map (hook: "autocmd ${hook} '${escape cfg.command}'") cfg.hooks);

  mkFunctionDef = name: body: let
    indented = "\t" + lib.concatStringsSep "\n\t" (lib.splitString "\n" body);
  in ''
    ${name}() {
    ${indented}
    }'';

  mkKeymapCmd = cfg: let
    flags = "-${lib.concatStrings cfg.modes}";
    keys = "'${escape cfg.keys}'";
    action = "'${escape cfg.command}'";
  in "keymap ${flags} ${keys} ${action}";

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
      (lib.optionalString (cfg.function != null) " -F '${escape cfg.function}'")
      (lib.optionalString (cfg.fallback != "no") " -o '${escape cfg.fallback}'")
      (lib.optionalString (cfg.wordList != []) " -W '${escape (lib.concatStringsSep " " cfg.wordList)}'")
    ];
  in "complete${flags} ${name}";

  escaped = lib.escapeShellArg;
  completeLines = lib.concatLines (lib.mapAttrsToList mkCompleteCmd cfg.extraCompletion);
  keymapLines = lib.concatLines (map mkKeymapCmd cfg.keymaps);
  functionLines = lib.concatLines (lib.mapAttrsToList mkFunctionDef cfg.functions);
  autocmdLines = lib.concatLines (map mkAutoCmd cfg.autocmds);
  aliasLines = lib.concatLines (lib.mapAttrsToList (name: value: "alias ${name}=${escaped value}") cfg.aliases);
  exportLines = lib.concatLines (lib.mapAttrsToList (name: value: "export ${name}=\"${value}\"") cfg.environmentVars);
  exCmdLines = lib.concatLines (lib.mapAttrsToList (name: value: "excmd ${name}=${escaped value}") cfg.exCommands);
  shoptLines = lib.concatLines [
    "shopt line.viewport_height=${toString cfg.shopts.line.viewport_height}"
    "shopt line.scroll_offset=${toString cfg.shopts.line.scroll_offset}"
    "shopt line.tab_width=${toString cfg.shopts.line.tab_width}"
    "shopt line.linebreak_on_incomplete=${boolToString cfg.shopts.line.linebreak_on_incomplete}"
    "shopt line.line_numbers=${boolToString cfg.shopts.line.line_numbers}"
    "shopt line.auto_indent=${boolToString cfg.shopts.line.auto_indent}"
    "shopt line.trim_on_submit=${boolToString cfg.shopts.line.trim_on_submit}"
    "shopt line.auto_suggest=${boolToString cfg.shopts.line.auto_suggest}"

    "shopt core.dotglob=${boolToString cfg.shopts.core.dotglob}"
    "shopt core.nullglob=${boolToString cfg.shopts.core.nullglob}"
    "shopt core.autocd=${boolToString cfg.shopts.core.autocd}"
    "shopt core.interactive_comments=${boolToString cfg.shopts.core.interactive_comments}"
    "shopt core.bell_enabled=${boolToString cfg.shopts.core.bell_enabled}"
    "shopt core.max_recurse_depth=${toString cfg.shopts.core.max_recurse_depth}"
    "shopt core.xpg_echo=${boolToString cfg.shopts.core.xpg_echo}"
    "shopt core.compact_errors=${boolToString cfg.shopts.core.compact_errors}"
    "shopt core.max_read_limit=${toString cfg.shopts.core.max_read_limit}"
    "shopt core.pipeline_style='${cfg.shopts.core.pipeline_style}'"

    "shopt history.auto_save=${boolToString cfg.shopts.history.auto_save}"
    "shopt history.ignore_dupes=${boolToString cfg.shopts.history.ignore_dupes}"
    "shopt history.ignore_space=${boolToString cfg.shopts.history.ignore_space}"
    "shopt history.max_entries=${toString cfg.shopts.history.max_entries}"
    "shopt history.enable_concat=${boolToString cfg.shopts.history.enable_concat}"

    "shopt set.hashall=${boolToString cfg.shopts.set.hashall}"
    "shopt set.vi=${boolToString cfg.shopts.set.vi}"
    "shopt set.allexport=${boolToString cfg.shopts.set.allexport}"
    "shopt set.errexit=${boolToString cfg.shopts.set.errexit}"
    "shopt set.ignoreeof=${boolToString cfg.shopts.set.ignoreeof}"
    "shopt set.noclobber=${boolToString cfg.shopts.set.noclobber}"
    "shopt set.monitor=${boolToString cfg.shopts.set.monitor}"
    "shopt set.noglob=${boolToString cfg.shopts.set.noglob}"
    "shopt set.noexec=${boolToString cfg.shopts.set.noexec}"
    "shopt set.nolog=${boolToString cfg.shopts.set.nolog}"
    "shopt set.notify=${boolToString cfg.shopts.set.notify}"
    "shopt set.nounset=${boolToString cfg.shopts.set.nounset}"
    "shopt set.verbose=${boolToString cfg.shopts.set.verbose}"
    "shopt set.xtrace=${boolToString cfg.shopts.set.xtrace}"
    "shopt set.pipefail=${boolToString cfg.shopts.set.pipefail}"

    "shopt prompt.leader='${cfg.shopts.prompt.leader}'"
    "shopt prompt.trunc_prompt_path=${toString cfg.shopts.prompt.trunc_prompt_path}"
    "shopt prompt.comp_limit=${toString cfg.shopts.prompt.comp_limit}"
    "shopt prompt.idle_timeout=${toString cfg.shopts.prompt.idle_timeout}"
    "shopt prompt.completion_ignore_case=${boolToString cfg.shopts.prompt.completion_ignore_case}"
    "shopt prompt.complete_style=${toString cfg.shopts.prompt.complete_style}"
    "shopt prompt.expand_aliases=${boolToString cfg.shopts.prompt.expand_aliases}"
    "shopt prompt.substitute=${boolToString cfg.shopts.prompt.substitute}"

    "shopt statline.enable=${boolToString cfg.shopts.statline.enable}"
    "shopt statline.left_string='${escape cfg.shopts.statline.left_string}'"
    "shopt statline.middle_string='${escape cfg.shopts.statline.middle_string}'"
    "shopt statline.right_string='${escape cfg.shopts.statline.right_string}'"

    ''shopt highlight.enable="${boolToString cfg.shopts.highlight.enable}"''
    ''shopt highlight.string="${cfg.shopts.highlight.string}"''
    ''shopt highlight.keyword="${cfg.shopts.highlight.keyword}"''
    ''shopt highlight.external_command="${cfg.shopts.highlight.external_command}"''
    ''shopt highlight.function="${cfg.shopts.highlight.function}"''
    ''shopt highlight.alias="${cfg.shopts.highlight.alias}"''
    ''shopt highlight.builtin="${cfg.shopts.highlight.builtin}"''
    ''shopt highlight.directory="${cfg.shopts.highlight.directory}"''
    ''shopt highlight.invalid_command="${cfg.shopts.highlight.invalid_command}"''
    ''shopt highlight.control_flow_keyword="${cfg.shopts.highlight.control_flow_keyword}"''
    ''shopt highlight.argument="${cfg.shopts.highlight.argument}"''
    ''shopt highlight.argument_file="${cfg.shopts.highlight.argument_file}"''
    ''shopt highlight.variable="${cfg.shopts.highlight.variable}"''
    ''shopt highlight.operator="${cfg.shopts.highlight.operator}"''
    ''shopt highlight.comment="${cfg.shopts.highlight.comment}"''
    ''shopt highlight.glob="${cfg.shopts.highlight.glob}"''
  ];
in
  lib.concatLines [
    cfg.extraPreConfig
    exportLines
    aliasLines
    exCmdLines
    shoptLines
    functionLines
    completeLines
    keymapLines
    autocmdLines
    cfg.extraPostConfig
  ]
