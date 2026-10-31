#!/usr/bin/env pwsh
# parity-diff.ps1 -- DEFINITIONS ONLY. Dot-source it; it runs no top-level code.
#
# The comparator core of plan 2026-08-31-published-build-parity-check, Phase 5:
# given two capability manifests (a DEVELOPMENT build's and a PUBLISHED build's)
# it classifies every capability row and produces the numbers the report and the
# workflow emit. Extracted from scripts/published-parity.ps1 so the classifier
# can be unit-tested with NO binary, NO install and NO network -- the tests are
# scripts/tests/test-parity-diff.ps1. They run as the FIRST step of
# .github/workflows/published-parity.yml, before any compile, and that step is
# deliberately NOT continue-on-error: a broken classifier is a harness defect,
# not a parity verdict, and is the one thing in that workflow allowed to go red.
#
# =============================================================================
# WHY THREE NUMBERS AND NOT ONE
# =============================================================================
#
# `success_metric/published-runner-parity-defects` asks for a single integer:
# "distinct capabilities that work in the development runner and not in the
# published runner". A naive implementation diffs the two row sets and prints
# how many rungs differ. That implementation is WRONG, and dangerously so.
#
# Measured 2026-09-02 on the dev build's cold CLI door
# (`--capability-manifest --json`):
#
#     workspace_root           operator_checkout
#     bundled_resources        unknown
#     spec_pages               unknown
#     fleet_commands           unknown
#     fleet_skills             unknown
#     fleet_agents             unknown
#     agent_definitions        unknown
#     agent_commands_registry  unknown
#     agent_skills_registry    unknown
#     slash_commands           unknown
#
# ALL BUT ONE row is `unknown`. Not "resolved by a low rung" -- unknown, the
# manifest's word for "nothing observed this capability here". The seven
# provisioning/registry rows fill from the Phase 3 session ledger, which only
# fills at SESSION SPAWN; a cold flag invocation has spawned nothing.
# `bundled_resources` needs a Tauri `AppHandle` that does not exist pre-GUI.
#
# So on a cold-CLI-vs-cold-CLI comparison, nine rows read `unknown` on BOTH
# sides. A naive row diff finds them equal, reports ZERO differences, and emits
# parity count 0 -- certifying parity while having observed essentially nothing.
# That false green is strictly worse than the blindness this plan exists to end.
#
# `unknown == unknown` is NOT agreement. It is the absence of two readings, and
# the fleet's own rule says so: `verification-and-evidence`
# `unknown-must-not-render-as-a-default` and `silent-empty-is-unknown`. The
# manifest's own doc says it in the binary's words: "`unresolved` is a finding
# about the machine ... `unknown` is a finding about the reporting binary".
#
# Hence three numbers, always emitted together:
#
#   parity_defects       rows where BOTH sides were genuinely observed and the
#                        answer differs, plus rows the dev build observed that
#                        the published build's roster does not carry at all.
#                        This is the metric's integer.
#   unobserved           rows where either side is `unknown`, so NO comparison
#                        was possible. Not agreement, not disagreement.
#   expected_differences rows on the documented debug-only allowlist below.
#
# and a fourth for the denominator, `comparable`, so a reader can never read
# "0 defects" without seeing how many rows that 0 was drawn from. "0 defects out
# of 1 comparable row" must never render as "in parity", and the formatter below
# refuses to let it: see Format-ParityVerdictLine.
#
# =============================================================================
# THE SCHEMA GATE
# =============================================================================
#
# `schema_version` exists precisely because this tool diffs two builds and will
# eventually diff two manifest FORMATS. When the two manifests disagree on it,
# this comparator REFUSES to diff the rows and says so. A cross-format row diff
# is meaningless: a rung renamed on the wire between versions would read as a
# parity defect on every row that carries it, and a row whose semantics changed
# would compare equal while meaning something else.
#
# In-repo precedent for the discipline: `agent_commands/mod.rs`'s `CACHE_VERSION`
# -- a cache written by a different version is IGNORED rather than parsed on a
# guess. Same rule, same reason.
#
# The refusal is a stated outcome, not an error: the run still exits 0 and still
# reports build identity, the two schema versions, and both row counts. What it
# does not do is invent a defect count. `parity_defects` is reported as $null
# (rendered "n/a"), never 0 -- 0 is a claim, and no claim was earned.
#
# =============================================================================
# THE ALLOWLIST -- WHAT IS ON IT, AND WHY
# =============================================================================
#
# Some differences between a debug build and a release build are DESIGNED, not
# defects. Two classes qualify, and only two:
#
# (1) CFG-GATED. Real and named in this repo:
#
#   mcp/test_fixtures::routes()  #[cfg(any(debug_assertions, feature = "test-fixtures"))]
#   mcp/debug_wedge::routes()    #[cfg(debug_assertions)]        (/__debug/wedge-ui-thread)
#
# Both are compiled OUT of the published build by construction. A capability
# resolved by such a module would legitimately answer differently on the two
# legs, and counting it as a parity defect would put permanent noise into the
# metric. Measured 2026-09-02, NONE of the CAPABILITY_SPECS rows is resolved by
# a cfg-gated module:
#
#   workspace_root  bundled_resources      spec_pages  fleet_commands  fleet_skills
#   fleet_agents    agent_definitions      agent_commands_registry
#   agent_skills_registry                  slash_commands  session_cli
#
# and neither debug-only surface above appears in `UI_BRIDGE_ROUTES` either, so
# the behavioural axis does not see them as a route delta.
#
# (2) PLACEMENT-EXCLUSIVE BY CONSTRUCTION: a row whose resolver decides the
# rung from WHERE THE EXE RUNS, so that each build can answer only its own
# rung and both rungs are working deliveries. Exactly one row, added
# 2026-09-27 with it (plan
# 2026-09-27-qontinui-pr-zero-byte-sidecar-placeholder-published-as-session-cli,
# Phase 1d):
#
#   session_cli  dev 'exe_relative_checkout' -> published 'bundle_resource'
#
# `shim_materializer::session_cli_placement` reads `exe_relative_checkout` iff
# the exe runs inside a cargo profile dir (`deps/` + `.fingerprint/`) OR is a
# debug build -- the dev leg is both -- and `bundle_resource` for a release
# build outside any target dir, which is exactly the installed published leg.
# So a working CLI on BOTH legs necessarily differs, and the metric -- "works
# in dev and NOT in published" -- must not count it. The entry
# is rung-pinned: a published `unresolved` (the sidecar missing or REFUSED as
# a 0-byte placeholder) is still a defect, which is the finding the row exists
# for. bundled_resources is deliberately NOT in this class: its bundle rung can
# answer on a dev box too, so a dev_checkout reading there is a real difference.
#
# An empty allowlist is reported, not assumed: Format-ParityReportText always
# prints the "Expected differences" section, prints every entry with its reason,
# and prints "(the allowlist is empty)" when it has none. An allowlist that
# silently swallowed rows would be as dishonest as a missing one.
#
# WHAT MUST NEVER GO ON IT: `workspace_root` reading `operator_checkout` on a dev
# box and `unresolved` on a clean runner. That is the plan's central TRUE
# POSITIVE -- "a row resolving via DevCheckout/OperatorCheckout on a dev box and
# Unresolved on a published install IS a parity defect" -- and allowlisting it
# would delete the finding this instrument was built for.

# ---------------------------------------------------------------------------
# The one rung that means "no reading was taken".
#
# `unresolved` is deliberately NOT in this set: it is a finding ABOUT THE
# MACHINE (every rung was tried, none answered) and comparing it is exactly the
# comparison the plan wants. `unknown` is a finding about the reporting BINARY.
# ---------------------------------------------------------------------------
$script:ParityUnobservedRungs = @('unknown')

# ---------------------------------------------------------------------------
# The debug-only allowlist. Each entry:
#   Id            capability id it applies to (required)
#   DevRung       the dev-side rung it excuses, or '*' for any
#   PublishedRung the published-side rung it excuses, or '*' for any
#   Reason        why this difference is DESIGNED (required; printed every run)
#
# Rung-pinning is deliberate: an entry excuses ONE specific designed difference,
# not every future difference on that row. A blanket per-id allowlist would hide
# a real regression behind a legitimate one.
# ---------------------------------------------------------------------------
$script:ParityExpectedDifferences = @(
    [PSCustomObject]@{
        Id            = 'session_cli'
        DevRung       = 'exe_relative_checkout'
        PublishedRung = 'bundle_resource'
        Reason        = ("placement-exclusive by construction: a dev build runs from a cargo " +
                         "profile dir and delivers the qontinui-pr it built there, an installed " +
                         "build delivers its bundle.externalBin sidecar -- both are working " +
                         "deliveries. A published 'unresolved' is NOT excused.")
    }
)

function Get-ParityRowRung {
    param($Row)
    if ($null -eq $Row) { return $null }
    if (-not ($Row.PSObject.Properties.Name -contains 'rung')) { return $null }
    return [string]$Row.rung
}

function Test-ParityRungObserved {
    param([string]$Rung)
    if ([string]::IsNullOrWhiteSpace($Rung)) { return $false }
    return (-not ($script:ParityUnobservedRungs -contains $Rung))
}

function Get-ParityAllowlistMatch {
    param([string]$Id, [string]$DevRung, [string]$PublishedRung, $Allowlist)
    if ($null -eq $Allowlist) { return $null }
    foreach ($e in @($Allowlist)) {
        if ($e.Id -ne $Id) { continue }
        $devOk = ($e.DevRung -eq '*') -or ($e.DevRung -eq $DevRung)
        $pubOk = ($e.PublishedRung -eq '*') -or ($e.PublishedRung -eq $PublishedRung)
        if ($devOk -and $pubOk) { return $e }
    }
    return $null
}

