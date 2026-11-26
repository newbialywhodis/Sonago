param(
    [ValidateSet('Debug', 'Release')]
    [string]$Configuration = 'Debug',
    [ValidateSet('win-x64', 'win-arm64')]
    [string]$Runtime = $(if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'win-arm64' } else { 'win-x64' }),
    [switch]$NoRun
)

$ErrorActionPreference = 'Stop'
$originalPath = $env:Path
$dotnet = (Get-Command dotnet.exe -CommandType Application -ErrorAction SilentlyContinue).Source
if (-not $dotnet) {
    $dotnet = Join-Path $env:LOCALAPPDATA 'RecallAudioDev/dotnet/dotnet.exe'
}
if (-not (Test-Path -LiteralPath $dotnet -PathType Leaf)) {
    throw '.NET 10 SDK not found. Install it and make dotnet.exe available in PATH.'
}

Push-Location -LiteralPath $PSScriptRoot
try {
    if (-not (Get-Command cmake.exe -CommandType Application -ErrorAction SilentlyContinue)) {
        $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
        $cmake = $null
        if (Test-Path -LiteralPath $vswhere -PathType Leaf) {
            $cmake = & $vswhere -products '*' -find '**\cmake.exe' | Select-Object -First 1
        }

        if (-not $cmake) {
            throw 'CMake not found. Add it to PATH or install the CMake tools in Visual Studio Build Tools.'
        }

        $env:Path = "$(Split-Path -Parent $cmake);$env:Path"
    }

    $output = Join-Path $PSScriptRoot "target/winui/$Runtime/$Configuration/app"
    & $dotnet build ui/Sonago.csproj -c $Configuration -r $Runtime -o "$output/" -p:RestoreLockedMode=true
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    if (-not $NoRun) {
        Start-Process -FilePath (Join-Path $output 'sonago.exe')
    }
} finally {
    $env:Path = $originalPath
    Pop-Location
}
