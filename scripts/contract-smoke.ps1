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
# Prerequisite: supervisor running on http://localhost:9875.
#   Check with: Invoke-WebRequest http://localhost:9875/health -UseBasicParsing
#   This script does NOT start the supervisor itself -- the supervisor lives
#   as a long-running service the user manages outside the smoke harness.
#
# Usage:
#   powershell -File scripts/contract-smoke.ps1            # fast run, no rebuild
#   powershell -File scripts/contract-smoke.ps1 -Rebuild   # force fresh build
#   powershell -File scripts/contract-smoke.ps1 -DryRun    # parse routes & exit
#
# Exit code: 0 on all-pass, 1 on any FAIL.

param(
    [switch]$Rebuild,
    [switch]$DryRun,
    [int]$WaitTimeoutSecs = 180,
    [string]$SdkTypesPath = "D:/qontinui-root/ui-bridge/packages/ui-bridge/src/server/types.ts",
    [string]$SupervisorBase = "http://localhost:9875"
)

$ErrorActionPreference = "Stop"

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
}

# ---------------------------------------------------------------------------
# Per-route fixture bodies. For routes with bodyRequired: true we send a
# minimal valid-shape body where one is needed to exercise the parser; for
# the rest, an empty {} is fine -- we only assert "route is registered" by
# checking status != 404. Validation 4xx (other than 404) counts as PASS.
# ---------------------------------------------------------------------------
$BODY_FIXTURES = @{
    "POST /control/element/:id/action"        = '{"action":"click"}'
    "POST /control/element/:id/expect"        = '{"state":"visible","timeoutMs":200,"pollMs":50}'
    "POST /control/actions/batch"             = '{"actions":[]}'
    "POST /control/elements/rank"             = '{"ids":[]}'
    "POST /control/component/:id/action/:actionId" = '{}'
    "POST /control/find"                      = '{"query":"smoke"}'
    "POST /control/discover"                  = '{"query":"smoke"}'
    "POST /control/get-element-images"        = '{"ids":[]}'
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
        [string]$Body = $null
    )
    try {
        $params = @{
            Uri             = $Url
            Method          = $Method
            UseBasicParsing = $true
            TimeoutSec      = 10
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
        # Pull HTTP status off non-success responses.
        $r = $_.Exception.Response
        if ($r) {
            $status = [int]$r.StatusCode
            $stream = $r.GetResponseStream()
            $reader = New-Object System.IO.StreamReader($stream)
            $body   = $reader.ReadToEnd()
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
# Verify supervisor is up.
# ---------------------------------------------------------------------------
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

# ---------------------------------------------------------------------------
# Spawn temp runner.
# ---------------------------------------------------------------------------
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

# ---------------------------------------------------------------------------
# Run all probes inside try/finally so we always stop the runner.
# ---------------------------------------------------------------------------
$exitCode = 0
try {
    Write-Host ("{0}  {1}  {2}  {3}" -f "STAT".PadRight(4), "METHOD".PadRight(6), "PATH".PadRight(50), "DETAIL")
    Write-Host ("-" * 90)

    foreach ($r in $routes) {
        if ($SKIP_ROUTES.ContainsKey($r.Key)) {
            Record "SKIP" $r.Method $r.Path $SKIP_ROUTES[$r.Key]
            continue
        }
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

        $res = Invoke-Probe -Method $r.Method -Url $url -Body $body
        $status = $res[0]
        if ($status -eq 0) {
            Record "FAIL" $r.Method $r.Path "connection error: $($res[1])"
            $exitCode = 1
        } elseif ($status -eq 404) {
            Record "FAIL" $r.Method $r.Path "404 (route not registered)"
            $exitCode = 1
        } else {
            Record "PASS" $r.Method $r.Path "$status"
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
} finally {
    # Always stop the temp runner so a probe failure doesn't leak it.
    Write-Host ""
    Write-Host "Stopping runner $runnerId ..."
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

# ---------------------------------------------------------------------------
# Summary.
# ---------------------------------------------------------------------------
$pass = ($results | Where-Object { $_.Status -eq "PASS" }).Count
$fail = ($results | Where-Object { $_.Status -eq "FAIL" }).Count
$skip = ($results | Where-Object { $_.Status -eq "SKIP" }).Count
$total = $pass + $fail
Write-Host ""
Write-Host ("Smoke: {0} pass / {1} total, {2} skip" -f $pass, $total, $skip)
if ($fail -gt 0) {
    Write-Host ("FAIL count: {0}" -f $fail) -ForegroundColor Red
}

exit $exitCode
