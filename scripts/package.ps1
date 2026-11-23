$ErrorActionPreference = 'Stop'

$repoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$targetDir = Join-Path $repoRoot 'target'
$exe = Join-Path $targetDir 'release\dragoman.exe'
$packageDir = Join-Path $repoRoot 'dist\dragoman-windows'
$packageExe = Join-Path $packageDir 'dragoman.exe'
$cargo = (Get-Command cargo -CommandType Application).Source

foreach ($process in (Get-Process -Name dragoman -ErrorAction SilentlyContinue)) {
    if ($process.Path -in @($exe, $packageExe) -and -not $process.HasExited) {
        Write-Host "Stopping dragoman.exe (PID $($process.Id))"
        Stop-Process -InputObject $process
        if (-not $process.WaitForExit(10000)) {
            throw "Timed out waiting for dragoman.exe (PID $($process.Id)) to exit."
        }
    }
}

Push-Location -LiteralPath $repoRoot
try {
    & $cargo build --release --locked --target-dir $targetDir
    if ($LASTEXITCODE -ne 0) {
        throw "Cargo build failed with exit code $LASTEXITCODE."
    }

    New-Item -ItemType Directory -Path $packageDir -Force | Out-Null
    $packageFiles = @(
        $exe
        (Join-Path $repoRoot 'README.md')
        (Join-Path $repoRoot 'LICENSE')
    )
    Copy-Item -LiteralPath $packageFiles -Destination $packageDir -Force
    Write-Host "Package files copied to $packageDir"
} finally {
    Pop-Location
}
