#!/usr/bin/env pwsh
# contract-smoke.ps1
# Phase 2b -- UI Bridge <-> runner behavior contract smoke.
#
# Spawns a temp runner via the supervisor, walks every route in the SDK's
# UI_BRIDGE_ROUTES (parsed from ui-bridge/packages/ui-bridge/src/server/types.ts)
# and confirms each route is reachable from the runner. Then runs three
# targeted shape probes covering the friction-table cases the offline
# manifest diff (Phase 2a) cannot see:
#   1. revealsAny= filter actually filters
#   2. scope field round-trips through /control/component/:id
#   3. /control/element/:id/expect returns 422 on timeout
#
# Two launch modes:
#
#   1. Supervisor mode (DEFAULT, dev). Spawns a temp runner via the supervisor
#      on http://localhost:9875. Prerequisite: supervisor running.
#        Check with: Invoke-WebRequest http://localhost:9875/health -UseBasicParsing
#      This script does NOT start the supervisor itself -- the supervisor lives
#      as a long-running service the user manages outside the smoke harness.
#
#   2. -DirectExe <path> mode (CI / supervisor-free). Boots the given
#      qontinui-runner.exe directly via Start-Process with an isolated
#      secondary-instance env (config / secure-storage / WebView2 dirs all in
#      fresh temp dirs, keychain disabled, CLAUDECODE removed), on a free port
#      >= 9877 the script chooses. No supervisor required. The runner is
#      Stop-Process'd in a finally. This is what the CI dev leg uses.
#
#   3. -UseInstalledExe mode (published-build parity leg). Same launch path as
#      mode 2, but the exe is LOCATED rather than named: the published artifact
#      is installed under a directory this repo does not pin, and it does NOT
#      carry the dev binary's filename. See Find-InstalledRunnerExe, in
#      scripts/lib/installed-runner.ps1.
#      Optional -InstallRoot <dir> short-circuits the probe.
#
# -SdkTypesPath and why the default is not good enough for modes 2/3
# ------------------------------------------------------------------
# The $SdkTypesPath default is derived from $PSScriptRoot -- from where THIS
# SCRIPT sits, not from where the exe under test came from. Inside a checkout
# that is right: scripts/ and ../../ui-bridge belong to the same tree the exe
# was built from. For an INSTALLED artifact it is meaningless. The published exe
# under %LOCALAPPDATA%\Qontinui Runner has no relationship to this checkout's
# ui-bridge, so the default silently pairs an installed binary with whatever SDK
# types happen to sit next to the script. Today that is the same answer -- but
# by accident, not by construction: copy this script anywhere else, or run it
# from a checkout other than the one the artifact was built from, and the parity
# leg starts diffing against the wrong contract without saying a word.
#
# So whenever the exe under test resolves OUTSIDE this checkout (always true for
# -UseInstalledExe), -SdkTypesPath is REQUIRED and the script refuses to guess.
# The dev leg already passes it explicitly (.github/workflows/ci.yml).
#
# Usage:
#   powershell -File scripts/contract-smoke.ps1            # fast run, supervisor, no rebuild
#   powershell -File scripts/contract-smoke.ps1 -Rebuild   # force fresh build (supervisor)
#   powershell -File scripts/contract-smoke.ps1 -DryRun    # parse routes & exit
#   powershell -File scripts/contract-smoke.ps1 -DirectExe path\to\qontinui-runner.exe
#   # dev leg (CI):
#   powershell -File scripts/contract-smoke.ps1 -DirectExe target/debug/qontinui-runner.exe -Profile ci -SdkTypesPath ../ui-bridge/packages/ui-bridge/src/server/types.ts
#   # published leg (CI) -- -SdkTypesPath is mandatory here, not optional:
#   powershell -File scripts/contract-smoke.ps1 -UseInstalledExe -Profile ci -SdkTypesPath ../ui-bridge/packages/ui-bridge/src/server/types.ts
#
# -Profile ci: skips the two model-loading routes (POST /vision/extract,
#   POST /vision/describe) that need llama-swap (absent in CI).
#
# Exit code: 0 on all-pass, 1 on any FAIL (or on a bad invocation -- a bad
#   -SdkTypesPath / an unlocatable installed exe both exit 1 before any probe).

param(
    [switch]$Rebuild,
    [switch]$DryRun,
    [int]$WaitTimeoutSecs = 180,
    # Derived, not hardcoded: $QONTINUI_ROOT if set, else the grandparent of this
    # script (<root>/qontinui-runner/scripts). An explicit -SdkTypesPath still wins.
    [string]$SdkTypesPath = (Join-Path $(if ($env:QONTINUI_ROOT) { $env:QONTINUI_ROOT } else { (Get-Item $PSScriptRoot).Parent.Parent.FullName }) 'ui-bridge/packages/ui-bridge/src/server/types.ts'),
    [string]$SupervisorBase = "http://localhost:9875",
    [string]$DirectExe = $null,
    # Published-build parity leg: locate the INSTALLED runner exe instead of
    # being handed a path. Mutually exclusive with -DirectExe.
    [switch]$UseInstalledExe,
    # Optional short-circuit for -UseInstalledExe: the directory the installer
    # wrote the exe into (or the exe itself). When absent the standard NSIS
    # locations are probed in order.
    [string]$InstallRoot = $null,
    [ValidateSet("dev", "ci")]
    [string]$Profile = "dev"
)

$ErrorActionPreference = "Stop"

# ---------------------------------------------------------------------------
# Summary accounting + reporting invariants (Get-SmokeSummary / Write-SmokeSummary).
# Dot-sourced rather than inlined so the tally can be unit-tested without booting
# a runner -- see scripts/tests/test-smoke-summary.ps1. $PSScriptRoot, not a
# relative path: CI invokes this as `powershell -File scripts/contract-smoke.ps1`
# from the repo root, dev runs it from elsewhere.
$SmokeSummaryLib = Join-Path $PSScriptRoot "lib/smoke-summary.ps1"
if (-not (Test-Path $SmokeSummaryLib)) {
    Write-Host "ERROR: missing $SmokeSummaryLib -- contract-smoke cannot report its own results." -ForegroundColor Red
    exit 1
}
. $SmokeSummaryLib

# ---------------------------------------------------------------------------
# Installed-exe locator (published-build parity leg).
#
# Extracted to scripts/lib/installed-runner.ps1 so the Phase 5 manifest
# comparator (scripts/published-parity.ps1) locates the published artifact by
# EXACTLY the same rules -- including the "never fall back to the dev binary"
# property, which is only a guarantee if every parity leg shares one
# implementation of it. Read that file for the full reasoning.
$InstalledRunnerLib = Join-Path $PSScriptRoot "lib/installed-runner.ps1"
if (-not (Test-Path $InstalledRunnerLib)) {
    Write-Host "ERROR: missing $InstalledRunnerLib -- contract-smoke cannot locate an installed exe." -ForegroundColor Red
    exit 1
}
. $InstalledRunnerLib

# ---------------------------------------------------------------------------
# Resolve the exe under test, BEFORE anything boots.
#   -UseInstalledExe : locate the published install (never a fallback).
#   -DirectExe       : an explicit path (the dev leg passes target/debug/...).
#   neither          : supervisor mode.
# ---------------------------------------------------------------------------
if ($UseInstalledExe) {
    if ($DirectExe) {
        Write-Host "ERROR: -UseInstalledExe and -DirectExe are mutually exclusive." -ForegroundColor Red
        Write-Host "       -UseInstalledExe LOCATES the published artifact; -DirectExe names an exe." -ForegroundColor Red
        exit 1
    }
    try {
        $DirectExe = Find-InstalledRunnerExe -InstallRoot $InstallRoot
    } catch {
        Write-Host "ERROR: $($_.Exception.Message)" -ForegroundColor Red
        exit 1
    }
    Write-Host "Installed runner exe located: $DirectExe"
} elseif ($InstallRoot) {
    Write-Host "ERROR: -InstallRoot applies only with -UseInstalledExe." -ForegroundColor Red
    exit 1
}

