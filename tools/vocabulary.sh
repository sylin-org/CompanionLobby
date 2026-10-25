#!/usr/bin/env sh
# Vocabulary lint for Companion Lobby (POSIX twin of tools/vocabulary.ps1).
#
# Searches crates/, docs/, README.md and .github/ with ripgrep for every rule in
# tools/vocabulary.rules, skipping each rule's allowed paths and the justified
# exceptions in tools/vocabulary.allow. A rule whose pattern holds a space is
# also searched across line breaks, so a phrase wrapped onto the next line, or
# the next line of a comment, is found too. Also refuses forbidden file names
# and any Cargo.toml that reaches outside the workspace.
#
# Usage: tools/vocabulary.sh [repository-root]
# Exit status: 0 clean, 1 violations found, 2 the lint itself could not run.

set -eu
set -f

here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
root=${1:-"$here/.."}
rules="$here/vocabulary.rules"
allow="$here/vocabulary.allow"
tab=$(printf '\t')

if ! command -v rg >/dev/null 2>&1; then
    echo "vocabulary: ripgrep (rg) is required" >&2
    exit 2
fi

cd "$root"

scope=""
for candidate in crates docs README.md .github; do
    if [ -e "$candidate" ]; then
        scope="$scope $candidate"
    fi
done
if [ -z "$scope" ]; then
    echo "vocabulary: nothing to scan under $root" >&2
    exit 2
fi

violations=0
report() {
    printf '%s\n' "$1"
    violations=$((violations + 1))
}

# Every exception must name a known rule and carry a justification.
while IFS="$tab" read -r apath arule ajustify || [ -n "$apath" ]; do
    case "$apath" in '' | '#'*) continue ;; esac
    if [ -z "${arule:-}" ] || [ -z "${ajustify:-}" ]; then
        report "allowlist: $apath needs a rule id and a one-line justification"
        continue
    fi
    if ! grep -q "^$arule$tab" "$rules"; then
        report "allowlist: $apath names an unknown rule '$arule'"
    fi
done <"$allow"

# What a space in a rule's pattern also matches: a line break, with the next
# line's indentation and comment marker.
wrap='(?:[ ]|[ \t]*\r?\n[ \t]*(?:(?://[/!]?|#+|\*|>)[ \t]*)?)'

# The pattern with every space outside a character class matching a line
# break too, or nothing when it holds no such space.
wrapped_pattern() {
    PATTERN=$1 WRAP=$wrap awk 'BEGIN {
        regex = ENVIRON["PATTERN"]; wrap = ENVIRON["WRAP"]
        out = ""; depth = 0; escaped = 0; spaced = 0
        for (i = 1; i <= length(regex); i++) {
            c = substr(regex, i, 1)
            if (escaped) { out = out c; escaped = 0; continue }
            if (c == "\\") { out = out c; escaped = 1; continue }
            if (c == "[") depth++
            else if (c == "]" && depth > 0) depth--
            if (c == " " && depth == 0) { out = out wrap; spaced = 1 }
            else out = out c
        }
        if (spaced) print out
    }'
}

# Searches the scope for the rule being read, with the pattern $1 and the
# rest of the arguments passed to ripgrep first, skipping the rule's allowed
# paths and its exceptions.
search() {
    pattern=$1
    shift
    set -- "$@" --no-heading --line-number --with-filename --color never --hidden -g '!target/**'
    if [ "$sensitivity" = "i" ]; then
        set -- "$@" --ignore-case
    else
        set -- "$@" --case-sensitive
    fi
    if [ "$allowed" != "-" ]; then
        saved_ifs=$IFS
        IFS=,
        for glob in $allowed; do
            set -- "$@" -g "!$glob"
        done
        IFS=$saved_ifs
    fi
    while IFS="$tab" read -r apath arule ajustify || [ -n "$apath" ]; do
        case "$apath" in '' | '#'*) continue ;; esac
        if [ "$arule" = "$id" ]; then
            set -- "$@" -g "!$apath"
        fi
    done <"$allow"
    # shellcheck disable=SC2086 # scope is a space-separated list of fixed paths
    rg "$@" -e "$pattern" -- $scope
}

