param(
    [Parameter(Mandatory=$true)][string]$SigningSeed,
    [string]$Iscc = 'ISCC.exe'
)
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot -Parent)
$releaseVersion = (Select-String -LiteralPath Cargo.toml -Pattern '^version = "([^"]+)"').Matches[0].Groups[1].Value
cargo test --release --locked
if ($LASTEXITCODE -ne 0) { throw 'Tests failed' }
cargo build --release --locked --bin poetrhymer
if ($LASTEXITCODE -ne 0) { throw 'Build failed' }
New-Item -ItemType Directory -Force dist | Out-Null
Copy-Item -LiteralPath target\release\poetrhymer.exe -Destination dist\Poetrhymer.exe
& $Iscc "/DAppVersion=$releaseVersion" release\installer.iss
if ($LASTEXITCODE -ne 0) { throw 'Installer compilation failed' }
python release\sign.py --seed $SigningSeed --version $releaseVersion
if ($LASTEXITCODE -ne 0) { throw 'Release signing failed' }
