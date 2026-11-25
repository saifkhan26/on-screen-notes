# Sign the extension for Firefox (yours only, not listed on addons.mozilla.org)
# and open the signed .xpi so Firefox installs it — one command per version.
#
# Once: make an account on addons.mozilla.org, create API keys under
# Tools → Manage API Keys, and keep them in your user environment (not in
# this folder):
#   setx AMO_JWT_ISSUER "user:12345:67"
#   setx AMO_JWT_SECRET "abc…"
# (open a new terminal after setx). Needs Node.js (npx fetches web-ext).
#
# Then each time:  .\sign-firefox.ps1          (bumps 0.4.0 → 0.4.1)
#                  .\sign-firefox.ps1 -Minor   (0.4.1 → 0.5.0)
param([switch]$Minor, [switch]$NoBump)
$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot

if (-not $env:AMO_JWT_ISSUER -or -not $env:AMO_JWT_SECRET) {
    throw "Set AMO_JWT_ISSUER and AMO_JWT_SECRET first (see the top of this script)."
}

# Mozilla signs each version once: a new one each time.
$manifestPath = Join-Path $PSScriptRoot "manifest.json"
$text = [IO.File]::ReadAllText($manifestPath)
$m = [regex]::Match($text, '"version":\s*"(\d+)\.(\d+)\.(\d+)"')
if (-not $m.Success) { throw "No version like 1.2.3 in manifest.json" }
$version = "$($m.Groups[1].Value).$($m.Groups[2].Value).$($m.Groups[3].Value)"
if (-not $NoBump) {
    $major = [int]$m.Groups[1].Value; $minorV = [int]$m.Groups[2].Value; $patch = [int]$m.Groups[3].Value
    if ($Minor) { $minorV++; $patch = 0 } else { $patch++ }
    $version = "$major.$minorV.$patch"
    $text = $text.Substring(0, $m.Index) + "`"version`": `"$version`"" + $text.Substring($m.Index + $m.Length)
    [IO.File]::WriteAllText($manifestPath, $text)
    "Version $version"
}

$artifacts = Join-Path $PSScriptRoot "web-ext-artifacts"
npx --yes web-ext sign `
    --channel=unlisted `
    --source-dir $PSScriptRoot `
    --artifacts-dir $artifacts `
    --ignore-files "tests/**" "web-ext-artifacts/**" "sign-firefox.ps1" "README.md" `
    --api-key $env:AMO_JWT_ISSUER `
    --api-secret $env:AMO_JWT_SECRET
if ($LASTEXITCODE -ne 0) { throw "web-ext sign failed (exit $LASTEXITCODE)" }

$xpi = Get-ChildItem $artifacts -Filter "*.xpi" | Sort-Object LastWriteTime -Descending | Select-Object -First 1
if (-not $xpi) { throw "No signed .xpi in $artifacts" }
"Signed: $($xpi.FullName)"
# Firefox installs an .xpi it is handed.
$firefox = @("$env:ProgramFiles\Mozilla Firefox\firefox.exe", "${env:ProgramFiles(x86)}\Mozilla Firefox\firefox.exe") | Where-Object { Test-Path $_ } | Select-Object -First 1
if ($firefox) { Start-Process $firefox -ArgumentList "`"$($xpi.FullName)`"" } else { "Open it in Firefox to install it." }
