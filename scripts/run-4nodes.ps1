# Mika 4-node mesh launcher (P2P-only).
#
# Starts n1..n4 where n2/n3/n4 bootstrap to n1 (establishes the gossipsub
# mesh through one hub — n1 sees every peer, peers see each other through it).
# Each node prints its multiaddr; the one you pass to
# `mika-cli upload --nodes /ip4/.../p2p/<PeerId>` is any member's.
#
# Usage:
#   .\scripts\run-4nodes.ps1                # default ports 34001..34004, data in .\mika-data{n}
#   .\scripts\run-4nodes.ps1 -Port 35000    # custom base port
#   -WithCommittee .\committee.json         # committee snapshot file (0x per line) for every node
#
# Stop: .\scripts\stop-4nodes.ps1 (or Ctrl+C in each console)

param(
    [int]$Port = 34000,
    [string]$WithCommittee = ""
)

$ErrorActionPreference = "Continue"
Set-Location (Join-Path $PSScriptRoot "..")

$ErrorActionPreference = "Stop"; cargo build -p mika-node -p mika-cli --quiet; $ErrorActionPreference = "Continue"
# Machine-readable addr map for the session (n1 /ip4/.../p2p/<PeerId> per line)
Remove-Item -Force .\.mesh -ErrorAction SilentlyContinue

function Start-MikaNode {
    param($Id, $DataDir, $ListenPort, $Bootstrap)
    $args = @("--node-id", $Id, "--data-dir", $DataDir, "--p2p-listen", $ListenPort)
    foreach ($b in $Bootstrap) { $args += "--bootstrap"; $args += $b }
    if ($WithCommittee) {
        $args += "--committee-signers"; $args += $WithCommittee
    }
    $job = Start-Job -ScriptBlock {
        param($a, $root) Set-Location $root; & .\target\debug\mika-node.exe @a 2>&1
    } -ArgumentList ($args, (Get-Location).Path)
    # Wait for the peer id line ("mika p2p: local peer id <PeerId>")
    $peerId = $null
    for ($i = 0; $i -lt 20 -and -not $peerId; $i++) {
        Start-Sleep -Seconds 1
        $out = Receive-Job $job 2>$null | Out-String -Width 4096
        $m = [regex]::Match($out, "local peer id (12D3\w+)")
        if ($m.Success) { $peerId = $m.Groups[1].Value }
    }
    if (-not $peerId) {
        Write-Host "node ${Id}: failed to start (no peer id)" -ForegroundColor Red
        return $null
    }
    $addr = "/ip4/127.0.0.1/tcp/$ListenPort/p2p/$peerId"
    Write-Host "node $Id  -> $addr"
    "$Id $addr" | Out-File -Append -Encoding ascii .\.mesh
    return $addr
}

$addrs = @{}
$first = Start-MikaNode -Id "n1" -DataDir ".\mika-data1" -ListenPort $Port -Bootstrap @()
$addrs.n1 = $first
foreach ($n in 1..3) {
    $id = "n$($n+1)"
    $a = Start-MikaNode -Id $id -DataDir ".\mika-data$($n+1)" -ListenPort ($Port + $n) -Bootstrap @($first)
    $addrs.$id = $a
}
Write-Host ""
Write-Host "Mesh ready. Upload to any one (mesh spreads the rest):"
Write-Host "  mika-cli upload --file <file> --nodes $($addrs.n1) --no-anchor"
Write-Host ""
Write-Host "Or add --committee-id <id> --password ... --storage-object-id <id> for on-chain."
