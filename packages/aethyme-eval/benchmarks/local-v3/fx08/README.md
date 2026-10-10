# FX08 brief usefulness pilot

The frozen task package and v14 behavior-oracle revision are in [`v14-frozen/`](./v14-frozen/README.md). The Playground tag `fx08-v14-frozen` points to commit `694ad1e4bf71eb40e669e0a8557dec1039bd73b1`; the original tagged Git history is preserved in [`fx08-v14-frozen.bundle`](./fx08-v14-frozen.bundle) with its SHA-256 in [`fx08-v14-frozen.bundle.sha256`](./fx08-v14-frozen.bundle.sha256). The per-file freeze digests are in `v14-frozen/freeze-manifest.sha256`.

Verify the bundle with `git bundle verify fx08-v14-frozen.bundle` and check the frozen files from `v14-frozen/` with `shasum -a 256 -c freeze-manifest.sha256`.

The v14 pilot report is [`JSON`](../../../../../evidence/fx08-brief-usefulness-pilot.json) and [`Markdown`](../../../../../evidence/fx08-brief-usefulness-pilot.md). It records 48 pilot runs, no held-out runs because H01/H02 had already been exposed, and the 8-run held-out shortfall. Raw per-run artifacts remain outside the Playground under `/private/tmp/fx08-v14-raw-runs/`.

This delivers briefs through a fixed `HANDOFF.md` file. It does not test L3 retrieval and does not close D30.

## FX08 v15 metadata follow-up

A post-run integrity audit found that the six v14 handoff framing sidecars still held the earlier digest 39cb1e70dbb145abceece6c949dc97142358c342abe428d86e8f68306d789423, while the actual shared framing and v14 pre-freeze check hash to 1f92fc7b2f2f1e1e7a74d15a63d63630fce59a216b27d8c3c39404602c2090c1. All 48 HANDOFF.md files used in the v14 runs match their frozen sources byte-for-byte.

The separate v15 freeze corrects only those six sidecars plus its README, pre-freeze record, and manifest; task content, briefs, prompts, oracles, browser settings, and pilot configuration are unchanged. The complete v15 source is preserved in the self-contained fx08-v15-frozen.bundle with its checksum in fx08-v15-frozen.bundle.sha256. Tag fx08-v15-frozen points to commit b684ea6cbcf2c248cdc7cc04fbd8bea4065ca718; bundle SHA-256: 784492acd0b051c8972e4bae84d1cf02b483c1243c5e0d6906b010efd80ae37e. Verify it with git bundle verify fx08-v15-frozen.bundle, then clone the fx08-v15-frozen tag to inspect its 137-entry manifest and pre-freeze record. V15 has no agent runs. The 48 measurements remain associated with v14 frozen revision 694ad1e4bf71eb40e669e0a8557dec1039bd73b1; no results were rescored. The eight held-out runs remain a shortfall.
