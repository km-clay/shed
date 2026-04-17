lib: cfg:

let
  # This method of escaping is O(n^2)
  # Shouldn't matter in practice though
  # We arent going to be handling monster strings here
  isEscaped = str:
    let
      chars = lib.reverseList lib.stringToCharacters str;
      result = lib.foldl' (acc: ch:
        if acc.done then acc
        else if ch == "\\" then { done = acc.done; escaped = !acc.escaped; }
        else { done = true; escaped = acc.escaped; }
      ) { done = false; escaped = false; } chars;
    in result.escaped;

  escape = str:
    let
      chars = lib.stringToCharacters str;
      escaped = lib.foldl' (acc: ch:
        if ch == "'" && !isEscaped acc
        then acc + "\\'"
        else acc + ch
      ) "" chars;
    in escaped;

  boolToString = b:
  if b then "true" else "false";

  mkAutoCmd = cfg:
    lib.concatLines (map (hook: "autocmd ${hook} '${escape cfg.command}'") cfg.hooks);


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
    keys = "'${escape cfg.keys}'";
    action = "'${escape cfg.command}'";
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
      (lib.optionalString (cfg.function != null) " -F '${escape cfg.function}'")
      (lib.optionalString (cfg.fallback != "no") " -o '${escape cfg.fallback}'")
      (lib.optionalString (cfg.wordList != []) " -W '${escape (lib.concatStringsSep " " cfg.wordList)}'")

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
        "shopt line.tab_width=${toString cfg.shopts.line.tab_width}"
        "shopt line.linebreak_on_incomplete=${boolToString cfg.shopts.line.linebreak_on_incomplete}"
        "shopt line.line_numbers=${boolToString cfg.shopts.line.line_numbers}"
        "shopt line.auto_indent=${boolToString cfg.shopts.line.auto_indent}"

        "shopt core.dotglob=${boolToString cfg.shopts.core.dotglob}"
        "shopt core.autocd=${boolToString cfg.shopts.core.autocd}"
        "shopt core.hist_ignore_dupes=${boolToString cfg.shopts.core.hist_ignore_dupes}"
        "shopt core.max_hist=${toString cfg.shopts.core.max_hist}"
        "shopt core.interactive_comments=${boolToString cfg.shopts.core.interactive_comments}"
        "shopt core.auto_hist=${boolToString cfg.shopts.core.auto_hist}"
        "shopt core.bell_enabled=${boolToString cfg.shopts.core.bell_enabled}"
        "shopt core.max_recurse_depth=${toString cfg.shopts.core.max_recurse_depth}"
        "shopt core.xpg_echo=${boolToString cfg.shopts.core.xpg_echo}"

        "shopt set.hashall=${boolToString cfg.shopts.set.hashall}"
        "shopt set.vi=${boolToString cfg.shopts.set.vi}"
        "shopt set.allexport=${boolToString cfg.shopts.set.allexport}"
        "shopt set.errexit=${boolToString cfg.shopts.set.errexit}"
        "shopt set.noclobber=${boolToString cfg.shopts.set.noclobber}"
        "shopt set.monitor=${boolToString cfg.shopts.set.monitor}"
        "shopt set.noglob=${boolToString cfg.shopts.set.noglob}"
        "shopt set.noexec=${boolToString cfg.shopts.set.noexec}"
        "shopt set.nolog=${boolToString cfg.shopts.set.nolog}"
        "shopt set.notify=${boolToString cfg.shopts.set.notify}"
        "shopt set.nounset=${boolToString cfg.shopts.set.nounset}"
        "shopt set.verbose=${boolToString cfg.shopts.set.verbose}"
        "shopt set.xtrace=${boolToString cfg.shopts.set.xtrace}"

        "shopt prompt.leader='${cfg.shopts.prompt.leader}'"
        "shopt prompt.trunc_prompt_path=${toString cfg.shopts.prompt.trunc_prompt_path}"
        "shopt prompt.comp_limit=${toString cfg.shopts.prompt.comp_limit}"
        "shopt prompt.screensaver_idle_time=${toString cfg.shopts.prompt.screensaver_idle_time}"
        "shopt prompt.screensaver_cmd='${cfg.shopts.prompt.screensaver_cmd}'"
        "shopt prompt.completion_ignore_case=${boolToString cfg.shopts.prompt.completion_ignore_case}"
        "shopt prompt.hist_cat=${boolToString cfg.shopts.prompt.hist_cat}"
        "shopt prompt.expand_aliases=${boolToString cfg.shopts.prompt.expand_aliases}"

        ''shopt highlight.enable="${boolToString cfg.shopts.highlight.enable}"''
        ''shopt highlight.string="${cfg.shopts.highlight.string}"''
        ''shopt highlight.keyword="${cfg.shopts.highlight.keyword}"''
        ''shopt highlight.valid_command="${cfg.shopts.highlight.valid_command}"''
        ''shopt highlight.invalid_command="${cfg.shopts.highlight.invalid_command}"''
        ''shopt highlight.control_flow_keyword="${cfg.shopts.highlight.control_flow_keyword}"''
        ''shopt highlight.argument="${cfg.shopts.highlight.argument}"''
        ''shopt highlight.argument_file="${cfg.shopts.highlight.argument_file}"''
        ''shopt highlight.variable="${cfg.shopts.highlight.variable}"''
        ''shopt highlight.operator="${cfg.shopts.highlight.operator}"''
        ''shopt highlight.comment="${cfg.shopts.highlight.comment}"''
        ''shopt highlight.glob="${cfg.shopts.highlight.glob}"''
        ''shopt highlight.selection="${cfg.shopts.highlight.selection}"''
        functionLines
        completeLines
        keymapLines
        autocmdLines
      ])
      cfg.extraPostConfig
    ]