# ---------------------------------------------------------------------------
# Resolve + validate $SdkTypesPath BEFORE anything boots.
#
# Two failures used to surface late and badly:
#
#   1. A missing file blew up inside Parse-UiBridgeRoutes with whatever
#      Get-Content raised. A harness whose OWN configuration is wrong has to say
#      so in its own voice, not hand back a parser stack trace.
#   2. The $PSScriptRoot-derived default silently applied to an exe that has
#      nothing to do with this checkout (see the header). For any exe resolving
#      OUTSIDE this checkout -- always the case for -UseInstalledExe --
#      -SdkTypesPath is now REQUIRED rather than guessed.
#
# Neither check can newly fail an existing green path: supervisor mode runs from
# a checkout, and the dev leg's exe (target/debug/...) is inside it.
# ---------------------------------------------------------------------------
$RepoRoot = (Get-Item $PSScriptRoot).Parent.FullName

if ($DirectExe -and -not $PSBoundParameters.ContainsKey('SdkTypesPath')) {
    $exeFull = $null
    try { $exeFull = (Resolve-Path -LiteralPath $DirectExe -ErrorAction Stop).Path } catch { $exeFull = $null }
    # An exe path that does not resolve at all is NOT this check's business --
    # defer to the "-DirectExe path not found" error further down, which names
    # the real problem.
    if ($exeFull) {
        $rootPrefix = $RepoRoot.TrimEnd('\', '/') + [System.IO.Path]::DirectorySeparatorChar
        if (-not $exeFull.StartsWith($rootPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
            Write-Host "ERROR: -SdkTypesPath is required when the exe under test lives outside this checkout." -ForegroundColor Red
            Write-Host "       exe:      $exeFull" -ForegroundColor Red
            Write-Host "       checkout: $RepoRoot" -ForegroundColor Red
            Write-Host "" -ForegroundColor Red
            Write-Host "       The default is derived from this SCRIPT's location, not from the exe's," -ForegroundColor Red
            Write-Host "       so relying on it here would pair an out-of-tree binary with whatever SDK" -ForegroundColor Red
            Write-Host "       types happen to sit beside the script -- right only by accident." -ForegroundColor Red
            Write-Host "       Pass it explicitly, e.g." -ForegroundColor Red
            Write-Host "         -SdkTypesPath ../ui-bridge/packages/ui-bridge/src/server/types.ts" -ForegroundColor Red
            exit 1
        }
    }
}

$sdkResolved = $null
try { $sdkResolved = (Resolve-Path -LiteralPath $SdkTypesPath -ErrorAction Stop).Path } catch { $sdkResolved = $null }
if (-not $sdkResolved) {
    # Best-effort absolute rendering so the message names a path the operator can
    # actually go look at, not the relative fragment they typed.
    $shown = $SdkTypesPath
    try {
        if (-not [System.IO.Path]::IsPathRooted($SdkTypesPath)) {
            $shown = [System.IO.Path]::GetFullPath((Join-Path (Get-Location).Path $SdkTypesPath))
        }
    } catch { $shown = $SdkTypesPath }

    $origin = if ($PSBoundParameters.ContainsKey('SdkTypesPath')) {
        "the -SdkTypesPath argument"
    } elseif ($env:QONTINUI_ROOT) {
        "the default, derived from `$env:QONTINUI_ROOT = $($env:QONTINUI_ROOT)"
    } else {
        "the default, derived from this script's location ($PSScriptRoot)"
    }

    Write-Host "ERROR: SDK types file not found -- contract-smoke cannot parse UI_BRIDGE_ROUTES." -ForegroundColor Red
    Write-Host "       looked for: $shown" -ForegroundColor Red
    Write-Host "       as given:   $SdkTypesPath" -ForegroundColor Red
    Write-Host "       source:     $origin" -ForegroundColor Red
    Write-Host "       cwd:        $((Get-Location).Path)" -ForegroundColor Red
    Write-Host "" -ForegroundColor Red
    Write-Host "       Fix by pointing -SdkTypesPath at the ui-bridge SDK's types.ts, e.g." -ForegroundColor Red
    Write-Host "         -SdkTypesPath ../ui-bridge/packages/ui-bridge/src/server/types.ts" -ForegroundColor Red
    Write-Host "       or by setting `$env:QONTINUI_ROOT to the workspace root holding ui-bridge/." -ForegroundColor Red
    exit 1
}
$SdkTypesPath = $sdkResolved

# ---------------------------------------------------------------------------
# Routes that need a real WS-transport setup, live UI state, or otherwise
# can't be smoke-tested with placeholder fixtures. SKIP_ROUTES is keyed by
# "<METHOD> <PATH>" exactly as it appears in UI_BRIDGE_ROUTES.
# ---------------------------------------------------------------------------
$SKIP_ROUTES = @{
    # Headless spawn requires the optional @qontinui/ui-bridge-headless peer
    # dep + ENABLE_HEADLESS_SPAWN=1; expected 503 on a stock runner.
    "POST /control/sdk/spawn-headless"       = "headless spawn gated behind feature flag (503 expected)"
    # Heartbeat is push-only from a paired SDK; a bare POST has no semantics
    # to assert against.
    "POST /heartbeat"                        = "push-only from SDK; no assertable response"
    # snapshot capture writes a file to user-data dir; not safe to spam in CI.
    "POST /render-log/snapshot"              = "writes to user-data dir; not idempotent"
    # Workflows by id need a real workflow registered first.
    "POST /control/workflow/:id/run"         = "needs a registered workflow id"
    "GET /control/workflow/:runId/status"    = "needs a real run id from a prior /run call"
    # vision/raw is intentionally invisible without QONTINUI_VISION_RAW=1
    # (Phase 2 of the UI Bridge vision-pipeline plan): it 404s by design so a
    # caller can't tell whether the route exists. Treating 404 as a route-not-
    # registered failure would be a false positive.
    "POST /vision/raw"                       = "gated behind QONTINUI_VISION_RAW=1 env var; 404 is intentional"
}

# ---------------------------------------------------------------------------
# Routes whose handler can legitimately exceed the default 10s probe timeout.
# vision/extract (PaddleOCR) and vision/describe (VLM) load a cold model via
# llama-swap on first request — and llama-swap UNLOADS one model to load the
# other, so describe can pay a full swap+load even right after extract ran;
# 300s covers the worst observed cold swap. /control/specs is a disk scan
# that can exceed 10s on a cold FS cache (fresh exe dir + Defender pass).
# Keyed by "<METHOD> <PATH>" exactly as it appears in UI_BRIDGE_ROUTES.
# ---------------------------------------------------------------------------
$SLOW_ROUTES = @{
    "POST /vision/extract"  = 300
    "POST /vision/describe" = 300
    "GET /control/specs"    = 60
}

# ---------------------------------------------------------------------------
# Accepted status codes. The smoke asserts "the route is registered AND its
# handler ran", so a 2xx passes and so does a 4xx that is the handler's own
# validation rejecting our placeholder fixture (400/405/409/415/422).
# EVERYTHING ELSE IS A FAIL -- notably every 5xx (the handler is registered but
# broken) and 401/403 (registered but gated). The rule this replaces was
# "anything that is not 404 passes", which recorded 500 / 401 / 503 as PASS and
# made the gate structurally incapable of seeing a broken-but-registered route.
# 404 is deliberately NOT listed here: it keeps its own body-aware classifier
# in the route loop (route-matched-but-resource-missing vs unregistered).
# ---------------------------------------------------------------------------
$DEFAULT_OK_STATUS = @(200, 201, 202, 204, 400, 405, 409, 415, 422)

# Per-route overrides for routes whose CORRECT smoke response falls outside the
# default set. Keyed by "<METHOD> <PATH>" exactly as it appears in
# UI_BRIDGE_ROUTES; the value REPLACES the default set for that route (it does
# not extend it). Add an entry here -- with a reason -- rather than widening
# $DEFAULT_OK_STATUS, so one route's quirk can never silently green-light every
# other route. A route that cannot be probed at all belongs in $SKIP_ROUTES.
$EXPECTED_STATUS = @{
}

# ---------------------------------------------------------------------------
# -Profile ci skip set. EXACTLY the two model-loading routes: extract loads
# PaddleOCR and describe loads a VLM, both via llama-swap — which does not run
# in CI. Their shape contracts are covered by the non-model probes; the model
# paths stay in the dev profile. Skipped loudly (no silent caps).
# ---------------------------------------------------------------------------
$CI_PROFILE_SKIP = @{
    "POST /vision/extract"  = "model routes - no llama-swap in CI"
    "POST /vision/describe" = "model routes - no llama-swap in CI"
}
if ($Profile -eq "ci") {
    foreach ($k in $CI_PROFILE_SKIP.Keys) {
        $SKIP_ROUTES[$k] = $CI_PROFILE_SKIP[$k]
    }
}

# ---------------------------------------------------------------------------
# Per-route fixture bodies. For routes with bodyRequired: true we send a
# minimal valid-shape body where one is needed to exercise the parser; for
# the rest, an empty {} is fine -- we only assert "route is registered and its
# handler ran", i.e. the status lands in $DEFAULT_OK_STATUS (validation 4xx
# counts as PASS; 5xx / auth statuses do not).
# ---------------------------------------------------------------------------
$BODY_FIXTURES = @{
    "POST /control/element/:id/action"        = '{"action":"click"}'
    "POST /control/element/:id/expect"        = '{"state":"visible","timeoutMs":200,"pollMs":50}'
    "POST /control/actions/batch"             = '{"actions":[]}'
    "POST /control/elements/rank"             = '{"ids":[]}'
    "POST /control/component/:id/action/:actionId" = '{}'
    "POST /control/find"                      = '{"query":"smoke"}'
    "POST /control/discover"                  = '{"query":"smoke"}'
    "POST /ai/search"                         = '{"query":"smoke"}'
    "POST /ai/find"                           = '{"query":"smoke"}'
    "POST /ai/execute"                        = '{"intent":"smoke"}'
    "POST /ai/assert"                         = '{"assertion":"smoke"}'
    "POST /ai/assert/batch"                   = '{"assertions":[]}'
    "POST /ai/assert-batch"                   = '{"assertions":[]}'
    "POST /ai/semantic-search"                = '{"query":"smoke"}'
    "POST /ai/execute-with-diff"              = '{"intent":"smoke"}'
    "POST /ai/wait-for-change"                = '{"timeoutMs":100}'
    "POST /ai/scoped-diff"                    = '{"scope":"page"}'
    "POST /ai/summarize-diff"                 = '{"diff":{}}'
    "POST /ai/intents/execute"                = '{"intent":"smoke"}'
    "POST /ai/intents/find"                   = '{"query":"smoke"}'
    "POST /ai/intents/register"               = '{"name":"smoke","steps":[]}'
    "POST /ai/intents/execute-from-query"     = '{"query":"smoke"}'
    "POST /ai/recovery/attempt"               = '{"context":{}}'
    "POST /ai/analyze/cross-app-compare"      = '{"sources":[]}'
    "POST /control/page/navigate"             = '{"url":"about:blank"}'
    "POST /control/page/evaluate"             = '{"expression":"1+1"}'
    "POST /control/page/scroll"               = '{"x":0,"y":0}'
    "POST /control/clipboard/write"           = '{"text":"smoke"}'
    "POST /annotations/import"                = '{"annotations":[]}'
    "PUT /annotations/:id"                    = '{"value":"smoke"}'
    "POST /control/annotation/:id"            = '{"value":"smoke"}'
    "PUT /control/annotation/:id"             = '{"value":"smoke"}'
    "POST /control/annotations/import"        = '{"annotations":[]}'
    "POST /control/error-baselines/capture"   = '{"name":"smoke"}'
    "POST /control/error-baselines/compare"   = '{"name":"smoke"}'
    "POST /design/element/:id/state-styles"   = '{"states":[]}'
    "POST /design/responsive"                 = '{"viewports":[]}'
    "POST /control/viewport-constraints"      = '{"width":1024,"height":768}'
    "POST /design/style-guide/load"           = '{"guide":{}}'
    "POST /control/fill"                      = '{"fields":{}}'
    "POST /control/forms/diff"                = '{}'
    "POST /control/clipboard"                 = '{"text":"smoke"}'
    "POST /control/network-requests/wait"     = '{"timeoutMs":100}'
    "POST /control/wait-for-targets"          = '{"targets":[]}'
    "POST /ai/bookmarks"                      = '{"name":"smoke"}'
    "POST /ai/wait-for-element-condition"     = '{"id":"dummy-id","condition":"visible","timeoutMs":100}'
    "POST /ai/wait-for-element"               = '{"id":"dummy-id","timeoutMs":100}'
    "POST /control/batch-execute"             = '{"steps":[]}'
    "POST /control/page/click-by-text"        = '{"text":"smoke"}'
    "POST /control/page/click-by-selector"    = '{"selector":"#smoke"}'
    "POST /control/page/type-into"            = '{"selector":"#smoke","text":"x"}'
    "POST /control/page/read-value"           = '{"selector":"#smoke"}'
    "POST /control/page/find-by-text"         = '{"text":"smoke"}'
    "POST /control/page/navigate-to"          = '{"route":"/"}'
    # F13 is inside the grammar's F1-F24 range, so this exercises the combo
    # parser AND the live dispatch -- but the runner binds no F13 shortcut, so
    # unlike "Escape" it cannot close a panel out from under another probe.
    # Without a fixture this route was probed with {} and passed on the
    # validation 400, proving only that it was registered.
    "POST /control/page/send-keys"            = '{"keys":"F13"}'
    "POST /control/states/find-path"          = '{"to":"smoke"}'
    "POST /control/states/navigate"           = '{"to":"smoke"}'
}

# ---------------------------------------------------------------------------
# Route parser -- UI_BRIDGE_ROUTES is a TS literal. Regex over the file is
# the same trick the runner's manifest_drift_tests uses for the .route()
# scrape. Captures method + path; ignores handler/params/bodyRequired since
# we re-derive bodyRequired from BODY_FIXTURES presence.
# ---------------------------------------------------------------------------
function Parse-UiBridgeRoutes {
    param([string]$Path)

    if (-not (Test-Path $Path)) {
        throw "SDK types file not found: $Path"
    }

    $text = Get-Content -Raw -Path $Path
    $startMarker = "UI_BRIDGE_ROUTES: RouteDefinition[] = ["
    $start = $text.IndexOf($startMarker)
    if ($start -lt 0) { throw "Could not find UI_BRIDGE_ROUTES literal in $Path" }
    $tail = $text.Substring($start)
    $end = $tail.IndexOf("];")
    if ($end -lt 0) { throw "Could not find UI_BRIDGE_ROUTES terminator" }
    $body = $tail.Substring(0, $end)

    $routes = @()
    $rx = [regex]"method:\s*'([^']+)',\s*path:\s*'([^']+)'(?:[^{}]*?bodyRequired:\s*(true|false))?"
    foreach ($m in $rx.Matches($body)) {
        $method = $m.Groups[1].Value
        $path = $m.Groups[2].Value
        $bodyRequired = ($m.Groups[3].Value -eq "true")
        $routes += [PSCustomObject]@{
            Method       = $method
            Path         = $path
            BodyRequired = $bodyRequired
            Key          = "$method $path"
        }
    }
    return ,$routes
}

# ---------------------------------------------------------------------------
# Substitute :param placeholders with dummy values so every URL is
# well-formed. The smoke pass only confirms the route is registered;
# 404-on-resource is fine for params, 404-on-route is the bug we're hunting.
# ---------------------------------------------------------------------------
function Substitute-Params {
    param([string]$Path)
    return ($Path -replace ":[A-Za-z]+", "dummy-id")
}

# ---------------------------------------------------------------------------
# HTTP helper -- returns ($status:int, $bodyText:string). Never throws on
# 4xx/5xx; only catches connection-level errors.
# ---------------------------------------------------------------------------
function Invoke-Probe {
    param(
        [string]$Method,
        [string]$Url,
        [string]$Body = $null,
        [int]$TimeoutSec = 10
    )
    try {
        $params = @{
            Uri             = $Url
            Method          = $Method
            UseBasicParsing = $true
            TimeoutSec      = $TimeoutSec
            ErrorAction     = "Stop"
        }
        if ($Body) {
            $params.ContentType = "application/json"
            $params.Body        = $Body
        } elseif ($Method -in @("POST", "PUT", "PATCH")) {
            $params.ContentType = "application/json"
            $params.Body        = "{}"
        }
        $resp = Invoke-WebRequest @params
        return @($resp.StatusCode, $resp.Content)
    } catch [System.Net.WebException] {
        # Pull HTTP status + body off non-success responses. PS 5.1 gotcha:
        # Invoke-WebRequest has already consumed the error response stream,
        # so GetResponseStream() reads back "" (position at END). The body
        # lives in ErrorDetails.Message; rewinding the seekable stream is
        # the fallback.
        $r = $_.Exception.Response
        if ($r) {
            $status = [int]$r.StatusCode
            $body = $_.ErrorDetails.Message
            if (-not $body) {
                $stream = $r.GetResponseStream()
                if ($stream -and $stream.CanSeek) {
                    $stream.Position = 0
                    $reader = New-Object System.IO.StreamReader($stream)
                    $body = $reader.ReadToEnd()
                }
            }
            if (-not $body) { $body = "" }
            return @($status, $body)
        }
        return @(0, $_.Exception.Message)
    } catch {
        # PS 7+ throws Microsoft.PowerShell.Commands.HttpResponseException
        $r = $_.Exception.Response
        if ($r) {
            $status = [int]$r.StatusCode
            try {
                $body = $_.ErrorDetails.Message
            } catch {
                $body = ""
            }
            return @($status, $body)
        }
        return @(0, $_.Exception.Message)
    }
}

# ---------------------------------------------------------------------------
# Pretty result emitters. Tab-aligned columns: STATUS METHOD PATH DETAIL.
# ---------------------------------------------------------------------------
$results = New-Object System.Collections.Generic.List[Object]
function Record {
    param([string]$Status, [string]$Method, [string]$Path, [string]$Detail)
    $results.Add([PSCustomObject]@{
        Status = $Status; Method = $Method; Path = $Path; Detail = $Detail
    })
    $methodPad = $Method.PadRight(6)
    $pathPad = $Path
    if ($pathPad.Length -lt 50) { $pathPad = $pathPad.PadRight(50) }
    Write-Host ("{0}  {1}  {2}  {3}" -f $Status.PadRight(4), $methodPad, $pathPad, $Detail)
}

# ---------------------------------------------------------------------------
# -DirectExe helpers: free-port picker + isolated secondary-instance launch.
# Env surface mirrors instance_manager.rs:351-357 + the supervisor's exe-mode
# spawn (process/manager.rs:1459-1546): QONTINUI_PORT, QONTINUI_INSTANCE_NAME,
# QONTINUI_PRIMARY_PORT, isolated config/secure-storage/WebView2 temp dirs,
# keychain disabled, CLAUDECODE removed.
# ---------------------------------------------------------------------------
function Get-FreePort {
    param([int]$Start = 9877)
    for ($p = $Start; $p -lt ($Start + 200); $p++) {
        $inUse = $false
        try {
            $listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, $p)
            $listener.Start()
            $listener.Stop()
        } catch {
            $inUse = $true
        }
        if (-not $inUse) { return $p }
    }
    throw "Could not find a free port in [$Start, $($Start + 200))"
}

function Start-DirectRunner {
    param([string]$ExePath, [int]$Port)

    # -LiteralPath, not -Path: the published exe is "Qontinui Runner.exe" and
    # lives under "…\Qontinui Runner\". A space is harmless to -Path, but the
    # install dir is user-controlled and a wildcard metacharacter ([ ] ? *) in
    # it would silently make -Path glob instead of address one file.
    $resolved = (Resolve-Path -LiteralPath $ExePath -ErrorAction Stop).Path
    $instanceName = "test-$Port"

    # Fresh temp dirs so CI never touches %APPDATA% / a real WebView2 profile.
    $tmpRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("contract-smoke-" + $instanceName + "-" + [System.Guid]::NewGuid().ToString("N").Substring(0, 8))
    $configDir = Join-Path $tmpRoot "config"
    $webviewDir = Join-Path $tmpRoot "webview2"
    $logDir = Join-Path $tmpRoot "logs"
    New-Item -ItemType Directory -Force -Path $configDir  | Out-Null
    New-Item -ItemType Directory -Force -Path $webviewDir | Out-Null
    New-Item -ItemType Directory -Force -Path $logDir     | Out-Null

    # Capture the runner's own stdout/stderr so an early hard-exit isn't a
    # black box. Start-Process needs distinct files for each stream.
    $stdoutFile = Join-Path $tmpRoot "runner-stdout.log"
    $stderrFile = Join-Path $tmpRoot "runner-stderr.log"

    # Env table (verified against the two authoritative spawners). Set on the
    # current process env so Start-Process inherits it, snapshot+restore around
    # the launch so we don't pollute the rest of the script's env.
    $prev = @{}
    $toSet = @{
        "QONTINUI_PORT"               = "$Port"
        "QONTINUI_INSTANCE_NAME"      = $instanceName
        "QONTINUI_PRIMARY_PORT"       = "$Port"   # self — no primary in CI
        "QONTINUI_CONFIG_DIR"         = $configDir
        "QONTINUI_SECURE_STORAGE_DIR" = $configDir
        "WEBVIEW2_USER_DATA_FOLDER"   = $webviewDir
        "QONTINUI_DISABLE_KEYCHAIN"   = "1"
        # Pin the runner's startup-panic log into our temp tree. The runner
        # writes <QONTINUI_RUNNER_LOG_DIR>/runner-panic.log on any early-init
        # panic (startup_panic.rs); an exit code 2 is *defined* as a panic
        # caught by main()'s catch_unwind (main.rs:272-276). Without this the
        # log lands in %LOCALAPPDATA%\qontinui-runner\dev-logs — outside the
        # temp dir we dump on failure — so the crash cause stays invisible.
        "QONTINUI_RUNNER_LOG_DIR"     = $logDir
    }
    foreach ($k in $toSet.Keys) {
        $prev[$k] = [System.Environment]::GetEnvironmentVariable($k, "Process")
        [System.Environment]::SetEnvironmentVariable($k, $toSet[$k], "Process")
    }
    # CLAUDECODE removed — both spawners strip it so the embedded Claude CLI can start.
    $prev["CLAUDECODE"] = [System.Environment]::GetEnvironmentVariable("CLAUDECODE", "Process")
    [System.Environment]::SetEnvironmentVariable("CLAUDECODE", $null, "Process")

    Write-Host "Launching direct-exe runner '$instanceName' on port $Port"
    Write-Host "  exe:     $resolved"
    Write-Host "  config:  $configDir"
    Write-Host "  webview: $webviewDir"
    Write-Host "  logs:    $logDir"
    Write-Host "  stdout:  $stdoutFile"
    Write-Host "  stderr:  $stderrFile"
    try {
        $proc = Start-Process -FilePath $resolved -PassThru -WorkingDirectory $tmpRoot `
            -RedirectStandardOutput $stdoutFile -RedirectStandardError $stderrFile
    } finally {
        # Restore env regardless of launch outcome.
        foreach ($k in $prev.Keys) {
            [System.Environment]::SetEnvironmentVariable($k, $prev[$k], "Process")
        }
    }

    return [PSCustomObject]@{
        Process     = $proc
        Port        = $Port
        Id          = $instanceName
        TmpRoot     = $tmpRoot
        LogDir      = $logDir
        StdoutFile  = $stdoutFile
        StderrFile  = $stderrFile
    }
}

# ---------------------------------------------------------------------------
# Kill a process and every descendant of it, children first.
#
# `Stop-Process -Id <parent>` kills ONLY the parent: Windows re-parents the
# survivors to the OS rather than cascading, so the runner's WebView2 host
# processes and embedded CLI outlive the run and keep the temp WebView2 profile
# locked. We therefore walk Win32_Process.ParentProcessId ourselves.
#
# The walk is strictly DOWNWARD from $RootPid — it can never climb to an
# ancestor, which is why this does not use a tree-kill flag (`taskkill /T`):
# on this box a mis-aimed tree kill would take out live editor/agent sessions.
# Two further guards: a visited set (PID reuse can make the parent graph
# cyclic), and a creation-time check so a recycled PID whose process predates
# the root is not adopted into the tree.
# ---------------------------------------------------------------------------
function Stop-ProcessTree {
    param([int]$RootPid)

    $all = @(Get-CimInstance -ClassName Win32_Process -ErrorAction Stop |
        Select-Object ProcessId, ParentProcessId, Name, CreationDate)

    $root = $all | Where-Object { $_.ProcessId -eq $RootPid } | Select-Object -First 1
    if (-not $root) { return 0 }
    $rootCreated = $root.CreationDate

    $byParent = @{}
    foreach ($p in $all) {
        $ppid = [int]$p.ParentProcessId
        if (-not $byParent.ContainsKey($ppid)) { $byParent[$ppid] = @() }
        $byParent[$ppid] += $p
    }

    # Breadth-first collect of descendants. Ordered deepest-last so the reverse
    # is a safe kill order (a child dies before the parent that supervises it).
    $ordered = New-Object System.Collections.Generic.List[Object]
    $visited = @{ $RootPid = $true }
    $queue = New-Object System.Collections.Generic.Queue[int]
    $queue.Enqueue($RootPid)
    while ($queue.Count -gt 0) {
        $cur = $queue.Dequeue()
        if (-not $byParent.ContainsKey($cur)) { continue }
        foreach ($child in $byParent[$cur]) {
            $cpid = [int]$child.ProcessId
            if ($cpid -le 4) { continue }                 # System / Idle
            if ($cpid -eq $PID) { continue }              # never ourselves
            if ($visited.ContainsKey($cpid)) { continue }
            # PID reuse: a "child" that started before the root is a stale
            # parent-id pointing at a recycled PID, not our process.
            if ($rootCreated -and $child.CreationDate -and $child.CreationDate -lt $rootCreated) { continue }
            $visited[$cpid] = $true
            $ordered.Add($child)
            $queue.Enqueue($cpid)
        }
    }

    $killed = 0
    for ($i = $ordered.Count - 1; $i -ge 0; $i--) {
        $target = $ordered[$i]
        try {
            Stop-Process -Id ([int]$target.ProcessId) -Force -ErrorAction Stop
            $killed++
        } catch {
            # Already gone (it may have died with a sibling) — not an error.
            Write-Host "  note: could not stop $($target.Name) ($($target.ProcessId)): $($_.Exception.Message)"
        }
    }
    try {
        Stop-Process -Id $RootPid -Force -ErrorAction Stop
        $killed++
    } catch {
        Write-Host "  note: could not stop root pid ${RootPid}: $($_.Exception.Message)"
    }
    return $killed
}

# ---------------------------------------------------------------------------
# Dump everything we captured from a direct-exe runner — its redirected
# stdout/stderr plus any *.log files the runner wrote under its temp tree
# (notably runner-panic.log, written on an early-init panic / exit code 2).
# Called on early-exit AND on ready-timeout so a CI failure is diagnosable.
# PS 5.1-compatible (no ?., no ternary).
# ---------------------------------------------------------------------------
function Dump-DirectRunnerDiagnostics {
    param($DirectRunner)
    if (-not $DirectRunner) { return }

    Write-Host ""
    Write-Host ("=" * 90)
    Write-Host "DIRECT-EXE RUNNER DIAGNOSTICS"
    Write-Host ("=" * 90)

    function _dumpFile([string]$label, [string]$path) {
        Write-Host ""
        Write-Host "=== $label ($path) ==="
        if ($path -and (Test-Path $path)) {
            $content = Get-Content -Raw -Path $path -ErrorAction SilentlyContinue
            if ($null -ne $content -and $content.Trim().Length -gt 0) {
                Write-Host $content
            } else {
                Write-Host "(empty)"
            }
        } else {
            Write-Host "(file not found)"
        }
    }

    _dumpFile "runner stdout" $DirectRunner.StdoutFile
    _dumpFile "runner stderr" $DirectRunner.StderrFile
    # An EMPTY pair here is expected, not suspicious, on the published leg: a
    # release build carries `#![cfg_attr(not(debug_assertions), windows_subsystem
    # = "windows")]` (src-tauri/src/main.rs:2), so the installed exe is a Windows
    # GUI-subsystem binary with no console attached and writes nothing to the
    # redirected handles. The *.log sweep below (fed by QONTINUI_RUNNER_LOG_DIR)
    # is the diagnostic that survives on that leg. Say so rather than letting a
    # reader take "(empty)" for a runner that produced no output at all.
    Write-Host ""
    Write-Host "NOTE: a release/installed build is windows-subsystem and writes NOTHING to the"
    Write-Host "      two files above. '(empty)' there is expected on the published leg -- read"
    Write-Host "      the *.log sweep below for that build's actual output."

    # Sweep the whole temp tree for *.log files (panic log, any future logs
    # the runner drops under config/log dirs) and dump each in full.
    Write-Host ""
    Write-Host "=== *.log files under $($DirectRunner.TmpRoot) ==="
    $logFiles = @()
    if ($DirectRunner.TmpRoot -and (Test-Path $DirectRunner.TmpRoot)) {
        $logFiles = @(Get-ChildItem -Path $DirectRunner.TmpRoot -Recurse -Filter *.log -File -ErrorAction SilentlyContinue)
    }
    # Exclude the two redirect files we already dumped above.
    $already = @($DirectRunner.StdoutFile, $DirectRunner.StderrFile)
    $logFiles = @($logFiles | Where-Object { $already -notcontains $_.FullName })
    if ($logFiles.Count -eq 0) {
        Write-Host "(no additional *.log files found)"
    } else {
        foreach ($lf in $logFiles) {
            _dumpFile "log file" $lf.FullName
        }
    }
    Write-Host ""
    Write-Host ("=" * 90)
}

function Wait-DirectRunnerReady {
    param([int]$Port, [int]$TimeoutSecs = 180, $Process)
    $healthUrl = "http://localhost:$Port/health"
    # Gate on `uiBridgeIpcObserved`, NOT `frontendReady`.
    #
    # What this smoke needs before it starts walking every UI_BRIDGE_ROUTES
    # entry is proof that a full UI Bridge IPC round-trip has actually
    # completed — otherwise the first route probes land in the 503
    # transport-failure path and CI goes intermittently red. That is exactly
    # what the one-way latch in `AppState::frontend_ready` measures, and it is
    # the only thing that does.
    #
    # It used to be published as `frontendReady`. As of 2026-08-05 `/health`
    # derives `frontendReady` from the frontend-liveness ladder instead (it now
    # goes true off the self-driving 3s pong loop, with no IPC round-trip
    # involved) and publishes the raw latch under its honest name,
    # `uiBridgeIpcObserved`. Polling `frontendReady` here would return true on
    # the first poll, skip the poke below, and start probing routes before the
    # IPC path was ever exercised.
    #
    # A passive /health poll never drives the round-trip, so once the HTTP shell
    # is responsive we actively poke a cheap UI Bridge route
    # (GET /ui-bridge/control/elements) to force it, then re-check the flag.
    # Both fields live under the `data` envelope in /health.
    $pokeUrl = "http://localhost:$Port/ui-bridge/control/elements"
    $deadline = (Get-Date).AddSeconds($TimeoutSecs)
    Write-Host "Polling $healthUrl for uiBridgeIpcObserved:true (timeout ${TimeoutSecs}s) ..."
    while ((Get-Date) -lt $deadline) {
        if ($Process -and $Process.HasExited) {
            throw "direct-exe runner exited early with code $($Process.ExitCode) before becoming ready"
        }
        $responsive = $false
        try {
            $resp = Invoke-WebRequest -Uri $healthUrl -UseBasicParsing -TimeoutSec 5 -ErrorAction Stop
            if ($resp.StatusCode -eq 200) {
                $h = $null
                try { $h = $resp.Content | ConvertFrom-Json } catch { $h = $null }
                # `uiBridgeIpcObserved` is nested under data. Fall back to the
                # legacy `frontendReady` (top level or under data) so this
                # script still gates correctly against a runner built BEFORE
                # the 2026-08-05 rename, where `frontendReady` still carried
                # the latch.
                $fr = $null
                if ($h) {
                    $respObj = if ($h.PSObject.Properties.Name -contains 'data' -and $h.data) { $h.data } else { $h }
                    if ($respObj.PSObject.Properties.Name -contains 'uiBridgeIpcObserved') {
                        $fr = $respObj.uiBridgeIpcObserved
                    } elseif ($respObj.PSObject.Properties.Name -contains 'frontendReady') {
                        $fr = $respObj.frontendReady
                    } elseif ($h.PSObject.Properties.Name -contains 'frontendReady') {
                        $fr = $h.frontendReady
                    }
                    if ($respObj.PSObject.Properties.Name -contains 'responsive') { $responsive = [bool]$respObj.responsive }
                }
                if ($fr -eq $true) {
                    Write-Host "Runner is ready (UI Bridge IPC round-trip observed)."
                    return
                }
            }
        } catch {
            # connection refused / not-yet-listening — keep polling.
        }
        # Shell is up but no IPC round-trip has completed yet: poke the UI
        # Bridge to force the first one, which flips the latch.
        if ($responsive) {
            try {
                $null = Invoke-WebRequest -Uri $pokeUrl -UseBasicParsing -TimeoutSec 10 -ErrorAction Stop
            } catch {
                # A non-200 (or even an error envelope) still drove an IPC
                # round-trip, which is all we need; ignore.
            }
        }
        Start-Sleep -Milliseconds 1000
    }
    throw "direct-exe runner did not report uiBridgeIpcObserved:true within ${TimeoutSecs}s"
}

# ---------------------------------------------------------------------------
# Parse routes (always -- needed for both DryRun and live).
# ---------------------------------------------------------------------------
Write-Host "Parsing UI_BRIDGE_ROUTES from $SdkTypesPath ..."
$routes = Parse-UiBridgeRoutes -Path $SdkTypesPath
Write-Host ("Parsed {0} routes." -f $routes.Count)

if ($DryRun) {
    foreach ($r in $routes) {
        $skipNote = if ($SKIP_ROUTES.ContainsKey($r.Key)) { " [SKIP: $($SKIP_ROUTES[$r.Key])]" } else { "" }
        $bodyNote = if ($r.BodyRequired) { " (body)" } else { "" }
        Write-Host ("  {0,-6}  {1}{2}{3}" -f $r.Method, $r.Path, $bodyNote, $skipNote)
    }
    Write-Host ""
    Write-Host ("Total: {0} routes, {1} skip" -f $routes.Count, $SKIP_ROUTES.Count)
    exit 0
}

# ---------------------------------------------------------------------------
# Bring up a runner. Two modes:
#   -DirectExe : Start-Process the exe directly (supervisor-free, CI). Set
#                either by the caller (dev leg) or by -UseInstalledExe's
#                locator above (published leg) -- both land here identically,
#                so the two legs exercise the SAME probe path against
#                different binaries, which is the whole point of the parity gate.
#   default    : spawn via the supervisor (dev).
# Both yield $runnerId / $runnerPort / $runnerBase; DirectExe also sets
# $directRunner (Stop-Process'd in the finally below).
# ---------------------------------------------------------------------------
$directRunner = $null
if ($DirectExe) {
    if (-not (Test-Path -LiteralPath $DirectExe)) {
        Write-Host "ERROR: -DirectExe path not found: $DirectExe" -ForegroundColor Red
        exit 1
    }
    $runnerPort = Get-FreePort -Start 9877
    $directRunner = Start-DirectRunner -ExePath $DirectExe -Port $runnerPort
    $runnerId = $directRunner.Id
    try {
        Wait-DirectRunnerReady -Port $runnerPort -TimeoutSecs $WaitTimeoutSecs -Process $directRunner.Process
    } catch {
        Write-Host "ERROR: $($_.Exception.Message)" -ForegroundColor Red
        # Stop the process FIRST so its redirected stdout/stderr handles are
        # released and fully flushed before we read them back.
        if ($directRunner.Process -and -not $directRunner.Process.HasExited) {
            try { Stop-Process -Id $directRunner.Process.Id -Force -ErrorAction SilentlyContinue } catch { }
        }
        # Dump captured output + any panic/log files so the failure (early-exit
        # OR ready-timeout) is diagnosable in CI logs instead of a bare
        # "exited early with code N".
        Dump-DirectRunnerDiagnostics -DirectRunner $directRunner
        exit 1
    }
    $runnerBase = "http://localhost:$runnerPort/ui-bridge"
    Write-Host "Direct-exe runner $runnerId on port $runnerPort. Base: $runnerBase"
    Write-Host ""
} else {
    # -----------------------------------------------------------------------
    # Verify supervisor is up.
    # -----------------------------------------------------------------------
    try {
        $h = Invoke-WebRequest "$SupervisorBase/health" -UseBasicParsing -TimeoutSec 5 -ErrorAction Stop
        if ($h.StatusCode -ne 200) {
            Write-Host "ERROR: supervisor /health returned $($h.StatusCode)" -ForegroundColor Red
            exit 1
        }
    } catch {
        Write-Host "ERROR: supervisor not reachable at $SupervisorBase. Start it before running this script." -ForegroundColor Red
        Write-Host "       $($_.Exception.Message)" -ForegroundColor Red
        exit 1
    }

    # -----------------------------------------------------------------------
    # Spawn temp runner.
    # -----------------------------------------------------------------------
    $spawnBody = @{
        requester_id      = "contract-smoke"
        wait              = $true
        wait_timeout_secs = $WaitTimeoutSecs
        rebuild           = [bool]$Rebuild
    } | ConvertTo-Json -Compress

    Write-Host "Spawning temp runner (rebuild=$([bool]$Rebuild)) ..."
    try {
        $spawnResp = Invoke-WebRequest -Uri "$SupervisorBase/runners/spawn-test" `
            -Method POST -ContentType "application/json" -Body $spawnBody `
            -UseBasicParsing -TimeoutSec ($WaitTimeoutSecs + 60) -ErrorAction Stop
    } catch {
        Write-Host "ERROR: spawn-test failed: $($_.Exception.Message)" -ForegroundColor Red
        if ($_.ErrorDetails.Message) { Write-Host $_.ErrorDetails.Message -ForegroundColor Red }
        exit 1
    }

    $spawn = $spawnResp.Content | ConvertFrom-Json
    $runnerId = $spawn.id
    $runnerPort = $spawn.port
    if (-not $runnerId -or -not $runnerPort) {
        Write-Host "ERROR: spawn-test response missing id/port: $($spawnResp.Content)" -ForegroundColor Red
        exit 1
    }
    $runnerBase = "http://localhost:$runnerPort/ui-bridge"
    Write-Host "Spawned runner $runnerId on port $runnerPort. Base: $runnerBase"
    Write-Host ""
}

# ---------------------------------------------------------------------------
# Run all probes inside try/finally so we always stop the runner.
# ---------------------------------------------------------------------------
$exitCode = 0
try {
    # Warm-up: prime the cold OCR/VLM models so the first *measured* slow-route
    # request is warm. Belt-and-suspenders — the per-route timeout bump alone
    # makes the gate deterministic; warm-up failures are non-fatal. Skipped for
    # any model route that's in SKIP_ROUTES (e.g. both under -Profile ci, where
    # there's no llama-swap to warm).
    $warmTargets = @("POST /vision/extract", "POST /vision/describe") |
        Where-Object { -not $SKIP_ROUTES.ContainsKey($_) }
    if ($warmTargets.Count -gt 0) {
        Write-Host "Warming $($warmTargets -join ' + ') (cold model load) ..."
        foreach ($warmKey in $warmTargets) {
            $warmPath = $warmKey.Split(" ")[1]
            try {
                $null = Invoke-Probe -Method "POST" -Url ($runnerBase + $warmPath) -Body "{}" -TimeoutSec 120
            } catch { }
        }
        Write-Host ""
    }

    Write-Host ("{0}  {1}  {2}  {3}" -f "STAT".PadRight(4), "METHOD".PadRight(6), "PATH".PadRight(50), "DETAIL")
    Write-Host ("-" * 90)

    foreach ($r in $routes) {
        if ($SKIP_ROUTES.ContainsKey($r.Key)) {
            Record "SKIP" $r.Method $r.Path $SKIP_ROUTES[$r.Key]
            continue
        }
        # Per-route try/catch. $ErrorActionPreference = "Stop" makes ANY error
        # in this body terminating, and an escape would unwind straight past the
        # summary block -- the script would exit 1 having recorded no FAIL row
        # and printed no totals, which reads exactly like a harness/invocation
        # problem instead of the route failure it is. Catch here so the failing
        # route gets a row and the remaining routes still run.
        try {
        $url = $runnerBase + (Substitute-Params $r.Path)
        $body = $null
        if ($r.BodyRequired) {
            if ($BODY_FIXTURES.ContainsKey($r.Key)) {
                $body = $BODY_FIXTURES[$r.Key]
            } else {
                $body = "{}"
            }
        } elseif ($BODY_FIXTURES.ContainsKey($r.Key)) {
            $body = $BODY_FIXTURES[$r.Key]
        }

        $probeTimeout = 10
        if ($SLOW_ROUTES.ContainsKey($r.Key)) { $probeTimeout = $SLOW_ROUTES[$r.Key] }
        $res = Invoke-Probe -Method $r.Method -Url $url -Body $body -TimeoutSec $probeTimeout
        $status = [int]$res[0]
        if ($status -eq 0) {
            Record "FAIL" $r.Method $r.Path "connection error: $($res[1])"
            $exitCode = 1
        } elseif ($status -eq 404) {
            # Body-aware 404 classifier: a route with a :param segment that
            # returns a structured JSON error envelope (success:false / any
            # non-empty JSON error) is route-matched-but-resource-not-found —
            # the route IS registered, so PASS. A genuinely unregistered route
            # falls through to axum's empty-body default 404 → keep FAIL.
            $hasParam = $r.Path -match ":[A-Za-z]+"
            $isStructuredErr = $false
            $rawBody = $res[1]
            if ($hasParam -and $rawBody -and ($rawBody.Trim().Length -gt 0)) {
                try {
                    $parsed = $rawBody | ConvertFrom-Json
                    if ($null -ne $parsed) {
                        $hasSuccessFalse = ($parsed.PSObject.Properties.Name -contains 'success') -and (-not $parsed.success)
                        $hasErrorField = ($parsed.PSObject.Properties.Name -contains 'error') -and ($null -ne $parsed.error)
                        if ($hasSuccessFalse -or $hasErrorField) { $isStructuredErr = $true }
                    }
                } catch {
                    $isStructuredErr = $false
                }
            }
            if ($isStructuredErr) {
                Record "PASS" $r.Method $r.Path "404 unknown-id (route reachable)"
            } else {
                Record "FAIL" $r.Method $r.Path "404 (route not registered)"
                $exitCode = 1
            }
        } else {
            # Accept only statuses in the route's expected set. Anything else --
            # every 5xx, 401/403, and any other unlisted code -- is a FAIL.
            $okSet = $DEFAULT_OK_STATUS
            if ($EXPECTED_STATUS.ContainsKey($r.Key)) { $okSet = @($EXPECTED_STATUS[$r.Key]) }
            if ($okSet -contains $status) {
                Record "PASS" $r.Method $r.Path "$status"
            } else {
                Record "FAIL" $r.Method $r.Path "$status (expected one of: $($okSet -join ','))"
                $exitCode = 1
            }
        }
        } catch {
            Record "FAIL" $r.Method $r.Path "probe threw: $($_.Exception.Message)"
            $exitCode = 1
        }
    }

    # ---------------------------------------------------------------------------
    # Targeted shape probes -- friction-table cases.
    # ---------------------------------------------------------------------------
    Write-Host ""
    Write-Host "--- Shape probes ---"

    # Probe 1: revealsAny= filter actually filters
    try {
        $unfilt = Invoke-Probe -Method "GET" -Url "$runnerBase/control/elements"
        $filt   = Invoke-Probe -Method "GET" -Url "$runnerBase/control/elements?revealsAny=non-existent-id-xyz"

        if ($unfilt[0] -ne 200) {
            Record "FAIL" "GET" "/control/elements (probe)" "baseline returned $($unfilt[0])"
            $exitCode = 1
        } elseif ($filt[0] -ne 200) {
            Record "FAIL" "GET" "/control/elements?revealsAny=" "filtered returned $($filt[0])"
            $exitCode = 1
        } else {
            # Response shape: { success, data: { elements: [...], count: N, timestamp } }
            # Trust the server's `count` field if present (handles empty-array
            # PSCustomObject quirk where `if ($obj.elements) { ... }` is falsy
            # for an empty array). Fall back to .Count on the array itself.
            $u = $unfilt[1] | ConvertFrom-Json
            $f = $filt[1] | ConvertFrom-Json
            function _extractCount($o) {
                if ($null -eq $o) { return -1 }
                $d = if ($o.PSObject.Properties.Name -contains 'data') { $o.data } else { $o }
                if ($null -ne $d.count) { return [int]$d.count }
                if ($d.PSObject.Properties.Name -contains 'elements') { return @($d.elements).Count }
                if ($o.PSObject.Properties.Name -contains 'elements') { return @($o.elements).Count }
                return -1
            }
            $uCount = _extractCount $u
            $fCount = _extractCount $f
            if ($uCount -lt 0 -or $fCount -lt 0) {
                Record "FAIL" "GET" "/control/elements?revealsAny=" "could not find elements array in response (u=$uCount f=$fCount)"
                $exitCode = 1
            } elseif ($fCount -ge $uCount -and $uCount -gt 0) {
                Record "FAIL" "GET" "/control/elements?revealsAny=" "filter ignored: unfilt=$uCount, filt=$fCount (expected filt < unfilt)"
                $exitCode = 1
            } else {
                Record "PASS" "GET" "/control/elements?revealsAny=" "unfilt=$uCount, filt=$fCount"
            }
        }
    } catch {
        Record "FAIL" "GET" "/control/elements?revealsAny= (probe)" "exception: $($_.Exception.Message)"
        $exitCode = 1
    }

    # Probe 2: scope field round-trips on /control/component/:id
    # The runner's /control/components handler returns Json(success(Value::Array(merged)))
    # so the wire shape is { success, data: [ {...}, ... ] }. Components may use
    # `id` (IPC path) or `componentId` (WS-transport path) as the identifier.
    try {
        $compsRes = Invoke-Probe -Method "GET" -Url "$runnerBase/control/components"
        if ($compsRes[0] -ne 200) {
            Record "SKIP" "GET" "/control/component/:id (scope probe)" "components list returned $($compsRes[0])"
        } else {
            $comps = $compsRes[1] | ConvertFrom-Json
            $compArr = $null
            if ($comps.data -is [System.Array] -or $comps.data -is [System.Collections.IList]) {
                $compArr = @($comps.data)
            } elseif ($comps.data -and $comps.data.components) {
                $compArr = @($comps.data.components)
            } elseif ($comps.components) {
                $compArr = @($comps.components)
            } elseif ($comps -is [System.Array]) {
                $compArr = @($comps)
            }
            if (-not $compArr -or $compArr.Count -eq 0) {
                Record "SKIP" "GET" "/control/component/:id (scope probe)" "no components registered on a stock temp runner"
            } else {
                $cid = $null
                foreach ($c in $compArr) {
                    foreach ($key in @('id', 'componentId', 'component_id', 'name', 'appId')) {
                        if ($c.PSObject.Properties.Name -contains $key) {
                            $v = $c.$key
                            if ($v -and ($v -is [string])) { $cid = $v; break }
                        }
                    }
                    if ($cid) { break }
                }
                if (-not $cid) {
                    $sample = ($compArr[0] | ConvertTo-Json -Compress -Depth 3)
                    if ($sample.Length -gt 120) { $sample = $sample.Substring(0, 117) + "..." }
                    Record "SKIP" "GET" "/control/component/:id (scope probe)" "no usable id key on components; sample=$sample"
                } else {
                    $cRes = Invoke-Probe -Method "GET" -Url "$runnerBase/control/component/$cid"
                    if ($cRes[0] -ne 200) {
                        Record "FAIL" "GET" "/control/component/$cid (scope probe)" "returned $($cRes[0])"
                        $exitCode = 1
                    } else {
                        # Look for "scope" key anywhere in the raw JSON text -- treats null/string/missing-by-omission as PASS-if-present.
                        if ($cRes[1] -match '"scope"\s*:') {
                            Record "PASS" "GET" "/control/component/:id" "scope key present in serialized output (id=$cid)"
                        } else {
                            Record "FAIL" "GET" "/control/component/:id" "scope field stripped -- likely missing from runner's serializeComponent allow-list (id=$cid)"
                            $exitCode = 1
                        }
                    }
                }
            }
        }
    } catch {
        Record "FAIL" "GET" "/control/component/:id (scope probe)" "exception: $($_.Exception.Message)"
        $exitCode = 1
    }

    # Probe 3: /control/element/:id/expect returns 422 on timeout
    try {
        $body = '{"state":"visible","timeoutMs":200,"pollMs":50}'
        $exp = Invoke-Probe -Method "POST" -Url "$runnerBase/control/element/non-existent-id-xyz/expect" -Body $body
        if ($exp[0] -eq 422) {
            Record "PASS" "POST" "/control/element/:id/expect (timeout)" "422 as expected"
        } elseif ($exp[0] -eq 404) {
            Record "FAIL" "POST" "/control/element/:id/expect (timeout)" "404 -- route not registered"
            $exitCode = 1
        } elseif ($exp[0] -eq 200) {
            Record "FAIL" "POST" "/control/element/:id/expect (timeout)" "200 -- status mapping regressed (timeout should be 422)"
            $exitCode = 1
        } elseif ($exp[0] -eq 500) {
            Record "FAIL" "POST" "/control/element/:id/expect (timeout)" "500 -- handler crashed"
            $exitCode = 1
        } else {
            Record "FAIL" "POST" "/control/element/:id/expect (timeout)" "$($exp[0]) -- expected 422"
            $exitCode = 1
        }
    } catch {
        Record "FAIL" "POST" "/control/element/:id/expect (timeout)" "exception: $($_.Exception.Message)"
        $exitCode = 1
    }

    # Probe 4: /vision/capture returns the Phase-2 envelope shape
    # POST {"contract":"claude"} should yield:
    #   200 + { success: true, data: { path, sha256, width, height, bytes,
    #          format: "jpeg", contract: "claude_vision_v1" } }
    # path must start with "tmp_vision_cache/" and bytes must be < 5 MiB
    # (Claude vision input ceiling from the OutputContract pipeline).
    try {
        $body = '{"contract":"claude"}'
        $cap = Invoke-Probe -Method "POST" -Url "$runnerBase/vision/capture" -Body $body
        if ($cap[0] -eq 404) {
            Record "FAIL" "POST" "/vision/capture (shape)" "404 -- route not registered"
            $exitCode = 1
        } elseif ($cap[0] -ne 200) {
            Record "FAIL" "POST" "/vision/capture (shape)" "$($cap[0]) -- expected 200"
            $exitCode = 1
        } else {
            $obj = $cap[1] | ConvertFrom-Json
            if (-not $obj.success -or -not $obj.data) {
                Record "FAIL" "POST" "/vision/capture (shape)" "envelope missing success/data"
                $exitCode = 1
            } else {
                $d = $obj.data
                $errs = @()
                foreach ($key in @('path','sha256','width','height','bytes','format','contract')) {
                    if (-not ($d.PSObject.Properties.Name -contains $key)) {
                        $errs += "missing $key"
                    }
                }
                if ($d.format -and $d.format -ne 'jpeg') {
                    $errs += "format=$($d.format) (expected jpeg for claude contract)"
                }
                if ($d.contract -and $d.contract -ne 'claude_vision_v1') {
                    $errs += "contract=$($d.contract) (expected claude_vision_v1)"
                }
                if ($d.bytes -and [int]$d.bytes -ge (5 * 1024 * 1024)) {
                    $errs += "bytes=$($d.bytes) >= 5 MiB ceiling"
                }
                if ($d.path -and -not ($d.path -match '^tmp_vision_cache[\\/].+\.(jpe?g|webp|png)$')) {
                    $errs += "path=$($d.path) (expected tmp_vision_cache/<sha>.<ext>)"
                }
                if ($errs.Count -gt 0) {
                    Record "FAIL" "POST" "/vision/capture (shape)" ($errs -join "; ")
                    $exitCode = 1
                } else {
                    Record "PASS" "POST" "/vision/capture (shape)" "$($d.width)x$($d.height) $($d.bytes)b $($d.format) $($d.contract)"
                }
            }
        }
    } catch {
        Record "FAIL" "POST" "/vision/capture (shape)" "exception: $($_.Exception.Message)"
        $exitCode = 1
    }
} catch {
    # Backstop for anything in the probe block that is NOT already caught (the
    # warm-up, the route loop and each shape probe have their own catch). Without
    # this the terminating error escapes past the summary and the script exits 1
    # with no FAIL row and no totals -- indistinguishable from a bad invocation.
    Record "FAIL" "-" "(smoke harness)" "unhandled: $($_.Exception.Message)"
    $exitCode = 1
} finally {
    # Always stop the temp runner so a probe failure doesn't leak it.
    Write-Host ""
    Write-Host "Stopping runner $runnerId ..."
    if ($directRunner) {
        # DirectExe mode: kill the launched exe AND everything it spawned. The
        # runner forks WebView2 host processes and an embedded CLI; killing the
        # parent alone orphans them (re-parented to the OS), so each CI run used
        # to leak a handful of live processes holding the temp WebView2 profile.
        try {
            $killed = Stop-ProcessTree -RootPid $directRunner.Process.Id
            Write-Host "Runner stopped ($killed process(es) in tree)."
        } catch {
            Write-Host "WARNING: Stop-ProcessTree failed: $($_.Exception.Message)" -ForegroundColor Yellow
        }
    } else {
        try {
            # Supervisor's stop_runner takes Option<Json<StopRunnerRequest>>;
            # an empty {} satisfies axum's Json extractor (no body returns 415).
            $null = Invoke-WebRequest -Uri "$SupervisorBase/runners/$runnerId/stop" `
                -Method POST -ContentType "application/json" -Body "{}" `
                -UseBasicParsing -TimeoutSec 30 -ErrorAction Stop
            Write-Host "Runner stopped."
        } catch {
            Write-Host "WARNING: stop failed: $($_.Exception.Message)" -ForegroundColor Yellow
        }
    }
}

# ---------------------------------------------------------------------------
# Summary.
#
# The tally, the four reporting invariants and the rendering all live in
# scripts/lib/smoke-summary.ps1 so they can be unit-tested without booting a
# runner (scripts/tests/test-smoke-summary.ps1, wired as a CI gate). That file
# carries the full write-up of the PS 5.1 scalar-collapse bug this guards -- the
# short version is that a run with exactly ONE failing probe used to print
# "193 pass / 193 total, 8 skip" and "fail=" while exiting 1.
#
# Get-SmokeSummary may only RAISE the exit code (0 -> 2 when the harness's own
# accounting is unsound); it can never turn a failing run green.
# ---------------------------------------------------------------------------
$summary = Get-SmokeSummary -Results $results -ExitCode $exitCode
$exitCode = Write-SmokeSummary -Summary $summary

exit $exitCode
