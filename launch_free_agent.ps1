# ⚡ High-speed launcher for Ferric AI Agent powered by Free Gateway
$ErrorActionPreference = "Continue"

# Resolve OpenCode binary dynamically
$opencodeExe = if ($env:OPENCODE_EXE -and (Test-Path $env:OPENCODE_EXE)) { $env:OPENCODE_EXE } else {
    $cmd = Get-Command opencode.exe -ErrorAction SilentlyContinue
    if ($cmd) { $cmd.Source } else {
        Get-ChildItem -Path "$env:LOCALAPPDATA\Microsoft\WinGet\Packages" -Filter "opencode.exe" -Recurse -ErrorAction SilentlyContinue | Select-Object -ExpandProperty FullName -First 1
    }
}

function Test-PortAlive($port) {
    try {
        $client = New-Object System.Net.Sockets.TcpClient
        $iar = $client.BeginConnect("127.0.0.1", $port, $null, $null)
        $success = $iar.AsyncWaitHandle.WaitOne(400, $false)
        if ($success -and $client.Connected) {
            $client.EndConnect($iar)
            $client.Close()
            return $true
        }
        $client.Close()
        return $false
    } catch {
        return $false
    }
}

# 1. Verify and start local OpenCode engine if offline
if (-not (Test-PortAlive 4096)) {
    if ($opencodeExe -and (Test-Path $opencodeExe)) {
        Write-Host "Launching background OpenCode engine on port 4096..." -ForegroundColor Cyan
        Start-Process -FilePath $opencodeExe -ArgumentList "serve --port 4096 --auto" -WindowStyle Hidden
        for ($i = 0; $i -lt 15; $i++) {
            Start-Sleep -Milliseconds 300
            if (Test-PortAlive 4096) { break }
        }
    }
}

# 2. Locate compiled Rust binary
$agentBin = "$HOME\.local\bin\ferric.exe"
if (-not (Test-Path $agentBin)) {
    $agentBin = "$HOME\.local\bin\agent.exe"
}
if (-not (Test-Path $agentBin)) {
    $agentBin = ".\target\release\ferric.exe"
}
if (-not (Test-Path $agentBin)) {
    $agentBin = ".\target\release\agent.exe"
}

Write-Host "Ready! Connected directly to OpenCode Free Engine (http://127.0.0.1:4096)" -ForegroundColor Green

& $agentBin --provider opencode --model "deepseek-v4.1-flash-free" @args

