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
# ---------------------------------------------------------------------------
function ConvertTo-ParityReportObject {
    param($Result, [string]$GeneratedAt, $Observability)

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
        report_version = 1
        generated_at = $GeneratedAt
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
