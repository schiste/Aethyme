# Open issue priorities: 2026-10-21

Last Updated: 2026-10-21

Source audit: `9cb14b76bb17bf7ab2027596ccc9e0903860659d`. Inventory: **61 open issues and 15 open PRs**.

## Scope and interpretation

Implementation includes Local, broker, engine and release. Coordination and Git Platform remain excluded. External tool reports are classified but require their owning maintainers. Evaluations use synthetic Playground repositories.

Rank combines supported safety/resource impact, frequency and downstream value. It does not imply that each ticket still needs implementation. Qualification gaps, completed bounded experiments and conditional work must retain their limitations. Effort and dependencies determine execution order within these priorities.

The accompanying [JSON inventory](open-issue-priorities-2026-10-21.json) stores every issue, evidence, dependency, remaining gap and next action. This report is a point-in-time audit, not a live issue-state mirror.

## Execution order

1. Finish #713 foreground supervision invariant and regressions.
2. Qualify existing #924/#931 fixes on current main, then merge only with passing required checks.
3. Recover #930/#927/#933 performance and overlap slices without duplicating their PRs.
4. Resolve root-binding integration and signing/CI regressions before release metadata.
5. Close exact Local contract/qualification gaps in dependency order; retain failed and unsupported outcomes.
6. Test the combined source and measure matched CLI/engine binaries on fresh and aged Playground workloads before rollout.

## Ranked inventory

