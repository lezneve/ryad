# builds the local release files in dist\ (not committed, not published):
#   dist\ryad-<version>-windows-x64\ryad.exe (+ README.md, LICENSE) and its .zip
#   dist\ryad-<version>-setup.exe (the same exe under a setup name)
#   a .sha256 next to the zip and the setup
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools\release.ps1
# structure from kadr tools\release.ps1 (29db81d, MIT).
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
Write-Host "ryad $version"

cargo test --release
if ($LASTEXITCODE) { throw 'unit tests failed' }
cargo build --release --target x86_64-pc-windows-msvc
if ($LASTEXITCODE) { throw 'build failed' }
$built = 'target\x86_64-pc-windows-msvc\release\ryad.exe'

$name = "ryad-$version-windows-x64"
$stage = Join-Path $root "dist\$name"
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force $stage | Out-Null
Copy-Item $built, README.md, LICENSE $stage
$zip = Join-Path $root "dist\$name.zip"
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path "$stage\*" -DestinationPath $zip
$hash = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
"$hash  $name.zip" | Set-Content "$zip.sha256" -Encoding ascii
Write-Host ("{0}  {1:N0} bytes, ryad.exe {2:N0} bytes" -f $zip, (Get-Item $zip).Length, (Get-Item $built).Length)

$setup = Join-Path $root "dist\ryad-$version-setup.exe"
Copy-Item $built $setup -Force
$sh = (Get-FileHash $setup -Algorithm SHA256).Hash.ToLower()
"$sh  ryad-$version-setup.exe" | Set-Content "$setup.sha256" -Encoding ascii
Write-Host "$setup  sha256 $sh"
