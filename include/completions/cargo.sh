_cargo_comp() {
	command -v cargo > /dev/null || return
	local CWORD="$2"
	local PWORD="$3"

	# 'cargo --list' is a fork we don't want on every Tab, so build the
	# subcommand map once into a namespaced global and reuse it.
	if [[ -z "${_cargo_subcmds_built:-}" ]]; then
		declare -A _cargo_subcmds
		while read -q name desc; do
			if (("${#desc}" == 1)); then desc=""; fi
			_cargo_subcmds[$name]=$desc
		done < <(cargo --list | tail -n +2 | vice --lines -q -m 'w' -c 'viW' -m 'W' -c '$')
		_cargo_subcmds_built=1
	fi

	# __find_subcmd expects the verb set in a local named 'subcommands'; copy
	# from the cache (cheap, no fork).
	local -A subcommands
	for name in "${!_cargo_subcmds[@]}"; do
		subcommands[$name]=${_cargo_subcmds[$name]}
	done

	local sub_cmd

	read -q sub_cmd idx <<< "$(__find_subcmd)"

	if [[ -z "$sub_cmd" ]]; then
		compadd -A subcommands
	else
		for ((n=0; n<idx; n++)); do
			fpop COMP_WORDS > /dev/null
		done
		case "$sub_cmd" in
			r|run) _cargo_run_comp ;;
			b|build) _cargo_build_comp ;;
			t|test) _cargo_test_comp ;;
			c|check) _cargo_check_comp ;;
			bench) _cargo_bench_comp ;;
			d|doc) _cargo_doc_comp ;;
			rm|remove) _cargo_remove_comp ;;
		esac
	fi
}

# ── tiers ───────────────────────────────────────────────────────────────────
# Universal flags, present on essentially every cargo subcommand.
declare -A _cargo_global_long=(
	[verbose]='Use verbose output'
	[quiet]='Do not print cargo log messages'
	[color]='Coloring [possible values: auto, always, never]'
	[config]='Override a config value'
	[help]='Print help'
	[package]='Package to operate on'
	[manifest-path]='Path to Cargo.toml'
	[locked]='Assert that `Cargo.lock` will remain unchanged'
	[offline]='Run without accessing the network'
	[frozen]='Equivalent to specifying both --locked and --offline'
)
declare -A _cargo_global_short=(
	[v]='Use verbose output'
	[q]='Do not print cargo log messages'
	[Z]='Unstable (nightly-only) flags to Cargo'
	[h]='Print help'
	[p]='Package to operate on'
)

# Compile flags, layered on top of the global tier by the build family
# (build, run, test, check, bench, doc, ...).
declare -A _cargo_build_long=(
	[message-format]='Error format'
	[bin]='Name of the bin target'
	[example]='Name of the example target'
	[features]='Space or comma separated list of features to activate'
	[all-features]='Activate all available features'
	[no-default-features]='Do not activate the `default` feature'
	[jobs]='Number of parallel jobs, defaults to # of CPUs'
	[keep-going]='Do not abort the build as soon as there is an error'
	[release]='Build artifacts in release mode, with optimizations'
	[profile]='Build artifacts with the specified profile'
	[target]='Build for the target triple'
	[target-dir]='Directory for all generated artifacts'
	[unit-graph]='Output build graph in JSON (unstable)'
	[timings]='Output a build timing report after the build'
	[ignore-rust-version]='Ignore `rust_version` specification in packages'
)
declare -A _cargo_build_short=(
	[F]='Space or comma separated list of features to activate'
	[j]='Number of parallel jobs, defaults to # of CPUs'
	[r]='Build artifacts in release mode, with optimizations'
)

# Workspace + target selection, layered on top of the build tier by the
# multi-target commands (build, test, check, bench, doc). `run` skips this
# tier since it operates on a single target. All long-only, no short forms.
declare -A _cargo_select_long=(
	[future-incompat-report]='Output a future incompatibility report at the end'
	[workspace]='Operate on all packages in the workspace'
	[exclude]='Exclude packages'
	[all]='Alias for --workspace (deprecated)'
	[lib]='Target only this package'\''s library'
	[bins]='Target all binaries'
	[examples]='Target all examples'
	[tests]='Target all test targets'
	[test]='Target only the specified test'
	[benches]='Target all bench targets'
	[bench]='Target only the specified bench'
	[all-targets]='Target all targets'
)

# ── data sources ────────────────────────────────────────────────────────────
_cargo_meta() {
	command -v jq > /dev/null || return
	cargo metadata --format-version 1 --no-deps 2>/dev/null
}

_cargo_project() {
	cargo locate-project --workspace --message-format plain 2>/dev/null
}

_cargo_add_targets() {
	local -a x=( $(rustc --print target-list 2>/dev/null) )
	compadd -a x
}

_cargo_add_bins() {
	local -a x=( $(_cargo_meta | jq -r '.packages[].targets[] | select(.kind[] == "bin") | .name') )
	compadd -a x
}

_cargo_add_examples() {
	local -a x=( $(_cargo_meta | jq -r '.packages[].targets[] | select(.kind[] == "example") | .name') )
	compadd -a x
}

_cargo_add_tests() {
	local -a x=( $(_cargo_meta | jq -r '.packages[].targets[] | select(.kind[] == "test") | .name') )
	compadd -a x
}

