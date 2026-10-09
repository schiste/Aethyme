# FX08 brief usefulness pilot

The frozen task package and v14 behavior-oracle revision are in [`v14-frozen/`](./v14-frozen/README.md). The Playground tag `fx08-v14-frozen` points to commit `694ad1e4bf71eb40e669e0a8557dec1039bd73b1`; the original tagged Git history is preserved in [`fx08-v14-frozen.bundle`](./fx08-v14-frozen.bundle) with its SHA-256 in [`fx08-v14-frozen.bundle.sha256`](./fx08-v14-frozen.bundle.sha256). The per-file freeze digests are in `v14-frozen/freeze-manifest.sha256`.

Verify the bundle with `git bundle verify fx08-v14-frozen.bundle` and check the frozen files from `v14-frozen/` with `shasum -a 256 -c freeze-manifest.sha256`.

The v14 pilot report is [`JSON`](../../../../../evidence/fx08-brief-usefulness-pilot.json) and [`Markdown`](../../../../../evidence/fx08-brief-usefulness-pilot.md). It records 48 pilot runs, no held-out runs because H01/H02 had already been exposed, and the 8-run held-out shortfall. Raw per-run artifacts remain outside the Playground under `/private/tmp/fx08-v14-raw-runs/`.

This delivers briefs through a fixed `HANDOFF.md` file. It does not test L3 retrieval and does not close D30.
