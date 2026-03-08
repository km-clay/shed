-- Generate Rust vi_test! macro invocations using neovim as oracle
-- Usage: nvim --headless --clean -l tests/gen_vi_tests.lua
--
-- Define test cases as { name, input_text, key_sequence }
-- Key sequences use vim notation: <Esc>, <CR>, <C-w>, etc.
-- The script executes each in a fresh buffer and captures the result.

local tests = {
	-- ===================== basic char motions =====================
	{ "dw_basic",          "hello world",       "dw" },
	{ "dw_middle",         "one two three",     "wdw" },
	{ "dd_whole_line",     "hello world",       "dd" },
	{ "x_single",          "hello",             "x" },
	{ "x_middle",          "hello",             "llx" },
	{ "X_backdelete",      "hello",             "llX" },
	{ "h_motion",          "hello",             "$h" },
	{ "l_motion",          "hello",             "l" },
	{ "h_at_start",        "hello",             "h" },
	{ "l_at_end",          "hello",             "$l" },

	-- ===================== word motions (small) =====================
	{ "w_forward",         "one two three",     "w" },
	{ "b_backward",        "one two three",     "$b" },
	{ "e_end",             "one two three",     "e" },
	{ "ge_back_end",       "one two three",     "$ge" },
	{ "w_punctuation",     "foo.bar baz",       "w" },
	{ "e_punctuation",     "foo.bar baz",       "e" },
	{ "b_punctuation",     "foo.bar baz",       "$b" },
	{ "w_at_eol",          "hello",             "$w" },
	{ "b_at_bol",          "hello",             "b" },

	-- ===================== word motions (big) =====================
	{ "W_forward",         "foo.bar baz",       "W" },
	{ "B_backward",        "foo.bar baz",       "$B" },
	{ "E_end",             "foo.bar baz",       "E" },
	{ "gE_back_end",       "one two three",     "$gE" },
	{ "W_skip_punct",      "one-two three",     "W" },
	{ "B_skip_punct",      "one two-three",     "$B" },
	{ "E_skip_punct",      "one-two three",     "E" },
	{ "dW_big",            "foo.bar baz",       "dW" },
	{ "cW_big",            "foo.bar baz",       "cWx<Esc>" },

	-- ===================== line motions =====================
	{ "zero_bol",          "  hello",           "$0" },
	{ "caret_first_char",  "  hello",           "$^" },
	{ "dollar_eol",        "hello world",       "$" },
	{ "g_last_nonws",      "hello   ",          "g_" },
	{ "g_no_trailing",     "hello",             "g_" },
	{ "pipe_column",       "hello world",       "6|" },
	{ "pipe_col1",         "hello world",       "1|" },
	{ "I_insert_front",    "  hello",           "Iworld <Esc>" },
	{ "A_append_end",      "hello",             "A world<Esc>" },

	-- ===================== find motions =====================
	{ "f_find",            "hello world",       "fo" },
	{ "F_find_back",       "hello world",       "$Fo" },
	{ "t_till",            "hello world",       "tw" },
	{ "T_till_back",       "hello world",       "$To" },
	{ "f_no_match",        "hello",             "fz" },
	{ "semicolon_repeat",  "abcabc",            "fa;;" },
	{ "comma_reverse",     "abcabc",            "fa;;," },
	{ "df_semicolon",      "abcabc",            "fa;;dfa" },
	{ "t_at_target",       "aab",               "lta" },

	-- ===================== delete operations =====================
	{ "D_to_end",          "hello world",       "wD" },
	{ "d_dollar",          "hello world",       "wd$" },
	{ "d0_to_start",       "hello world",       "$d0" },
	{ "dw_multiple",       "one two three",     "d2w" },
	{ "dt_char",           "hello world",       "dtw" },
	{ "df_char",           "hello world",       "dfw" },
	{ "dh_back",           "hello",             "lldh" },
	{ "dl_forward",        "hello",             "dl" },
	{ "dge_back_end",      "one two three",     "$dge" },
	{ "dG_to_end",         "hello world",       "dG" },
	{ "dgg_to_start",      "hello world",       "$dgg" },
	{ "d_semicolon",       "abcabc",            "fad;" },

	-- ===================== change operations =====================
	{ "cw_basic",          "hello world",       "cwfoo<Esc>" },
	{ "C_to_end",          "hello world",       "wCfoo<Esc>" },
	{ "cc_whole",          "hello world",       "ccfoo<Esc>" },
	{ "ct_char",           "hello world",       "ctwfoo<Esc>" },
	{ "s_single",          "hello",             "sfoo<Esc>" },
	{ "S_whole_line",      "hello world",       "Sfoo<Esc>" },
	{ "cl_forward",        "hello",             "clX<Esc>" },
	{ "ch_backward",       "hello",             "llchX<Esc>" },
	{ "cb_word_back",      "hello world",       "$cbfoo<Esc>" },
	{ "ce_word_end",       "hello world",       "cefoo<Esc>" },
	{ "c0_to_start",       "hello world",       "wc0foo<Esc>" },

	-- ===================== yank and paste =====================
	{ "yw_p_basic",        "hello world",       "ywwP" },
	{ "dw_p_paste",        "hello world",       "dwP" },
	{ "dd_p_paste",        "hello world",       "ddp" },
	{ "y_dollar_p",        "hello world",       "wy$P" },
	{ "ye_p",              "hello world",       "yewP" },
	{ "yy_p",              "hello world",       "yyp" },
	{ "Y_p",               "hello world",       "Yp" },
	{ "p_after_x",         "hello",             "xp" },
	{ "P_before",          "hello",             "llxP" },
	{ "paste_empty",       "hello",             "p" },

	-- ===================== replace =====================
	{ "r_replace",         "hello",             "ra" },
	{ "r_middle",          "hello",             "llra" },
	{ "r_at_end",          "hello",             "$ra" },
	{ "r_space",           "hello",             "r " },
	{ "r_with_count",      "hello",             "3rx" },

	-- ===================== case operations =====================
	{ "tilde_single",      "hello",             "~" },
	{ "tilde_count",       "hello",             "3~" },
	{ "tilde_at_end",      "HELLO",             "$~" },
	{ "tilde_mixed",       "hElLo",             "5~" },
	{ "gu_word",           "HELLO world",       "guw" },
	{ "gU_word",           "hello WORLD",       "gUw" },
	{ "gu_dollar",         "HELLO WORLD",       "gu$" },
	{ "gU_dollar",         "hello world",       "gU$" },
	{ "gu_0",              "HELLO WORLD",       "$gu0" },
	{ "gU_0",              "hello world",       "$gU0" },
	{ "gtilde_word",       "hello WORLD",       "g~w" },
	{ "gtilde_dollar",     "hello WORLD",       "g~$" },

	-- ===================== text objects: word =====================
	{ "diw_inner",         "one two three",     "wdiw" },
	{ "ciw_replace",       "hello world",       "ciwfoo<Esc>" },
	{ "daw_around",        "one two three",     "wdaw" },
	{ "yiw_p",             "hello world",       "yiwAp <Esc>p" },
	{ "diW_big_inner",     "one-two three",     "diW" },
	{ "daW_big_around",    "one two-three end", "wdaW" },
	{ "ciW_big",           "one-two three",     "ciWx<Esc>" },

	-- ===================== text objects: quotes =====================
	{ "di_dquote",         'one "two" three',   'f"di"' },
	{ "da_dquote",         'one "two" three',   'f"da"' },
	{ "ci_dquote",         'one "two" three',   'f"ci"x<Esc>' },
	{ "di_squote",         "one 'two' three",   "f'di'" },
	{ "da_squote",         "one 'two' three",   "f'da'" },
	{ "di_backtick",       "one `two` three",   "f`di`" },
	{ "da_backtick",       "one `two` three",   "f`da`" },
	{ "ci_dquote_empty",   'one "" three',      'f"ci"x<Esc>' },

	-- ===================== text objects: delimiters =====================
	{ "di_paren",          "one (two) three",   "f(di(" },
	{ "da_paren",          "one (two) three",   "f(da(" },
	{ "ci_paren",          "one (two) three",   "f(ci(x<Esc>" },
	{ "di_brace",          "one {two} three",   "f{di{" },
	{ "da_brace",          "one {two} three",   "f{da{" },
	{ "di_bracket",        "one [two] three",   "f[di[" },
	{ "da_bracket",        "one [two] three",   "f[da[" },
	{ "di_angle",          "one <two> three",   "f<di<" },
	{ "da_angle",          "one <two> three",   "f<da<" },
	{ "di_paren_nested",   "fn(a, (b, c))",     "f(di(" },
	{ "di_paren_empty",    "fn() end",          "f(di(" },
	{ "dib_alias",         "one (two) three",   "f(dib" },
	{ "diB_alias",         "one {two} three",   "f{diB" },

	-- ===================== delimiter matching =====================
	{ "percent_paren",     "(hello) world",     "%" },
	{ "percent_brace",     "{hello} world",     "%" },
	{ "percent_bracket",   "[hello] world",     "%" },
	{ "percent_from_close", "(hello) world",    "f)%" },
	{ "d_percent_paren",   "(hello) world",     "d%" },

	-- ===================== insert mode entry =====================
	{ "i_insert",          "hello",             "iX<Esc>" },
	{ "a_append",          "hello",             "aX<Esc>" },
	{ "I_front",           "  hello",           "IX<Esc>" },
	{ "A_end",             "hello",             "AX<Esc>" },
	{ "o_open_below",      "hello",             "oworld<Esc>" },
	{ "O_open_above",      "hello",             "Oworld<Esc>" },

	-- ===================== insert mode operations =====================
	{ "empty_input",       "",                  "i hello<Esc>" },
	{ "insert_escape",     "hello",             "aX<Esc>" },
	{ "ctrl_w_del_word",   "hello world",       "A<C-w><Esc>" },
	{ "ctrl_h_backspace",  "hello",             "A<C-h><Esc>" },

	-- ===================== undo / redo =====================
	{ "u_undo_delete",     "hello world",       "dwu" },
	{ "u_undo_change",     "hello world",       "ciwfoo<Esc>u" },
	{ "u_undo_x",          "hello",             "xu" },
	{ "ctrl_r_redo",       "hello",             "xu<C-r>" },
	{ "u_multiple",        "hello world",       "xdwu" },
	{ "redo_after_undo",   "hello world",       "dwu<C-r>" },

	-- ===================== dot repeat =====================
	{ "dot_repeat_x",      "hello",             "x." },
	{ "dot_repeat_dw",     "one two three",     "dw." },
	{ "dot_repeat_cw",     "one two three",     "cwfoo<Esc>w." },
	{ "dot_repeat_r",      "hello",             "ra.." },
	{ "dot_repeat_s",      "hello",             "sX<Esc>l." },

	-- ===================== counts =====================
	{ "count_h",           "hello world",       "$3h" },
	{ "count_l",           "hello world",       "3l" },
	{ "count_w",           "one two three four", "2w" },
	{ "count_b",           "one two three four", "$2b" },
	{ "count_x",           "hello",             "3x" },
	{ "count_dw",          "one two three four", "2dw" },
	{ "verb_count_motion", "one two three four", "d2w" },
	{ "count_s",           "hello",             "3sX<Esc>" },

	-- ===================== indent / dedent =====================
	{ "indent_line",       "hello",             ">>" },
	{ "dedent_line",       "\thello",           "<<" },
	{ "indent_double",     "hello",             ">>>>" },

	-- ===================== join =====================
	{ "J_join_lines",      "hello\nworld",      "J" },

	-- ===================== case in visual =====================
	{ "v_u_lower",         "HELLO",             "vlllu" },
	{ "v_U_upper",         "hello",             "vlllU" },

	-- ===================== visual mode =====================
	{ "v_d_delete",        "hello world",       "vwwd" },
	{ "v_x_delete",        "hello world",       "vwwx" },
	{ "v_c_change",        "hello world",       "vwcfoo<Esc>" },
	{ "v_y_p_yank",        "hello world",       "vwyAp <Esc>p" },
	{ "v_dollar_d",        "hello world",       "wv$d" },
	{ "v_0_d",             "hello world",       "$v0d" },
	{ "ve_d",              "hello world",       "ved" },
	{ "v_o_swap",          "hello world",       "vllod" },
	{ "v_r_replace",       "hello",             "vlllrx" },
	{ "v_tilde_case",      "hello",             "vlll~" },

	-- ===================== visual line mode =====================
	{ "V_d_delete",        "hello world",       "Vd" },
	{ "V_y_p",             "hello world",       "Vyp" },
	{ "V_S_change",        "hello world",       "VSfoo<Esc>" },

	-- ===================== increment / decrement =====================
	{ "ctrl_a_inc",        "num 5 end",         "w<C-a>" },
	{ "ctrl_x_dec",        "num 5 end",         "w<C-x>" },
	{ "ctrl_a_negative",   "num -3 end",        "w<C-a>" },
	{ "ctrl_x_to_neg",     "num 0 end",         "w<C-x>" },
	{ "ctrl_a_count",      "num 5 end",         "w3<C-a>" },

	-- ===================== misc / edge cases =====================
	{ "delete_empty",      "",                  "x" },
	{ "undo_on_empty",     "",                  "u" },
	{ "w_single_char",     "a b c",             "w" },
	{ "dw_last_word",      "hello",             "dw" },
	{ "dollar_single",     "h",                 "$" },
	{ "caret_no_ws",       "hello",             "$^" },
	{ "f_last_char",       "hello",             "fo" },
	{ "r_on_space",        "hello world",       "5|r-" },
}