# ---------------------------------------------------------------------------
# Compare two parsed manifests.
#
# -Dev / -Published are the objects `ConvertFrom-Json` produces from a manifest
# emitted by `--capability-manifest --json` or `GET /capability-manifest`.
#
# Returns one result object. Every row lands in EXACTLY ONE disposition, so the
# buckets partition the row-id union and no row is counted twice:
#
#   match                            same rung, both sides observed
#   defect                           rungs differ, both observed, not allowlisted
#   expected_difference              rungs differ, both observed, allowlisted
#   unobserved                       either side `unknown`; no comparison possible
#   only_in_dev                      id in dev's roster, absent from published's,
#                                    and the DEV side was observed
#   only_in_dev_unobserved           same, but dev never observed it either --
#                                    a roster difference with no reading behind it
#   only_in_published                id in published's roster, absent from dev's
#
# parity_defects = (defect) + (only_in_dev). The sum is printed as that
# expression, never as a bare number, so the two contributing classes stay
# visible. `only_in_published` is a roster finding but NOT a parity defect: the
# metric counts capabilities that work in dev and not in the published build,
# and a capability the published build has and dev lacks is the other direction.
# ---------------------------------------------------------------------------
function Compare-CapabilityManifests {
    param(
        [Parameter(Mandatory = $true)] $Dev,
        [Parameter(Mandatory = $true)] $Published,
        $Allowlist = $script:ParityExpectedDifferences,
        [string]$DevDoor = 'unknown',
        [string]$PublishedDoor = 'unknown'
    )

    $devSchema = $null
    if ($Dev.PSObject.Properties.Name -contains 'schema_version') { $devSchema = $Dev.schema_version }
    $pubSchema = $null
    if ($Published.PSObject.Properties.Name -contains 'schema_version') { $pubSchema = $Published.schema_version }

    $devRows = @()
    if ($Dev.PSObject.Properties.Name -contains 'rows' -and $null -ne $Dev.rows) { $devRows = @($Dev.rows) }
    $pubRows = @()
    if ($Published.PSObject.Properties.Name -contains 'rows' -and $null -ne $Published.rows) { $pubRows = @($Published.rows) }

    $identity = [PSCustomObject]@{
        DevSchemaVersion       = $devSchema
        PublishedSchemaVersion = $pubSchema
        DevGitSha              = (Get-ParityField $Dev 'git_sha')
        PublishedGitSha        = (Get-ParityField $Published 'git_sha')
        DevBuildId             = (Get-ParityField $Dev 'build_id')
        PublishedBuildId       = (Get-ParityField $Published 'build_id')
        DevAppVersion          = (Get-ParityField $Dev 'app_version')
        PublishedAppVersion    = (Get-ParityField $Published 'app_version')
        DevDoor                = $DevDoor
        PublishedDoor          = $PublishedDoor
        DevRowCount            = @($devRows).Count
        PublishedRowCount      = @($pubRows).Count
    }

    # --- the schema gate. Refuse to diff rows across formats. ---------------
    if (($null -eq $devSchema) -or ($null -eq $pubSchema) -or ([string]$devSchema -ne [string]$pubSchema)) {
        $why = if (($null -eq $devSchema) -or ($null -eq $pubSchema)) {
            "one or both manifests carry no schema_version at all"
        } else {
            "dev reports schema_version $devSchema, published reports $pubSchema"
        }
        return [PSCustomObject]@{
            SchemaRefusal        = $true
            SchemaRefusalReason  = $why
            Identity             = $identity
            Rows                 = @()
            Allowlist            = @($Allowlist)
            # Every count is $null, never 0: 0 is a claim, and a refused run
            # earned none. Every field the non-refusal branch emits is present
            # here so a consumer never has to distinguish "absent" from "null".
            ParityDefectCount        = $null
            RungDifferCount          = $null
            OnlyInDevCount           = $null
            OnlyInDevUnobservedCount = $null
            OnlyInPublishedCount     = $null
            UnobservedCount          = $null
            ExpectedDiffCount        = $null
            MatchCount               = $null
            ComparableCount          = $null
        }
    }

    $devMap = @{}
    foreach ($r in $devRows) { if ($null -ne $r -and $r.id) { $devMap[[string]$r.id] = $r } }
    $pubMap = @{}
    foreach ($r in $pubRows) { if ($null -ne $r -and $r.id) { $pubMap[[string]$r.id] = $r } }

    # Union in dev-roster order first (that is CAPABILITY_SPECS order, which the
    # binary guarantees), then any published-only ids appended.
    $ids = New-Object System.Collections.Generic.List[string]
    foreach ($r in $devRows) { if ($null -ne $r -and $r.id -and -not $ids.Contains([string]$r.id)) { $ids.Add([string]$r.id) } }
    foreach ($r in $pubRows) { if ($null -ne $r -and $r.id -and -not $ids.Contains([string]$r.id)) { $ids.Add([string]$r.id) } }

    $out = [System.Collections.Generic.List[Object]]::new()
    foreach ($id in $ids) {
        $devRow = $null
        if ($devMap.ContainsKey($id)) { $devRow = $devMap[$id] }
        $pubRow = $null
        if ($pubMap.ContainsKey($id)) { $pubRow = $pubMap[$id] }

        $devRung = Get-ParityRowRung $devRow
        $pubRung = Get-ParityRowRung $pubRow
        $devObserved = Test-ParityRungObserved $devRung
        $pubObserved = Test-ParityRungObserved $pubRung

        $disposition = $null
        $allowEntry = $null
        $note = $null

        if ($null -eq $pubRow) {
            if ($devObserved) {
                $disposition = 'only_in_dev'
                $note = "the published build's roster carries no '$id' row at all, and the dev build observed it on rung '$devRung'"
            } else {
                $disposition = 'only_in_dev_unobserved'
                $note = "the published build's roster carries no '$id' row, and the dev build did not observe it either -- a roster difference with no reading behind it"
            }
        } elseif ($null -eq $devRow) {
            $disposition = 'only_in_published'
            $note = "the dev build's roster carries no '$id' row; the published build reports rung '$pubRung'"
        } elseif ((-not $devObserved) -or (-not $pubObserved)) {
            $disposition = 'unobserved'
            $sides = @()
            if (-not $devObserved) { $sides += 'dev' }
            if (-not $pubObserved) { $sides += 'published' }
            $note = "no comparison possible: dev rung '$devRung', published rung '$pubRung' -- unobserved on the $($sides -join ' and ') side. 'unknown' is the absence of a reading, never agreement."
        } elseif ($devRung -eq $pubRung) {
            $disposition = 'match'
        } else {
            $allowEntry = Get-ParityAllowlistMatch -Id $id -DevRung $devRung -PublishedRung $pubRung -Allowlist $Allowlist
            if ($null -ne $allowEntry) {
                $disposition = 'expected_difference'
                $note = $allowEntry.Reason
            } else {
                $disposition = 'defect'
                $note = "resolved '$devRung' in the development build and '$pubRung' in the published build"
            }
        }

        $out.Add([PSCustomObject]@{
            Id                = $id
            Disposition       = $disposition
            DevRung           = $devRung
            PublishedRung     = $pubRung
            DevObserved       = $devObserved
            PublishedObserved = $pubObserved
            DevPath           = (Get-ParityField $devRow 'resolved_path')
            PublishedPath     = (Get-ParityField $pubRow 'resolved_path')
            DevDetail         = (Get-ParityField $devRow 'detail')
            PublishedDetail   = (Get-ParityField $pubRow 'detail')
            DevNote           = (Get-ParityField $devRow 'note')
            PublishedNote     = (Get-ParityField $pubRow 'note')
            Note              = $note
            AllowlistReason   = $(if ($null -ne $allowEntry) { $allowEntry.Reason } else { $null })
        })
    }

    # @() around every filter: PS 5.1's scalar `Count` adapter does NOT cover
    # PSCustomObject, so a Where-Object matching exactly ONE row yields $null and
    # the tally silently reads blank. Same defect that produced the
    # "193 pass / 193 total, fail=" line -- see lib/smoke-summary.ps1.
    $rungDiffer   = @($out | Where-Object { $_.Disposition -eq 'defect' }).Count
    $onlyDev      = @($out | Where-Object { $_.Disposition -eq 'only_in_dev' }).Count
    $onlyDevUnobs = @($out | Where-Object { $_.Disposition -eq 'only_in_dev_unobserved' }).Count
    $onlyPub      = @($out | Where-Object { $_.Disposition -eq 'only_in_published' }).Count
    $unobs        = @($out | Where-Object { $_.Disposition -eq 'unobserved' }).Count
    $expected     = @($out | Where-Object { $_.Disposition -eq 'expected_difference' }).Count
    $match        = @($out | Where-Object { $_.Disposition -eq 'match' }).Count

    return [PSCustomObject]@{
        SchemaRefusal            = $false
        SchemaRefusalReason      = $null
        Identity                 = $identity
        Rows                     = $out.ToArray()
        Allowlist                = @($Allowlist)
        # The metric's integer, as an explicit sum of its two contributing classes.
        ParityDefectCount        = ($rungDiffer + $onlyDev)
        RungDifferCount          = $rungDiffer
        OnlyInDevCount           = $onlyDev
        OnlyInDevUnobservedCount = $onlyDevUnobs
        OnlyInPublishedCount     = $onlyPub
        # Rows where no comparison was possible at all.
        UnobservedCount          = ($unobs + $onlyDevUnobs)
        ExpectedDiffCount        = $expected
        MatchCount               = $match
        # The denominator the defect count must always be read against.
        ComparableCount          = ($match + $rungDiffer + $expected)
    }
}

function Get-ParityField {
    param($Obj, [string]$Name)
    if ($null -eq $Obj) { return $null }
    if (-not ($Obj.PSObject.Properties.Name -contains $Name)) { return $null }
    return $Obj.$Name
}