| Rank | Priority | Issue | Evidence status and remaining work |
| --- | --- | --- | --- |
| 1 | P0 | [#713](https://github.com/schiste/Aethyme/issues/713) [Bug]: Brokered build survives terminal timeout without returning an outcome | TTY mode selects the unsupervised legacy runner, allowing a child to outlive an externally terminated caller. |
| 2 | P0 | [#765](https://github.com/schiste/Aethyme/issues/765) Broker: own and reap orphaned processes and per-worktree services; surface idle sessions under memory pressure | Non-TTY supervision and managed resource/debris ownership exist; arbitrary process/service ownership and pressure-ranked idle-session guidance remain. |
| 3 | P0 | [#659](https://github.com/schiste/Aethyme/issues/659) [Local v3 / L2] Reclaim expired collaboration data with reference-safe GC | D25 reservation/reclamation coupling remains open; full disk does not trigger reclaim; host/daemon pressure not generally qualified. |
| 4 | P0 | [#673](https://github.com/schiste/Aethyme/issues/673) [Local v3 / L6] Bound verifier resources, cache keys, and cleanup | D25 full host/daemon ENOSPC with concurrency remains; host admission is not global; resource recovery is not budget/acceptance receipt. |
| 5 | P0 | [#658](https://github.com/schiste/Aethyme/issues/658) [Local v3 / L2] Make capture intent, record, and receipt crash-safe | D09 Linux and native mid-journal ENOSPC unqualified; D26 is separate sync boundary; legacy verdict must stay independent. |
| 6 | P0 | [#656](https://github.com/schiste/Aethyme/issues/656) [Local v3 / L2] Isolate collaboration state from broker state and cleanup | Power-loss and cross-resource atomicity are not claimed; Linux D09 and full old-client cleanup matrix incomplete. |
| 7 | P0 | [#335](https://github.com/schiste/Aethyme/issues/335) Worktree lifecycle: prevent persistent disk and branch debris across agent sessions | Cleanup recovery, reclamation allowlists, storage inventory and GC exist. Remaining gaps need narrowly scoped child fixes. |
| 8 | P0 | [#909](https://github.com/schiste/Aethyme/issues/909) broker finish: partial finish leaves tracked files deleted; cleanup then refuses without --force | The reported tracked-deletion interruption has a recovery implementation and exact CLI positive/negative regressions already on main; fresh local execution remains to confirm before closing. |
| 9 | P1 | [#892](https://github.com/schiste/Aethyme/issues/892) Avoid CPU-heavy full-patch git log during concurrent submits | Submission still builds/replays complete patch history before queue admission and during replay. Existing PR930 guards same-invocation prepared-plan reuse. |
| 10 | P1 | [#706](https://github.com/schiste/Aethyme/issues/706) Make broker GitHub admission timeouts bounded and observable | GitHub reads still default to90seconds although reported caller terminates at45seconds. Intentional long reads need caller-selected bounds preserved. |
| 11 | P1 | [#460](https://github.com/schiste/Aethyme/issues/460) broker status (incl. --summary and doctor) never returns at scale: serial per-worktree git status + per-commit merge-base/diff --binary/patch-id chain with no cache or bound | Brief status avoids checkout/history Git; full status has one7second inspection deadline and budget-cut output. No current reproduction of original hang. |
| 12 | P1 | [#893](https://github.com/schiste/Aethyme/issues/893) Keep agent worktrees fresh against the integration branch | New starts select fresh integration/default; explicit reuse sync exists. Running worktrees lack a recorded freshness observation and pre-task stale check. |
| 13 | P1 | [#760](https://github.com/schiste/Aethyme/issues/760) Broker: surface default-branch breakage ownership and task overlap before duplicate work starts | Open-PR path/line overlap exists, but default-branch incident ownership and task-text overlap remain. |
| 14 | P1 | [#767](https://github.com/schiste/Aethyme/issues/767) [bug] Patch lint formats Rust files as the wrong edition | Aethyme FormatFixer must honor the nearest/workspace Cargo edition and fail closed on invalid metadata. This does not establish that Hermes' patch tool routes through Aethyme, so closing the Hermes-specific report requires caller evidence. |
| 15 | P1 | [#887](https://github.com/schiste/Aethyme/issues/887) broker: repo-level defaults for ready PRs and PR watch, plus a built-in watch tick | PR watch remains opt-in; push --pr is draft-only and no background poller runs. |
| 16 | P1 | [#213](https://github.com/schiste/Aethyme/issues/213) graph: rebuild capability in vertical slices around the three stated uses — CI sizing, nightly doc impact, session bootstrap | Tranche 0's per-language, per-use capability report is not present. Existing coverage is inventory/count-based, not a claim that CI sizing, doc impact, or session bootstrap is answerable. Tests and Documents producers are conservative/heuristic, so fixtures must establish precision/recall boundaries and graph health must surface unsupported cases. Surface-flow quality/fallback criteria also remain separate work. |
| 17 | P1 | [#655](https://github.com/schiste/Aethyme/issues/655) [Local v3 / L1] Add the exact-source Analysis envelope and baseline adapter | L1 analysis doc explicitly records L0 follow-up 3: producer_version is not checked on the envelope read path; profile metadata is only useful when an actual consumer resolves the pinned profile.; D38/D39/D44/D47 and broad analysis coverage remain open; do not infer generic stale acceptance without resolving the profile reference. |
| 18 | P1 | [#668](https://github.com/schiste/Aethyme/issues/668) [Local v3 / L5] Reserve execution budgets before dispatch and preserve retry lineage | Resource receipt/cleanup coupling and live workers remain L6-dependent; D15/D25 and full T43/T44 open; no host-capacity guarantee. |
| 19 | P1 | [#669](https://github.com/schiste/Aethyme/issues/669) [Local v3 / L5] Fence stop, restore, revoke, and stale controller outcomes | D16/D29 and external publisher/signing/reconciliation interleavings remain; restoring DB does not prove process death. |
| 20 | P1 | [#670](https://github.com/schiste/Aethyme/issues/670) [Local v3 / L6] Freeze exact source, build inputs, and materialized execution identity | T13 actual-executor and full toolchain/generated-input identity are bounded; unsupported filters/submodules must refuse. |
| 21 | P1 | [#672](https://github.com/schiste/Aethyme/issues/672) [Local v3 / L6] Produce independent, non-forgeable verification evidence | Full Q3/T24-T28 exact-candidate, stale/revoked, cleanup/resource coupling remains; verifier is not provider acceptance. |
| 22 | P1 | [#671](https://github.com/schiste/Aethyme/issues/671) [Local v3 / L6] Supervise and isolate candidate execution | No VM/root boundary or protection from host administrator; daemon resource scope and D07/D25/D29 remain limited. |
| 23 | P1 | [#681](https://github.com/schiste/Aethyme/issues/681) [Local v3 / L9] Preserve old-binary, old-policy, and repository compatibility | Not a universal matrix for I01/I02/network denial/shared clone/migration; already-open old clients need quiescence. |
| 24 | P1 | [#660](https://github.com/schiste/Aethyme/issues/660) [Local v3 / L2] Integrate capture as explicit opt-in beside legacy submit | Paired old/new binary and T64 public/native rehearsal remain; old open clients need quiescence before migration. |
| 25 | P1 | [#666](https://github.com/schiste/Aethyme/issues/666) [Local v3 / L5] Define target revisions and one authoritative local controller | Publisher/signing/restore and D16/D29 not closed; no general autonomy claim; old open clients beyond fresh-open floors. |
| 26 | P1 | [#667](https://github.com/schiste/Aethyme/issues/667) [Local v3 / L5] Schedule finite candidate waves with starvation protection | All six FX17 model rows failed before candidates at provider schema boundary; D14 general fairness/indefinite arrivals remains open. |
| 27 | P1 | [#675](https://github.com/schiste/Aethyme/issues/675) [Local v3 / L7] Bind acceptance to configured GitHub/Mergify policy | No designated Playground live qualification. This is Local observation only: no branch protection changes, queue policy or Mergify enablement; Coordination/Git Platform excluded. |
| 28 | P1 | [#677](https://github.com/schiste/Aethyme/issues/677) [Local v3 / L8] Prepare an accepted candidate as a fresh workspace generation | Depends on #675 live acceptance; one joined path not general remote-source mapping; squash/rebase requires content revalidation. |
| 29 | P1 | [#678](https://github.com/schiste/Aethyme/issues/678) [Local v3 / L8] Acknowledge generation handoff and fence stale agents | No VM/process-tree confinement; Native gate join is present; positive public handoff/T64 and descendant settlement remain to qualify; a pre-GO receipt does not prove workload ran. |
| 30 | P1 | [#682](https://github.com/schiste/Aethyme/issues/682) [Local v3 / QA] Qualify Local retention, composition, recovery, and progress claims | T64 public rehearsal, live provider, host/daemon and mid-journal pressure, power-loss claims remain; FX17 six rows failed before candidates; no D30 or global reliability claim. |
| 31 | P1 | [#680](https://github.com/schiste/Aethyme/issues/680) [Local v3 / L9] Add explicit opt-in Local collaboration commands and policy | Fresh T64 public/native rehearsal and two-contributor path remain; target run/remote sync/positive handoff refuse unbound profiles. |
| 32 | P1 | [#652](https://github.com/schiste/Aethyme/issues/652) [Local v3 / L1] Define project, contribution, and exact source identities | No ratified minting/canonical identity lifecycle; T03/T05 and actual consumer parity remain; shared schemas experimental. |
| 33 | P1 | [#653](https://github.com/schiste/Aethyme/issues/653) [Local v3 / L1] Specify canonical, versioned contribution and receipt records | Concrete records and old/new compatibility schemas remain experimental; T85, D18/D32/D47 open. |
| 34 | P1 | [#657](https://github.com/schiste/Aethyme/issues/657) [Local v3 / L2] Retain exact source blobs and contribution lineage | Filters, submodules, LFS and disk/remote durability excluded; D20 retention/receipt boundary remains. |
| 35 | P1 | [#663](https://github.com/schiste/Aethyme/issues/663) [Local v3 / L4] Extract a candidate boundary and keep the legacy Git adapter | L4 docs contain pre-E1 historical statements; D04-D06/D12/D18 and Q3 whole-candidate gates remain incomplete. |
| 36 | P1 | [#665](https://github.com/schiste/Aethyme/issues/665) [Local v3 / L4] Return bounded resolution requests and exact candidate manifests | Pre-E1 text remains stale; generic exact-candidate manifest/re-analysis and Q3/T20-T23/T80/T87 are not all qualified. |
| 37 | P1 | [#664](https://github.com/schiste/Aethyme/issues/664) [Local v3 / L4] Compose pinned contributions under one bounded profile | Original FX04 failed; held-outs not rerun; support only bounded HTML/task distribution and no universal composition. |
| 38 | P1 | [#650](https://github.com/schiste/Aethyme/issues/650) [Local v3 / E1] Qualify a bounded composition strategy on Playground fixtures | Only bounded external HTML distribution; original FX04 semantic case failed; evidence is split rather than one Q1 JSON; frozen held-outs not rerun. |
| 39 | P1 | [#649](https://github.com/schiste/Aethyme/issues/649) [Plan v3 / Local] Track contextual contribution workflow and revision-bound analysis | Live GitHub/Mergify, T64, Linux/mid-journal pressure, identity/schema decisions and usefulness/reliability remain incomplete. |
| 40 | P2 | [#661](https://github.com/schiste/Aethyme/issues/661) [Local v3 / L3] Retrieve a bounded, explainable contribution context | No reliable brief usefulness; dynamic dependencies stay partial/unknown; D27 hot-project scale unmeasured; no multi-member ACL claim. |
| 41 | P2 | [#662](https://github.com/schiste/Aethyme/issues/662) [Local v3 / L3] Invalidate context caches for new matches and visibility changes | Dynamic dependency/broad-risk bounds provisional; one Local reader class; external membership/ACL provider is excluded Coordination. |
| 42 | P2 | [#674](https://github.com/schiste/Aethyme/issues/674) [Local v3 / L7] Transport exact retained candidates over existing Git paths | No remote durability claim; absent/changed refs do not prove no prior write; ambiguity stays in broker journal; live provider is #675. |
| 43 | P2 | [#676](https://github.com/schiste/Aethyme/issues/676) [Local v3 / L8] Add resumable Local inbox/outbox synchronization | Receipt cannot claim remote source retention/verification; membership provider and real Coordination service out of scope. |
| 44 | P2 | [#903](https://github.com/schiste/Aethyme/issues/903) Measure end-to-end savings from worktree and dependency reuse | Measure lifecycle reuse with existing preparation/cache contracts rather than adding mutable shared dependency trees. |
| 45 | P2 | [#902](https://github.com/schiste/Aethyme/issues/902) Evaluate Git many-files defaults for broker repositories | Do not enable the manyFiles bundle at the declared Git 2.38 floor. |
| 46 | P2 | [#901](https://github.com/schiste/Aethyme/issues/901) Measure Git fsmonitor benefit across broker worktree workloads | Leave fsmonitor opt-in until local and external clean/dirty/untracked/ignored-output broker workloads justify the daemon cost. |
| 47 | P2 | [#907](https://github.com/schiste/Aethyme/issues/907) Prototype sparse checkout for broker-managed agent worktrees | Keep full checkouts as the production default while sparse safety is unqualified. |
| 48 | P2 | [#906](https://github.com/schiste/Aethyme/issues/906) Assess Git per-worktree config for broker session settings | Defer automatic worktreeConfig migration until a concrete scoped setting needs it. |
| 49 | P2 | [#904](https://github.com/schiste/Aethyme/issues/904) Evaluate reftable for broker-heavy Git repositories | Keep existing repositories on the files ref backend. |
| 50 | P3 | [#654](https://github.com/schiste/Aethyme/issues/654) [Local v3 / L1] Pin the 150-token decision brief profile | TS consumer and CLI validation remain; FX08 did not prove reliable usefulness or close D30. |
| 51 | P3 | [#709](https://github.com/schiste/Aethyme/issues/709) [Local v3 / FX08] Brief usefulness: frozen fixtures, brief variants, oracle and a pre-L3 pilot | D30 remains open; current report path differs from requested evidence filename; model/charges/retry details have unknowns; never rerun consumed rows. |
| 52 | P3 | [#679](https://github.com/schiste/Aethyme/issues/679) [Local v3 / L8] Export and restore retained Local records safely | Host config, credentials, active leases and current provider/agent authority excluded; imported evidence historical only. |
| 53 | P3 | [#813](https://github.com/schiste/Aethyme/issues/813) Add opt-in end-of-session skill evolution review | No opt-in config, session-log export, idempotent skill-review job, configured model validation, close-triggered asynchronous adapter launch, or skill-change/result report exists in the reviewed broker contract. Full chronological transcripts are not present in the durable handoff. |
| 54 | P4 | [#685](https://github.com/schiste/Aethyme/issues/685) [Local v3 / X3] Report advisory impact for exact combined candidates | Must analyze candidate C itself; graphs A/B cannot be stitched; no gate/lease/acceptance authority. |
| 55 | P4 | [#684](https://github.com/schiste/Aethyme/issues/684) [Local v3 / X2] Prototype isolated one-base private analysis views | Do not reimplement incremental overlays; D42/D49 only reopen on measured need. |
| 56 | P4 | [#683](https://github.com/schiste/Aethyme/issues/683) [Local v3 / X1] Experiment with an optional pinned Rust SCIP import | No demonstrated need or approved producer profile; producer execution not authorized solely by issue scope. |
| 57 | P4 | [#686](https://github.com/schiste/Aethyme/issues/686) [Local v3 / X4] Evaluate SPARTA only for a demonstrated fixed-point need | No need/benefit evidence; solver dependency would invert plan order. |
| 58 | External | [#766](https://github.com/schiste/Aethyme/issues/766) [Integration] Make Google Workspace setup runnable from Hermes runtime | Hermes PM setup/runtime activation must expose a deterministic supported command or fail promptly with the required action. |
| 59 | External | [#772](https://github.com/schiste/Aethyme/issues/772) [Bug] terminal returns exit -1 for long multi-argument commands without diagnostics | The terminal wrapper must enforce/diagnose argument or command limits; Aethyme does not own the reported execution surface. |
| 60 | External | [#780](https://github.com/schiste/Aethyme/issues/780) [Tool friction] Browser Use CLI fails to start when its stdout file is missing | Browser process startup and stale-tab recovery belong to the external browser tool. |
| 61 | External | [#770](https://github.com/schiste/Aethyme/issues/770) [Bug] web_extract returns 429 for public pages in a multi-URL batch | External fetch backend needs per-URL retry/backoff honoring Retry-After or a clear retryable result/fallback. Avoid assuming the Aethyme repo can fix provider rate limits. |

## Current integration risks

- Existing PRs must be reviewed and qualified against the current source; old green checks do not prove current integration. #924 and #931 have been synced during this sweep. Their new qualification remains pending.
- #941 has contract/workspace failures; #955 has a release-signing contract failure. Fix those assertions and behavior without weakening release identity or verification. Reconcile #954 release metadata afterwards.
- #886/#889/#891/#928 overlap root identity and recovery. Integrate one reviewed result rather than merge competing stale branches blindly.
- #927, #930 and #933 cover separate cleanup, submission and planned-path overlap slices; none closes an umbrella issue by itself.
- #958/#959 are dependency maintenance, with lower direct user impact than current process, storage and latency gaps.

## Git configuration findings

- Keep fsmonitor and many-files settings opt-in until broker end-to-end evidence supports them. Git documents that many-files enables skipped index checksums, which older fsck clients reject; the declared Git 2.38 floor needs explicit compatibility qualification. [Git configuration](https://git-scm.com/docs/git-config).
- Keep existing repositories on files refs: the documented migration does not handle repositories with worktrees and does not block concurrent writes. Benchmark and rollback qualification remain open. [Git refs migration](https://git-scm.com/docs/git-refs).
- Sparse checkout needs isolated per-worktree settings and missing-input checks before production use. The Git prototype does not establish that Aethyme context and gate consumers tolerate omitted tracked files. [Git sparse checkout](https://git-scm.com/docs/git-sparse-checkout).

## Claim limits

E1 already has a bounded HTML profile; do not rerun consumed held-outs or claim universal composition. FX08 records a completed, inconclusive usefulness campaign and does not close D30. FX17 failed before candidate generation. Fresh analysis views remain supported; the measured overlay experiment stays stopped. Optional SCIP and SPARTA work requires demonstrated need and a pinned execution profile.

Linux/power-loss, mid-journal pressure, live provider observation and full public/native T64 remain explicit qualification gaps. A documented unsupported mode is not automatically a source defect. The Local acceptance observer does not authorize enabling Mergify, changing provider policy or building excluded products.

No new local native pass, merged fix, end-to-end latency gain or deployment is claimed by this inventory. The implementation and qualification records must establish those separately.
