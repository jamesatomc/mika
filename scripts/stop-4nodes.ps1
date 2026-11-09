# Stop the 4-node mesh started by run-4nodes.ps1.
Get-Job | Stop-Job -PassThru -ErrorAction SilentlyContinue | Remove-Job -Force -ErrorAction SilentlyContinue
Write-Host "mesh stopped"
