# Pilot exit survey

Last Updated: 2026-09-26

One copy per developer, at the end of week two. About 15 minutes. Estimates
are fine; say so when a number is a guess. Send answers as plain text in any
format.

Your team: ______  Your role: ______  Agents you ran at once, typically: ______

## Setup

**1. Time to first submit.** How long from starting the install to your first
successful `aethyme broker submit`? Include time spent writing or trimming
gates.

- [ ] under 30 minutes  [ ] 30–60 minutes  [ ] 1–2 hours  [ ] half a day
  [ ] more  [ ] never got there

**2. The hardest step.** Which setup step took longest or needed help:
install, `aethyme init`, the gates draft, `trust`, the first `start`, the
first `submit`, something else? What happened?

## Conflicts caught

**3. Overlaps and conflicts.** Over the two weeks, how many times did the
broker tell you about an overlap (`status`) or refuse a submit because of a
conflict with another session, before the problem reached your default
branch?

- Overlap warnings you noticed: ______
- Submits refused for a conflict: ______

**4. Would they have hurt?** Of those, how many would you have hit anyway
without the broker, as a merge conflict, a broken build or lost work? Give
one example if you can.

## Recovery

**5. Recovery incidents.** How many times did you have to stop and recover
the broker or a session: a stuck blocker, a refused `finish`, a gate failing
for reasons unrelated to your change, a worktree you had to clean up by hand,
anything else?

- Number of incidents: ______
- Total time spent on them: ______

**6. The worst one.** Describe it: what you saw, what you ran, how long it
took, and whether `status` or `unblock` told you what to do.

## Keeping it

**7. Week three.** After week two, did you keep routing agent work through
the broker?

- [ ] yes, for all agent work  [ ] for some of it  [ ] no

If not for all of it, why not?

**8. Would you notice?** If we took the broker away tomorrow, what would you
miss first, if anything?

## Cutting

**9. What would you cut?** Name any command, output, step or rule you would
remove or make optional. Say why.

**10. Anything else.** What should we fix before the next team starts?