# Content rules.
while IFS="$tab" read -r id sensitivity regex allowed <&3 || [ -n "$id" ]; do
    case "$id" in '' | '#'*) continue ;; esac
    status=0
    found=$(search "$regex") || status=$?
    if [ "$status" -eq 0 ]; then
        printf '%s\n' "$found" | while IFS= read -r line; do
            printf '%s: %s\n' "$id" "$line"
        done
        violations=$((violations + $(printf '%s\n' "$found" | wc -l)))
    elif [ "$status" -ne 1 ]; then
        echo "vocabulary: ripgrep failed on rule $id" >&2
        exit 2
    fi
    wrapped=$(wrapped_pattern "$regex")
    [ -n "$wrapped" ] || continue
    # The same search across line breaks. Each line of a match that spans
    # lines is a piece the rule alone does not match; a match on one line
    # was reported above.
    if [ "$sensitivity" = "i" ]; then
        case_flag=--ignore-case
    else
        case_flag=--case-sensitive
    fi
    status=0
    pieces=$(search "$wrapped" --multiline --only-matching) || status=$?
    if [ "$status" -gt 1 ]; then
        echo "vocabulary: ripgrep failed on rule $id across lines" >&2
        exit 2
    fi
    [ "$status" -eq 0 ] || continue
    spanned=$(printf '%s\n' "$pieces" | while IFS= read -r piece; do
        text=${piece#*:}
        text=${text#*:}
        if ! printf '%s\n' "$text" | rg --quiet "$case_flag" -e "$regex"; then
            printf '%s: %s\n' "$id" "$piece"
        fi
    done)
    if [ -n "$spanned" ]; then
        printf '%s\n' "$spanned"
        violations=$((violations + $(printf '%s\n' "$spanned" | wc -l)))
    fi
done 3<"$rules"

# Forbidden file and directory names.
names='(^|[/\\])((hub|forum|intake|participation|session|boundary|poller|perspective|operations|diagnostics|refs|lockfile)\.rs|manager\.html|atmosphere\.(css|js)|ascii-scenes\.js)$|(^|[/\\])(application|presentation)[/\\]|(^|[/\\])crates[/\\]cli[/\\]'
# shellcheck disable=SC2086
files=$(rg --files --hidden -g '!target/**' -- $scope) || {
    echo "vocabulary: ripgrep could not list files" >&2
    exit 2
}
status=0
named=$(printf '%s\n' "$files" | rg --ignore-case -e "$names") || status=$?
if [ "$status" -gt 1 ]; then
    echo "vocabulary: ripgrep failed on the file name check" >&2
    exit 2
fi
if [ -n "$named" ]; then
    printf '%s\n' "$named" | while IFS= read -r line; do
        printf 'file-names: %s\n' "$line"
    done
    violations=$((violations + $(printf '%s\n' "$named" | wc -l)))
fi

# Manifests never reach outside the workspace.
status=0
escaping=$(rg --no-heading --line-number --with-filename --color never --hidden \
    -g 'Cargo.toml' -g '!target/**' -e 'CompanionLobby-harvest' -e '\.\./' -- .) || status=$?
if [ "$status" -eq 0 ]; then
    printf '%s\n' "$escaping" | while IFS= read -r line; do
        printf 'manifest-path: %s\n' "$line"
    done
    violations=$((violations + $(printf '%s\n' "$escaping" | wc -l)))
elif [ "$status" -ne 1 ]; then
    echo "vocabulary: ripgrep failed on the manifest check" >&2
    exit 2
fi

if [ "$violations" -gt 0 ]; then
    echo "vocabulary: $violations violation(s)" >&2
    exit 1
fi
echo "vocabulary: clean"