_cargo_add_benches() {
	local -a x=( $(_cargo_meta | jq -r '.packages[].targets[] | select(.kind[] == "bench") | .name') )
	compadd -a x
}

_cargo_add_packages() {
	local -a x=( $(_cargo_meta | jq -r '.packages[].name') )
	compadd -a x
}

_cargo_add_profiles() {
	local manifest
	manifest=$(_cargo_project)
	local -a profiles=( dev release bench test )
	if [[ -f "$manifest" ]]; then
		while read -r p; do
			push profiles "$p"
		done < <(vice --lines -m '/profile<CR>' -m 'f.w' -c 't]' "$manifest")
	fi
	compadd -a profiles
}

_cargo_message_format() {
	compadd human short json json-diagnostic-short \
		json-diagnostic-rendered-ansi json-render-diagnostics
}

# ── value completion ────────────────────────────────────────────────────────
# Each returns 0 when it handled $PWORD so callers can `&& return`.
_cargo_global_values() {
	case "$PWORD" in
		--color) compadd auto always never ;;
		--package | --exclude) _cargo_add_packages ;;
		*) return 1 ;;
	esac
}

_cargo_build_values() {
	case "$PWORD" in
		--target) _cargo_add_targets ;;
		--bin) _cargo_add_bins ;;
		--example) _cargo_add_examples ;;
		--test) _cargo_add_tests ;;
		--bench) _cargo_add_benches ;;
		--profile) _cargo_add_profiles ;;
		--message-format) _cargo_message_format ;;
		*) _cargo_global_values; return ;;
	esac
}

# ── flag offering ───────────────────────────────────────────────────────────
# Emit flags (no position guard); the caller decides when to call these.
_cargo_global_flags() {
	compadd -A _cargo_global_short -P '-'
	compadd -A _cargo_global_long -P '--'
	compadd -A short_opts -P '-'
	compadd -A long_opts -P '--'
}

_cargo_build_flags() {
	compadd -A _cargo_build_short -P '-'
	compadd -A _cargo_build_long -P '--'
	_cargo_global_flags
}

_cargo_select_flags() {
	compadd -A _cargo_select_long -P '--'
	_cargo_build_flags
}

# Compile-only entry (run): flags at a flag or fresh-arg position.
_cargo_build_opts() {
	[[ "$CWORD" == -* || -z "$CWORD" ]] && _cargo_build_flags
}

# Multi-target entry (build, test, check, ...): adds the select tier.
_cargo_select_opts() {
	[[ "$CWORD" == -* || -z "$CWORD" ]] && _cargo_select_flags
}

# ── subcommands ─────────────────────────────────────────────────────────────
_cargo_run_comp() {
	# run adds no flags beyond the build category.
	local -A long_opts=()
	local -A short_opts=()
	_cargo_build_values && return
	_cargo_build_opts
}

_cargo_build_comp() {
	local -A long_opts=(
		[artifact-dir]='Copy final artifacts to this directory (unstable)'
	)
	local -A short_opts=()
	_cargo_build_values && return
	_cargo_select_opts
}

_cargo_test_comp() {
	local -A long_opts=(
		[no-run]='Compile, but don'\''t run tests'
		[no-fail-fast]='Run all tests regardless of failure'
		[doc]='Test only this library'\''s documentation'
	)
	local -A short_opts=()
	_cargo_build_values && return
	_cargo_select_opts
}

_cargo_check_comp() {
	# check adds no flags beyond the select tier.
	local -A long_opts=()
	local -A short_opts=()
	_cargo_build_values && return
	_cargo_select_opts
}

_cargo_bench_comp() {
	local -A long_opts=(
		[no-run]='Compile, but don'\''t run benchmarks'
		[no-fail-fast]='Run all benchmarks regardless of failure'
	)
	local -A short_opts=()
	_cargo_build_values && return
	_cargo_select_opts
}

_cargo_doc_comp() {
	# doc only has part of the select tier (no --tests/--benches/--all-targets),
	# so it lists its selection flags directly rather than over-offering.
	local -A long_opts=(
		[workspace]='Document all packages in the workspace'
		[exclude]='Exclude packages from the build'
		[all]='Alias for --workspace (deprecated)'
		[lib]='Document only this package'\''s library'
		[bins]='Document all binaries'
		[examples]='Document all examples'
		[no-deps]='Do not build documentation for dependencies'
		[open]='Open the docs in a browser after building'
		[document-private-items]='Document private items'
	)
	local -A short_opts=()
	_cargo_build_values && return
	_cargo_build_opts
}

_cargo_remove_comp() {
	local -A long_opts=(
		[dry-run]='Don'\''t actually write the manifest'
		[dev]='Remove a dev-dependency'
		[build]='Remove a build-dependency'
		[target]='Remove a target-specific dependency'
	)
	local -A short_opts=(
		[n]='Don'\''t actually write the manifest'
	)

	_cargo_global_values && return

	if [[ "$CWORD" == -* ]]; then
		_cargo_global_flags
	else
		# positional: the dependencies declared in the manifest
		local -a deps=(
			$(_cargo_meta | jq -r '.packages[].dependencies[] | .rename // .name' | sort -u)
		)
		compadd -a deps
	fi
}

complete -d -f -F _cargo_comp cargo
