[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$RouterPath,

    [Parameter(Mandatory = $true)]
    [string]$EnginePath,

    [Parameter(Mandatory = $true)]
    [string]$IndexerPath
)

$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $false

foreach ($binary in @($RouterPath, $EnginePath)) {
    if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) {
        throw "release binary is missing: $binary"
    }
    & $binary --version
    if ($LASTEXITCODE -ne 0) {
        throw "$binary --version failed with exit code $LASTEXITCODE"
    }
}

$versionOutput = & $RouterPath --version 2>&1 | Out-String
$versionFields = $versionOutput.Trim() -split " "
if ($versionFields.Count -lt 2 -or $versionFields[0] -ne "aethyme") {
    throw "could not read the router version for the graph fixture: $versionOutput"
}
$engineVersion = $versionFields[1]

if (-not (Test-Path -LiteralPath $IndexerPath -PathType Leaf)) {
    throw "graph indexer fixture binary is missing: $IndexerPath"
}

$workRoot = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [System.IO.Path]::GetTempPath() }
$repo = Join-Path $workRoot ("aethyme-windows-smoke-" + [guid]::NewGuid().ToString("N"))

try {
    New-Item -ItemType Directory -Path (Join-Path $repo "src") -Force | Out-Null
    $utf8 = [System.Text.UTF8Encoding]::new($false)
    [System.IO.File]::WriteAllText((Join-Path $repo "README.md"), "# Windows smoke" + [Environment]::NewLine, $utf8)
    [System.IO.File]::WriteAllText(
        (Join-Path $repo "src/lib.rs"),
        'pub fn greet() { let _ = 1; }' + [Environment]::NewLine,
        $utf8
    )

    & git init --quiet --initial-branch=main $repo
    if ($LASTEXITCODE -ne 0) { throw "git init failed with exit code $LASTEXITCODE" }
    & git -C $repo -c user.name="Aethyme Release" -c user.email="release@example.invalid" add README.md src/lib.rs
    if ($LASTEXITCODE -ne 0) { throw "git add failed with exit code $LASTEXITCODE" }
    & git -C $repo -c user.name="Aethyme Release" -c user.email="release@example.invalid" commit --quiet -m "Windows release smoke fixture"
    if ($LASTEXITCODE -ne 0) { throw "git commit failed with exit code $LASTEXITCODE" }

    $fragmentOutput = & $IndexerPath --repo-root $repo --repo-name aethyme-windows-smoke --engine-version $engineVersion --json 2>&1
    $fragmentExit = $LASTEXITCODE
    if ($fragmentExit -ne 0) {
        throw ("graph fragment generation failed (" + $fragmentExit + "): " + ($fragmentOutput | Out-String))
    }

    $indexOutput = & $EnginePath index --repo $repo 2>&1
    $indexExit = $LASTEXITCODE
    if ($indexExit -ne 0) {
        throw ("engine index failed (" + $indexExit + "): " + ($indexOutput | Out-String))
    }

    $exploreOutput = & $RouterPath explore --repo $repo --request "Where is greet defined?" --format brief 2>&1
    $exploreExit = $LASTEXITCODE
    if ($exploreExit -ne 0) {
        throw ("native explore failed (" + $exploreExit + "): " + ($exploreOutput | Out-String))
    }
    if (($exploreOutput | Out-String) -notmatch "Explore") {
        throw ("native explore did not produce its brief response: " + ($exploreOutput | Out-String))
    }

    $graphOutput = & $RouterPath graph overview --repo $repo --json 2>&1
    $graphExit = $LASTEXITCODE
    if ($graphExit -ne 0) {
        throw ("native graph overview failed (" + $graphExit + "): " + ($graphOutput | Out-String))
    }

    $deployOutput = & $RouterPath deploy --generated-only --repo $repo 2>&1
    $deployExit = $LASTEXITCODE
    if ($deployExit -ne 0) {
        throw ("generated-only deploy failed (" + $deployExit + "): " + ($deployOutput | Out-String))
    }
    if (-not (Test-Path -LiteralPath (Join-Path $repo ".codex/skills/aethyme/SKILL.md") -PathType Leaf)) {
        throw "generated-only deploy did not write the Aethyme skill"
    }

    $brokerOutput = & $RouterPath broker quick-test 2>&1
    $brokerExit = $LASTEXITCODE
    $brokerText = $brokerOutput | Out-String
    if ($brokerExit -eq 0 -or $brokerText -notmatch "the broker is not yet supported on Windows") {
        throw ("broker command did not fail with the documented Windows refusal (" + $brokerExit + "): " + $brokerText)
    }

    Write-Output "Windows release smoke passed: version, paired binaries, graph fragments and store, explore, graph navigation, generated-only deploy, broker refusal."
}
finally {
    if (Test-Path -LiteralPath $repo) {
        Remove-Item -LiteralPath $repo -Recurse -Force
    }
}

# The expected broker refusal above leaves a native exit code of 1 behind.
# The smoke script itself succeeded, so do not propagate that code to CI.
$global:LASTEXITCODE = 0
