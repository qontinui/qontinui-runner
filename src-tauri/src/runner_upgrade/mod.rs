//! The runner upgrades itself: preflight, executor, rollback guard and
//! successor probation.
//!
//! Plan
//! `2026-11-24-runner-upgrades-itself-daily-keeping-sessions-alive-with-no-outage-and-automatic-rollback`,
//! Phase 5 (D3, D7's upgrade rule, Risks). Served `production-and-cost`
//! `runner-self-upgrade` reads THIS verb's preflight: until a running build
//! serves `GET /runner/upgrade/preflight` that clause authorizes nothing.
//!
//! | Piece | Module |
//! |---|---|
//! | `GET /runner/upgrade/preflight`, `POST /runner/upgrade` | [`routes`] |
//! | the checks, over the restart-readiness verdict | [`preflight`] |
//! | record → marker → guard → swap → handoff exit | [`executor`] |
//! | `--upgrade-guard`: roll back a successor that never proved itself | [`guard`] |
//! | the successor's probation at boot, and the outstanding reports | [`probation`], [`report`] |
//! | `PROVENANCE.json` and `--provenance` | [`provenance`] |
//! | the record and the `in-flight.json` booting marker | [`record`] |
//! | argv doors (`--provenance`, `--upgrade-guard`, `--upgrade-successor`) | [`cli`] |
//! | the rehome contract (plan Phase 5b) | [`rehome`] |
//! | the listener-fd registry (plan Phase 2) | [`listener_fds`] |
//! | the embedded-PG schema fingerprint | [`pg_schema`] |
//! | programs, the atomic link swap, process starts | [`sys`], [`spawn`] |

pub mod cli;
pub mod executor;
pub mod guard;
pub mod listener_fds;
pub mod pg_schema;
pub mod preflight;
pub mod probation;
pub mod provenance;
pub mod record;
pub mod rehome;
pub mod report;
pub mod routes;
pub mod spawn;
pub mod sys;
