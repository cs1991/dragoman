$ErrorActionPreference = 'Stop'

$repoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$targetDir = Join-Path $repoRoot 'target'
$exe = Join-Path $targetDir 'release\dragoman.exe'
$distDir = Join-Path $repoRoot 'dist'
$archive = Join-Path $distDir 'dragoman-windows.zip'
$cargo = (Get-Command cargo -CommandType Application).Source

foreach ($process in (Get-Process -Name dragoman -ErrorAction SilentlyContinue)) {
    if ($process.Path -eq $exe -and -not $process.HasExited) {
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

    New-Item -ItemType Directory -Path $distDir -Force | Out-Null
    $packageFiles = @(
        $exe
        (Join-Path $repoRoot 'README.md')
        (Join-Path $repoRoot 'LICENSE')
    )
    Compress-Archive -LiteralPath $packageFiles -DestinationPath $archive -Force
    Write-Host "Package created at $archive"
} finally {
    Pop-Location
}