# ---------------------------------------------------------------------------
# The one line a human reads. It must never let "0 defects" stand alone.
# ---------------------------------------------------------------------------
function Format-ParityVerdictLine {
    param($Result)

    if ($Result.SchemaRefusal) {
        return "PARITY-REFUSED schema_mismatch parity_defects=n/a ($($Result.SchemaRefusalReason))"
    }

    $line = "PARITY-COMPLETE parity_defects={0} (rung_differs={1} + only_in_dev={2}) comparable={3} unobserved={4} expected_differences={5} match={6} only_in_published={7}" -f `
        $Result.ParityDefectCount, $Result.RungDifferCount, $Result.OnlyInDevCount, `
        $Result.ComparableCount, $Result.UnobservedCount, $Result.ExpectedDiffCount, `
        $Result.MatchCount, $Result.OnlyInPublishedCount

    if ($Result.ComparableCount -eq 0) {
        $line += " -- NOTHING WAS COMPARED: 0 comparable rows. This is NOT a statement of parity."
    } elseif ($Result.UnobservedCount -ge $Result.ComparableCount) {
        $line += " -- THIN OBSERVATION: at least as many rows were unobserved as were compared. Read parity_defects as a floor, not a count."
    }
    return $line
}

# ---------------------------------------------------------------------------
# The full human report.
# ---------------------------------------------------------------------------
function Format-ParityReportText {
    param($Result)

    $L = New-Object System.Collections.Generic.List[string]
    $id = $Result.Identity

    $L.Add("=== Published-build capability parity =========================================")
    $L.Add("")
    $L.Add(("  development build : {0}  git_sha={1}  build_id={2}  (door: {3})" -f $id.DevAppVersion, $id.DevGitSha, $id.DevBuildId, $id.DevDoor))
    $L.Add(("  published build   : {0}  git_sha={1}  build_id={2}  (door: {3})" -f $id.PublishedAppVersion, $id.PublishedGitSha, $id.PublishedBuildId, $id.PublishedDoor))
    $L.Add(("  schema_version    : dev={0}  published={1}" -f $id.DevSchemaVersion, $id.PublishedSchemaVersion))
    $L.Add(("  rows              : dev={0}  published={1}" -f $id.DevRowCount, $id.PublishedRowCount))
    $L.Add("")

    if ($Result.SchemaRefusal) {
        $L.Add("REFUSED: the two manifests do not share a schema version --")
        $L.Add("  $($Result.SchemaRefusalReason)")
        $L.Add("")
        $L.Add("A row diff across two manifest FORMATS is meaningless: a rung renamed on the")
        $L.Add("wire would read as a defect on every row carrying it, and a row whose meaning")
        $L.Add("changed would compare equal while meaning something else. So no defect count")
        $L.Add("is reported -- not 0, which would be a claim this run did not earn.")
        $L.Add("")
        $L.Add("Rebuild both legs from manifests that share a schema_version, then re-run.")
        $L.Add("")
        $L.Add((Format-ParityVerdictLine $Result))
        return ($L -join [Environment]::NewLine)
    }

    # --- 1. Parity defects ---------------------------------------------------
    $L.Add("-- Parity defects ({0}) --------------------------------------------------------" -f $Result.ParityDefectCount)
    $L.Add("   Capabilities the development build resolved and the published build did not")
    $L.Add("   resolve the same way. This is the metric's integer.")
    $L.Add("")
    $defects = @($Result.Rows | Where-Object { $_.Disposition -eq 'defect' -or $_.Disposition -eq 'only_in_dev' })
    if ($defects.Count -eq 0) {
        $L.Add("   (none)")
    } else {
        foreach ($r in $defects) {
            $L.Add("   * {0}" -f $r.Id)
            $L.Add(("       dev       : {0}{1}" -f $r.DevRung, $(if ($r.DevPath) { "  <- $($r.DevPath)" } else { "" })))
            $L.Add(("       published : {0}{1}" -f $(if ($null -eq $r.PublishedRung) { "<row absent>" } else { $r.PublishedRung }), $(if ($r.PublishedPath) { "  <- $($r.PublishedPath)" } else { "" })))
            $L.Add("       {0}" -f $r.Note)
        }
    }
    $L.Add("")

    # --- 2. Expected differences (the allowlist) -----------------------------
    $L.Add("-- Expected differences ({0}) --------------------------------------------------" -f $Result.ExpectedDiffCount)
    $L.Add("   Designed debug-vs-release differences. NOT counted as defects. The allowlist")
    $L.Add("   is printed in full every run: an allowlist that silently swallowed rows would")
    $L.Add("   be as dishonest as a missing one.")
    $L.Add("")
    if (@($Result.Allowlist).Count -eq 0) {
        $L.Add("   allowlist: (empty) -- nothing is excused. The classes it exists for are")
        $L.Add("              real (cfg-gated modules such as mcp/test_fixtures and")
        $L.Add("              mcp/debug_wedge, compiled out of a release build; and rows whose")
        $L.Add("              rung is placement-exclusive by construction).")
    } else {
        foreach ($e in @($Result.Allowlist)) {
            $L.Add(("   allowlist: {0}  dev='{1}' published='{2}'" -f $e.Id, $e.DevRung, $e.PublishedRung))
            $L.Add("              {0}" -f $e.Reason)
        }
    }
    $matched = @($Result.Rows | Where-Object { $_.Disposition -eq 'expected_difference' })
    $L.Add("")
    if ($matched.Count -eq 0) {
        $L.Add("   matched this run: (none)")
    } else {
        foreach ($r in $matched) {
            $L.Add(("   * {0}: dev={1} published={2}" -f $r.Id, $r.DevRung, $r.PublishedRung))
            $L.Add("       {0}" -f $r.AllowlistReason)
        }
    }
    $L.Add("")

    # --- 3. Unobserved -------------------------------------------------------
    $L.Add("-- Unobserved ({0}) ------------------------------------------------------------" -f $Result.UnobservedCount)
    $L.Add("   Rows where at least one side reported 'unknown', so NO comparison was")
    $L.Add("   possible. These are neither agreement nor disagreement: 'unknown' is a")
    $L.Add("   finding about the reporting binary, never about the machine. They are")
    $L.Add("   excluded from the defect count AND from the comparable denominator.")
    $L.Add("")
    $unobs = @($Result.Rows | Where-Object { $_.Disposition -eq 'unobserved' -or $_.Disposition -eq 'only_in_dev_unobserved' })
    if ($unobs.Count -eq 0) {
        $L.Add("   (none)")
    } else {
        foreach ($r in $unobs) {
            $L.Add(("   * {0}: dev={1} published={2}" -f $r.Id, $r.DevRung, $r.PublishedRung))
        }
    }
    $L.Add("")

    # --- 4. Roster differences the other way ---------------------------------
    if ($Result.OnlyInPublishedCount -gt 0) {
        $L.Add("-- Only in the published roster ({0}) ------------------------------------------" -f $Result.OnlyInPublishedCount)
        $L.Add("   Capabilities the published build enumerates and the development build does")
        $L.Add("   not. A roster finding, NOT a parity defect: the metric counts capabilities")
        $L.Add("   that work in dev and not in the published build, and this is the other")
        $L.Add("   direction. Usually means the two legs are different commits.")
        $L.Add("")
        foreach ($r in @($Result.Rows | Where-Object { $_.Disposition -eq 'only_in_published' })) {
            $L.Add(("   * {0}: published={1}" -f $r.Id, $r.PublishedRung))
        }
        $L.Add("")
    }

    # --- 5. Matched ----------------------------------------------------------
    $L.Add("-- In parity ({0}) -------------------------------------------------------------" -f $Result.MatchCount)
    $matchedRows = @($Result.Rows | Where-Object { $_.Disposition -eq 'match' })
    if ($matchedRows.Count -eq 0) {
        $L.Add("   (none)")
    } else {
        foreach ($r in $matchedRows) { $L.Add(("   * {0}: {1}" -f $r.Id, $r.DevRung)) }
    }
    $L.Add("")
    $L.Add((Format-ParityVerdictLine $Result))

    return ($L -join [Environment]::NewLine)
}

# ---------------------------------------------------------------------------
# The machine-readable artifact. Snake_case keys, mirroring the manifest's own
# wire style, so a future coord parity label can consume it without a translator.
#
# -Provenance is carried through UNTOUCHED -- the same object, not a copy and
# not a re-derivation. This function classifies nothing about it: in
# particular it never reads, rounds, defaults or recomputes `skew_commits`. The
# count states what the comparison was computed ACROSS, and the comparator has
# no business changing that (see "PROVENANCE" below).
# ---------------------------------------------------------------------------
function ConvertTo-ParityReportObject {
    param($Result, [string]$GeneratedAt, $Observability, $Provenance = $null)

    $rows = @()
    foreach ($r in @($Result.Rows)) {
        $rows += [PSCustomObject]@{
            id                 = $r.Id
            disposition        = $r.Disposition
            dev_rung           = $r.DevRung
            published_rung     = $r.PublishedRung
            dev_observed       = $r.DevObserved
            published_observed = $r.PublishedObserved
            dev_resolved_path  = $r.DevPath
            published_resolved_path = $r.PublishedPath
            dev_detail         = $r.DevDetail
            published_detail   = $r.PublishedDetail
            note               = $r.Note
        }
    }

    $allow = @()
    foreach ($e in @($Result.Allowlist)) {
        $allow += [PSCustomObject]@{
            id = $e.Id; dev_rung = $e.DevRung; published_rung = $e.PublishedRung; reason = $e.Reason
        }
    }

    return [PSCustomObject]@{
        report_kind  = 'published-build-capability-parity'
        # 2: adds the top-level `provenance` block (plan
        # 2026-09-20-published-runner-parity-count-comes-from-a-run-not-from-reports,
        # Phase 2). A consumer keyed on 1 must not read a 2 as the same shape.
        report_version = $script:ParityReportVersion
        generated_at = $GeneratedAt
        provenance   = $Provenance
        schema_refused = $Result.SchemaRefusal
        schema_refusal_reason = $Result.SchemaRefusalReason
        build_identity = [PSCustomObject]@{
            dev = [PSCustomObject]@{
                app_version = $Result.Identity.DevAppVersion
                git_sha     = $Result.Identity.DevGitSha
                build_id    = $Result.Identity.DevBuildId
                schema_version = $Result.Identity.DevSchemaVersion
                door        = $Result.Identity.DevDoor
                row_count   = $Result.Identity.DevRowCount
            }
            published = [PSCustomObject]@{
                app_version = $Result.Identity.PublishedAppVersion
                git_sha     = $Result.Identity.PublishedGitSha
                build_id    = $Result.Identity.PublishedBuildId
                schema_version = $Result.Identity.PublishedSchemaVersion
                door        = $Result.Identity.PublishedDoor
                row_count   = $Result.Identity.PublishedRowCount
            }
        }
        counts = [PSCustomObject]@{
            # The metric's integer. Null (not 0) when the schema gate refused.
            parity_defects         = $Result.ParityDefectCount
            rung_differs           = $Result.RungDifferCount
            only_in_dev            = $Result.OnlyInDevCount
            only_in_dev_unobserved = $Result.OnlyInDevUnobservedCount
            only_in_published      = $Result.OnlyInPublishedCount
            unobserved             = $Result.UnobservedCount
            expected_differences   = $Result.ExpectedDiffCount
            match                  = $Result.MatchCount
            comparable             = $Result.ComparableCount
        }
        observability = $Observability
        allowlist = $allow
        rows = $rows
    }
}

# ===========================================================================
# PROVENANCE (plan
# 2026-09-20-published-runner-parity-count-comes-from-a-run-not-from-reports,
# Phase 2)
# ===========================================================================
#
# A parity count is a statement about TWO artifacts, and before this block the
# artifact never said which two. The nightly compares "whatever main is tonight"
# against the latest release, so every row that differs is confounded with
# version skew -- measured at 736 commits on 2026-09-22 -- and nothing in the
# report said so. The provenance block states, beside the numbers:
#
#   dev_sha / published_tag / published_sha   what was compared
#   skew_commits      `git rev-list --count <tag>..HEAD`. 0 is a same-SHA
#                     reading: one commit, two build shapes. When it cannot be
#                     computed (no tag in the checkout, a shallow clone) it is
#                     `unknown(<reason>)` -- NEVER 0, which would claim a
#                     same-SHA reading nobody took.
#   run_id / run_event / generated_at          which run produced it
#   axes.manifest / axes.behavioural           observed | unknown(<reason>)
#   siblings          the qontinui-schemas / ui-bridge commits this dev leg
#                     compiled against. They are checked out by
#                     .github/actions/checkout-sibling (declaration, then pin,
#                     then default branch) and NOT at the release tag, so they
#                     are recorded as SHAs and never claimed to be same-SHA.
#
# Unknown values are the string `unknown(<reason>)` rather than null, matching
# the axes vocabulary, so a reader of the raw JSON sees WHY next to the gap.
# ---------------------------------------------------------------------------
$script:ParityReportVersion = 2

# The summary's FIRST line says the rows were unobserved, in capitals, at or
# above this many. Seven is the session-ledger row count: every row filled only
# by provisioning. At seven or more unobserved the run has, at best, read the
# boot-time rows and nothing a session would see -- the headline must say that
# before any number does.
$script:ParityUnobservedHeadlineThreshold = 7

function Format-ParityUnknown {
    param([string]$Reason)
    return "unknown($Reason)"
}

function Test-ParityAxisValue {
    param($Value)
    if ($null -eq $Value) { return $false }
    return ([string]$Value -match '^(observed|unknown\(.+\))$')
}

# One git call, never throwing. $ErrorActionPreference is 'Stop' in every
# caller, and Windows PowerShell 5.1 turns a native command's stderr into a
# terminating NativeCommandError under it -- so a missing tag would escape as a
# crash instead of landing as unknown(tag_not_in_checkout). Local 'Continue'
# plus the exit code is the honest reading.
function Invoke-ParityGit {
    param([string]$RepoDir, [string[]]$GitArgs)
    $ErrorActionPreference = 'Continue'
    $out = $null
    $code = $null
    try {
        $out = & git -C $RepoDir @GitArgs 2>$null
        $code = $LASTEXITCODE
    } catch {
        return [PSCustomObject]@{ Ok = $false; Out = $null; Why = 'git_unavailable'; ExitCode = $null }
    }
    if ($code -ne 0) {
        return [PSCustomObject]@{ Ok = $false; Out = $null; Why = "git_exit_$code"; ExitCode = $code }
    }
    return [PSCustomObject]@{ Ok = $true; Out = (($out | Out-String).Trim()); Why = $null; ExitCode = 0 }
}

# dev_sha, published_sha and skew_commits from the dev leg's own checkout.
#
# Shallow is the trap. actions/checkout defaults to fetch-depth 1, where
# `rev-list --count <tag>..HEAD` either fails (the tag is not fetched) or
# counts only the commits that happen to be present -- a SMALLER number than
# the truth, with exit 0. So a shallow checkout yields unknown(shallow_clone),
# with one exception that is exact at any depth: HEAD IS the tag commit, which
# is the same-SHA leg itself (a dispatch at the tag ref), and the count is 0.
#
# Ancestry is the second trap. `rev-list --count <tag>..HEAD` counts commits
# reachable from HEAD and not from the tag, which is a skew only when the tag is
# an ANCESTOR of HEAD. HEAD behind the tag (detached at v1.0.11, -Tag v1.0.12)
# counts 0 while the SHAs differ -- a fabricated same-SHA reading -- and two
# diverged histories undercount. So the count is taken only after
# `merge-base --is-ancestor <tag> HEAD` exits 0; exit 1 is
# unknown(tag_not_ancestor_of_head), with the two one-sided counts recorded
# beside it in `divergence` (never folded into skew_commits); any other exit is
# unknown too.
function Get-ParitySkewProvenance {
    param([string]$RepoDir, [string]$Tag)

    $out = [PSCustomObject]@{
        dev_sha       = $null
        published_sha = $null
        skew_commits  = $null
        # Set only when the two histories are not linear: the one-sided counts
        # from `rev-list --left-right --count`, for a reader. Never a skew.
        divergence    = $null
    }

    $head = Invoke-ParityGit -RepoDir $RepoDir -GitArgs @('rev-parse', '--verify', 'HEAD^{commit}')
    if ($head.Ok -and $head.Out -match '^[0-9a-f]{40}$') {
        $out.dev_sha = $head.Out
    } else {
        $why = $(if ($head.Why) { $head.Why } else { 'unparseable_rev_parse' })
        $out.dev_sha = Format-ParityUnknown "dev_checkout_unreadable: $why"
    }

    if ([string]::IsNullOrWhiteSpace($Tag)) {
        $out.published_sha = Format-ParityUnknown 'no_published_tag'
        $out.skew_commits  = Format-ParityUnknown 'no_published_tag'
        return $out
    }

    $tagRev = Invoke-ParityGit -RepoDir $RepoDir -GitArgs @('rev-parse', '--verify', '--quiet', "refs/tags/$Tag^{commit}")
    if ($tagRev.Ok -and $tagRev.Out -match '^[0-9a-f]{40}$') {
        $out.published_sha = $tagRev.Out
    } else {
        $out.published_sha = Format-ParityUnknown "tag_not_in_checkout: $Tag"
        $out.skew_commits  = Format-ParityUnknown "tag_not_in_checkout: $Tag"
        return $out
    }

    if (-not ($out.dev_sha -match '^[0-9a-f]{40}$')) {
        $out.skew_commits = Format-ParityUnknown 'dev_sha_unknown'
        return $out
    }

    # Exact at any depth: no commit lies between a commit and itself.
    if ($out.dev_sha -eq $out.published_sha) {
        $out.skew_commits = 0
        return $out
    }

    $shallow = Invoke-ParityGit -RepoDir $RepoDir -GitArgs @('rev-parse', '--is-shallow-repository')
    if (-not $shallow.Ok -or $shallow.Out -ne 'false') {
        # 'true', or a git too old to answer (it echoes the flag back): either
        # way the count below could be a silent undercount.
        $why = $(if ($shallow.Ok -and $shallow.Out -eq 'true') { 'shallow_clone' } else { 'shallow_state_unreadable' })
        $out.skew_commits = Format-ParityUnknown $why
        return $out
    }

    $anc = Invoke-ParityGit -RepoDir $RepoDir -GitArgs @('merge-base', '--is-ancestor', $out.published_sha, $out.dev_sha)
    if ($anc.ExitCode -eq 1) {
        $out.skew_commits = Format-ParityUnknown 'tag_not_ancestor_of_head'
        $lr = Invoke-ParityGit -RepoDir $RepoDir -GitArgs @('rev-list', '--left-right', '--count', "$($out.dev_sha)...$($out.published_sha)")
        if ($lr.Ok -and $lr.Out -match '^([0-9]+)\s+([0-9]+)$') {
            $out.divergence = [PSCustomObject]@{ dev_only_commits = [int]$Matches[1]; published_only_commits = [int]$Matches[2] }
        }
        return $out
    }
    if ($anc.ExitCode -ne 0) {
        $why = $(if ($anc.Why) { $anc.Why } else { 'no_exit_code' })
        $out.skew_commits = Format-ParityUnknown "ancestry_unreadable: $why"
        return $out
    }

    $count = Invoke-ParityGit -RepoDir $RepoDir -GitArgs @('rev-list', '--count', "$($out.published_sha)..$($out.dev_sha)")
    if ($count.Ok -and $count.Out -match '^[0-9]+$') {
        $out.skew_commits = [int]$count.Out
    } else {
        $why = $(if ($count.Why) { $count.Why } else { 'unparseable_count' })
        $out.skew_commits = Format-ParityUnknown "rev_list_failed: $why"
    }
    return $out
}

# The sibling checkouts this dev leg compiled against, as SHAs. Path is the
# checkout-sibling action's default: a sibling of the runner checkout.
function Get-ParitySiblingProvenance {
    param([string]$RepoRoot, [string[]]$Repos = @('qontinui/qontinui-schemas', 'qontinui/ui-bridge'))
    $parent = Split-Path -Parent $RepoRoot
    $rows = @()
    foreach ($repo in $Repos) {
        $name = ($repo -split '/')[-1]
        $dir = Join-Path $parent $name
        $sha = $null
        if (-not (Test-Path -LiteralPath $dir -PathType Container)) {
            $sha = Format-ParityUnknown 'not_checked_out'
        } else {
            $r = Invoke-ParityGit -RepoDir $dir -GitArgs @('rev-parse', '--verify', 'HEAD^{commit}')
            $sha = $(if ($r.Ok -and $r.Out -match '^[0-9a-f]{40}$') { $r.Out } else { Format-ParityUnknown "unreadable: $($r.Why)" })
        }
        $rows += [PSCustomObject]@{ repo = $repo; path = $dir; sha = $sha }
    }
    return @($rows)
}

# What the manifest axis observed. A refusal and an all-unknown comparison
# both read the manifests and still observed nothing comparable, so neither is
# `observed`.
function Get-ParityManifestAxis {
    param($Result)
    if ($null -eq $Result) { return (Format-ParityUnknown 'no_result') }
    if ($Result.SchemaRefusal) { return (Format-ParityUnknown "schema_refused: $($Result.SchemaRefusalReason)") }
    if ($Result.ComparableCount -eq 0) { return (Format-ParityUnknown 'no_comparable_rows') }
    return 'observed'
}

# Assemble the block. A $null skew is "nobody computed it" and becomes
# unknown(not_computed): the one value this constructor refuses to invent is 0.
function New-ParityProvenance {
    param(
        $DevSha, [string]$PublishedTag, $PublishedSha, $SkewCommits,
        [string]$RunId, [string]$RunEvent, [string]$GeneratedAt,
        [string]$ManifestAxis, [string]$BehaviouralAxis, $Siblings = @(), $Divergence = $null,
        [string]$Platform = $null
    )

    $skew = $SkewCommits
    if ($null -eq $skew -or ($skew -is [string] -and [string]::IsNullOrWhiteSpace($skew))) {
        $skew = Format-ParityUnknown 'not_computed'
    } elseif ($skew -is [string] -and -not ($skew -match '^unknown\(.+\)$')) {
        $skew = Format-ParityUnknown "unparseable_skew: $skew"
    }

    $axes = [PSCustomObject]@{
        manifest    = $(if (Test-ParityAxisValue $ManifestAxis) { $ManifestAxis } else { Format-ParityUnknown 'not_recorded' })
        behavioural = $(if (Test-ParityAxisValue $BehaviouralAxis) { $BehaviouralAxis } else { Format-ParityUnknown 'not_recorded' })
    }

    return [PSCustomObject]@{
        dev_sha       = $(if ($DevSha) { $DevSha } else { Format-ParityUnknown 'not_computed' })
        published_tag = $(if ($PublishedTag) { $PublishedTag } else { Format-ParityUnknown 'no_published_tag' })
        published_sha = $(if ($PublishedSha) { $PublishedSha } else { Format-ParityUnknown 'not_computed' })
        skew_commits  = $skew
        divergence    = $Divergence
        run_id        = $(if ($RunId) { $RunId } else { Format-ParityUnknown 'not_a_workflow_run' })
        run_event     = $(if ($RunEvent) { $RunEvent } else { Format-ParityUnknown 'not_a_workflow_run' })
        generated_at  = $GeneratedAt
        # Which platform BOTH legs ran on (Phase 6B). The cross-platform list
        # (-CrossPlatform) refuses a pair whose platforms are not one windows
        # and one linux, so a report that cannot say is unknown, never assumed.
        platform      = $(if ($Platform) { $Platform } else { Format-ParityUnknown 'not_recorded' })
        axes          = $axes
        siblings      = @($Siblings)
        siblings_note = ("Checked out by .github/actions/checkout-sibling (declared PR, else " +
                         ".github/sibling-pins.conf, else the default branch) -- NOT at the release " +
                         "tag. Recorded as the commits this dev leg compiled against; never claimed " +
                         "to be what the published build was compiled against.")
    }
}

# The one summary line a run that could not compare writes (published-parity.ps1's
# exit-2 paths). No row was read on at least one leg, so the line says every row
# is UNOBSERVED -- the same always-state-the-unobserved-count rule as the full
# summary, with the count it can honestly give -- and carries skew_commits
# verbatim. Without it the job summary was silent and the report step printed ''.
function Format-ParityUnavailableSummaryLine {
    param([string]$Reason, $Provenance = $null)
    $skew = $(if ($null -ne $Provenance) { $Provenance.skew_commits } else { Format-ParityUnknown 'no_provenance_block' })
    return ("### Published-build capability parity -- UNAVAILABLE ($Reason): all rows UNOBSERVED, " +
            "no comparison ran (unobserved: all; parity_defects: n/a) -- skew_commits: $skew")
}

# Stamp the behavioural axis into an already-written report. The manifest step
# writes the artifact before the contract-smoke legs run, so it can only say
# unknown(not_yet_measured); the workflow calls this once the behavioural diff
# has an answer. Touches that one field and nothing else -- skew_commits above
# all. A malformed value is recorded as unknown, never passed through.
function Set-ParityBehaviouralAxis {
    param($Report, [string]$Axis)
    if ($null -eq $Report -or $null -eq $Report.provenance -or $null -eq $Report.provenance.axes) { return $false }
    $value = $(if (Test-ParityAxisValue $Axis) { $Axis } else { Format-ParityUnknown "unparseable_axis_value: $Axis" })
    $Report.provenance.axes.behavioural = $value
    return $true
}

# The whole stamp, file to file -- what the workflow's stamp step calls, kept
# here so the parse gate and the unit tests reach it rather than an inline
# script nothing checks. Returns a status word and prints the matching line:
#   stamped        the axis was written
#   no_artifact    the manifest step wrote none (UNKNOWN, not a clean run)
#   no_axes_block  the artifact predates provenance; left untouched
#
# A missing axis file is unknown(behavioural_step_did_not_report): the
# behavioural step writes one on every path that reaches an answer.
#
# ATOMIC: the new JSON goes to <json>.tmp and File.Replace swaps it over the
# original, so a kill mid-write leaves the old artifact whole rather than a
# truncated one for the upload step to ship.
function Update-ParityReportBehaviouralAxis {
    param([string]$JsonPath, [string]$AxisPath)
    if (-not (Test-Path -LiteralPath $JsonPath)) {
        Write-Host "::warning::No parity artifact to stamp -- the manifest step wrote none. Its absence is UNKNOWN, not a clean run."
        return 'no_artifact'
    }
    $axis = Format-ParityUnknown 'behavioural_step_did_not_report'
    if (Test-Path -LiteralPath $AxisPath) { $axis = (Get-Content -LiteralPath $AxisPath -Raw).Trim() }
    $full = (Resolve-Path -LiteralPath $JsonPath).Path
    # -DateKind String where it exists (pwsh 7.5+): without it pwsh would turn the
    # ISO generated_at strings into DateTime and could re-serialize them in
    # another format. Windows PowerShell 5.1 has no such conversion and no such
    # parameter; the Linux leg has only pwsh.
    $fromJson = @{}
    if ((Get-Command ConvertFrom-Json).Parameters.ContainsKey('DateKind')) { $fromJson['DateKind'] = 'String' }
    $report = Get-Content -LiteralPath $full -Raw -Encoding UTF8 | ConvertFrom-Json @fromJson
    if (-not (Set-ParityBehaviouralAxis -Report $report -Axis $axis)) {
        Write-Host "::warning::The parity artifact carries no provenance.axes block (report_version $($report.report_version)); behavioural axis not stamped."
        return 'no_axes_block'
    }
    $tmp = "$full.tmp"
    [System.IO.File]::WriteAllText($tmp, ($report | ConvertTo-Json -Depth 10), (New-Object System.Text.UTF8Encoding($false)))
    # [NullString]::Value, not $null: PowerShell coerces $null to '' for a
    # [string] .NET argument, and Replace rejects '' as a backup path. A true
    # null means "no backup file".
    [System.IO.File]::Replace($tmp, $full, [NullString]::Value)
    Write-Host "provenance.axes.behavioural = $($report.provenance.axes.behavioural)"
    return 'stamped'
}

# The job-summary Markdown, as an array of lines. Lives here, not in
# published-parity.ps1, so scripts/tests/test-parity-diff.ps1 pins what a human
# reads first.
#
# THE RULE THE TESTS PIN: a line that says "parity" while rows went unobserved
# must carry the unobserved count ON THAT LINE. A heading that reads
# "capability parity" above a 0 is read as "in parity" by everyone who stops at
# the heading; on the measured cold door that was 8 of 9 rows never compared.
function Format-ParitySummaryMarkdown {
    param($Result, $Provenance = $null, [string]$SlashCommandsStatus = $null,
          $SelfReportDisagreements = @(), $SessionLedgerRows = @())

    $md = New-Object System.Collections.Generic.List[string]
    # The platform, from the provenance block, so the two legs' summaries are
    # told apart on the first line.
    $plat = $(if ($null -ne $Provenance -and $Provenance.platform -and -not ([string]$Provenance.platform -match '^unknown\(')) { " ($($Provenance.platform))" } else { '' })
    $total = @($Result.Rows).Count
    $u = $Result.UnobservedCount

    if ($Result.SchemaRefusal) {
        # A refusal compared nothing, so every row is unobserved. The row union
        # was never built; the larger leg's roster is the honest denominator.
        $n = [Math]::Max([int]$Result.Identity.DevRowCount, [int]$Result.Identity.PublishedRowCount)
        $md.Add("### Published-build capability parity$plat -- REFUSED (schema mismatch): $n of $n rows UNOBSERVED (refused), none compared")
    } elseif ($u -ge $script:ParityUnobservedHeadlineThreshold) {
        $md.Add("### Published-build capability parity$plat -- THIN: $u of $total rows UNOBSERVED, never compared")
    } else {
        $md.Add("### Published-build capability parity$plat -- $u of $total rows unobserved")
    }
    $md.Add("")

    # skew_commits, verbatim from the provenance block -- never recomputed here.
    $skew = $(if ($null -ne $Provenance) { $Provenance.skew_commits } else { Format-ParityUnknown 'no_provenance_block' })
    if ($null -ne $Provenance) {
        $md.Add(("**skew_commits: " + $skew + "** -- dev ``" + $Provenance.dev_sha + "`` vs published ``" +
                 $Provenance.published_tag + "`` (``" + $Provenance.published_sha + "``); run " +
                 $Provenance.run_id + " (" + $Provenance.run_event + "). 0 is one commit in two build " +
                 "shapes; any other number confounds every row difference below with that many commits."))
    } else {
        $md.Add("**skew_commits: " + $skew + "**")
    }
    $md.Add("")

    if ($Result.SchemaRefusal) {
        $md.Add("**Refused -- schema version mismatch.** $($Result.SchemaRefusalReason)")
        $md.Add("")
        $md.Add("No defect count is reported. A row diff across two manifest formats is meaningless, and ``0`` would be a claim this run did not earn.")
    } else {
        $md.Add("**parity_defects = $($Result.ParityDefectCount)** (rung_differs $($Result.RungDifferCount) + only_in_dev $($Result.OnlyInDevCount)) -- out of **$($Result.ComparableCount) comparable** rows, with **$u unobserved**.")
        $md.Add("")
        $md.Add("**$u rows were unobserved** on at least one leg, so no comparison was possible for them. ``unknown`` is the absence of a reading, never agreement -- read ``parity_defects`` as a floor over the comparable set, not a verdict on the roster.")
        $md.Add("")
        $md.Add("| Capability | Development build | Published build | Disposition |")
        $md.Add("|---|---|---|---|")
        foreach ($r in @($Result.Rows)) {
            $devCell = $(if ($null -eq $r.DevRung) { "_(no row)_" } else { "``$($r.DevRung)``" })
            $pubCell = $(if ($null -eq $r.PublishedRung) { "_(no row)_" } else { "``$($r.PublishedRung)``" })
            $disp = switch ($r.Disposition) {
                'defect'                 { "**DEFECT**" }
                'only_in_dev'            { "**DEFECT** (absent from published roster)" }
                'only_in_dev_unobserved' { "roster difference, unobserved" }
                'only_in_published'      { "only in published roster" }
                'expected_difference'    { "expected (allowlisted)" }
                'unobserved'             { "unobserved" }
                default                  { "match" }
            }
            $md.Add("| ``$($r.Id)`` | $devCell | $pubCell | $disp |")
        }
        $md.Add("")
        $md.Add("Allowlisted expected differences: **$(@($Result.Allowlist).Count)** entries" + $(if (@($Result.Allowlist).Count -eq 0) { " -- the allowlist is empty; nothing was excused." } else { ":" }))
        foreach ($e in @($Result.Allowlist)) {
            $md.Add("- ``$($e.Id)`` (dev ``$($e.DevRung)`` / published ``$($e.PublishedRung)``): $($e.Reason)")
        }
        $md.Add("")
        if ($SlashCommandsStatus) {
            $md.Add("**slash_commands_status = ``" + $SlashCommandsStatus + "``** (from the filesystem witness, not the manifest's self-report).")
            $md.Add("")
        }
        if (@($SelfReportDisagreements).Count -gt 0) {
            $md.Add("**Self-report disagrees with the filesystem on " + @($SelfReportDisagreements).Count + " row(s).** A finding about the INSTRUMENT, counted separately from both numbers:")
            foreach ($d in @($SelfReportDisagreements)) {
                $md.Add("- ``" + $d.id + "`` (" + $d.leg + "): " + $d.kind + " -- " + $d.note)
            }
            $md.Add("")
        }
        if (@($SessionLedgerRows).Count -gt 0) {
            $md.Add("Provisioning rows, driven through the artifact's own doors before the manifest read: " +
                    ((@($SessionLedgerRows) | ForEach-Object { "``$_``" }) -join ", ") + ". A row still reading ``unknown`` means the door it needed refused -- see ``provisioning_drive`` in the JSON artifact for which one and why. Nothing here fabricates a spawn.")
        }
    }
    $md.Add("")
    $md.Add("Development build: ``$($Result.Identity.DevAppVersion)`` / ``$($Result.Identity.DevGitSha)`` via ``$($Result.Identity.DevDoor)``  ")
    $md.Add("Published build: ``$($Result.Identity.PublishedAppVersion)`` / ``$($Result.Identity.PublishedGitSha)`` via ``$($Result.Identity.PublishedDoor)``")
    if ($null -ne $Provenance -and @($Provenance.siblings).Count -gt 0) {
        $md.Add("")
        $md.Add("Sibling checkouts on the development leg (recorded, NOT same-SHA): " +
                ((@($Provenance.siblings) | ForEach-Object { "``" + $_.repo + "@" + $_.sha + "``" }) -join ", "))
    }
    $md.Add("")
    $md.Add("_This report gates nothing._")
    return $md.ToArray()
}

# ---------------------------------------------------------------------------
# The filesystem witness rules (plan
# 2026-09-20-published-runner-parity-count-comes-from-a-run-not-from-reports,
# Phase 5).
#
# A capability manifest is a SELF-REPORT. The provisioning rows say which rung
# answered, and nothing in them is evidence that a file landed. So after driving
# the real provisioning doors the harness lists the directories itself, and these
# two pure rules compare the claim against the listing.
#
# A disagreement is a finding ABOUT THE INSTRUMENT, never a parity defect: it is
# counted and reported separately and is never folded into parity_defects or
# unobserved. A harness that quietly reported its own blindness as parity is the
# failure this whole plan exists to prevent.
#
# Pure: both take already-parsed data and touch no disk, so
# scripts/tests/test-parity-diff.ps1 pins them without provisioning anything.
# ---------------------------------------------------------------------------

# Which directory each provisioning row's units land in. A row absent from this
# map has no filesystem footprint to witness (workspace_root, spec_pages, ...)
# and is skipped rather than guessed at.
# `slash_commands` is DELIBERATELY ABSENT. It is not a provision-into-a-workdir
# at all: capability_manifest.rs describes it as the IMPORT of
# <workspace-root>/qontinui-claude-config/.claude/commands/*.md as runner
# workflows, and slash_commands.rs points its report at that CHECKOUT directory.
# It writes nothing into the session workdir, so the workdir listing can neither
# confirm nor contradict it -- and mapping it here made every run emit a
# `directory_has_units_but_row_is_unknown` finding whose note ("provisioning ran
# and the ledger did not record it") was false in both halves. A row with no
# footprint in the witnessed tree belongs with workspace_root and spec_pages:
# outside this map.
$script:ParityWitnessDirs = @{
    'fleet_commands'          = 'commands'
    'agent_commands_registry' = 'commands'
    'fleet_skills'            = 'skills'
    'agent_skills_registry'   = 'skills'
    'fleet_agents'            = 'agents'
    'agent_definitions'       = 'agents'
}

# Read one field from a witness that may be either a [PSCustomObject] (what
# Get-ParityProvisionWitness returns) or a [hashtable] (what this file's own doc
# comments describe, and what a caller is most likely to hand-build).
#
# The two need different accessors and the difference is SILENT: on a hashtable
# `$w.PSObject.Properties.Name` enumerates IsReadOnly/Keys/Count/... and never
# the keys, so a membership test written for one shape reports "absent" for the
# other and the rules above resolve to "nothing to say". That is the false-clean
# this file exists to prevent, so both shapes are handled here rather than in
# each rule.
#
# Returns a 2-element tuple: ($present, $value). $present distinguishes "the key
# is not there" from "the key is there and is $null" -- which is the whole
# unknown-vs-zero distinction these rules turn on.
function Get-ParityWitnessField {
    param($Witness, [string]$Name)
    if ($null -eq $Witness) { return @($false, $null) }
    if ($Witness -is [System.Collections.IDictionary]) {
        if ($Witness.Contains($Name)) { return @($true, $Witness[$Name]) }
        return @($false, $null)
    }
    if ($Witness.PSObject.Properties.Name -contains $Name) {
        return @($true, $Witness.$Name)
    }
    return @($false, $null)
}

function Get-ParityManifestRow {
    param($Manifest, [string]$Id)
    if ($null -eq $Manifest) { return $null }
    if (-not ($Manifest.PSObject.Properties.Name -contains 'rows')) { return $null }
    foreach ($r in @($Manifest.rows)) {
        if ($r.id -eq $Id) { return $r }
    }
    return $null
}

# Compare each provisioning row's claim with what the directory listing shows.
#
# $Witness is the harness's own listing: @{ commands = <int>; skills = <int>;
# agents = <int> } as file counts. A count that could not be taken must be
# $null, NOT 0 -- "could not look" and "looked and found nothing" are different
# findings and only the second one can contradict a row.
#
# Emits one record per disagreement, each naming the direction:
#   row_claims_units_but_directory_is_empty     a rung that claims units, zero files
#   directory_has_units_but_row_is_unknown      files present, row took no reading
#   directory_has_units_but_row_is_unresolved   files present, row read and
#                                               resolved NO source
#
# `unresolved` is special-cased HERE, and only here. For the parity count it is
# an OBSERVED rung (a reading was taken; it found no source), so it stays out of
# $script:ParityUnobservedRungs. But it claims NO units: agent_runtime's
# agent-definitions resolver returns `unresolved` with zero files by design on
# any install with no qontinui-claude-config checkout -- every normal published
# leg. So `unresolved` over an empty directory is CONSISTENT, and `unresolved`
# over N>0 files is the contradiction -- UNLESS a sibling row that shares the
# directory claims units, because the directories are shared: on that same
# published leg `fleet_agents` writes its embedded floor into the very
# `.claude/agents` that `agent_definitions` reports `unresolved` for, so those
# files are explained by the sibling and contradict nothing. Treating
# `unresolved` as "claims units" (the first version of this rule, inherited from
# #1844) inverted both answers; treating any file as contradicting it would have
# fired on every normal published leg in the mirror-image direction.
function Get-ParitySelfReportDisagreements {
    param($Manifest, $Witness)

    $out = @()
    if ($null -eq $Manifest -or $null -eq $Witness) { return @($out) }

    # Which directories have at least one row claiming units in them. An
    # `unresolved` row's directory may legitimately hold a sibling's files.
    $claimedDirs = @{}
    foreach ($sid in $script:ParityWitnessDirs.Keys) {
        $srung = Get-ParityRowRung -Row (Get-ParityManifestRow -Manifest $Manifest -Id $sid)
        if ($null -ne $srung -and (Test-ParityRungObserved -Rung $srung) -and $srung -ne 'unresolved') {
            $claimedDirs[$script:ParityWitnessDirs[$sid]] = $true
        }
    }

    foreach ($id in ($script:ParityWitnessDirs.Keys | Sort-Object)) {
        $dirKey = $script:ParityWitnessDirs[$id]
        $field = Get-ParityWitnessField -Witness $Witness -Name $dirKey
        if (-not $field[0]) { continue }
        $count = $field[1]
        # UNKNOWN count: a listing that could not be taken contradicts nothing.
        if ($null -eq $count) { continue }

        $row = Get-ParityManifestRow -Manifest $Manifest -Id $id
        $rung = Get-ParityRowRung -Row $row
        if ($null -eq $rung) { continue }
        $observed = Test-ParityRungObserved -Rung $rung
        $claimsUnits = $observed -and ($rung -ne 'unresolved')

        if ($claimsUnits -and [int]$count -eq 0) {
            $out += [PSCustomObject]@{
                id          = $id
                kind        = 'row_claims_units_but_directory_is_empty'
                rung        = $rung
                witness_dir = ".claude/$dirKey"
                witness_files = 0
                note        = ("the manifest row resolved to rung '$rung' while .claude/$dirKey " +
                               "holds no files. The row is a self-report; the listing is the witness.")
            }
        } elseif ($observed -and -not $claimsUnits -and [int]$count -gt 0 -and -not $claimedDirs.ContainsKey($dirKey)) {
            $out += [PSCustomObject]@{
                id          = $id
                kind        = 'directory_has_units_but_row_is_unresolved'
                rung        = $rung
                witness_dir = ".claude/$dirKey"
                witness_files = [int]$count
                note        = ("$count file(s) are present in .claude/$dirKey while the row reports " +
                               "rung 'unresolved' -- a reading that found no source -- and no other row " +
                               "sharing that directory claims units. The files are unaccounted for.")
            }
        } elseif (-not $observed -and [int]$count -gt 0) {
            $out += [PSCustomObject]@{
                id          = $id
                kind        = 'directory_has_units_but_row_is_unknown'
                rung        = $rung
                witness_dir = ".claude/$dirKey"
                witness_files = [int]$count
                note        = ("$count file(s) are present in .claude/$dirKey while the row took no " +
                               "reading at all. Provisioning ran and the ledger did not record it.")
            }
        }
    }
    return @($out)
}

# The typed slash-commands verdict the metric's baseline defect is stated in.
#
# WHAT IT IS MEASURED OVER, stated because the name invites the wrong reading:
# the COMMAND BODIES PROVISIONED INTO A SESSION WORKDIR (`.claude/commands/*.md`
# -- the `fleet_commands` bundle plus any `agent_commands_registry` overlay), on
# each leg. That is the operator-facing question the metric asks ("does a
# published install give a session the fleet commands"), and it is NOT the
# `slash_commands` capability row, which is a different mechanism entirely (the
# import of a checkout's commands as runner workflows -- see the note on
# $script:ParityWitnessDirs). The artifact carries
# `slash_commands_status_source` beside this value so no reader has to infer it.
#
# Exactly one of:
#   provisioned_equal                  both legs provisioned the same count
#   provisioned_fewer(dev=N,published=M)  published provisioned fewer
#   provisioned_more(dev=N,published=M)   published provisioned MORE (stated,
#                                         not silently folded into 'equal')
#   none_provisioned                   both legs provisioned nothing
#   unknown(<reason>)                  a count could not be taken on a leg
#
# Counts come from the WITNESS, not the manifest: the question "does a published
# install get the fleet commands" is answered by files on disk.
function Get-ParitySlashCommandsStatus {
    param($DevWitness, $PublishedWitness)

    $devField = Get-ParityWitnessField -Witness $DevWitness -Name 'commands'
    $pubField = Get-ParityWitnessField -Witness $PublishedWitness -Name 'commands'
    $devCount = $devField[1]
    $pubCount = $pubField[1]

    if ($null -eq $devCount -and $null -eq $pubCount) {
        return 'unknown(no_command_listing_on_either_leg)'
    }
    if ($null -eq $devCount) { return 'unknown(no_command_listing_on_the_dev_leg)' }
    if ($null -eq $pubCount) { return 'unknown(no_command_listing_on_the_published_leg)' }

    $d = [int]$devCount
    $p = [int]$pubCount
    if ($d -eq 0 -and $p -eq 0) { return 'none_provisioned' }
    if ($d -eq $p) { return 'provisioned_equal' }
    if ($p -lt $d) { return "provisioned_fewer(dev=$d,published=$p)" }
    return "provisioned_more(dev=$d,published=$p)"
}

# ---------------------------------------------------------------------------
# The filesystem witness. The manifest is a self-report; this is the listing
# that can contradict it. Counts are $null when the directory could not be
# listed at all -- "could not look" is not "looked and found nothing", and only
# the second can contradict a row (see Get-ParitySelfReportDisagreements).
# ---------------------------------------------------------------------------
function Get-ParityProvisionWitness {
    # $ProbeWorkdir is where the provision-probe wrote, which is NOT $Workdir:
    # the probe creates its own directory so a pre-placed .claude symlink cannot
    # be followed. The commands and skills come from the terminal chokepoint and
    # do land in $Workdir. Passing $null leaves the agents count $null (UNKNOWN),
    # never 0 -- "the probe did not answer" is not "the probe wrote nothing".
    #
    # $TerminalOutcome is the drive's `terminal` field. The commands and skills
    # are written ONLY by POST /terminals (acquire_for_terminal), so unless that
    # door answered `created...` nothing was asked to write them, and an empty
    # `.claude/commands` there is "never provisioned", not "provisioned zero".
    # Counting it as 0 turned a refused terminal on one leg into a fabricated
    # `provisioned_fewer(dev=N,published=0)` parity defect, and refusals on both
    # legs into `none_provisioned` (a defect in #1844, corrected on adoption).
    # Omitted or anything but `created*`, both counts are $null -- UNKNOWN --
    # the same way the agents count already treats a probe that did not answer.
    param([string]$Workdir, [string]$ProbeWorkdir = $null, [string]$TerminalOutcome = $null)

    $count = {
        param([string]$Dir, [string]$Filter, [bool]$Recurse)
        try {
            # A path this harness did not build itself can carry Rust's VERBATIM
            # prefix: `std::fs::canonicalize` returns `\\?\C:\...` on Windows,
            # and `provisioned_into` comes straight from it. Windows PowerShell
            # 5.1's FileSystem provider does not interpret that prefix -- it
            # parses the leading `\\` as UNC -- so `Test-Path` answers $false for
            # a directory that plainly exists, and this scriptblock would return
            # 0: "could not look" rendered as "looked and found nothing", which
            # is the precise conflation this whole file exists to prevent. Two
            # fabricated `row_claims_units_but_directory_is_empty` findings per
            # leg, on every Windows run, about the instrument itself.
            # Belt and braces: the boundary normalization in
            # Invoke-ParityProvisioningDrive (published-parity.ps1) is what
            # actually fixes this, but a path reaching here verbatim must not
            # throw.
            $Dir = ConvertFrom-VerbatimPath $Dir

            # Test-Path lives INSIDE the try on purpose. $ErrorActionPreference
            # is script-scope 'Stop', so a provider that cannot interpret the
            # path throws a TERMINATING error; outside the try that escapes this
            # scriptblock entirely, propagates through Get-ParityProvisionWitness
            # into Get-ManifestOverHttp's catch, and loses the whole leg as a
            # manifest-read failure.
            if (-not (Test-Path -LiteralPath $Dir)) { return 0 }
            $items = Get-ChildItem -LiteralPath $Dir -Filter $Filter -File -Recurse:$Recurse -ErrorAction Stop
            return @($items).Count
        } catch {
            # UNKNOWN, never 0.
            return $null
        }
    }

    $claude = Join-Path $Workdir '.claude'
    $agentsCount = $null
    if (-not [string]::IsNullOrWhiteSpace($ProbeWorkdir)) {
        $agentsCount = & $count (Join-Path (Join-Path $ProbeWorkdir '.claude') 'agents') '*.md' $false
    }
    $commandsCount = $null
    $skillsCount = $null
    if ($TerminalOutcome -like 'created*') {
        $commandsCount = & $count (Join-Path $claude 'commands') '*.md' $false
        $skillsCount   = & $count (Join-Path $claude 'skills') 'SKILL.md' $true
    }
    return [PSCustomObject]@{
        commands = $commandsCount
        skills   = $skillsCount
        agents   = $agentsCount
    }
}

# ---------------------------------------------------------------------------
# Normalize a path that came from another process.
#
# Rust's `std::fs::canonicalize` returns a VERBATIM path on Windows
# (`\\?\D:\a\...`, or `\\?\UNC\server\share\...`), and the probe's
# `provisioned_into` is exactly that. Windows PowerShell 5.1 cannot carry those:
# `Join-Path` fails with *"the value of argument \"drive\" is null"* because it
# tries to resolve `\\?\D:` as a drive qualifier, and the FileSystem provider
# reads the leading `\\` as UNC.
#
# MEASURED, not theorised: the first version of this harness stripped the prefix
# inside the directory-counting scriptblock, which is too LATE -- the `Join-Path`
# calls that build the path run before it. On CI run 36615500004 that threw out of
# Get-ParityProvisionWitness, was caught as a manifest-read failure, and lost BOTH
# legs of the negative control ("NEGATIVE-CONTROL-UNAVAILABLE manifest_read").
# So normalization happens HERE, once, at the boundary where the foreign path
# enters this script, and every consumer downstream sees a 5.1-usable path.
# ---------------------------------------------------------------------------
function ConvertFrom-VerbatimPath {
    param([string]$Path)
    if ([string]::IsNullOrWhiteSpace($Path)) { return $Path }
    if ($Path -like '\\?\UNC\*') { return '\\' + $Path.Substring(8) }
    if ($Path -like '\\?\*')      { return $Path.Substring(4) }
    return $Path
}

# ===========================================================================
# PLATFORM (plan 2026-09-20-published-runner-parity-count-comes-from-a-run-not-from-reports,
# Phase 6B -- the Linux published leg).
#
# The harness was Windows-shaped in three places that are not about the
# artifact at all: the dev binary's FILE NAME (`.exe`), the separator its
# build-dir guard matched on (`\target\debug\`), and the process table its
# teardown walked (Win32_Process). Each is decided here, as a pure function, so
# scripts/tests/test-parity-diff.ps1 pins it on whichever interpreter runs the
# suite -- the 5.1 gate on the Windows job, pwsh 7 on the Linux one.
# ===========================================================================

# 'windows' | 'linux' | 'macos'. $IsLinux / $IsMacOS do not exist on Windows
# PowerShell 5.1, and $null is false, so 5.1 lands on 'windows' -- which is the
# only platform 5.1 runs on.
function Get-ParityHostPlatform {
    if ($IsLinux) { return 'linux' }
    if ($IsMacOS) { return 'macos' }
    return 'windows'
}

# The cargo binary's file name on a platform. Tauri 2 does not rename it to
# productName on any of them, so the only difference is the extension.
function Get-ParityDevExeName {
    param([ValidateSet('windows', 'linux', 'macos')] [string]$Platform)
    if ($Platform -eq 'windows') { return 'qontinui-runner.exe' }
    return 'qontinui-runner'
}

# True when $Path lies under a cargo build directory (target/debug or
# target/release), with EITHER separator. The dev leg is required to be such a
# path, so this is one half of the disjointness the harness header promises. It
# used to match only `\target\`, which on Linux would refuse every dev binary.
function Test-ParityDevBuildPath {
    param([string]$Path)
    if ([string]::IsNullOrWhiteSpace($Path)) { return $false }
    $norm = ($Path -replace '\\', '/')
    return [bool]($norm -match '(?i)/target/(debug|release)/')
}

# One line of /proc/<pid>/stat -> { ProcessId, ParentProcessId, Name,
# CreationDate }, or $null when the line does not parse. CreationDate is the
# kernel's `starttime` (field 22, clock ticks since boot): not a date, but
# ordered the same way within one boot, which is the only use the teardown makes
# of it (a child created BEFORE the root is a recycled pid, never a descendant).
#
# Field 2 (comm) is parenthesised and may itself contain spaces and ')', so the
# remaining fields are counted from the LAST ')'. After it: field 3 (state) is
# index 0, field 4 (ppid) index 1, field 22 (starttime) index 19.
function ConvertFrom-ParityProcStat {
    param([string]$Line)
    if ([string]::IsNullOrWhiteSpace($Line)) { return $null }
    $open = $Line.IndexOf('(')
    $close = $Line.LastIndexOf(')')
    if ($open -lt 1 -or $close -lt $open) { return $null }
    $rest = @($Line.Substring($close + 1).Trim() -split '\s+')
    if ($rest.Count -lt 20) { return $null }
    $procId = 0
    $parentId = 0
    [long]$start = 0
    if (-not [int]::TryParse($Line.Substring(0, $open).Trim(), [ref]$procId)) { return $null }
    if (-not [int]::TryParse($rest[1], [ref]$parentId)) { return $null }
    if (-not [long]::TryParse($rest[19], [ref]$start)) { return $null }
    return [PSCustomObject]@{
        ProcessId       = $procId
        ParentProcessId = $parentId
        Name            = $Line.Substring($open + 1, $close - $open - 1)
        CreationDate    = $start
    }
}

# ===========================================================================
# CROSS-PLATFORM, PUBLISHED SIDE ONLY (Phase 6B's third number).
#
# Given the Windows leg's report and the Linux leg's report -- each the JSON
# ConvertTo-ParityReportObject wrote, read back -- list the capability rows
# whose PUBLISHED rung differs between the two platforms.
#
# This is deliberately NOT a parity number and is never added to
# parity_defects: the metric's unit is development-vs-published on ONE
# platform. A row that resolves `bundle_resource` on Windows and `unresolved` on
# Linux is a fact about the Linux artifact that neither per-platform count can
# show, which is why it is reported -- as its own list.
#
# The same rule as the main comparator: a row either leg did not observe is
# UNOBSERVED, never agreement. And the comparison REFUSES (Available = $false,
# DifferCount = $null) rather than producing a number when it would be
# meaningless: a missing report, a schema-refused report, or two published
# builds of different versions -- a difference between v1.0.11 on one side and
# v1.0.12 on the other is version skew, not platform.
# ===========================================================================
function New-ParityCrossPlatformRefusal {
    param([string]$Reason, $WindowsVersion = $null, $LinuxVersion = $null)
    return [PSCustomObject]@{
        Available       = $false
        Reason          = $Reason
        Rows            = @()
        DifferCount     = $null
        SameCount       = $null
        UnobservedCount = $null
        OnlyOnOneCount  = $null
        WindowsVersion  = $WindowsVersion
        LinuxVersion    = $LinuxVersion
    }
}

function Compare-ParityPublishedAcrossPlatforms {
    param($WindowsReport, $LinuxReport)

    if ($null -eq $WindowsReport) { return (New-ParityCrossPlatformRefusal 'windows_report_missing') }
    if ($null -eq $LinuxReport) { return (New-ParityCrossPlatformRefusal 'linux_report_missing') }
    if ($WindowsReport.schema_refused) { return (New-ParityCrossPlatformRefusal 'windows_report_schema_refused') }
    if ($LinuxReport.schema_refused) { return (New-ParityCrossPlatformRefusal 'linux_report_schema_refused') }

    $winVer = $null
    $linVer = $null
    if ($WindowsReport.build_identity -and $WindowsReport.build_identity.published) { $winVer = $WindowsReport.build_identity.published.app_version }
    if ($LinuxReport.build_identity -and $LinuxReport.build_identity.published) { $linVer = $LinuxReport.build_identity.published.app_version }
    if ([string]::IsNullOrWhiteSpace([string]$winVer) -or [string]::IsNullOrWhiteSpace([string]$linVer)) {
        return (New-ParityCrossPlatformRefusal 'published_version_unknown' $winVer $linVer)
    }
    if ([string]$winVer -ne [string]$linVer) {
        return (New-ParityCrossPlatformRefusal 'published_versions_differ' $winVer $linVer)
    }

    $win = @{}
    $lin = @{}
    $order = New-Object System.Collections.Generic.List[string]
    foreach ($row in @($WindowsReport.rows)) {
        if ($null -eq $row) { continue }
        $win[[string]$row.id] = $row
        if (-not $order.Contains([string]$row.id)) { $order.Add([string]$row.id) }
    }
    foreach ($row in @($LinuxReport.rows)) {
        if ($null -eq $row) { continue }
        $lin[[string]$row.id] = $row
        if (-not $order.Contains([string]$row.id)) { $order.Add([string]$row.id) }
    }

    $rows = @()
    $differ = 0
    $same = 0
    $unobserved = 0
    $onlyOnOne = 0
    foreach ($id in $order) {
        $w = $win[$id]
        $l = $lin[$id]
        $wr = $null
        $lr = $null
        if ($null -ne $w) { $wr = $w.published_rung }
        if ($null -ne $l) { $lr = $l.published_rung }
        if ($null -eq $w) {
            $disp = 'only_on_linux'
            $onlyOnOne++
        } elseif ($null -eq $l) {
            $disp = 'only_on_windows'
            $onlyOnOne++
        } elseif (-not $w.published_observed -or -not $l.published_observed) {
            $disp = 'unobserved'
            $unobserved++
        } elseif ([string]$wr -ne [string]$lr) {
            $disp = 'differs'
            $differ++
        } else {
            $disp = 'same'
            $same++
        }
        $rows += [PSCustomObject]@{
            id                     = $id
            windows_published_rung = $wr
            linux_published_rung   = $lr
            disposition            = $disp
        }
    }

    # Nothing compared is not "0 differ". With every row unobserved on at least
    # one platform (an artifact that predates the probe route, a refused
    # provisioning drive) the honest answer is UNKNOWN.
    # The rows are kept on the refusal: which ones went unobserved is exactly
    # what diagnoses an artifact that predates the probe route.
    if (($differ + $same) -eq 0) {
        $r = New-ParityCrossPlatformRefusal 'no_row_observed_on_both' $winVer $linVer
        $r.Rows = $rows
        $r.UnobservedCount = $unobserved
        $r.OnlyOnOneCount = $onlyOnOne
        return $r
    }

    return [PSCustomObject]@{
        Available       = $true
        Reason          = $null
        Rows            = $rows
        DifferCount     = $differ
        SameCount       = $same
        UnobservedCount = $unobserved
        OnlyOnOneCount  = $onlyOnOne
        WindowsVersion  = $winVer
        LinuxVersion    = $linVer
    }
}
