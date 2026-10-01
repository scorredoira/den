# Optional per-user installation of this extracted portable release.
$ErrorActionPreference = 'Stop'
$destination = Join-Path $env:LOCALAPPDATA 'Programs\Sik'
New-Item -ItemType Directory -Force -Path $destination | Out-Null
Get-ChildItem -LiteralPath $PSScriptRoot | Where-Object { $_.Name -ne 'install.ps1' } | ForEach-Object {
    Copy-Item -LiteralPath $_.FullName -Destination $destination -Recurse -Force
}
$shell = New-Object -ComObject WScript.Shell
$shortcut = $shell.CreateShortcut((Join-Path ([Environment]::GetFolderPath('Programs')) 'Sik.lnk'))
$shortcut.TargetPath = Join-Path $destination 'sik.exe'
$shortcut.WorkingDirectory = $env:USERPROFILE
$shortcut.Save()
Write-Host "Installed Sik in $destination. Open Sik from the Start menu."
