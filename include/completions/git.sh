_compadd_gen() {
	local c d
	{ read -q -a c; read -q -a d; } < <("$@")
	compadd -a c -d d
}
_git_local_branches() {
	local b cands=() descs=()
	while IFS= read -r b; do
		push cands "$b"
		push descs "local branch"
	done < <(git for-each-ref --format='%(refname:strip=2)' --sort=-committerdate refs/heads/ 2>/dev/null | grep -v '/HEAD$')
	quote "${cands[@]}"
	quote "${descs[@]}"
}
_git_remote_branches() {
	local b cands=() descs=()
	while IFS= read -r b; do
		push cands "$b"
		push descs "remote branch"
	done < <(git for-each-ref --format='%(refname:strip=2)' refs/remotes/ 2>/dev/null | grep -v '/HEAD$')
	quote "${cands[@]}"
	quote "${descs[@]}"
}
_git_branches() {
	local local_c local_d remote_c remote_d
	{ read -q -a local_c; read -q -a local_d; } < <(_git_local_branches)
	{ read -q -a remote_c; read -q -a remote_d; } < <(_git_remote_branches)
	quote "${local_c[@]}" "${remote_c[@]}"
	quote "${local_d[@]}" "${remote_d[@]}"
}
_git_tags() {
	local b cands=() descs=()
	while IFS=$'\t' read -r tag hash; do
		push cands "$tag"
		push descs "$hash"
	done < <(git for-each-ref \
		--format='%(refname:strip=2)%09%(objectname:short)' \
	 	--sort=-version:refname refs/tags/ 2>/dev/null
	)
	quote "${cands[@]}"
	quote "${descs[@]}"
}
_git_files() {
    # $1 = mode: modified | staged | untracked
    local mode="$1" entry x y path desc _old
    local cands=() descs=()
    while IFS= read -r -d '' entry; do
        x="${entry:0:1}"; y="${entry:1:1}"; path="${entry:3}"
        # rename/copy entries carry an extra NUL field (the old path) — consume it
        case "$x" in R|C) IFS= read -r -d '' _old ;; esac
        case "$mode" in
            modified)
                case "$y" in M) desc="modified" ;; D) desc="deleted" ;; *) continue ;; esac ;;
            staged)
                case "$x" in
                    M) desc="staged" ;; A) desc="added" ;; D) desc="deleted (staged)" ;;
                    R) desc="renamed" ;; C) desc="copied" ;; *) continue ;;
                esac ;;
            untracked)
                case "$x$y" in "??") desc="untracked" ;; *) continue ;; esac ;;
        esac
        push cands "$path"; push descs "$desc"
    done < <(git status --porcelain -z 2>/dev/null)
    quote "${cands[@]}"
    quote "${descs[@]}"
}
_git_files_modified()  { _git_files modified; }
_git_files_staged()    { _git_files staged; }
_git_files_untracked() { _git_files untracked; }

_git() {
	local cur="${COMP_WORDS[$COMP_CWORD]}"
	local cmd="" i=1
	while (( i < "$COMP_CWORD" )); do
		case "${COMP_WORDS[$i]}" in
			-C|-c|--git-dir|--work-tree|--namespace|--exec-path|--config-env)
				# option with argument
				let i=i+2
			;;
			-*)
				# flag
				let i=i+1
			;;
			*)
				cmd="${COMP_WORDS[$i]}"; break
			;;
		esac
	done

	if [ -z "$cmd" ]; then _git_complete_commands; return; fi

	case "$cmd" in
		checkout|switch) _git_checkout ;;
    branch) _git_branch ;;
    merge) _git_merge ;;
    add) _git_add ;;
    commit) _git_commit ;;
		*) _compadd_gen _git_files_modified ;;
	esac
}
_git_complete_commands() {
	declare -A c=(
		[checkout]="Switch branches or restore working tree files"
		[branch]="List, create, or delete branches"
		[merge]="Join branches together"
		[commit]="Record staged changes to the repository"
		[add]="Stage changes to the repository for committing"
	)
	compadd -A c
}
_git_seen_doubledash() {
  _git_has_flag --; return $?
}

_git_has_flag() {
  local w f
  for w in "${COMP_WORDS[@]}"; do
    for f in "$@"; do
      [ "$w" == "$f" ] && return 0
    done
  done
  return 1
}

