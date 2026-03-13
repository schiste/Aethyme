### 2026-03-12 — bug-fix: Codex on Playground Control (isolated repos)

Aethyme commit: `0578ff5`
Agent: Codex (gpt-5.4), sandbox `workspace-write`
Isolation: 4 independent `git clone --local` at `/tmp/benchmark-bug-fix-001/`
First run with per-condition repo isolation (via `repos.py` + `prepare_bug_fix_benchmark()`).

**Reference answer:** Bug in `packages/auth/src/rbac-canonical.ts` — `Action.SHARE` removed from `PERMISSION_IMPLICATIONS[Action.MANAGE]`. Fix: restore the line.

#### Starting Prompts

**Control (CTO off and CTO on — same prompt, different CTO setting):**

```
A test is failing in this repository.

Repository path: /private/tmp/benchmark-bug-fix-001/<CONDITION>
Test file: packages/auth/src/__tests__/ability-implications.test.ts
Run command: npx vitest run packages/auth/src/__tests__/ability-implications.test.ts

Failure output:
FAIL packages/auth/src/__tests__/ability-implications.test.ts
  x permission implications > manage:suppliers grants share permission via ability builder
    -> expected true to be false
  x permission implications > manage:suppliers grants all expected permissions
    -> share check failed
  x permission implications > getImpliedActions for manage includes share
    -> expected array to contain 'share'
  x permission implications > actionImplies correctly checks manage -> share
    -> expected false to be true

The test file is correct. The bug is in the source code.
Find the root cause and fix the bug so all tests pass.
You can verify your fix by running the test command above.
```

**Explore:** Same as control. Aethyme skill deployed at `.codex/skills/aethyme/SKILL.md`.

**Leverage:**

```
Use Aethyme tools to navigate the repository graph.

A test is failing in this repository.

Repository path: /private/tmp/benchmark-bug-fix-001/leverage
Test file: packages/auth/src/__tests__/ability-implications.test.ts
Run command: npx vitest run packages/auth/src/__tests__/ability-implications.test.ts

Failure output:
FAIL packages/auth/src/__tests__/ability-implications.test.ts
  x permission implications > manage:suppliers grants share permission via ability builder
    -> expected true to be false
  x permission implications > manage:suppliers grants all expected permissions
    -> share check failed
  x permission implications > getImpliedActions for manage includes share
    -> expected array to contain 'share'
  x permission implications > actionImplies correctly checks manage -> share
    -> expected false to be true

The test file is correct. The bug is in the source code.
Find the root cause and fix the bug so all tests pass.
You can verify your fix by running the test command above.
```

Navigation context file at `/tmp/aethyme-eval-navigation-context.json` was generated but **not referenced in the leverage prompt** — the prompt only says "Use Aethyme tools to navigate the repository graph." Aethyme skill deployed at `.codex/skills/aethyme/SKILL.md`.

#### Scores

| | Control (CTO off) | Control (CTO on) | Explore | Leverage |
|---|---|---|---|---|
| Score | 100 / 100 | 100 / 100 | 100 / 100 | 100 / 100 |
| Tokens | 138,724 | 86,632 | 107,897 | 132,568 |
| Run Time | not recorded | not recorded | not recorded | not recorded |

#### Per-field breakdown

| Field (weight) | Control (CTO off) | Control (CTO on) | Explore | Leverage |
|---|---|---|---|---|
| fix_test (60) | pass | pass | pass | pass |
| regression (20) | pass (10/10 tests) | pass (10/10 tests) | pass (file verified) | pass (file verified) |
| correct_file (10) | `packages/auth/src/rbac-canonical.ts` — correct | `/private/tmp/.../rbac-canonical.ts` — correct | `/private/tmp/.../rbac-canonical.ts` — correct | `packages/auth/src/rbac-canonical.ts` — correct |
| efficiency (10) | 138,724 (worst) | 86,632 (best) | 107,897 | 132,568 |

All conditions produced identical one-line diffs: `+    Action.SHARE,` added to `PERMISSION_IMPLICATIONS[Action.MANAGE]`.

#### Qualitative Assessment

**Control CTO off** (100/100, 138,724 tokens): Found the bug through direct file exploration. 35 tool calls (26 shell, 6 stdin, 3 plan updates). 107,646 chars of tool output — highest among conditions *with CTO active* but inflated by uncompressed terminal output. Verified fix with `node -e` since `npx vitest` was blocked by the workspace-write sandbox (no network). Clean root cause analysis referencing backend canonical source drift.

