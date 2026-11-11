# ⚡ High-speed launcher for Ferric AI Agent powered by Free Gateway
$ErrorActionPreference = "Continue"

# Resolve OpenCode binary dynamically
$opencodeExe = if ($env:OPENCODE_EXE -and (Test-Path $env:OPENCODE_EXE)) { $env:OPENCODE_EXE } else {
    $cmd = Get-Command opencode.exe -ErrorAction SilentlyContinue
    if ($cmd) { $cmd.Source } else {
        Get-ChildItem -Path "$env:LOCALAPPDATA\Microsoft\WinGet\Packages" -Filter "opencode.exe" -Recurse -ErrorAction SilentlyContinue | Select-Object -ExpandProperty FullName -First 1
    }
}

# Resolve Free AI Gateway proxy script dynamically
$proxyScript = if ($env:FREE_AI_GATEWAY_PROXY -and (Test-Path $env:FREE_AI_GATEWAY_PROXY)) { $env:FREE_AI_GATEWAY_PROXY } else {
    @(
        "$PSScriptRoot\..\free-ai-gateway\proxy.mjs",
        "$PSScriptRoot\free-ai-gateway\proxy.mjs",
        "$env:USERPROFILE\free-ai-gateway\proxy.mjs",
        "$env:USERPROFILE\Downloads\REA\free_ai_gateway\proxy.mjs"
    ) | Where-Object { Test-Path $_ } | Select-Object -First 1
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
    Write-Host "[1/2] Launching background OpenCode engine on port 4096..." -ForegroundColor Cyan
    Start-Process -FilePath $opencodeExe -ArgumentList "serve --port 4096" -WindowStyle Hidden
    for ($i = 0; $i -lt 10; $i++) {
        Start-Sleep -Milliseconds 300
        if (Test-PortAlive 4096) { break }
    }
}

# 2. Verify and start Free AI Gateway proxy if offline
if (-not (Test-PortAlive 4097)) {
    Write-Host "[2/2] Launching Free AI Gateway on port 4097..." -ForegroundColor Cyan
    Start-Process -FilePath "node" -ArgumentList "`"$proxyScript`"" -WindowStyle Hidden
    for ($i = 0; $i -lt 10; $i++) {
        Start-Sleep -Milliseconds 300
        if (Test-PortAlive 4097) { break }
    }
}

# 3. Locate compiled Rust binary
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

$env:OPENAI_API_KEY = "free"
Write-Host "Ready! Connected to Free Gateway (http://127.0.0.1:4097/v1)" -ForegroundColor Green

& $agentBin --provider openai --base-url "http://127.0.0.1:4097/v1" --model "deepseek-v4.1-flash-free" @args

