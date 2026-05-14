# Reports

Last Updated: 2026-05-14

This directory contains three different kinds of report material.

## Directory Roles

- `evals/`: generated local evaluation reports. The repository keeps only a
  small curated subset of reports that are cited elsewhere in the docs.
  Additional timestamped eval reports are runtime output and should not be
  committed by default.
- `navigation/`: curated engineering reports about navigation, graph behavior,
  performance, and design iterations. These are maintained reference docs and
  belong in git.
- `judge-backtests/`: curated calibration and judge-comparison records. These
  are small, stable reference artifacts and belong in git.

## Rule

If a report is generated automatically on every run, treat it like runtime
output unless there is a concrete reason to keep that specific artifact as
reference material.
