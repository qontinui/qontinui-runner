# bound-hit-signature.jq -- recognise a JOB-level `timeout-minutes` expiry from
# job durations alone, with no declared bound.
#
# WHY. GitHub renders a job-level `timeout-minutes` expiry as `cancelled` --
# never `failure`, never `timed_out` (a STEP-level expiry concludes `failure`).
# So a job that blows its bound every night looks exactly like a run that was
# superseded or killed by infrastructure. What separates the two is shape: a
# bound cuts every over-long attempt at the SAME wall-clock point, so its
# cancellations pile up in a tight cluster ABOVE every success that fit under
# it; supersedes land wherever the superseding push happened to arrive.
# Specification: plan 2026-09-13-a-timeout-minutes-expiry-renders-as-cancelled-
# and-three-programs-are-blind-to-it, Phase 3.
#
# INPUT: an array of job rows, one per job per run, API field names kept:
#   {run_id, created_at, name, conclusion, started_at, completed_at}
# `created_at` is the RUN's creation time (the jobs API row carries none); the
# caller joins it in. Durations are JOB-level (`completed_at - started_at`),
# never run-level: a runner queue inflated one measured run-level duration 2x.
# Rows without both timestamps carry no duration and are ignored. So are rows
# whose duration is <= 0: a job cancelled while still QUEUED (a fail-fast
# matrix sibling, a run cancelled before a runner picked the job up) reports
# started_at == completed_at. It never ran, so it cannot have hit a bound --
# and a pile of identical zeros would otherwise be the tightest "cluster" of
# all.
#
# PARAMETERS (optional, via --argjson / --arg; $ARGS.named):
#   cluster_min    K, minimum cluster size (default 3, mirrors --min-streak).
#   success_scope  "prior" (default) or "all". "all" DISABLES the refinement to
#                  condition 3 below; it exists only so a test can prove the
#                  refinement is load-bearing. Never pass it in production.
#
# THE SIGNATURE, per job NAME, over the rows given:
#   1. >= K cancelled durations form a cluster.
#   2. span(cluster) = max - min <= clamp(5% x median(cluster), 30 s, 60 s).
#      The 30 s floor: cancel propagation alone was measured at up to 24 s, so a
#      flat 5% would over-constrain a short-bound job into a false negative.
#   3. min(cluster) > max(success durations of the job) -- STRICT, no gap: any
#      margin requirement fails on precisely the most-censored (worst) cases.
#      REFINEMENT (found at implement time): only successes whose run was
#      created AT OR BEFORE the cluster's newest member count. A success AFTER
#      the cluster may have run under a RAISED bound. Measured on the runner
#      Frontend Coverage Producer: the bound went 45 -> 90 min on 2026-09-13 and
#      two later successes ran 2867 s and 2967 s, above the 2714-2724 s cluster;
#      counting them would falsely break condition 3 on the clearest timeout
#      pattern in the corpus. Successes before the cluster ends are the ones that
#      ran under the same bound the cluster hit.
#      A cluster that passes 1 and 2 but has NO prior success to order against
#      is NOT a bound-hit: there is no ordinal evidence. It is reported as
#      `undecided` so a caller can count it as UNKNOWN rather than as health.
#   4. (declared-bound corroboration) NOT IMPLEMENTED. The plan makes it
#      printed-never-gating; this program reads no workflow file and needs no
#      bound. A caller that wants it prints it beside this output.
#
# CLUSTER CHOICE. Over the job's cancelled durations sorted ascending, every
# contiguous window is a candidate. The chosen cluster is the largest window
# satisfying 1-3 (ties: smallest span, then newest). Its members are removed
# and the search repeats, so a second, later cluster at a raised bound is found
# too. A cancelled duration in no qualifying cluster (a supersede -- measured:
# 651 s against a 2714-2724 s cluster) is NOT a bound-hit.
#
# CENSORING PROHIBITION -- a rule, not a caveat. Success durations are used
# ONLY as an ordinal separator (condition 3). Under a bound, max(success) is by
# construction the largest value that FIT below the cut: a sound separator and a
# meaningless magnitude. NEVER compute from it a margin, a headroom, a
# percentage, a ratio, or an estimate of the job's true duration, and NEVER emit
# a phrase of the form "N x the healthy maximum". `max_prior_success` is
# emitted for audit of the ordering only. Re-measure after the bound stops
# truncating.
#
# OUTPUT:
#   { bound_hits: [{run_id, name, duration}],          -- members of qualifying clusters
#     undecided:  [{run_id, name, duration}],          -- 1+2 pass, no prior success
#     jobs: [{name, cancelled_n, success_n,
#             clusters: [<cluster>],                   -- qualifying, in find order
#             best_rejected: <cluster> | null}] }      -- largest non-qualifying window
#   <cluster> = {n, span, min, max, median, tolerance, newest_created_at,
#                max_prior_success, status, members: [{run_id, duration}]}
#   status: bound_hit | no_prior_success | too_small | too_wide | success_above

