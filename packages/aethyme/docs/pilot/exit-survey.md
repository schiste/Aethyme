# Internal dogfooding feedback

Last Updated: 2026-10-08

Use one copy per internal team member at the end of an observation window. Keep
responses in the team's internal workspace. Before summarizing, remove names,
private repository identity, task descriptions, paths, and secrets. Report themes
and aggregate counts rather than forwarding raw notes.

Team role category (no name): ______  Typical concurrent agents: ______
Observation window: __________ to __________

## Setup

**1. Time to first submit.** How long from starting setup to the first
successful `aethyme broker submit`? Include time spent writing or trimming
gates.

- [ ] under 30 minutes  [ ] 30–60 minutes  [ ] 1–2 hours  [ ] half a day
  [ ] more  [ ] did not get there

**2. Hardest step.** Which step took longest or needed help: install,
`aethyme init`, gate setup, `trust`, first `start`, first `submit`, or something
else? Describe the friction without including task details.

## Conflicts and recovery

**3. Overlaps and conflicts.** How many times did the broker report an overlap
or refuse a submit because another session changed the same area?

- Overlap warnings noticed: ______
- Submits refused for a conflict: ______

**4. Potential impact.** How many incidents might otherwise have caused a merge
conflict, broken build, or lost work? Keep examples generic and redacted.

**5. Recovery incidents.** How many times did you need to recover a blocker,
refused finish, infrastructure-related gate failure, or worktree cleanup?

- Number of incidents: ______
- Approximate operator time: ______
- Did status or unblock explain the next step? ____________________

## Continued use and friction

**6. Continued use.** Will you keep routing agent work through the broker?

- [ ] yes, for all agent work  [ ] for some of it  [ ] no

Why? ____________________

**7. Value.** If the broker disappeared tomorrow, what would you miss first, if
anything? ____________________

**8. What would you cut?** Which command, output, step, or rule should be
removed or made optional, and why? ____________________

**9. Anything else.** What should the team fix before the next internal
observation window? ____________________

Summarize responses with the [internal dogfooding protocol](../guides/internal-dogfooding.md).
Do not treat this small internal sample as external adoption evidence or proof
of product-market fit.