_git_add() {
  case "$cur" in -*) _git_add_flags; return ;; esac
  _compadd_gen _git_files_untracked
  _compadd_gen _git_files_modified
}
_git_add_flags() {
  local -A flags=(
    [-A]="match files both in working tree and index"
    [--all]="match files both in working tree and index"
    [-e]="manually create a patch"
    [--edit]="manually create a patch"
    [-f]="allow adding otherwise ignored files"
    [--force]="allow adding otherwise ignored files"
    [-i]="interactive picking of hunks"
    [--interactive]="interactive picking of hunks"
    [-n]="dry run"
    [--dry-run]="dry run"
    [-p]="select hunks interactively"
    [--patch]="select hunks interactively"
    [-u]="update tracked files"
    [--update]="update tracked files"
    [-v]="verbose"
    [--verbose]="verbose"
    [--chmod]="override executable bit of the file"
    [--ignore-errors]="ignore files that cannot be added due to errors"
    [--ignore-missing]="ignore files that cannot be found"
    [--refresh]="refresh the index"
  )
  compadd -A flags
}

_git_checkout() {
  _git_seen_doubledash && { _compadd_gen _git_files_modified; return; }
  case "$cur" in -*) _git_checkout_flags; return ;; esac
  _compadd_gen _git_branches
  _compadd_gen _git_files_modified
}
_git_checkout_flags() {
  local -A flags=(
    [-b]="create a new branch and switch to it"
    [-B]="create or reset a branch and switch to it"
    [--detach]="detach HEAD at named commit"
    [-f]="force checkout"
    [--force]="force checkout"
    [-q]="quiet"
    [--quiet]="quiet"
    [-t]="set up tracking mode"
    [--track]="set up tracking mode"
    [-h]="show help message"
    [--help]="show help message"
    [--guess]="guess the remote tracking branch"
    [--no-guess]="do not guess the remote tracking branch"
    [--ignore-skip-worktree-bits]="check out all files including sparse entries"
    [--ours]="checkout our version for unmerged files"
    [--theirs]="checkout their version for unmerged files"
    [--overlay]="overlay untracked working tree files"
    [--no-overlay]="do not overlay untracked working tree files"
    [--no-progress]="do not show progress"
    [--recurse-submodules]="recurse into submodules"
    [--no-recurse-submodules]="do not recurse into submodules"
  )
  compadd -A flags
}

_git_merge() {
  : #todo
}
_git_merge_flags() {
  local -A flags=(
    [-n]="do not show a diffstat at the end of the merge"
    [--stat]="show a diffstat at the end of the merge"
    [--summary]="synonym to --stat"
    [--compact-summary]="show a compact-summary at the end of the merge"
    [--log]="add (at most <n>) entries from shortlog to merge commit message"
    [--squash]="create a single commit instead of doing a merge"
    [--commit]="perform a commit if the merge succeeds (default)"
    [-e]="edit message before committing"
    [--edit]="edit message before committing"
    [--cleanup]="how to strip spaces and #comments from message"
    [--ff]="allow fast-forward (default)"
    [--ff-only]="abort if fast-forward is not possible"
    [--rerere-autoupdate]="update the index with reused conflict resolution if possible"
    [--verify-signatures]="verify that the named commit has a valid GPG signature"
    [-s]="merge strategy to use"
    [--strategy]="merge strategy to use"
    [-X]="option for selected merge strategy"
    [--strategy-option]="option for selected merge strategy"
    [-m]="merge commit message (for a non-fast-forward merge)"
    [--message]="merge commit message (for a non-fast-forward merge)"
    [-F]="read message from file"
    [--file]="read message from file"
    [--into-name]="use the given name instead of the real target"
    [-v]="be more verbose"
    [--verbose]="be more verbose"
    [-q]="be more quiet"
    [--quiet]="be more quiet"
    [--abort]="abort the current in-progress merge"
    [--quit]="like --abort but leave index and working tree alone"
    [--continue]="continue the current in-progress merge"
    [--allow-unrelated-histories]="allow merging unrelated histories"
    [--progress]="force progress reporting"
    [-S]="GPG sign commit"
    [--gpg-sign]="GPG sign commit"
    [--autostash]="automatically stash/stash pop before and after"
    [--overwrite-ignore]="update ignored files (default)"
    [--signoff]="add a Signed-off-by trailer"
    [--no-verify]="bypass pre-merge-commit and commit-msg hooks"
    [--verify]="opposite of --no-verify"
  )
  compadd -A flags
}
_git_branch() {
  case "$cur" in -*) _git_branch_flags; return ;; esac
  _git_has_flag -d -D --delete -m -M --move --copy -C && {
    _compadd_gen _git_local_branches
    return
  }

  _git_has_flag -r --remotes && {
    _compadd_gen _git_remote_branches
    return
  }

  _compadd_gen _git_branches
}
_git_branch_flags() {
  local -A flags=(
    [-v]="show hash and subject, give twice for upstream branch"
    [--verbose]="show hash and subject, give twice for upstream branch"
    [-q]="suppress informational messages"
    [--quiet]="suppress informational messages"
    [-t]="set branch tracking configuration"
    [--track]="set branch tracking configuration"
    [-u]="change the upstream info"
    [--set-upstream-to]="change the upstream info"
    [--unset-upstream]="unset the upstream info"
    [--color]="use colored output"
    [-r]="act on remote-tracking branches"
    [--remotes]="act on remote-tracking branches"
    [--contains]="print only branches that contain the commit"
    [--no-contains]="print only branches that don't contain the commit"
    [--abbrev]="use <n> digits to display object names"
    [-a]="list both remote-tracking and local branches"
    [--all]="list both remote-tracking and local branches"
    [-d]="delete fully merged branch"
    [--delete]="delete fully merged branch"
    [-D]="delete branch (even if not merged)"
    [-m]="move/rename a branch and its reflog"
    [--move]="move/rename a branch and its reflog"
    [-M]="move/rename a branch, even if target exists"
    [--omit-empty]="do not output a newline after empty formatted refs"
    [-c]="copy a branch and its reflog"
    [--copy]="copy a branch and its reflog"
    [-C]="copy a branch, even if target exists"
    [-l]="list branch names"
    [--list]="list branch names"
    [--show-current]="show current branch name"
    [--create-reflog]="create the branch's reflog"
    [--edit-description]="edit the description for the branch"
    [-f]="force creation, move/rename, deletion"
    [--force]="force creation, move/rename, deletion"
    [--merged]="print only branches that are merged"
    [--no-merged]="print only branches that are not merged"
    [--column]="list branches in columns"
    [--sort]="field name to sort on"
    [--points-at]="print only branches of the object"
    [-i]="sorting and filtering are case insensitive"
    [--ignore-case]="sorting and filtering are case insensitive"
    [--recurse-submodules]="recurse through submodules"
    [--format]="format to use for the output"
  )
  compadd -A flags
}