**Control CTO on** (100/100, 86,632 tokens): Most token-efficient condition. 40 tool calls (29 shell, 11 stdin) but only 79,345 chars of tool output — CTO compressed terminal output by ~26% vs CTO-off. Followed the same trace: test imports → `rbac-canonical.ts` → missing `Action.SHARE`. Attempted vitest verification but correctly reported sandbox network restriction.

**Explore** (100/100, 107,897 tokens): 49 tool calls (35 shell, 11 stdin, 3 plan updates). 84,973 chars of output. The Aethyme skill was deployed but **Aethyme engine calls failed** — the Codex sandbox intercepts the `cargo` binary via CTO shims (`Operation not permitted`). The explore condition effectively degenerated to control with extra failed tool attempts. The 11 stdin writes and 49 total calls suggest the agent tried interactive Aethyme CLI invocations before falling back.

**Leverage** (100/100, 132,568 tokens): Most interesting and most problematic condition. 43 tool calls including **9 Chau7 MCP calls** (4 `tab_status`, 1 `tab_create`, 1 `tab_list`, 1 `tab_set_cto`, 1 `tab_exec`, 1 `runtime_session_create`). The prompt said "Use Aethyme tools" but the Rust engine was sandbox-blocked, so the agent **discovered and exploited Chau7 MCP tools** from its tool list. It:
1. Called `tab_create` to try spawning a new terminal tab
2. Called `tab_list` (4,751 chars response — full metadata for all open tabs)
3. Called `tab_set_cto(forceOff)` on its own tab — **self-sabotaging CTO compression**
4. Checked 4 other tab statuses
5. Tried `runtime_session_create` to launch a new agent session
6. Called `tab_exec` on its own tab to attempt an Aethyme CLI command

After these detours, it fell back to direct file exploration. But with CTO now forceOff (self-inflicted), all subsequent shell output was uncompressed, producing 130,251 chars of output — 64% more than control-cto-on's 79K.

Additionally, the navigation context file (if the agent had read it) was **actively misleading**: anchors pointed to `gcp-run-proxy/test`, `backend/tests`, `config/observability` — none relevant to `packages/auth/`. Worse, `packages/auth` was explicitly listed as **out of scope**. The engine's task context resolution failed to connect the bug-fix task to the actual bug location.

#### Tool Call Analysis

| | Control (CTO off) | Control (CTO on) | Explore | Leverage |
|---|---|---|---|---|
| exec_command | 26 | 29 | 35 | 27 |
| write_stdin | 6 | 11 | 11 | 5 |
| update_plan | 3 | 0 | 3 | 2 |
| mcp__chau7__* | 0 | 0 | 0 | 9 |
| **Total** | **35** | **40** | **49** | **43** |
| Output chars | 107,646 | 79,345 | 84,973 | 130,251 |

Explore made the most tool calls (49) — the extra 14 over control likely reflect failed Aethyme engine attempts. Leverage had fewer shell commands (27) than any other condition but its 9 MCP calls and self-sabotaged CTO inflated total tokens.

#### Context Pack Audit

Navigation context file: `/tmp/aethyme-eval-navigation-context.json` (4,581 bytes)

| Metric | Value |
|---|---|
| Anchors | 3 (`gcp-run-proxy/test`, `backend/tests`, `config/observability`) |
| In-scope files | 0 |
| In-scope symbols | 0 |
| In-scope areas | 3 (same as anchors) |
| Out-of-scope areas | 143 (including `packages/auth` — the actual bug location) |
| File contents | empty |
| Navigation order | 3 items (same as anchors) |

**Signal-to-noise: NEGATIVE.** The navigation context is worse than no context:
- All 3 anchors are irrelevant to the bug (`gcp-run-proxy/test`, `backend/tests`, `config/observability`)
- The actual bug area (`packages/auth`) is explicitly marked **out of scope**
- Zero in-scope files or symbols — no useful targeting data
- Empty file contents — no pre-loaded code

The engine's `build_task_pack()` failed to connect "Fix failing test: manage permission does not imply share in ability-implications.test.ts" to `packages/auth/src/`. This is a **generic engine deficiency** in task-to-area resolution, not an eval-specific issue — the task text contains the file path but the engine doesn't extract it.

**Note per Cardinal Rule 2:** This observation diagnoses a generic weakness. Any fix must improve task context resolution across all task types, not add bug-fix-specific heuristics.

#### Comparison

