<#
.SYNOPSIS
Vocabulary lint for Companion Lobby (PowerShell twin of tools/vocabulary.sh).

.DESCRIPTION
Searches crates/, docs/, README.md and .github/ with ripgrep for every rule in
tools/vocabulary.rules, skipping each rule's allowed paths and the justified
exceptions in tools/vocabulary.allow. A rule whose pattern holds a space is
also searched across line breaks, so a phrase wrapped onto the next line, or
the next line of a comment, is found too. Also refuses forbidden file names and
any Cargo.toml that reaches outside the workspace.

Exit status: 0 clean, 1 violations found, 2 the lint itself could not run.

.PARAMETER Root
The repository root to scan. Defaults to the parent of this script's folder.
#>
[CmdletBinding()]
param(
    [string] $Root
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$here = $PSScriptRoot
if (-not $Root) { $Root = Join-Path $here '..' }
$rulesPath = Join-Path $here 'vocabulary.rules'
$allowPath = Join-Path $here 'vocabulary.allow'

function Fail-Lint([string] $message) {
    [Console]::Error.WriteLine("vocabulary: $message")
    exit 2
}

# What a space in a rule's pattern also matches: a line break, with the next
# line's indentation and comment marker.
$wrap = '(?:[ ]|[ \t]*\r?\n[ \t]*(?:(?://[/!]?|#+|\*|>)[ \t]*)?)'

# The pattern with every space outside a character class matching a line
# break too, or nothing when it holds no such space.
function Get-WrappedPattern([string] $regex) {
    $out = New-Object System.Text.StringBuilder
    $depth = 0
    $escaped = $false
    $spaced = $false
    foreach ($character in $regex.ToCharArray()) {
        if ($escaped) { [void] $out.Append($character); $escaped = $false; continue }
        if ($character -eq '\') { [void] $out.Append($character); $escaped = $true; continue }
        if ($character -eq '[') { $depth++ }
        elseif ($character -eq ']' -and $depth -gt 0) { $depth-- }
        if ($character -eq ' ' -and $depth -eq 0) { [void] $out.Append($wrap); $spaced = $true }
        else { [void] $out.Append($character) }
    }
    if ($spaced) { $out.ToString() } else { $null }
}

function Read-Table([string] $path) {
    foreach ($line in (Get-Content -LiteralPath $path -Encoding UTF8)) {
        if ($line.Trim().Length -eq 0 -or $line.StartsWith('#')) { continue }
        , ($line -split "`t")
    }
}

if (-not (Get-Command rg -ErrorAction SilentlyContinue)) {
    Fail-Lint 'ripgrep (rg) is required'
}

$rules = @(Read-Table $rulesPath)
$allow = @(Read-Table $allowPath)
$violations = New-Object System.Collections.Generic.List[string]

Push-Location -LiteralPath $Root
try {
    $scope = @('crates', 'docs', 'README.md', '.github' | Where-Object { Test-Path -LiteralPath $_ })
    if ($scope.Count -eq 0) { Fail-Lint "nothing to scan under $Root" }

    # Every exception must name a known rule and carry a justification.
    $ruleIds = @($rules | ForEach-Object { $_[0] })
    foreach ($entry in $allow) {
        if ($entry.Count -lt 3 -or -not $entry[1] -or -not $entry[2].Trim()) {
            $violations.Add("allowlist: $($entry[0]) needs a rule id and a one-line justification")
            continue
        }
        if ($ruleIds -notcontains $entry[1]) {
            $violations.Add("allowlist: $($entry[0]) names an unknown rule '$($entry[1])'")
        }
    }

    # Content rules.
    foreach ($rule in $rules) {
        if ($rule.Count -ne 4) { Fail-Lint "malformed rule line: $($rule -join ' | ')" }
        $id, $sensitivity, $regex, $allowed = $rule
        $rgArgs = @('--no-heading', '--line-number', '--with-filename', '--color', 'never', '--hidden', '-g', '!target/**')
        if ($sensitivity -eq 'i') { $rgArgs += '--ignore-case' } else { $rgArgs += '--case-sensitive' }
        if ($allowed -ne '-') {
            foreach ($glob in $allowed.Split(',')) { $rgArgs += @('-g', "!$glob") }
        }
        foreach ($entry in $allow) {
            if ($entry.Count -ge 2 -and $entry[1] -eq $id) { $rgArgs += @('-g', "!$($entry[0])") }
        }
        $wrapped = Get-WrappedPattern $regex
        $wrapArgs = $rgArgs + @('--multiline', '--only-matching', '-e', $wrapped, '--') + $scope
        $rgArgs += @('-e', $regex, '--') + $scope
        $found = @(& rg @rgArgs)
        if ($LASTEXITCODE -eq 0) {
            foreach ($line in $found) { $violations.Add("${id}: $line") }
        }
        elseif ($LASTEXITCODE -ne 1) {
            Fail-Lint "ripgrep failed on rule $id"
        }
        if (-not $wrapped) { continue }
        # The same search across line breaks. Each line of a match that
        # spans lines is a piece the rule alone does not match; a match on
        # one line was reported above.
        $pieces = @(& rg @wrapArgs)
        if ($LASTEXITCODE -gt 1) { Fail-Lint "ripgrep failed on rule $id across lines" }
        $caseArg = if ($sensitivity -eq 'i') { '--ignore-case' } else { '--case-sensitive' }
        foreach ($piece in $pieces) {
            $text = ($piece -split ':', 3)[2]
            $null = $text | & rg --quiet $caseArg -e $regex
            if ($LASTEXITCODE -eq 1) { $violations.Add("${id}: $piece") }
            elseif ($LASTEXITCODE -ne 0) { Fail-Lint "ripgrep failed on rule $id across lines" }
        }
    }

    # Forbidden file and directory names.
    $names = '(^|[/\\])((hub|forum|intake|participation|session|boundary|poller|perspective|operations|diagnostics|refs|lockfile)\.rs|manager\.html|atmosphere\.(css|js)|ascii-scenes\.js)$|(^|[/\\])(application|presentation)[/\\]|(^|[/\\])crates[/\\]cli[/\\]'
    $files = @(& rg --files --hidden -g '!target/**' -- @scope)
    if ($LASTEXITCODE -ne 0) { Fail-Lint 'ripgrep could not list files' }
    foreach ($file in $files) {
        if ($file -match $names) { $violations.Add("file-names: $file") }
    }

    # Manifests never reach outside the workspace.
    $escaping = @(& rg --no-heading --line-number --with-filename --color never --hidden -g 'Cargo.toml' -g '!target/**' -e 'CompanionLobby-harvest' -e '\.\./' -- .)
    if ($LASTEXITCODE -eq 0) {
        foreach ($line in $escaping) { $violations.Add("manifest-path: $line") }
    }
    elseif ($LASTEXITCODE -ne 1) {
        Fail-Lint 'ripgrep failed on the manifest check'
    }
}
finally {
    Pop-Location
}

if ($violations.Count -gt 0) {
    $violations | ForEach-Object { Write-Output $_ }
    [Console]::Error.WriteLine("vocabulary: $($violations.Count) violation(s)")
    exit 1
}
Write-Output 'vocabulary: clean'
exit 0
