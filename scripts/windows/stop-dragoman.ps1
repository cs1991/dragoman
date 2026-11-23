param(
    [Parameter(Mandatory = $true)]
    [string]$ExecutablePath,
    [int]$Port = 8080
)

$ErrorActionPreference = 'Stop'

function Get-NormalizedPath {
    param([string]$Path)
    if ([string]::IsNullOrWhiteSpace($Path)) {
        return $null
    }
    try {
        return [System.IO.Path]::GetFullPath($Path)
    } catch {
        return $null
    }
}

$targetPath = Get-NormalizedPath $ExecutablePath
if (-not $targetPath) {
    Write-Error "Invalid Dragoman executable path: $ExecutablePath"
    exit 1
}

$candidates = @(
    Get-Process -Name 'dragoman' -ErrorAction SilentlyContinue | Where-Object {
        $candidatePath = Get-NormalizedPath $_.Path
        $candidatePath -and [string]::Equals(
            $candidatePath,
            $targetPath,
            [System.StringComparison]::OrdinalIgnoreCase
        )
    }
)

if ($Port -gt 0) {
    $listeners = Get-NetTCPConnection -LocalPort $Port -State Listen -ErrorAction SilentlyContinue
    foreach ($listener in $listeners) {
        $owner = Get-Process -Id $listener.OwningProcess -ErrorAction SilentlyContinue
        if ($owner -and $owner.ProcessName -ieq 'dragoman') {
            $candidates += $owner
        }
    }
}

$candidates = $candidates | Sort-Object Id -Unique
foreach ($process in $candidates) {
    Write-Host ("Stopping previous dragoman.exe (PID {0})" -f $process.Id)
    Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
    try {
        [void]$process.WaitForExit(10000)
    } catch {
    }
}

if ($Port -le 0) {
    exit 0
}

$deadline = [DateTime]::UtcNow.AddSeconds(10)
do {
    $listeners = Get-NetTCPConnection -LocalPort $Port -State Listen -ErrorAction SilentlyContinue
    if (-not $listeners) {
        exit 0
    }
    Start-Sleep -Milliseconds 200
} while ([DateTime]::UtcNow -lt $deadline)

$listener = $listeners | Select-Object -First 1
$owner = Get-Process -Id $listener.OwningProcess -ErrorAction SilentlyContinue
$ownerName = if ($owner) { $owner.ProcessName } else { 'unknown' }
Write-Error ("Port {0} is still in use by PID {1} ({2})." -f $Port, $listener.OwningProcess, $ownerName)
exit 1