| Dimension | Control (CTO off) | Control (CTO on) | Explore | Leverage |
|---|---|---|---|---|
| Target accuracy | Correct file, correct fix | Correct file, correct fix | Correct file, correct fix | Correct file, correct fix |
| Reasoning quality | Clean — traced imports from test | Clean — traced imports from test | Same as control (Aethyme unavailable) | Same as control after MCP detour |
| Fix verification | `node -e` runtime verification | Reported sandbox limitation | `node --experimental-strip-types` | `node --experimental-strip-types` |
| Token efficiency | 138,724 (worst) | 86,632 (best) | 107,897 | 132,568 |
| Tool behavior | Normal | Normal | Failed Aethyme calls, fell back | Chau7 MCP exploitation, self-sabotaged CTO |
| Unique behavior | None | None | 49 calls (most of any condition) | Used Chau7 tools to try creating sessions |

#### Verdict

All 4 conditions ceiling at 100/100 — the bug-fix task is too easy to differentiate conditions. The test failure output directly names the test file, which imports from `../rbac-canonical`, making the root cause discoverable in 2-3 file reads regardless of tooling.

The only meaningful gradient is **token efficiency**: control-cto-on (87K) < explore (108K) < leverage (133K) < control-cto-off (139K). CTO compression is the dominant factor — CTO-on saves ~37% vs CTO-off. Aethyme tools provided no benefit because (a) the Rust engine was sandbox-blocked for explore, and (b) the navigation context was actively misleading for leverage.

Leverage's high token count has three root causes: (1) the agent discovered Chau7 MCP tools and spent tokens on 9 cross-session calls, (2) it self-sabotaged CTO by calling `tab_set_cto(forceOff)`, and (3) the prompt's "Use Aethyme tools" instruction sent it searching for tools that couldn't work in the sandbox.

#### Graph Quality Notes

The engine's task context resolution is the primary quality issue revealed by this run. Given the task text "Fix failing test: manage permission does not imply share in ability-implications.test.ts", a functioning context resolver should:
1. Extract the file path from the task text (`ability-implications.test.ts`)
2. Locate it in the graph → `packages/auth/src/__tests__/ability-implications.test.ts`
3. Anchor on `packages/auth/` as the relevant area

Instead, the engine produced anchors in `gcp-run-proxy/test`, `backend/tests`, and `config/observability` — entirely unrelated areas. This suggests the task-to-anchor pipeline relies on keyword overlap (matching "test" to test directories) rather than file path extraction. The graph itself may be correct; the task resolution layer is the weak link.

#### Prompt Effectiveness

The **control/explore prompt** is well-designed for this task: it includes the test file path, run command, and failure output, giving the agent everything needed to trace the bug.

The **leverage prompt** has two problems:
1. It says "Use Aethyme tools to navigate the repository graph" but doesn't reference the navigation context file. The agent has no way to know about `/tmp/aethyme-eval-navigation-context.json` unless it's injected as a file path or env var.
2. The "Aethyme tools" phrasing is ambiguous in a Codex sandbox where the Rust engine can't run — the agent interprets it broadly and discovers Chau7 MCP tools.

The leverage prompt should either reference the navigation context file explicitly or be identical to control when the nav context is poor.

#### Lessons & Action Items

- [ ] **Design a harder bug-fix eval** — the current task is solvable from test imports alone. A harder eval should require navigating multi-hop dependencies where symptom and root cause are in distant modules.
- [ ] **Fix task-to-area resolution** — the engine should extract file paths from task text and use them for anchoring. This is a generic improvement (Cardinal Rule 2 compliant).
- [ ] **Reference nav context file in leverage prompt** — `_build_bug_fix_prompt(leverage=True)` should include the file path, e.g., "Read the navigation context at /tmp/aethyme-eval-navigation-context.json".
- [ ] **Sandbox-proof the Aethyme skill** — the Codex `workspace-write` sandbox blocks cargo. Either pre-build the engine binary and invoke it directly, or fall back gracefully when cargo is unavailable.
- [ ] **Prevent MCP tool leakage** — leverage agent should not be able to call Chau7 MCP tools (tab_create, tab_set_cto, etc.). Investigate whether Codex exposes MCP tools from the host environment.
- [ ] **Pre-install node_modules in clones** — `git clone --local` doesn't include gitignored `node_modules/`. Add `pnpm install --frozen-lockfile` to `create_condition_repos()` or document as a required post-step.
- [ ] **Record wall-clock timing** — note start/end times per condition for the Scores table.
- [ ] **Capture Chau7 telemetry per protocol** — `codex exec` runs don't register as Chau7 telemetry runs. Investigate tab-based run detection or add explicit telemetry capture.
- [ ] **Store artifacts per protocol** — this run wrote results to `/tmp/` but not to the `eval-runs/` directory structure required by the Local Eval Storage contract.
