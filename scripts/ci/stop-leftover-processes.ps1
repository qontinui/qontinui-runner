# stop-leftover-processes.ps1 -- self-hosted Windows pool only. Stops a qontinui-runner.exe
# this workspace started, and the msedgewebview2.exe hosts serving one, that are still
# alive under THIS job's account: a live one holds target\debug\qontinui-runner.exe open
# and fails the next link with LNK1104 / os error 5, which reads as the PR's fault.
#
# Called by the prep action (what an earlier job on this registration left) and by
# ci.yml's post-smoke step (what this job left). It lives here, not beside the action:
# ci-integrity.yml treats every file under .github/actions/ as gate surface that must
# parse as YAML, and fails a non-YAML file there closed. Its exit 1 can fail the job
# on a host fault (a process that will not stop); it judges nothing about the change
# under test, so leaving its body unseen by ci-integrity, as every scripts/ci/
# helper's is, costs no verdict. Both call sites stay compared there, and
# src-tauri/tests/ci_rust_test_steps_split.rs pins that both call it. Both call it only after the prep
# action's account check passed: a provisioned pool account with no other job running
# under it, so "this account" means "this registration". The runner exe sits behind the ./target junction, so its
# image path is matched under the workspace OR the persistent target behind it.
#
# Exit 0 when nothing of ours is left alive afterwards; 1, with an ::error naming the
# runner and the process, when one survives the stop.
param(
  [Parameter(Mandatory)] [string]$Workspace,
  [string]$TargetRoot = '',
  [string]$Why = 'an earlier job'
)
$ErrorActionPreference = 'Stop'
$me = [Security.Principal.WindowsIdentity]::GetCurrent().Name
# Each prefix ends in a separator, so ...\nawa-winvm-1 never matches ...\nawa-winvm-10.
$prefixes = @([IO.Path]::GetFullPath($Workspace).TrimEnd('\') + '\')
if ($TargetRoot) { $prefixes += [IO.Path]::GetFullPath($TargetRoot).TrimEnd('\') + '\' }

function Test-Ours($p) {
  $o = Invoke-CimMethod -InputObject $p -MethodName GetOwner -ErrorAction SilentlyContinue
  if (-not $o -or "$($o.Domain)\$($o.User)" -ne $me) { return $false }
  if ($p.Name -eq 'qontinui-runner.exe') {
    if (-not $p.ExecutablePath) { return $false }
    foreach ($x in $prefixes) {
      if ($p.ExecutablePath.StartsWith($x, [StringComparison]::OrdinalIgnoreCase)) { return $true }
    }
    return $false
  }
  return [bool]($p.CommandLine -match 'qontinui-runner\.exe')
}

$ours = @(Get-CimInstance Win32_Process -Filter "Name='qontinui-runner.exe' OR Name='msedgewebview2.exe'" |
  Where-Object { Test-Ours $_ })
if (-not $ours.Count) { "no qontinui-runner / WebView2 process left by $Why under $me"; exit 0 }
foreach ($p in $ours) {
  "::warning title=leftover process::stopping $($p.Name) pid $($p.ProcessId) left by $Why under $me"
  Stop-Process -Id $p.ProcessId -Force -ErrorAction SilentlyContinue
}
Wait-Process -Id $ours.ProcessId -Timeout 30 -ErrorAction SilentlyContinue
# A survivor is the same pid AND the same creation time: Windows reuses pids quickly.
$alive = @($ours | Where-Object {
  $now = Get-CimInstance Win32_Process -Filter "ProcessId=$($_.ProcessId)" -ErrorAction SilentlyContinue
  $now -and $now.CreationDate -eq $_.CreationDate })
if ($alive.Count) {
  foreach ($p in $alive) {
    "::error title=leftover process::runner '$env:RUNNER_NAME' could not stop $($p.Name) pid $($p.ProcessId) left by $Why"
  }
  exit 1
}
"stopped $($ours.Count) process(es) left by $Why"
exit 0