_git_commit() {
  case "$cur" in -*) _git_commit_flags; return ;; esac
  _compadd_gen _git_files_staged
}
_git_commit_flags() {
  local -A flags=(
    [-q]="suppress summary after successful commit"
    [--quiet]="suppress summary after successful commit"
    [-v]="show diff in commit message template"
    [--verbose]="show diff in commit message template"
    [-F]="read message from file"
    [--file]="read message from file"
    [--author]="override author for commit"
    [--date]="override date for commit"
    [-m]="commit message"
    [--message]="commit message"
    [-c]="reuse and edit message from specified commit"
    [--reedit-message]="reuse and edit message from specified commit"
    [-C]="reuse message from specified commit"
    [--reuse-message]="reuse message from specified commit"
    [--fixup]="use autosquash formatted message to fixup or amend/reword specified commit"
    [--squash]="use autosquash formatted message to squash specified commit"
    [--reset-author]="the commit is authored by me now (used with -C/-c/--amend)"
    [--trailer]="add custom trailer(s)"
    [-s]="add a Signed-off-by trailer"
    [--signoff]="add a Signed-off-by trailer"
    [-t]="use specified template file"
    [--template]="use specified template file"
    [-e]="force edit of commit"
    [--edit]="force edit of commit"
    [--cleanup]="how to strip spaces and #comments from message"
    [--status]="include status in commit message template"
    [-S]="GPG sign commit"
    [--gpg-sign]="GPG sign commit"
    [-a]="commit all changed files"
    [--all]="commit all changed files"
    [-i]="add specified files to index for commit"
    [--include]="add specified files to index for commit"
    [--interactive]="interactively add files"
    [-p]="interactively add changes"
    [--patch]="interactively add changes"
    [-U]="generate diffs with <n> lines context"
    [--unified]="generate diffs with <n> lines context"
    [--inter-hunk-context]="show context between diff hunks up to the specified number of lines"
    [-o]="commit only specified files"
    [--only]="commit only specified files"
    [-n]="bypass pre-commit and commit-msg hooks"
    [--no-verify]="bypass pre-commit and commit-msg hooks"
    [--verify]="opposite of --no-verify"
    [--dry-run]="show what would be committed"
    [--short]="show status concisely"
    [--branch]="show branch information"
    [--ahead-behind]="compute full ahead/behind values"
    [--porcelain]="machine-readable output"
    [--long]="show status in long format (default)"
    [-z]="terminate entries with NUL"
    [--null]="terminate entries with NUL"
    [--amend]="amend previous commit"
    [--no-post-rewrite]="bypass post-rewrite hook"
    [--post-rewrite]="opposite of --no-post-rewrite"
    [-u]="show untracked files, optional modes: all, normal, no (default: all)"
    [--untracked-files]="show untracked files, optional modes: all, normal, no (default: all)"
    [--pathspec-from-file]="read pathspec from file"
    [--pathspec-file-nul]="with --pathspec-from-file, pathspec elements are separated with NUL"
  )
  compadd -A flags
}
complete -F _git git