-- Map vim special key names to Rust string escape sequences
local key_to_bytes = {
	["<Esc>"]   = "\\x1b",
	["<CR>"]    = "\\r",
	["<BS>"]    = "\\x7f",
	["<Tab>"]   = "\\t",
	["<Del>"]   = "\\x1b[3~",
	["<Up>"]    = "\\x1b[A",
	["<Down>"]  = "\\x1b[B",
	["<Right>"] = "\\x1b[C",
	["<Left>"]  = "\\x1b[D",
	["<Home>"]  = "\\x1b[H",
	["<End>"]   = "\\x1b[F",
}

-- Convert vim key notation to Rust string escape sequences
local function keys_to_rust(keys)
	local result = keys
	result = result:gsub("<C%-(.)>", function(ch)
		local byte = string.byte(ch:lower()) - string.byte('a') + 1
		return string.format("\\x%02x", byte)
	end)
	for name, bytes in pairs(key_to_bytes) do
		result = result:gsub(vim.pesc(name), bytes)
	end
	return result
end

-- Escape a string for use in a Rust string literal
local function rust_escape(s)
	return s:gsub("\\", "\\\\"):gsub('"', '\\"'):gsub("\n", "\\n"):gsub("\t", "\\t")
end

io.write("vi_test! {\n")

