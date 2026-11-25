[CmdletBinding()]
param(
    [int]$StartIp = 2,
    [int]$Count = 50,
    [int]$AccountsPerIp = 1,
    [int]$ProxyPort = 7897,
    [string]$Controller = 'http://127.0.0.1:9097',
    [string]$Secret = '',
    [string]$NodePattern = '',
    [switch]$HealthyOnly,
    [string]$ProbeUrl = 'http://www.gstatic.com/generate_204',
    [int]$ProbeTimeout = 5000,
    [string]$ScriptPath = '',
    [string]$MergePath = '',
    [switch]$KeepMerge,
    [switch]$Write
)

$ErrorActionPreference = 'Stop'

$vergeDir = Join-Path $env:APPDATA 'io.github.clash-verge-rev.clash-verge-rev'
if (-not $ScriptPath) {
    $ScriptPath = Join-Path $vergeDir 'profiles\Script.js'
}
if (-not $MergePath) {
    $MergePath = Join-Path $vergeDir 'profiles\Merge.yaml'
}

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

if ($StartIp -lt 1 -or $StartIp -gt 254) {
    throw 'StartIp must be between 1 and 254.'
}
if ($AccountsPerIp -lt 1) {
    throw 'AccountsPerIp must be at least 1.'
}
if ($Count -lt 1) {
    throw 'Count must be at least 1.'
}
if ($StartIp + $Count - 1 -gt 254) {
    throw 'Source address range exceeds 127.0.0.254. Lower Count or StartIp.'
}
if ($Count -gt $nodes.Count) {
    Write-Warning ("Only {0} nodes match. {1} source addresses will reuse nodes." -f $nodes.Count, $Count)
}

Write-Host ("Nodes available: {0}" -f $nodes.Count)
Write-Host ("Source addresses: 127.0.0.{0}..127.0.0.{1}  accounts per address: {2}" -f $StartIp, ($StartIp + $Count - 1), $AccountsPerIp)
Write-Host ''

$rules = New-Object System.Collections.Generic.List[string]
for ($index = 0; $index -lt $Count; $index++) {
    $octet = $StartIp + $index
    $node = $nodes[$index % $nodes.Count]
    $escaped = $node.Replace('\', '\\').Replace('"', '\"')
    $rules.Add("  `"SRC-IP-CIDR,127.0.0.$octet/32,$escaped`"")
}
$items = $rules -join ",`r`n"
$block = "// BEGIN dragoman src rules`r`nconst dragomanSrcRules = [`r`n$items`r`n];`r`n// END dragoman src rules"

Write-Host $block
Write-Host ''

$account = 1
$total = $Count * $AccountsPerIp
while ($account -le $total) {
    $first = $account
    $last = [Math]::Min($account + $AccountsPerIp - 1, $total)
    $octet = $StartIp + [Math]::Floor(($account - 1) / $AccountsPerIp)
    Write-Host ("accounts {0}-{1} -> 127.0.0.{2} via http://127.0.0.1:{3}" -f $first, $last, $octet, $ProxyPort)
    $account = $last + 1
}

if (-not $Write) {
    return
}

$template = @"
$block

function main(config, profileName) {
  const rules = Array.isArray(config.rules) ? config.rules : [];
  const known = new Set(rules);
  const missing = dragomanSrcRules.filter((rule) => !known.has(rule));
  config.rules = missing.concat(rules);
  return config;
}
"@

if (Test-Path -LiteralPath $ScriptPath) {
    Copy-Item -LiteralPath $ScriptPath -Destination "$ScriptPath.bak" -Force
}
[System.IO.File]::WriteAllText($ScriptPath, $template, [System.Text.UTF8Encoding]::new($false))
Write-Host ''
Write-Host ("Wrote source rules to {0}" -f $ScriptPath)

if (-not $KeepMerge -and (Test-Path -LiteralPath $MergePath)) {
    $existing = [System.IO.File]::ReadAllText($MergePath)
    $cleaned = [regex]::Replace(
        $existing,
        '(?s)# BEGIN dragoman src rules.*?# END dragoman src rules\r?\n?',
        ''
    )
    $cleaned = [regex]::Replace(
        $cleaned,
        '(?s)# BEGIN dragoman listeners.*?# END dragoman listeners\r?\n?',
        ''
    )
    $legacy = [regex]::Match($cleaned, '(?ms)^prepend-rules:[ \t]*\r?\n(?:(?:[ \t]+.*|[ \t]*)\r?\n)*')
    if ($legacy.Success) {
        $entries = @(
            $legacy.Value -split "`r?`n" |
                Where-Object { $_ -match '^\s*-\s' } |
                ForEach-Object { $_.Trim() }
        )
        $ours = @(
            $entries | Where-Object {
                $_ -match '^-\s*"?SRC-IP-CIDR,127\.0\.0\.\d+/32,'
            }
        )
        if ($entries.Count -eq $ours.Count) {
            $cleaned = $cleaned.Remove($legacy.Index, $legacy.Length)
        }
    }
    if ($cleaned -ne $existing) {
        Copy-Item -LiteralPath $MergePath -Destination "$MergePath.bak" -Force
        [System.IO.File]::WriteAllText($MergePath, $cleaned, [System.Text.UTF8Encoding]::new($false))
        Write-Host ("Removed legacy blocks from {0}" -f $MergePath)
    }
}

Write-Host ("Open Clash Verge and activate the subscription again. The mixed port stays {0}." -f $ProxyPort)
