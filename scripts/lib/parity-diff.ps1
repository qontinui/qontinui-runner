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
#     slash_commands           unknown
#
# EIGHT of nine rows are `unknown`. Not "resolved by a low rung" -- unknown, the
# manifest's word for "nothing observed this capability here". The six
# provisioning/registry rows fill from the Phase 3 session ledger, which only
# fills at SESSION SPAWN; a cold flag invocation has spawned nothing.
# `bundled_resources` needs a Tauri `AppHandle` that does not exist pre-GUI.
#
# So on a cold-CLI-vs-cold-CLI comparison, eight rows read `unknown` on BOTH
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
# THE ALLOWLIST -- WHAT IS ON IT, AND WHY IT IS EMPTY TODAY
# =============================================================================
#
# Some differences between a debug build and a release build are DESIGNED, not
# defects. The class is real and named in this repo:
#
#   mcp/test_fixtures::routes()  #[cfg(any(debug_assertions, feature = "test-fixtures"))]
#   mcp/debug_wedge::routes()    #[cfg(debug_assertions)]        (/__debug/wedge-ui-thread)
#
# Both are compiled OUT of the published build by construction. A capability
# resolved by such a module would legitimately answer differently on the two
# legs, and counting it as a parity defect would put permanent noise into the
# metric.
#
# $ParityExpectedDifferences is therefore a first-class, per-row allowlist -- and
# measured 2026-09-02 it is EMPTY, because NONE of the nine CAPABILITY_SPECS rows
# is resolved by a cfg-gated module:
#
#   workspace_root  bundled_resources  spec_pages  fleet_commands  fleet_skills
#   fleet_agents    agent_definitions  agent_commands_registry     slash_commands
#
# and neither debug-only surface above appears in `UI_BRIDGE_ROUTES` either, so
# the behavioural axis does not see them as a route delta.
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
$script:ParityExpectedDifferences = @()

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

    $out = New-Object System.Collections.Generic.List[Object]
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
        Rows                     = @($out)
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
    $L.Add("  development build : {0}  git_sha={1}  build_id={2}  (door: {3})" -f $id.DevAppVersion, $id.DevGitSha, $id.DevBuildId, $id.DevDoor)
    $L.Add("  published build   : {0}  git_sha={1}  build_id={2}  (door: {3})" -f $id.PublishedAppVersion, $id.PublishedGitSha, $id.PublishedBuildId, $id.PublishedDoor)
    $L.Add("  schema_version    : dev={0}  published={1}" -f $id.DevSchemaVersion, $id.PublishedSchemaVersion)
    $L.Add("  rows              : dev={0}  published={1}" -f $id.DevRowCount, $id.PublishedRowCount)
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
            $L.Add("       dev       : {0}{1}" -f $r.DevRung, $(if ($r.DevPath) { "  <- $($r.DevPath)" } else { "" }))
            $L.Add("       published : {0}{1}" -f $(if ($null -eq $r.PublishedRung) { "<row absent>" } else { $r.PublishedRung }), $(if ($r.PublishedPath) { "  <- $($r.PublishedPath)" } else { "" }))
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
        $L.Add("   allowlist: (empty) -- no CAPABILITY_SPECS row is resolved by a cfg-gated")
        $L.Add("              module, so nothing is excused. The class it exists for is real")
        $L.Add("              (mcp/test_fixtures, mcp/debug_wedge -- both compiled out of a")
        $L.Add("              release build); no capability row is resolved by either today.")
    } else {
        foreach ($e in @($Result.Allowlist)) {
            $L.Add("   allowlist: {0}  dev='{1}' published='{2}'" -f $e.Id, $e.DevRung, $e.PublishedRung)
            $L.Add("              {0}" -f $e.Reason)
        }
    }
    $matched = @($Result.Rows | Where-Object { $_.Disposition -eq 'expected_difference' })
    $L.Add("")
    if ($matched.Count -eq 0) {
        $L.Add("   matched this run: (none)")
    } else {
        foreach ($r in $matched) {
            $L.Add("   * {0}: dev={1} published={2}" -f $r.Id, $r.DevRung, $r.PublishedRung)
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
            $L.Add("   * {0}: dev={1} published={2}" -f $r.Id, $r.DevRung, $r.PublishedRung)
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
            $L.Add("   * {0}: published={1}" -f $r.Id, $r.PublishedRung)
        }
        $L.Add("")
    }

    # --- 5. Matched ----------------------------------------------------------
    $L.Add("-- In parity ({0}) -------------------------------------------------------------" -f $Result.MatchCount)
    $matches = @($Result.Rows | Where-Object { $_.Disposition -eq 'match' })
    if ($matches.Count -eq 0) {
        $L.Add("   (none)")
    } else {
        foreach ($r in $matches) { $L.Add("   * {0}: {1}" -f $r.Id, $r.DevRung) }
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
