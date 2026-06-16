$ErrorActionPreference = "Continue"

& pnpm tauri build --target x86_64-pc-windows-msvc --bundles nsis *> tauri-build.log
$buildExitCode = $LASTEXITCODE

Set-Content -Path tauri-build-exit-code.txt -Value $buildExitCode
Get-Content tauri-build.log -Tail 180
