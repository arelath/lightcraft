param([Parameter(Mandatory=$true)][string]$RequestPath)
$ErrorActionPreference = 'Stop'
# Test-only child profiler. Arguments are structured JSON, never shell source.
$request = Get-Content -LiteralPath $RequestPath -Raw -Encoding UTF8 | ConvertFrom-Json
$start = [System.Diagnostics.ProcessStartInfo]::new()
$start.FileName = $request.executable
$start.UseShellExecute = $false
$start.CreateNoWindow = $true
$start.WindowStyle = [System.Diagnostics.ProcessWindowStyle]::Hidden
$start.RedirectStandardOutput = $true
$start.RedirectStandardError = $true
foreach ($argument in $request.arguments) { $start.ArgumentList.Add([string]$argument) }
$child = [System.Diagnostics.Process]::new()
$child.StartInfo = $start
[void]$child.Start()
$output = $child.StandardOutput.ReadToEndAsync()
$errors = $child.StandardError.ReadToEndAsync()
$watch = [System.Diagnostics.Stopwatch]::StartNew()
[long]$workingPeak = 0
[long]$commitPeak = 0
[long]$samples = 0
try {
    while (-not $child.HasExited) {
        if ($watch.Elapsed.TotalSeconds -gt 240) {
            $child.Kill($true)
            throw 'Cloud replay memory check exceeded 240 seconds; only its own child was stopped'
        }
        try {
            $child.Refresh()
            $workingPeak = [Math]::Max($workingPeak, $child.PeakWorkingSet64)
            $commitPeak = [Math]::Max($commitPeak, $child.PeakPagedMemorySize64)
            $samples++
        } catch [System.InvalidOperationException] { }
        Start-Sleep -Milliseconds 5
    }
    $child.WaitForExit()
    [ordered]@{
        exit = $child.ExitCode
        working = $workingPeak
        commit = $commitPeak
        samples = $samples
        stdout = $output.GetAwaiter().GetResult()
        stderr = $errors.GetAwaiter().GetResult()
    } | ConvertTo-Json -Compress -Depth 4
} finally { $child.Dispose() }