def K: ($ARGS.named.cluster_min // 3);
def scope: ($ARGS.named.success_scope // "prior");

def dur: ((.completed_at | fromdate) - (.started_at | fromdate));

def median:
  length as $n
  | if $n % 2 == 1 then .[($n - 1) / 2]
    else (.[$n / 2 - 1] + .[$n / 2]) / 2 end;

def clamp($lo; $hi): if . < $lo then $lo elif . > $hi then $hi else . end;

# evaluate(window; successes) -> <cluster>. `window` is a slice of rows sorted
# by duration ascending.
def evaluate($succ):
  . as $w
  | ($w | map(.duration)) as $d
  | ($d | median) as $med
  | ($w | map(.created_at) | max) as $newest
  | ([ $succ[]
       | select(scope == "all" or .created_at <= $newest)
       | .duration ] | max) as $mps
  | ($d[-1] - $d[0]) as $span
  | ($med * 0.05 | clamp(30; 60)) as $tol
  | {n: ($d | length), span: $span, min: $d[0], max: $d[-1], median: $med,
     tolerance: $tol, newest_created_at: $newest, max_prior_success: $mps,
     status: (if $span > $tol then "too_wide"
              elif ($d | length) < K then "too_small"
              elif $mps == null then "no_prior_success"
              elif $d[0] > $mps then "bound_hit"
              else "success_above" end),
     members: ($w | map({run_id, duration}))};

def rank: {bound_hit: 4, no_prior_success: 3, success_above: 2, too_small: 1, too_wide: 0}[.status];

# best window over sorted rows, by (status rank, n, -span, newest).
def best($succ):
  . as $c | length as $n
  | [ range(0; $n) as $i | range($i; $n) as $j
      | $c[$i:$j + 1] | evaluate($succ)
      | select(.status != "too_wide") ]
  | if length == 0 then null
    else max_by([rank, .n, -.span, .newest_created_at]) end;

# Peel qualifying clusters off repeatedly; stop at the first non-qualifier.
def peel($succ):
  def go($acc):
    . as $c
    | ($c | best($succ)) as $b
    | if $b != null and ($b.status == "bound_hit" or $b.status == "no_prior_success") then
        ($b.members | map(.run_id)) as $ids
        | ($c | map(select(.run_id as $r | $ids | index($r) | not)))
        | go($acc + [$b])
      else {clusters: $acc, best_rejected: $b} end;
  go([]);

[ .[]
  | select(.started_at != null and .completed_at != null)
  | . + {duration: dur}
  | select(.duration > 0) ]
| group_by(.name)
| map(
    .[0].name as $name
    | (map(select(.conclusion == "cancelled")) | sort_by(.duration, .created_at)) as $c
    | map(select(.conclusion == "success")) as $s
    | ($c | peel($s)) as $p
    | {name: $name, cancelled_n: ($c | length), success_n: ($s | length),
       clusters: $p.clusters, best_rejected: $p.best_rejected})
| { bound_hits: [ .[] | .name as $nm | .clusters[] | select(.status == "bound_hit")
                  | .members[] | {run_id, name: $nm, duration} ],
    undecided:  [ .[] | .name as $nm | .clusters[] | select(.status == "no_prior_success")
                  | .members[] | {run_id, name: $nm, duration} ],
    jobs: . }
