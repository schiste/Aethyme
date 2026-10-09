# Why v13 task scores are superseded by oracle v2

The frozen v13 task-score oracle did not fully cover the written tasks:

- P01 did not assert an associated accessible label or exercise the Search button; its simulated DOM did not model the browser's default submit behavior.
- P02 checked a literal CSS media-query spelling instead of rendered behavior at a narrow viewport.
- P03 did not exercise the All filter.
- P04 tested a maximum violation but omitted the required and minimum-invalid cases.

The v13 JSON and all 48 raw Codex artifacts remain unchanged. V14 replaces the P01–P04 checker with browser-observed behavior assertions derived from the approved task text. This is a new oracle version, not a rescore of v13 and not tuning to agent output.
