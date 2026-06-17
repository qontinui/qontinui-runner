//! Compare a comprehended `FunctionalSpec` (path arg) against the frozen
//! connect-runner oracle via `enumerate_nodes`, and round-trip it through
//! `evaluate_completeness`. Prints the honest node-set delta + coverage.
//!
//! Usage: cargo run --example verify_against_oracle -- <comprehended_spec_path>

use std::collections::{BTreeMap, BTreeSet};

use qontinui_types::completeness_eval::{enumerate_nodes, evaluate_completeness, CoverageEvidence};
use qontinui_types::functional_spec::{FunctionalSpec, SpecProvenance};

const ORACLE: &str =
    include_str!("../tests/fixtures/comprehension/connect-runner.functional_spec.oracle.json");

fn tuples(spec: &FunctionalSpec) -> BTreeSet<(String, String, String)> {
    enumerate_nodes(spec)
        .into_iter()
        .map(|n| {
            (
                n.r#ref,
                format!("{:?}", n.section),
                format!("{:?}", n.provenance),
            )
        })
        .collect()
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: <spec_path>");
    let got: FunctionalSpec =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).expect("spec parses");
    let oracle: FunctionalSpec = serde_json::from_str(ORACLE).expect("oracle parses");

    let g = tuples(&got);
    let o = tuples(&oracle);

    println!("=== ORACLE node-set ({} nodes) ===", o.len());
    for (r, s, p) in &o {
        println!("  [{s:>11}] {r}  ::{p}");
    }
    println!("\n=== COMPREHENDED node-set ({} nodes) ===", g.len());
    for (r, s, p) in &g {
        println!("  [{s:>11}] {r}  ::{p}");
    }

    let matched: BTreeSet<_> = g.intersection(&o).collect();
    let only_oracle: BTreeSet<_> = o.difference(&g).collect();
    let only_got: BTreeSet<_> = g.difference(&o).collect();

    println!("\n=== DELTA ===");
    println!(
        "matched: {} / oracle {} / comprehended {}",
        matched.len(),
        o.len(),
        g.len()
    );
    println!("-- in oracle, NOT comprehended ({}):", only_oracle.len());
    for (r, s, p) in &only_oracle {
        println!("   [{s:>11}] {r} ::{p}");
    }
    println!("-- comprehended, NOT in oracle ({}):", only_got.len());
    for (r, s, p) in &only_got {
        println!("   [{s:>11}] {r} ::{p}");
    }

    // Round-trip the COMPREHENDED spec through evaluate_completeness with
    // complete evidence built from its own node-set.
    let mut covered = BTreeSet::new();
    let mut filled_assumed = BTreeSet::new();
    for node in enumerate_nodes(&got) {
        match node.provenance {
            SpecProvenance::Assumed => {
                filled_assumed.insert(node.r#ref);
            }
            _ => {
                covered.insert(node.r#ref);
            }
        }
    }
    let evidence = CoverageEvidence {
        covered,
        gaps: BTreeMap::new(),
        filled_assumed,
    };
    let verdict = evaluate_completeness(&got, &evidence, "2026-06-14T00:00:00Z");
    println!("\n=== ROUND-TRIP (comprehended spec → evaluate_completeness) ===");
    println!("coverage:            {}", verdict.coverage);
    println!("assumed_fill_rate:   {}", verdict.assumed_fill_rate);
    println!("gaps:                {}", verdict.gaps.len());
    println!("consistent:          {}", verdict.coverage_is_consistent());
}
