# Behavior oracles

`behavior_oracle.mjs` uses headless Chrome to exercise P01–P04 in a temporary loopback server. It checks the rendered accessibility tree, browser keyboard and click events, responsive geometry, native form validity, visible results, and selection state. It never compares application files to a golden implementation.

The Playwright module and Chrome executable/version are pinned by absolute path in `pilot-config.json` for this local pilot. The CLI wrapper is not installed, so the independent checker uses the Playwright Node API; no Playwright test runner is added to any task repo.

H01/H02 use the previously frozen simulator oracle only for provenance; they are held out and not rerun because they were already exposed.


Each P01–P04 browser check was exercised before freeze against its A-state, a separate hand-built behavior-success control, and a separate behavior control that deliberately violates the decision. None of these controls is copied into an agent run. The checker never loads expected application source.