for i, test in ipairs(tests) do
	local name, input, keys = test[1], test[2], test[3]

	-- Fresh buffer and register state
	local input_lines = vim.split(input, "\n", { plain = true })
	vim.api.nvim_buf_set_lines(0, 0, -1, false, input_lines)
	vim.api.nvim_win_set_cursor(0, { 1, 0 })
	vim.fn.setreg('"', '')

	-- Execute the key sequence synchronously
	local translated = vim.api.nvim_replace_termcodes(keys, true, false, true)
	vim.api.nvim_feedkeys(translated, "ntx", false)
	vim.api.nvim_exec_autocmds("CursorMoved", {})

	-- Capture result
	local lines = vim.api.nvim_buf_get_lines(0, 0, -1, false)
	local result = table.concat(lines, "\n")
	local cursor_col = vim.api.nvim_win_get_cursor(0)[2]

	local rust_keys = keys_to_rust(keys)
	local rust_input = rust_escape(input)
	local rust_result = rust_escape(result)

	local sep = ";"
	if i == #tests then sep = "" end

	io.write(string.format('\tvi_%s: "%s" => "%s" => "%s", %d%s\n',
		name, rust_input, rust_keys, rust_result, cursor_col, sep))
end

io.write("}\n")

vim.cmd("qa!")
