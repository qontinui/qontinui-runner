# resource-report-windows.ps1 — the Windows readings for scripts/ci/resource-report.sh.
#
# Prints `key=value` lines (MB, integers) and nothing else; resource-report.sh
# keeps the FIRST well-formed value per key, so the GetPerformanceInfo block
# below wins and the CIM block is a fallback for the keys both can answer.
# Every block is independently try/catch'd: a reading that fails prints no
# line (absent, never zero), and this script always exits 0. With
# RESOURCE_REPORT_DEBUG set, a swallowed error is written to stderr (and the
# stdout contract is unchanged), so a CI log can say WHY a reading is absent.
#
# Plan 2026-10-27-coord-routes-ci-jobs-by-runner-capability, Phase 2.

$ErrorActionPreference = 'Stop'

# One `key=value` line, or nothing: a missing CIM property is $null, and
# [decimal]$null is 0 — exactly the fabricated zero this report must not emit.
function Write-Reading([string]$Key, $Value, [decimal]$Divisor = 1) {
    if ($null -eq $Value -or "$Value" -eq '') { return }
    '{0}={1}' -f $Key, [decimal]::Floor([decimal]$Value / $Divisor)
}

function Write-Diag([string]$Block, $Err) {
    if ($env:RESOURCE_REPORT_DEBUG) { [Console]::Error.WriteLine("resource-report-windows: ${Block}: $Err") }
}

# GetPerformanceInfo (psapi) is the one API that reports the commit PEAK as
# well as the commit LIMIT. Values are in pages.
try {
    Add-Type -Namespace QontinuiResourceReport -Name Psapi -MemberDefinition @'
[StructLayout(LayoutKind.Sequential)]
public struct PerfInfo {
    public uint cb;
    public UIntPtr CommitTotal;
    public UIntPtr CommitLimit;
    public UIntPtr CommitPeak;
    public UIntPtr PhysicalTotal;
    public UIntPtr PhysicalAvailable;
    public UIntPtr SystemCache;
    public UIntPtr KernelTotal;
    public UIntPtr KernelPaged;
    public UIntPtr KernelNonpaged;
    public UIntPtr PageSize;
    public uint HandleCount;
    public uint ProcessCount;
    public uint ThreadCount;
}
[DllImport("psapi.dll", SetLastError = true)]
public static extern bool GetPerformanceInfo(out PerfInfo info, uint size);
'@
    # Resolved by name at run time rather than through a [type] literal, so the
    # lookup can never bind before Add-Type has loaded the assembly.
    $type = 'QontinuiResourceReport.Psapi+PerfInfo' -as [type]
    $api = 'QontinuiResourceReport.Psapi' -as [type]
    if ($null -eq $type -or $null -eq $api) { throw 'Add-Type produced no PerfInfo/Psapi type' }
    $info = [Activator]::CreateInstance($type)
    # Measure an INSTANCE: PowerShell binds Marshal.SizeOf($type) to the generic
    # SizeOf<T>(T) with T inferred as System.RuntimeType, which throws (measured
    # on pwsh 7.6, where SizeOf(object) throws too and SizeOf(Type) returns 104;
    # which overload Windows PowerShell 5.1 binds was not measured), so the
    # P/Invoke never ran and peak_commit_mb was always absent. (Casting at the call
    # site, SizeOf([type]$type), binds SizeOf(Type); a [type]-constrained variable
    # or a bare type literal still binds SizeOf<T>. The instance form needs no cast.)
    $size = [uint32][System.Runtime.InteropServices.Marshal]::SizeOf($info)
    $ok = $api::GetPerformanceInfo([ref]$info, $size)
    if (-not $ok) {
        throw ('GetPerformanceInfo returned false (Win32 error {0}, size {1})' -f [System.Runtime.InteropServices.Marshal]::GetLastWin32Error(), $size)
    }
    if ($ok) {
        $page = [decimal]$info.PageSize.ToUInt64()
        if ($page -gt 0) {
            Write-Reading 'phys_mem_mb' ([decimal]$info.PhysicalTotal.ToUInt64() * $page) 1048576
            Write-Reading 'commit_limit_mb' ([decimal]$info.CommitLimit.ToUInt64() * $page) 1048576
            Write-Reading 'peak_commit_mb' ([decimal]$info.CommitPeak.ToUInt64() * $page) 1048576
        }
    }
} catch { Write-Diag 'perfinfo' $_ }

# Pagefiles. AllocatedBaseSize and PeakUsage are already MB. A peak is
# reported only for exactly one pagefile: per-file peaks need not coincide,
# so their sum is not a peak.
try {
    $files = @(Get-CimInstance Win32_PageFileUsage)
    if ($files.Count -gt 0) {
        Write-Reading 'pagefile_alloc_mb' ($files | Measure-Object -Property AllocatedBaseSize -Sum).Sum
        if ($files.Count -eq 1) {
            Write-Reading 'pagefile_peak_mb' $files[0].PeakUsage
        }
    }
} catch { Write-Diag 'pagefile' $_ }

# Fallback for RAM and the commit limit (KB).
try {
    $os = Get-CimInstance Win32_OperatingSystem
    Write-Reading 'phys_mem_mb' $os.TotalVisibleMemorySize 1024
    Write-Reading 'commit_limit_mb' $os.TotalVirtualMemorySize 1024
} catch { Write-Diag 'cim-os' $_ }

exit 0
