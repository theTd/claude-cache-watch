$ErrorActionPreference = "Stop"
$releaseProjectDir = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$releaseEncodedFlags = [Environment]::GetEnvironmentVariable("CARGO_ENCODED_RUSTFLAGS")
$releaseStage = $null
Push-Location $releaseProjectDir
try {
    & node scripts/version.mjs check
    if ($LASTEXITCODE -ne 0) { throw "Version validation failed" }
    if ($env:RUSTFLAGS -and -not $releaseEncodedFlags) {
        throw "Use CARGO_ENCODED_RUSTFLAGS instead of RUSTFLAGS for a public release build"
    }
    $releaseVersion = (Get-Content -Raw -Encoding UTF8 package.json | ConvertFrom-Json).version
    $releaseCommit = (& git rev-parse HEAD).Trim()
    if ($LASTEXITCODE -ne 0 -or $releaseCommit -notmatch "^[a-f0-9]{40}$") { throw "Git source commit is unavailable" }
    $releaseCargoDir = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE ".cargo" }
    $releaseRustupDir = if ($env:RUSTUP_HOME) { $env:RUSTUP_HOME } else { Join-Path $env:USERPROFILE ".rustup" }
    $releaseMappings = @(
        @($env:USERPROFILE, "/user"),
        @($releaseCargoDir, "/cargo"),
        @($releaseRustupDir, "/rustup"),
        @($releaseProjectDir, "/source")
    )
    $releaseFlags = @()
    if ($releaseEncodedFlags) { $releaseFlags += $releaseEncodedFlags.Split([char]31) }
    foreach ($releaseMapping in $releaseMappings) {
        if (-not $releaseMapping[0]) { throw "A required build path is unavailable" }
        $releaseAbsolute = [IO.Path]::GetFullPath($releaseMapping[0])
        $releaseFlags += "--remap-path-prefix=$releaseAbsolute=$($releaseMapping[1])"
        $releaseFlags += "--remap-path-prefix=$($releaseAbsolute.Replace('\', '/'))=$($releaseMapping[1])"
    }
    $env:CARGO_ENCODED_RUSTFLAGS = $releaseFlags -join [char]31
    & "$releaseProjectDir/node_modules/.bin/tauri.cmd" build --no-bundle --target x86_64-pc-windows-msvc
    if ($LASTEXITCODE -ne 0) { throw "Tauri release build failed" }
    $releaseExecutable = Join-Path $releaseProjectDir "src-tauri/target/x86_64-pc-windows-msvc/release/claude-cache-watch.exe"
    & node scripts/audit-public.mjs --artifact $releaseExecutable
    if ($LASTEXITCODE -ne 0) { throw "Public artifact audit failed" }
    $releaseOutput = Join-Path $releaseProjectDir "dist/releases"
    New-Item -ItemType Directory -Force -Path $releaseOutput | Out-Null
    $releaseStage = Join-Path $releaseOutput ([Guid]::NewGuid().ToString("N"))
    New-Item -ItemType Directory -Path $releaseStage | Out-Null
    Copy-Item -LiteralPath $releaseExecutable -Destination (Join-Path $releaseStage "Claude Cache Watch.exe")
    $releaseReadme = (Get-Content -Raw -Encoding UTF8 (Join-Path $releaseProjectDir "docs/portable-readme.md")).Replace("{{VERSION}}", $releaseVersion)
    [IO.File]::WriteAllText((Join-Path $releaseStage "README.md"), $releaseReadme, [Text.UTF8Encoding]::new($false))
    Copy-Item -LiteralPath (Join-Path $releaseProjectDir "CHANGELOG.md") -Destination $releaseStage
    $releaseStem = "claude-cache-watch-$releaseVersion-windows-x64-portable"
    $releaseZip = Join-Path $releaseOutput "$releaseStem.zip"
    Compress-Archive -LiteralPath @((Join-Path $releaseStage "Claude Cache Watch.exe"), (Join-Path $releaseStage "README.md"), (Join-Path $releaseStage "CHANGELOG.md")) -DestinationPath $releaseZip -Force
    $releaseHasher = [Security.Cryptography.SHA256]::Create()
    $releaseStream = [IO.File]::OpenRead($releaseZip)
    try { $releaseHash = [BitConverter]::ToString($releaseHasher.ComputeHash($releaseStream)).Replace("-", "").ToLowerInvariant() }
    finally { $releaseStream.Dispose(); $releaseHasher.Dispose() }
    $releaseChecksum = "$releaseStem.sha256"
    [IO.File]::WriteAllText((Join-Path $releaseOutput $releaseChecksum), "$releaseHash  $releaseStem.zip`n", [Text.UTF8Encoding]::new($false))
    $releaseFinalCommit = (& git rev-parse HEAD).Trim()
    if ($LASTEXITCODE -ne 0 -or $releaseFinalCommit -ne $releaseCommit) { throw "Git HEAD moved during the build; rerun it" }
    $releaseStatus = & git status --porcelain --untracked-files=normal
    if ($LASTEXITCODE -ne 0) { throw "Cannot inspect source worktree state" }
    $releaseDirty = [bool]$releaseStatus
    $releaseManifest = @{ version = $releaseVersion; tag = "v$releaseVersion"; archive = "$releaseStem.zip"; checksum = $releaseChecksum; sha256 = $releaseHash; source_commit = $releaseCommit; source_dirty = $releaseDirty } | ConvertTo-Json
    [IO.File]::WriteAllText((Join-Path $releaseOutput "release-manifest.json"), $releaseManifest, [Text.UTF8Encoding]::new($false))
    & node scripts/audit-public.mjs --artifact $releaseZip
    if ($LASTEXITCODE -ne 0) { throw "Portable ZIP audit failed" }
    Write-Output "Public release package ready: $releaseZip"
} finally {
    if ($releaseStage -and (Test-Path -LiteralPath $releaseStage)) {
        # Remove only the three files created here, then the empty staging directory.
        foreach ($releaseFilename in @("Claude Cache Watch.exe", "README.md", "CHANGELOG.md")) {
            Remove-Item -LiteralPath (Join-Path $releaseStage $releaseFilename) -ErrorAction SilentlyContinue
        }
        Remove-Item -LiteralPath $releaseStage -ErrorAction SilentlyContinue
    }
    if ($null -eq $releaseEncodedFlags) { Remove-Item Env:CARGO_ENCODED_RUSTFLAGS -ErrorAction SilentlyContinue }
    else { $env:CARGO_ENCODED_RUSTFLAGS = $releaseEncodedFlags }
    Pop-Location
}
