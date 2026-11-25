[CmdletBinding()]
param(
    [int]$StartPort = 7911,
    [int]$Count = 50,
    [int]$AccountsPerPort = 1,
    [string]$Controller = 'http://127.0.0.1:9097',
    [string]$Secret = '',
    [string]$NodePattern = '',
    [switch]$HealthyOnly,
    [string]$ProbeUrl = 'http://www.gstatic.com/generate_204',
    [int]$ProbeTimeout = 5000,
    [switch]$WriteMerge
)

$ErrorActionPreference = 'Stop'

$vergeDir = Join-Path $env:APPDATA 'io.github.clash-verge-rev.clash-verge-rev'

if (-not $Secret) {
    $configPath = Join-Path $vergeDir 'config.yaml'
    if (Test-Path -LiteralPath $configPath) {
        $match = Select-String -Path $configPath -Pattern '^secret:\s*(.+)$' | Select-Object -First 1
        if ($match) {
            $Secret = $match.Matches[0].Groups[1].Value.Trim().Trim('"').Trim("'")
        }
    }
}

Add-Type -AssemblyName System.Net.Http
$client = New-Object System.Net.Http.HttpClient
$client.Timeout = [TimeSpan]::FromSeconds(15)
if ($Secret) {
    $client.DefaultRequestHeaders.Authorization =
        New-Object System.Net.Http.Headers.AuthenticationHeaderValue('Bearer', $Secret)
}

function Get-JsonUtf8([string]$Url) {
    $bytes = $client.GetByteArrayAsync($Url).GetAwaiter().GetResult()
    return [System.Text.Encoding]::UTF8.GetString($bytes) | ConvertFrom-Json
}

$response = Get-JsonUtf8 "$Controller/proxies"
$groupTypes = @(
    'Selector', 'URLTest', 'Fallback', 'LoadBalance',
    'Direct', 'Reject', 'Pass', 'Compatible', 'RejectDrop'
)
$nodes = @(
    $response.proxies.PSObject.Properties |
        Where-Object {
            $groupTypes -notcontains $_.Value.type -and
            $_.Name -notmatch '^(Traffic|Expire|remaining|expire|\u5f53\u524d\u7f51\u5740|\u5269\u4f59\u6d41\u91cf|\u5957\u9910\u5230\u671f|\u6d41\u91cf\u91cd\u7f6e)'
        } |
        ForEach-Object { $_.Name }
)

if ($NodePattern) {
    $nodes = @($nodes | Where-Object { $_ -match $NodePattern })
}

if ($nodes.Count -eq 0) {
    throw 'No proxy nodes were returned by the Clash controller. Start Clash Verge and select a profile first.'
}

if ($HealthyOnly) {
    $healthy = @{}
    $groups = @(
        $response.proxies.PSObject.Properties |
            Where-Object { $groupTypes -contains $_.Value.type } |
            ForEach-Object { $_.Name }
    )
    foreach ($group in $groups) {
        $encoded = [uri]::EscapeDataString($group)
        $url = "$Controller/group/$encoded/delay?timeout=$ProbeTimeout&url=$([uri]::EscapeDataString($ProbeUrl))"
        try {
            $delays = Get-JsonUtf8 $url
            foreach ($property in $delays.PSObject.Properties) {
                if ($property.Value -gt 0) { $healthy[$property.Name] = $true }
            }
        } catch {
        }
    }
    $nodes = @($nodes | Where-Object { $healthy.ContainsKey($_) })
    Write-Host ("Healthy nodes: {0}" -f $nodes.Count)
    if ($nodes.Count -eq 0) {
        throw 'No healthy nodes were reported by the Clash controller. Check the subscription or try another probe URL.'
    }
}

if ($Count -gt $nodes.Count) {
    Write-Warning ("Only {0} nodes match. {1} listeners will reuse nodes." -f $nodes.Count, $Count)
}
if ($AccountsPerPort -lt 1) {
    throw 'AccountsPerPort must be at least 1.'
}

Write-Host ("Nodes available: {0}" -f $nodes.Count)
Write-Host ("Ports: {0}..{1}  accounts per port: {2}" -f $StartPort, ($StartPort + $Count - 1), $AccountsPerPort)
Write-Host ''

$lines = New-Object System.Collections.Generic.List[string]
$lines.Add('# BEGIN dragoman listeners')
$lines.Add('listeners:')
for ($index = 0; $index -lt $Count; $index++) {
    $port = $StartPort + $index
    $node = $nodes[$index % $nodes.Count]
    $escaped = $node.Replace('\', '\\').Replace('"', '\"')
    $lines.Add("  - name: dg-$port")
    $lines.Add('    type: mixed')
    $lines.Add("    port: $port")
    $lines.Add('    listen: 127.0.0.1')
    $lines.Add("    proxy: `"$escaped`"")
}
$lines.Add('# END dragoman listeners')
$block = $lines -join "`r`n"

Write-Host $block
Write-Host ''

$account = 1
while ($account -le $Count * $AccountsPerPort) {
    $first = $account
    $last = [Math]::Min($account + $AccountsPerPort - 1, $Count * $AccountsPerPort)
    $port = $StartPort + [Math]::Floor(($account - 1) / $AccountsPerPort)
    Write-Host ("accounts {0}-{1} -> http://127.0.0.1:{2}" -f $first, $last, $port)
    $account = $last + 1
}

if ($WriteMerge) {
    $mergePath = Join-Path $vergeDir 'profiles\Merge.yaml'
    $existing = if (Test-Path -LiteralPath $mergePath) {
        [System.IO.File]::ReadAllText($mergePath)
    } else {
        ''
    }
    $pattern = '(?s)# BEGIN dragoman listeners.*?# END dragoman listeners\r?\n?'
    $existing = [regex]::Replace($existing, $pattern, '')
    $merged = $existing.TrimEnd() + "`r`n`r`n" + $block + "`r`n"
    [System.IO.File]::WriteAllText($mergePath, $merged, [System.Text.UTF8Encoding]::new($false))
    Write-Host ''
    Write-Host ("Wrote listeners to {0}" -f $mergePath)
    Write-Host 'Reload the profile in Clash Verge to apply the new ports.'
}
