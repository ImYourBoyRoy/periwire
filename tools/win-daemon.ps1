# Start (or restart) the PeriWire daemon on the Windows lab VM as a detached process (it survives the WinRM session).
$exe = 'C:\periwire\periwire.exe'
& $exe stop 2>&1 | Out-Null
Start-Sleep 2
# C:\periwire\trace.flag (created by `winrun run 'New-Item C:\periwire\trace.flag'`) turns on URB tracing
$env = if (Test-Path C:\periwire\trace.flag) { 'set PERIWIRE_TRACE=1&& ' } else { '' }
$r = Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{
  CommandLine = "cmd /c `"$env$exe daemon --no-export > C:\periwire\daemon.log 2>&1`""; CurrentDirectory = 'C:\periwire' }
Start-Sleep 3
& $exe status 2>&1 | Out-String
