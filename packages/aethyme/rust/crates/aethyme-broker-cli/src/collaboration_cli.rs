//! `aethyme collab`: the opt-in Local collaboration commands (#680, plan
//! §6.7-6.8; D18, D32). This first slice covers what the L2/L3 packages
//! provide: inspection, capture recovery, reclamation, context retrieval,
//! decision briefs and explicit enrollment. Target, sync, handoff, stop and
//! export are extended by the lifecycle slice below; unavailable native profiles
//! refuse without dispatching work.
//!
//! - **Disabled by default.** Collaboration is enabled exactly when the
//!   repository's `[collaboration] capture` is `advisory` or `required` with
//!   a valid `project`, read by the same fail-closed reader as `submit`
//!   (#660). `status` and `enroll` always work; every other subcommand
//!   refuses with the setting to change.
//! - **Looking never writes.** `status` checks the state root and reads an
//!   existing store through a read-only snapshot (no pragmas, migrations or
//!   created files), and reads the fence without raising it. Only `broker
//!   submit` creates collaboration state; every other subcommand refuses with
//!   `not_initialized` until it exists, and then opens it writable (which
//!   migrates an older store).
//! - **No background work.** Every subcommand runs in the foreground and
//!   exits; nothing here starts a daemon or contacts a service.
//! - **Versioned output.** `--json` prints one object whose `schema` names
//!   its shape (`aethyme.collab-<command>/experimental-v0`). A refusal is
//!   `aethyme.collab-error/experimental-v0` with `code` and `next_action`.

use std::io::Read;
use std::path::{Path, PathBuf};

use aethyme_contracts::experimental_v0::RecordId;
use aethyme_contracts::experimental_v0::SourceSnapshotId;
use aethyme_contracts::experimental_v0::analysis::AnalysisEnvelope;
use aethyme_contracts::experimental_v0::brief::Brief;
use serde_json::{Value, json};

use crate::collaboration_capture::{OperationId, abort, receipt, recover};
use crate::collaboration_context::{Budget, ContextQuery, LocalProject, Served, attach_brief};
use crate::collaboration_gc::{GcOptions, apply as gc_apply, plan as gc_plan, resume as gc_resume};
use crate::collaboration_state::{
    CollaborationStore, ProjectKey, inspect_for_repository, locate_for_repository,
    open_for_repository,
};
use crate::collaboration_submit::{Setting, setting};
use crate::exit_status;

#[path = "collaboration_cli/context_revisions.rs"]
mod context_revisions;
#[path = "collaboration_cli/contribution.rs"]
mod contribution;
#[path = "collaboration_cli/lifecycle.rs"]
mod lifecycle;
#[path = "collaboration_cli/project.rs"]
mod project;
#[path = "collaboration_cli/project_association.rs"]
pub(crate) mod project_association;
#[path = "collaboration_cli/resolution.rs"]
mod resolution;
#[cfg(test)]
pub(crate) use project::test_support as project_test_support;
#[path = "collaboration_cli/sensitive.rs"]
mod sensitive;

#[cfg(test)]
pub(crate) fn project_command_for_test(
    main_root: &Path,
    worktree: &Path,
    rest: &[&str],
) -> Result<Value, String> {
    project::command(main_root, worktree, rest)
        .map(|(value, _)| value)
        .map_err(|failure| format!("{}: {}", failure.code, failure.message))
}

pub const STATUS_SCHEMA: &str = "aethyme.collab-status/experimental-v0";
pub const CAPTURE_RECOVER_SCHEMA: &str = "aethyme.collab-capture-recover/experimental-v0";
pub const CAPTURE_ABORT_SCHEMA: &str = "aethyme.collab-capture-abort/experimental-v0";
pub const CAPTURE_RECEIPT_SCHEMA: &str = "aethyme.collab-capture-receipt/experimental-v0";
pub const CAPTURE_RELEASE_SCHEMA: &str = "aethyme.collab-capture-release/experimental-v0";
pub const GC_PLAN_SCHEMA: &str = "aethyme.collab-gc-plan/experimental-v0";
pub const GC_APPLY_SCHEMA: &str = "aethyme.collab-gc-apply/experimental-v0";
pub const GC_RESUME_SCHEMA: &str = "aethyme.collab-gc-resume/experimental-v0";
pub const CONTEXT_SCHEMA: &str = "aethyme.collab-context/experimental-v0";
pub const BRIEF_ATTACH_SCHEMA: &str = "aethyme.collab-brief-attach/experimental-v0";
pub const ENROLL_SCHEMA: &str = "aethyme.collab-enroll/experimental-v0";
pub const ERROR_SCHEMA: &str = "aethyme.collab-error/experimental-v0";

pub const USAGE: &str = "\
aethyme collab — opt-in Local collaboration (experimental)

Disabled unless the repository's .aethyme/config.toml enables it:
  [collaboration]
  capture = \"advisory\"     # or \"required\"; \"off\" or absent = disabled
  project = \"proj-...\"     # from `aethyme collab enroll`
Under required capture any other [collaboration] setting is refused; under
advisory it is ignored with a warning. Only `broker submit` creates
collaboration state; until then every subcommand but status and enroll
refuses with not_initialized.
Every subcommand takes --json (one versioned object) and --repo <path>.
Nothing here runs in the background or contacts a service.

Usage:
  aethyme collab status [--json]
      Policy, state root, durability profile, schema, captures in flight or
      needing attention, reservations, unfinished reclamation and the
      required-capture fence. Works while disabled; never writes anything.
  aethyme collab enroll [--write] [--json]
      Mint a project ID and print the [collaboration] section that enables
      advisory capture. --write appends it to .aethyme/config.toml in the
      current worktree (refused in the main checkout while broker sessions
      exist) when neither that file nor the policy in force mentions
      collaboration; review and commit it.
  aethyme collab project enroll --commit <full-commit-oid> [--write] [--json]
      Propose a logical project descriptor anchored to an explicit committed snapshot.
      The proposal grants no local binding or authority until its pointer is committed.
  aethyme collab project bind --confirm <descriptor-record-id> [--json]
      Bind this local store to the exact descriptor and pointer committed in this clone.
      Binding grants no execution or acceptance authority.
  aethyme collab project association propose --peer-repo <path> [--write] [--json]
      Propose a bounded association between two independently approved local projects.
      The proposal grants no authority until its exact RecordId is confirmed locally.
  aethyme collab project association confirm --peer-repo <path> --confirm <record-id> [--json]
      Revalidate both committed descriptors and bindings, then persist confirmation only
      in the invoking project's local identity/object store.
  aethyme collab project association revoke --confirm <record-id> [--json]
      Revoke one exact local association record without accessing the peer store.
      A revoked RecordId stays revoked; a fresh proposal requires a new exact confirmation.
  aethyme collab contribution revise --operation ID [--brief-file PATH]
       [--file-operations PATH] [--depends-on RECORD_ID...] [--clear-dependencies] [--json]
      Append an immutable revision for one retained capture operation. A retry
      returns the same revision only when its normalized content matches; omitted
      file operations and dependencies inherit from the current head. A changed
      brief creates a new rationale revision.
      The optional file-operation document uses schema
      aethyme.collab-file-operations/experimental-v0 with lowercase-hex Git paths.
      Rename/copy entries require kind, from_hex and to_hex; split entries use
      kind, from_hex and a to_hex array. --clear-dependencies removes prior deps.
  aethyme collab capture recover [--json]
      Resolve captures a crashed process left in flight. Live ones are
      skipped; per-operation failures are reported and do not stop the rest.
  aethyme collab capture abort --operation <id> [--json]
      Abort an unfinished capture and release its reservation. A committed
      capture is refused: its source is released by reclamation.
  aethyme collab capture receipt --operation <id> [--json]
      Show a capture's receipt. `released` or `reclaimed` means the source is
      no longer promised; only `retained_local` is.
  aethyme collab capture release --operation <id> --confirm <receipt-record-id> [--json]
      Release only that committed capture's contribution root. Other roots may
      still retain source; this command does not reclaim or erase it.
  aethyme collab gc plan [--json]
      Survey retained data and record a plan. Removes nothing.
  aethyme collab gc apply --confirm <digest> [--json]
      Remove what that plan named and is still eligible; the rest is skipped
      with a reason. Refused while a capture, recovery or reader is active.
  aethyme collab gc resume [--json]
      Finish an interrupted apply, putting back anything now relied on.
  aethyme collab context --path <path> [--path <path>...] [--source <snapshot-id>]
                         [--analysis <envelope-file>...] [--max-items N]
                         [--revision-pin CONTRIBUTION@REVISION@sha256:HEX...]
                         [--max-brief-tokens N] [--max-matched-paths N] [--max-bytes N]
                         [--json]
      Retained contributions relevant to the paths, ranked, budgeted and
      explained. Briefs are untrusted data written by other agents. Without
      --source and a complete analysis covering every path, the answer is
      partial: an empty answer is not evidence that nothing relevant exists.
      --revision-pin selects exact immutable revisions; repeat it for each pin.
  aethyme collab brief attach (--contribution <record-id> | --operation <id>) <decision-file> [--json]
      Attach a decision brief (aethyme-brief-tokens/v0, at most 150 tokens) to
      a retained contribution. Bound contributions append an immutable revision;
      select --operation for a lineage shared by multiple logical contributions.

Additional explicit lifecycle commands (provisional Local packages):
  aethyme collab resolution prepare --request ID --object sha256:HEX
       --session-label LABEL --client-key KEY --limits PATH
       --target NAME --issuer NAME --wave ID --version N [--json]
      Charge one bounded attempt before exposing its exact metadata packet.
      Limits are a strict JSON file; labels are provenance, not authentication.
      Preparation and handoff intent require the exact current wave and target version.
      Repeating a client key returns status without a new charge or packet.
  aethyme collab resolution status --attempt TOKEN --session-label LABEL [--json]
      Read-only attempt status, including the retained limits and unknowns.
  aethyme collab resolution mark-dispatching --attempt TOKEN --session-label LABEL
       --target NAME --issuer NAME --wave ID --version N [--json]
      Record handoff intent before giving the packet to a local agent.
      This command records state only; it neither starts nor authorizes an agent.
  aethyme collab resolution mark-unknown --attempt TOKEN --session-label LABEL
       --detail PATH [--json]
      Record uncertainty using a UTF-8 detail file (at most 4096 bytes).
      The attempt stays charged; a late bound response may settle it.
  aethyme collab resolution packet --attempt TOKEN --session-label LABEL [--json]
      Recover an exported metadata packet without charging another attempt.
  aethyme collab resolution input --attempt TOKEN --session-label LABEL
       --snapshot sha256:HEX --path PATH [--json]
      Read one exact manifested input, with bytes encoded as lowercase hex.
  aethyme collab resolution import --attempt TOKEN --session-label LABEL
       --proposal PATH [--json]
      Import one strict attempt/request/manifest-bound response file. The
      result is retained and unverified; no dispatch, wave attachment or acceptance.
  aethyme collab resolution attach --attempt TOKEN --request ID --object sha256:HEX
       --target NAME --issuer NAME --wave ID --version N [--json]
      Attach a settled synthesis to its exact current wave for fresh verification.
      The result must cover every selected contribution. Grants no acceptance.
  aethyme collab authority status [--json]
      Read the native epoch/witness and ledger binding without changing state.
      Observation grants no authority; unbound, frozen and drift remain distinct.
  aethyme collab target status --target NAME [--json]
      Existing target revision, issuer, phase and version. Read-only.
  aethyme collab target propose --target NAME --issuer NAME --file PATH [--json]
      Strict regular-file TargetSpec, at most 256 KiB. Only valid explicit
      proposals may appoint a new target; no selection or dispatch follows.
  aethyme collab target select --target NAME --issuer NAME --revision ID [--json]
      Select an existing immutable revision. Active waves/budgets refuse.
  aethyme collab target hold --target NAME --issuer NAME --version N [--json]
      Hold an active phase waiting for operator input at the exact version.
  aethyme collab target resume --target NAME --issuer NAME --version N [--json]
      Resume the recorded held phase, without skipping checks.
  aethyme collab target native-gates list --target NAME --issuer NAME [--json]
      List up to 500 native-job retention records for the current target owner.
  aethyme collab target native-gates status --target NAME --issuer NAME --job ID [--json]
      Inspect native-job state and whether its roots remain retained.
  aethyme collab target native-gates release --target NAME --issuer NAME --job ID [--json]
      Abandon a prepared job or release completed-job roots. Dispatching and
      unknown jobs remain held because process-tree inactivity is unproven.
  aethyme collab target budget project --target NAME --issuer NAME --file PATH [--json]
      Appoint the immutable project budget policy from a strict bounded file.
  aethyme collab target budget objective --target NAME --issuer NAME --file PATH [--json]
      Appoint the immutable cap for the current target objective authorization.
  aethyme collab target budget status --target NAME --issuer NAME
       [--reservation ID | --nonce NONCE] [--json]
      Reconcile immutable policies, totals, and up to 500 newest reservation rows.
      Exact reservation or nonce lookup is included even when older than that
      page; nonce lookup is read-only and never retries or resumes work.
  aethyme collab target verifier prepare --harness-repo PATH --commit OID
       --inventory PATH --operation ID --docker ABSOLUTE_PATH --endpoint UNIX_SOCKET
       --file PLAN.json [--json]
      Retain the exact harness commit and canonical inventory, derive the runtime
      identity at the pinned Docker endpoint, and print the policy ID. This
      configures no producer and dispatches no work.
  aethyme collab target verifier configure --target NAME --issuer NAME
       --producer NAME --expected-generation none|N --file POLICY.json [--json]
      Appoint a producer by compare-and-swap after validating the retained roots.
  aethyme collab target verifier status --target NAME --issuer NAME --producer NAME [--json]
      Read appointment, authority, and selected-policy status; grants no dispatch.
  aethyme collab target verifier revoke --target NAME --issuer NAME
       --producer NAME --generation N [--json]
      Revoke exactly the observed producer generation.
  aethyme collab target capture-notice ingest --target NAME --issuer NAME
       --revision ID --alias ALIAS --operation ID [--json]
      Append an idempotent notice for an already retained selected contribution.
      The owner tick revalidates it before wave admission; notice receipt alone
      does not make the alias available.
  aethyme collab target capture-notice list --target NAME [--limit 1..512] [--json]
      Read notice outcomes for lost-response reconciliation.
  aethyme collab stop --epoch EPOCH --reason TEXT [--json]
      Freeze project ingress and rotate native authority. Running processes
      and external effects are not assumed cancelled; reconcile them.
  aethyme collab sensitive quarantine --incident ID --issuer LABEL --epoch FROZEN_EPOCH
                       --reason CODE --scope project [--json]
      Permanently block project-local retrieval after authority has been
      frozen. LABEL is audit text, not authentication.
  aethyme collab sensitive purge|resume --incident ID --confirm DIGEST [--json]
      Attempt or resume managed-store purge. Active/unknown work and readers
      keep the project quarantined; external copies remain explicit residuals.
  aethyme collab sensitive status --incident ID [--json]
      Show managed purge progress and unmanaged residual classes.
  aethyme collab export --target NAME --issuer NAME --source ID [--source ID...]
                       [--record ID@sha256:HEX...] --audience NAME --output PATH [--json]
      Closed source/advisory historical bundle. Exclusive private output;
      never overwrites, releases retained source or restores authority.
  aethyme collab handoff show --id ID [--json]
      Verify a retained request and display historical state/receipt IDs.
      Opens existing state (which may migrate it); grants no current agent guard.
  aethyme collab sync status [--json]
      Read local exchange bookkeeping, with no remote delivery/source promise.
  aethyme collab target run --target NAME --issuer NAME --appointment PATH
       --resources PATH --producer NAME --generation N --session N --remote NAME
       --destination refs/heads/NAME --docker ABSOLUTE_PATH --endpoint UNIX_SOCKET
       --timeout SECONDS
       --authorization TEXT --allow-replacement yes|no [--json]
      Explicit finite native controller appointment; existing target, budget,
      producer and pinned recipe required. Supports local bare Git only.
      A durable interrupted appointment requires reconciliation; no auto-replay.
  aethyme collab target run (without --appointment) / sync / handoff dispatch
      Refused with unsupported_profile. No state or argument file is read.
      These commands require their own appointed native adapters.
  aethyme collab target run status --target NAME --issuer NAME
       (--appointment ID | --nonce NONCE) [--json]
      Inspect one finite appointment after a lost response. Nonce lookup is
      bounded across revisions/epochs; dispatching is unknown and never replayed.

Exit codes: 0 done, 1 failed (I/O, database, Git, or an integrity failure
such as a corrupt object or receipt), 2 usage, 3 refused (disabled, policy,
location, or a state that forbids the action; do not retry unchanged).
";

/// A refused or failed command: what to print and how to exit.
#[derive(Debug)]
pub(crate) struct Failure {
    code: String,
    message: String,
    next_action: Option<String>,
    exit: u8,
}

impl Failure {
    fn usage(message: impl Into<String>) -> Self {
        Self {
            code: "usage".into(),
            message: message.into(),
            next_action: Some("run `aethyme collab --help`".into()),
            exit: exit_status::USAGE,
        }
    }

    fn refused(code: &str, message: impl Into<String>, next: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            next_action: Some(next.into()),
            exit: exit_status::REFUSED,
        }
    }

    /// A domain error: I/O and database failures are failures; everything
    /// else is a refusal, with the safe next action for its code.
    fn domain(code: &str, message: String) -> Self {
        let exit = if matches!(
            code,
            "io" | "sqlite"
                | "git"
                | "state"
                | "archive"
                | "failed"
                | "injected"
                | "corrupt_receipt"
                | "corrupt_object"
                | "corrupt_state"
                | "archive_lock"
                | "missing_object"
        ) {
            exit_status::FAILED
        } else {
            exit_status::REFUSED
        };
        Self {
            code: code.into(),
            message,
            next_action: next_action_for(code).map(str::to_string),
            exit,
        }
    }
}

/// The safe next action for a refusal code.
fn next_action_for(code: &str) -> Option<&'static str> {
    Some(match code {
        "overlaps_cleanup_root" => {
            "move the worktree container or host cache away from the host state directory, \
             as the message says, then run `aethyme collab status`"
        }
        "inside_git_worktree" | "ephemeral_repository" | "unavailable" => {
            "set AETHYME_HOST_STATE_DIR to a private directory outside every repository"
        }
        "insecure_permissions" => "chmod 700 the directory named, then run the command again",
        "foreign_root" => {
            "move the files named out of the collaboration root, or choose another \
             AETHYME_HOST_STATE_DIR"
        }
        "schema_too_new" => "upgrade Aethyme (`aethyme self-update`)",
        "project_mismatch" | "not_a_collaboration_database" => {
            "check [collaboration] project in .aethyme/config.toml; never rename or copy \
             project directories"
        }
        "archive_in_use" => {
            "wait for the capture, recovery or reader to finish, then run \
             `aethyme collab gc apply` again"
        }
        "blocked" => "resolve the blockers listed (often `aethyme collab capture recover`)",
        "unknown_plan" | "stale_plan" => {
            "run `aethyme collab gc plan` and confirm the digest it prints"
        }
        "unknown_operation" => "run `aethyme collab status` to list operations",
        "already_committed" => {
            "a committed capture is released by reclamation (`aethyme collab gc plan`), not \
             by abort"
        }
        "insufficient_space" => {
            "free disk space, or run `aethyme collab capture recover` and `aethyme collab gc \
             plan` to release reservations and reclaim expired data"
        }
        "invalid_grace" | "symlinked_path" => "inspect the collaboration root by hand",
        "budget_out_of_range" | "request_too_large" | "budget_too_small" => {
            "change the budget flags; `aethyme collab --help` lists their ranges"
        }
        "invalid_brief" => "fix the brief file; the message lists every problem",
        "not_retained" => "check the record ID with `aethyme collab capture receipt`",
        "not_initialized" => {
            "nothing is retained yet: an advisory or required `aethyme broker submit` \
             creates collaboration state"
        }
        "resolution_wave_refused" => {
            "inspect the latest target and wave, then use their exact request and version"
        }
        "file_too_large" => "pass a smaller file; the message gives the limit",
        _ => return None,
    })
}

struct Options {
    repo: Option<PathBuf>,
    rest: Vec<String>,
}

fn options(args: &[String]) -> Result<Options, Failure> {
    let mut repo = None;
    let mut rest = Vec::new();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--json" => {}
            "--repo" => {
                let value = iter
                    .next()
                    .ok_or_else(|| Failure::usage("--repo needs a path"))?;
                repo = Some(PathBuf::from(value));
            }
            _ => rest.push(arg.clone()),
        }
    }
    Ok(Options { repo, rest })
}

/// Run `aethyme collab <args>`; returns the exit status.
pub fn run(args: &[String]) -> u8 {
    if args.is_empty() || args.iter().any(|arg| arg == "-h" || arg == "--help") {
        print!("{USAGE}");
        return exit_status::SUCCESS;
    }
    let json_requested = args.iter().any(|arg| arg == "--json");
    let command = command_name(args);
    match options(args).and_then(dispatch) {
        Ok((value, text)) => {
            if json_requested {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&value).expect("JSON values serialize")
                );
            } else {
                print!("{text}");
            }
            exit_status::SUCCESS
        }
        Err(failure) => {
            if json_requested {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "schema": ERROR_SCHEMA,
                        "command": command,
                        "code": failure.code,
                        "message": failure.message,
                        "next_action": failure.next_action,
                    }))
                    .expect("JSON values serialize")
                );
            } else {
                eprintln!("Error: {}", failure.message);
                if let Some(next) = &failure.next_action {
                    eprintln!("next: {next}");
                }
            }
            failure.exit
        }
    }
}

/// The subcommand verbs, for the error object's `command`: only known verbs,
/// read before the first flag, so no path or argument value is echoed.
fn command_name(args: &[String]) -> String {
    const VERBS: &[&str] = &[
        "status",
        "enroll",
        "capture",
        "recover",
        "abort",
        "receipt",
        "gc",
        "plan",
        "apply",
        "resume",
        "context",
        "brief",
        "attach",
        "target",
        "budget",
        "verifier",
        "prepare",
        "configure",
        "objective",
        "project",
        "association",
        "contribution",
        "revise",
        "capture-notice",
        "ingest",
        "list",
        "revoke",
        "policy-id",
        "native-gates",
        "propose",
        "select",
        "hold",
        "run",
        "authority",
        "stop",
        "export",
        "handoff",
        "show",
        "dispatch",
        "sync",
        "resolution",
        "mark-dispatching",
        "mark-unknown",
        "packet",
        "input",
        "import",
    ];
    let words: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .take_while(|arg| !arg.starts_with('-'))
        .take(2)
        .collect();
    if words.is_empty() {
        return String::new();
    }
    if words.iter().all(|word| VERBS.contains(word)) {
        words.join(" ")
    } else {
        "unknown".into()
    }
}

type Output = (Value, String);

fn dispatch(options: Options) -> Result<Output, Failure> {
    let main_root = main_root(options.repo.as_deref())?;
    let words: Vec<&str> = options.rest.iter().map(String::as_str).collect();
    // Everything but `status` and `enroll` needs an enabled policy; refuse
    // before reading any argument file or touching state.
    let project_command = matches!(
        words.as_slice(),
        ["project", "enroll", ..] | ["project", "bind", ..]
    );
    if !matches!(words.first(), Some(&"status" | &"enroll") | None) && !project_command {
        require_enabled(&main_root)?;
    }
    match words.as_slice() {
        ["status"] => status(&main_root),
        ["enroll", rest @ ..] => enroll(
            &main_root,
            &current_worktree(options.repo.as_deref())?,
            rest,
        ),
        ["project", rest @ ..] => project::command(
            &main_root,
            &current_worktree(options.repo.as_deref())?,
            rest,
        ),
        ["contribution", "revise", rest @ ..] => contribution::revise(&main_root, rest),
        ["capture", "recover"] => capture_recover(&main_root),
        ["capture", "abort", rest @ ..] => capture_abort(&main_root, rest),
        ["capture", "receipt", rest @ ..] => capture_receipt(&main_root, rest),
        ["capture", "release", rest @ ..] => lifecycle::capture_release(&main_root, rest),
        ["gc", "plan"] => gc_plan_command(&main_root),
        ["gc", "apply", rest @ ..] => gc_apply_command(&main_root, rest),
        ["gc", "resume"] => gc_resume_command(&main_root),
        ["context", rest @ ..] => context(&main_root, rest),
        ["brief", "attach", rest @ ..] => brief_attach(&main_root, rest),
        [
            "sensitive",
            verb @ ("quarantine" | "purge" | "resume" | "status"),
            rest @ ..,
        ] => sensitive::command(&main_root, verb, rest),
        ["resolution", "prepare", rest @ ..] => resolution::prepare(&main_root, rest),
        ["resolution", "status", rest @ ..] => resolution::status(&main_root, rest),
        ["resolution", "mark-dispatching", rest @ ..] => {
            resolution::mark_dispatching(&main_root, rest)
        }
        ["resolution", "mark-unknown", rest @ ..] => resolution::mark_unknown(&main_root, rest),
        ["resolution", "packet", rest @ ..] => resolution::packet(&main_root, rest),
        ["resolution", "input", rest @ ..] => resolution::input(&main_root, rest),
        ["resolution", "import", rest @ ..] => resolution::import(&main_root, rest),
        ["resolution", "attach", rest @ ..] => resolution::attach(&main_root, rest),
        ["authority", "status", rest @ ..] => lifecycle::authority_status(&main_root, rest),
        ["target", "status", rest @ ..] => lifecycle::target_status(&main_root, rest),
        ["target", "budget", "project", rest @ ..] => {
            lifecycle::budget_project_configure(&main_root, rest)
        }
        ["target", "budget", "objective", rest @ ..] => {
            lifecycle::budget_objective_authorize(&main_root, rest)
        }
        ["target", "budget", "status", rest @ ..] => lifecycle::budget_status(&main_root, rest),
        ["target", "verifier", "policy-id", rest @ ..] => {
            lifecycle::verifier_policy_id(&main_root, rest)
        }
        ["target", "verifier", "prepare", rest @ ..] => {
            lifecycle::verifier_prepare(&main_root, rest)
        }
        ["target", "verifier", "configure", rest @ ..] => {
            lifecycle::verifier_configure(&main_root, rest)
        }
        ["target", "verifier", "status", rest @ ..] => lifecycle::verifier_status(&main_root, rest),
        ["target", "verifier", "revoke", rest @ ..] => lifecycle::verifier_revoke(&main_root, rest),
        [
            "target",
            "capture-notice",
            verb @ ("ingest" | "list"),
            rest @ ..,
        ] => lifecycle::capture_notice(&main_root, verb, rest),
        ["target", "run", "status", rest @ ..] => lifecycle::service_status(&main_root, rest),
        ["target", "native-gates", rest @ ..] => lifecycle::native_gate_retention(&main_root, rest),
        [
            "target",
            verb @ ("propose" | "select" | "hold" | "resume"),
            rest @ ..,
        ] => lifecycle::target_change(&main_root, verb, rest),
        ["stop", rest @ ..] => lifecycle::stop(&main_root, rest),
        ["export", rest @ ..] => lifecycle::export(&main_root, rest),
        ["handoff", "show", rest @ ..] => lifecycle::handoff_show(&main_root, rest),
        ["sync", "status", rest @ ..] => lifecycle::sync_status(&main_root, rest),
        ["target", "run", rest @ ..] if rest.contains(&"--appointment") => {
            lifecycle::target_run(&main_root, rest)
        }
        ["target", "run", ..] | ["sync", ..] | ["handoff", "dispatch", ..] => {
            Err(lifecycle::unsupported())
        }

        _ => Err(Failure::usage(format!(
            "unknown collab command `{}`",
            options.rest.join(" ")
        ))),
    }
}

fn main_root(repo: Option<&Path>) -> Result<PathBuf, Failure> {
    let start = match repo {
        Some(path) => path.to_path_buf(),
        None => std::env::current_dir().map_err(|error| {
            Failure::domain("io", format!("cannot read the current directory: {error}"))
        })?,
    };
    crate::git::GitRepo::discover(&start)
        .and_then(|repo| repo.main_root())
        .map_err(|error| {
            Failure::refused(
                "not_a_repository",
                format!("{}: {error}", start.display()),
                "run it inside a Git repository, or pass --repo <path>",
            )
        })
}

/// The policy, when collaboration is enabled.
struct Enabled {
    project: ProjectKey,
}

fn require_enabled(main_root: &Path) -> Result<Enabled, Failure> {
    match setting(main_root).0 {
        Setting::Off => Err(Failure::refused(
            "collaboration_disabled",
            "collaboration is disabled for this repository: .aethyme/config.toml has no \
             [collaboration] capture = \"advisory\" or \"required\"",
            "run `aethyme collab enroll` to get the section that enables it, commit it, \
             then run this again",
        )),
        Setting::Unsupported { code, detail } => Err(Failure::refused(
            code,
            detail,
            "fix [collaboration] in .aethyme/config.toml; `aethyme collab status` shows what \
             is read",
        )),
        Setting::On {
            project: Err(problem),
            ..
        } => Err(Failure::refused(
            "not_configured",
            format!("[collaboration] project is not usable: {problem}"),
            "set [collaboration] project from `aethyme collab enroll`",
        )),
        Setting::On {
            project: Ok(project),
            ..
        } => Ok(Enabled { project }),
    }
}

/// Open the existing store. Nothing here creates collaboration state: only
/// `broker submit` does, so a missing store is `not_initialized`.
fn open(main_root: &Path) -> Result<CollaborationStore, Failure> {
    let enabled = require_enabled(main_root)?;
    let (_, _, exists) = locate_for_repository(main_root, &enabled.project)
        .map_err(|error| Failure::domain(error.code(), error.to_string()))?;
    if !exists {
        return Err(Failure::refused(
            "not_initialized",
            format!(
                "project {} has no collaboration state yet",
                enabled.project.as_str()
            ),
            next_action_for("not_initialized").unwrap_or_default(),
        ));
    }
    open_for_repository(main_root, &enabled.project)
        .map_err(|error| Failure::domain(error.code(), error.to_string()))
}

/// Read at most `limit` bytes of `path`; a larger file is refused before
/// anything parses it.
fn read_capped(path: &str, limit: u64, what: &str) -> Result<Vec<u8>, Failure> {
    let file = std::fs::File::open(path)
        .map_err(|error| Failure::domain("io", format!("{path}: {error}")))?;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| Failure::domain("io", format!("{path}: {error}")))?;
    if bytes.len() as u64 > limit {
        return Err(Failure::refused(
            "file_too_large",
            format!("{path}: a {what} may be at most {limit} bytes"),
            next_action_for("file_too_large").unwrap_or_default(),
        ));
    }
    Ok(bytes)
}

/// The largest decision file `brief attach` reads; a valid brief is far
/// smaller (2048 prose bytes, 150 tokens).
const MAX_BRIEF_FILE_BYTES: u64 = 64 * 1024;
/// The largest analysis envelope `context --analysis` reads.
const MAX_ANALYSIS_FILE_BYTES: u64 = 1024 * 1024;

fn flag_value<'a>(rest: &[&'a str], name: &str) -> Result<Option<&'a str>, Failure> {
    match rest.iter().position(|word| *word == name) {
        None => Ok(None),
        Some(index) => rest
            .get(index + 1)
            .copied()
            .filter(|value| !value.starts_with("--"))
            .map(Some)
            .ok_or_else(|| Failure::usage(format!("{name} needs a value"))),
    }
}

fn no_extra(rest: &[&str], allowed_flags: &[&str], positional: usize) -> Result<(), Failure> {
    let mut seen_positional = 0;
    let mut iter = rest.iter();
    while let Some(word) = iter.next() {
        if allowed_flags.contains(word) {
            iter.next();
        } else if word.starts_with("--") {
            return Err(Failure::usage(format!("unknown flag {word}")));
        } else {
            seen_positional += 1;
        }
    }
    if seen_positional != positional {
        return Err(Failure::usage("unexpected arguments"));
    }
    Ok(())
}

fn operation(rest: &[&str]) -> Result<OperationId, Failure> {
    no_extra(rest, &["--operation"], 0)?;
    let id = flag_value(rest, "--operation")?
        .ok_or_else(|| Failure::usage("--operation <id> is required"))?;
    OperationId::parse(id).map_err(|error| Failure::domain(error.code(), error.to_string()))
}

// ----------------------------------------------------------------- status

fn status(main_root: &Path) -> Result<Output, Failure> {
    let (setting, source) = setting(main_root);
    let mut value = json!({
        "schema": STATUS_SCHEMA,
        "config_source": source,
    });
    let mut text = String::new();
    let mut next = Vec::<String>::new();
    let project = match &setting {
        Setting::Off => {
            value["enabled"] = json!(false);
            value["policy"] = json!("off");
            text.push_str("collaboration: disabled (no [collaboration] capture policy)\n");
            next.push("`aethyme collab enroll` prints the section that enables it".into());
            None
        }
        Setting::Unsupported { code, detail } => {
            value["enabled"] = json!(false);
            value["policy"] = json!("unsupported");
            value["policy_error"] = json!({ "code": code, "detail": detail });
            text.push_str(&format!("collaboration: refused ({code}): {detail}\n"));
            next.push("fix [collaboration] in .aethyme/config.toml".into());
            None
        }
        Setting::On {
            policy,
            project,
            ignored,
        } => {
            value["policy"] = json!(policy.as_str());
            if !ignored.is_empty() {
                value["ignored_settings"] = json!(ignored);
            }
            let project = match project {
                Ok(project) => {
                    value["enabled"] = json!(true);
                    value["project"] = json!(project.as_str());
                    text.push_str(&format!(
                        "collaboration: enabled ({} capture), project {}\n",
                        policy.as_str(),
                        project.as_str()
                    ));
                    Some(project.clone())
                }
                Err(problem) => {
                    value["enabled"] = json!(false);
                    value["policy_error"] = json!({ "code": "not_configured", "detail": problem });
                    text.push_str(&format!(
                        "collaboration: {} capture configured but not usable: {problem}\n",
                        policy.as_str()
                    ));
                    next.push("set [collaboration] project from `aethyme collab enroll`".into());
                    None
                }
            };
            if !ignored.is_empty() {
                text.push_str(&format!(
                    "  warning: ignored [collaboration] {} this binary does not implement: {}\n",
                    if ignored.len() == 1 {
                        "setting"
                    } else {
                        "settings"
                    },
                    ignored.join(", ")
                ));
            }
            project
        }
    };
    if let Some(source) = source {
        text.push_str(&format!("  config: {source} .aethyme/config.toml\n"));
    }
    fence_status(main_root, &mut value, &mut text, &mut next);
    if let Some(project) = project {
        state_status(main_root, &project, &mut value, &mut text, &mut next);
    }
    value["next_actions"] = json!(next);
    for action in &next {
        text.push_str(&format!("  next: {action}\n"));
    }
    Ok((value, text))
}

/// The required-capture fence on broker.db (#660), computed by
/// `broker status`'s own `collaboration_fence_state` on a read-only broker
/// snapshot, so looking never raises it. `pending` means the committed
/// config requires it and a writable broker command will raise it. A read
/// that fails is reported as unknown, never as "not raised".
fn fence_status(main_root: &Path, value: &mut Value, text: &mut String, next: &mut Vec<String>) {
    let read = crate::Broker::open_snapshot(main_root)
        .map_err(|error| ("broker_unavailable", error.to_string()))
        .and_then(|broker| {
            // Read once directly so a database error is reported, not taken
            // as "no fence". The committed config is rechecked by Open.
            crate::schema::collaboration_fence(broker.store_ref().connection())
                .map_err(|error| ("fence_unreadable", error.to_string()))?;
            Ok(broker.collaboration_fence_state(crate::broker::FenceTrigger::Open))
        });
    match read {
        Err((code, message)) => {
            value["collaboration_fence"] = Value::Null;
            value["collaboration_fence_error"] = json!({ "code": code, "message": message });
            text.push_str(&format!("  fence: unknown ({code}: {message})\n"));
        }
        Ok(fence) => {
            value["collaboration_fence"] = serde_json::to_value(&fence).unwrap_or(Value::Null);
            match fence {
                Some(fence)
                    if fence.state == "pending"
                        && fence.reason == crate::schema::COLLABORATION_FENCE_UNRESOLVED_REASON =>
                {
                    text.push_str(
                        "  fence: pending (the committed config cannot be resolved; older \
                         binaries can still open this repository until the policy is understood)\n",
                    );
                    next.push(
                        "resolve the committed repository config; if it requires capture, the next writable broker command will raise the compatibility fence"
                            .into(),
                    );
                }
                Some(fence) if fence.state == "pending" => {
                    text.push_str(
                        "  fence: pending (the committed config requires capture; older \
                         binaries can still open this repository until it is raised)\n",
                    );
                    next.push(
                        "run a writable broker command to raise the required-capture fence".into(),
                    );
                }
                Some(fence) => text.push_str(&format!(
                    "  fence: broker.db requires schema {} or newer ({}, {} config); older \
                     binaries cannot open this repository, and this does not lift if required \
                     capture is turned off\n",
                    fence.min_compatible_schema, fence.reason, fence.source
                )),
                None => {}
            }
        }
    }
}

/// The state root and, when it exists, the store read through a read-only
/// snapshot. Every problem is reported under `state.refusal`.
fn state_status(
    main_root: &Path,
    project: &ProjectKey,
    value: &mut Value,
    text: &mut String,
    next: &mut Vec<String>,
) {
    let refusal = |value: &mut Value,
                   text: &mut String,
                   next: &mut Vec<String>,
                   code: &str,
                   message: String| {
        value["state"]["refusal"] = json!({
            "code": code,
            "message": message,
            "next_action": next_action_for(code),
        });
        text.push_str(&format!("  state: refused ({code}): {message}\n"));
        if let Some(action) = next_action_for(code) {
            next.push(action.into());
        }
    };
    let (root, project_dir, exists) = match locate_for_repository(main_root, project) {
        Ok(located) => located,
        Err(error) => {
            value["state"] = json!({});
            refusal(value, text, next, error.code(), error.to_string());
            return;
        }
    };
    value["state"] = json!({
        "root": root.path(),
        "root_source": root.source(),
        "project_dir": project_dir,
        "initialized": exists,
    });
    text.push_str(&format!(
        "  state root: {} ({})\n",
        root.path().display(),
        serde_json::to_value(root.source())
            .ok()
            .and_then(|source| source
                .get("setting")
                .and_then(Value::as_str)
                .map(str::to_string))
            .unwrap_or_else(|| "explicit".into())
    ));
    if !exists {
        text.push_str("  state: not created yet (the first captured submit creates it)\n");
        return;
    }
    let snapshot = match inspect_for_repository(main_root, project) {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) => {
            value["state"]["initialized"] = json!(false);
            return;
        }
        Err(error) => {
            refusal(value, text, next, error.code(), error.to_string());
            return;
        }
    };
    let profile = &snapshot.durability;
    value["state"]["durability"] = serde_json::to_value(profile).unwrap_or(Value::Null);
    value["state"]["receipt_label"] = json!(profile.receipt_label());
    value["state"]["schema_version"] = json!(snapshot.schema_version);
    value["state"]["min_compatible_schema"] = json!(snapshot.min_compatible_schema);
    value["state"]["needs_migration"] = json!(snapshot.needs_migration);
    text.push_str(&format!(
        "  durability: {} ({}{}), schema {} (readable from {}){}\n",
        profile.receipt_label(),
        profile.filesystem,
        profile
            .limitation
            .as_deref()
            .map(|why| format!("; {why}"))
            .unwrap_or_default(),
        snapshot.schema_version,
        snapshot.min_compatible_schema,
        if snapshot.needs_migration {
            "; the next writable open migrates it"
        } else {
            ""
        }
    ));
    match capture_summary(&snapshot) {
        Ok((captures, unfinished_gc)) => {
            let attention = captures["attention"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let by_state = captures["by_state"]
                .as_object()
                .cloned()
                .unwrap_or_default();
            text.push_str(&format!(
                "  captures: {}; {} bytes reserved\n",
                if by_state.is_empty() {
                    "none".to_string()
                } else {
                    by_state
                        .iter()
                        .map(|(state, count)| format!("{count} {state}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                },
                captures["reserved_bytes"]
            ));
            for operation in &attention {
                text.push_str(&format!(
                    "    {} {}{}\n",
                    operation["operation_id"].as_str().unwrap_or("?"),
                    operation["state"].as_str().unwrap_or("?"),
                    operation["code"]
                        .as_str()
                        .map(|code| format!(" ({code})"))
                        .unwrap_or_default()
                ));
            }
            if attention.iter().any(|operation| {
                matches!(
                    operation["state"].as_str(),
                    Some("intent" | "copying" | "sealed")
                )
            }) {
                next.push(
                    "`aethyme collab capture recover` resolves captures a crashed process left \
                     in flight (live ones are skipped)"
                        .into(),
                );
            }
            if unfinished_gc > 0 {
                text.push_str(&format!(
                    "  reclamation: {unfinished_gc} unfinished apply\n"
                ));
                next.push("`aethyme collab gc resume` finishes the interrupted apply".into());
            }
            value["captures"] = captures;
            value["gc"] = json!({ "unfinished_generations": unfinished_gc });
        }
        Err(error) => refusal(value, text, next, "sqlite", error.to_string()),
    }
}

/// Captures by state, reservations and captures needing attention, and the
/// number of unfinished reclamation generations, from a read-only snapshot.
/// Tables an older store lacks read as empty.
fn capture_summary(
    snapshot: &crate::collaboration_state::CollaborationSnapshot,
) -> rusqlite::Result<(Value, i64)> {
    let connection = snapshot.connection();
    let mut by_state = serde_json::Map::new();
    let mut reserved = 0_i64;
    let mut attention = Vec::new();
    if snapshot.has_table("capture_operations") {
        let mut statement = connection.prepare(
            "SELECT state, count(*) FROM capture_operations GROUP BY state ORDER BY state",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (state, count) = row?;
            by_state.insert(state, json!(count));
        }
        reserved = connection.query_row(
            "SELECT COALESCE(SUM(reserved_bytes), 0) FROM capture_operations
             WHERE state IN ('intent', 'copying', 'sealed')",
            [],
            |row| row.get(0),
        )?;
        attention = operations(
            connection,
            "WHERE state IN ('intent', 'copying', 'sealed', 'failed', 'incomplete')",
        )?;
    }
    let unfinished_gc = if snapshot.has_table("gc_generations") {
        connection.query_row(
            "SELECT count(*) FROM gc_generations WHERE state != 'done'",
            [],
            |row| row.get(0),
        )?
    } else {
        0
    };
    Ok((
        json!({
            "by_state": by_state,
            "reserved_bytes": reserved,
            "attention": attention,
        }),
        unfinished_gc,
    ))
}

/// Up to 50 capture operations matching `filter`, most recent first.
fn operations(connection: &rusqlite::Connection, filter: &str) -> rusqlite::Result<Vec<Value>> {
    let mut statement = connection.prepare(&format!(
        "SELECT operation_id, state, outcome_code, updated_ms FROM capture_operations {filter}
         ORDER BY updated_ms DESC, operation_id LIMIT 50"
    ))?;
    let rows = statement.query_map([], |row| {
        Ok(json!({
            "operation_id": row.get::<_, String>(0)?,
            "state": row.get::<_, String>(1)?,
            "code": row.get::<_, Option<String>>(2)?,
            "updated_ms": row.get::<_, i64>(3)?,
        }))
    })?;
    rows.collect()
}

// ----------------------------------------------------------------- enroll

/// A project ID as #652 proposes it: 128 random bits, base32. The ID is
/// `proj:<base32>`; its directory key is `proj-<base32>`.
fn mint_project_id() -> Result<String, Failure> {
    let mut bytes = [0_u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut source| source.read_exact(&mut bytes))
        .map_err(|error| Failure::domain("io", format!("cannot read randomness: {error}")))?;
    Ok(base32_lower(&bytes))
}

/// RFC 4648 base32, lowercase, unpadded.
fn base32_lower(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut out = String::new();
    let mut buffer = 0_u32;
    let mut bits = 0;
    for byte in bytes {
        buffer = (buffer << 8) | u32::from(*byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buffer >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// The worktree `aethyme collab` runs in: `--repo` or the current
/// directory's checkout (a session worktree or the main checkout).
fn current_worktree(repo: Option<&Path>) -> Result<PathBuf, Failure> {
    let start = match repo {
        Some(path) => path.to_path_buf(),
        None => std::env::current_dir().map_err(|error| {
            Failure::domain("io", format!("cannot read the current directory: {error}"))
        })?,
    };
    crate::git::GitRepo::discover(&start)
        .map(|repo| repo.root().to_path_buf())
        .map_err(|error| {
            Failure::refused(
                "not_a_repository",
                format!("{}: {error}", start.display()),
                "run it inside a Git repository, or pass --repo <path>",
            )
        })
}

fn enroll(main_root: &Path, worktree: &Path, rest: &[&str]) -> Result<Output, Failure> {
    let others: Vec<&str> = rest
        .iter()
        .copied()
        .filter(|word| *word != "--write")
        .collect();
    no_extra(&others, &[], 0)?;
    let write = rest.contains(&"--write");
    if let (
        Setting::On {
            project: Ok(project),
            ..
        },
        _,
    ) = setting(main_root)
    {
        return Err(Failure::refused(
            "already_enrolled",
            format!(
                "this repository is already enrolled as project {}",
                project.as_str()
            ),
            "nothing to do; `aethyme collab status` shows the policy",
        ));
    }
    let encoded = mint_project_id()?;
    let project_id = format!("proj:{encoded}");
    let project_key = format!("proj-{encoded}");
    let section = format!("[collaboration]\ncapture = \"advisory\"\nproject = \"{project_key}\"\n");
    let written = if write {
        Some(write_section(main_root, worktree, &section)?)
    } else {
        None
    };
    let next = match &written {
        Some(path) => format!(
            "review and commit {} on a branch that lands on the default branch: policy reads \
             the committed copy where one exists",
            path.display()
        ),
        None => "add this section to .aethyme/config.toml in a session worktree (or rerun there \
                 with --write), review and commit it"
            .to_string(),
    };
    let value = json!({
        "schema": ENROLL_SCHEMA,
        "project_id": project_id,
        "project_key": project_key,
        "section": section,
        "written": written.is_some(),
        "path": written,
        "next_action": next,
    });
    let text = format!(
        "project {project_id} (directory key {project_key})\n\n{section}\n{}next: {next}\n",
        written
            .as_ref()
            .map(|path| format!("written to {}\n", path.display()))
            .unwrap_or_default()
    );
    Ok((value, text))
}

/// Append `section` to `<worktree>/.aethyme/config.toml`, atomically, and
/// return the path written.
fn write_section(main_root: &Path, worktree: &Path, section: &str) -> Result<PathBuf, Failure> {
    let same = |a: &Path, b: &Path| match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    };
    if same(worktree, main_root) {
        let sessions = crate::BrokerStore::open_snapshot_in_repo(main_root)
            .and_then(|store| store.live_sessions())
            .map(|sessions| sessions.len())
            .unwrap_or(0);
        if sessions > 0 {
            return Err(Failure::refused(
                "main_checkout_in_use",
                format!(
                    "{} is the main checkout and {sessions} broker session(s) are live; \
                     editing its config would change policy under them",
                    worktree.display()
                ),
                "run `aethyme collab enroll --write` in a session worktree, then review and \
                 commit the change",
            ));
        }
    }
    let directory = worktree.join(".aethyme");
    let config = directory.join("config.toml");
    for path in [&directory, &config] {
        if std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink()) {
            return Err(Failure::refused(
                "symlinked_config",
                format!("{} is a symbolic link, so it is not edited", path.display()),
                "add the printed section by hand",
            ));
        }
    }
    let existing = match std::fs::read_to_string(&config) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(Failure::domain(
                "io",
                format!("{}: {error}", config.display()),
            ));
        }
    };
    // Both the file being edited and the policy in force (the committed
    // copy where one exists): never append a second [collaboration] table,
    // nor shadow one the policy reader would use.
    let in_force = match crate::merge::repository_config_checked(main_root) {
        Ok(Some((text, _))) => text,
        Ok(None) => String::new(),
        Err(error) => {
            return Err(Failure::refused(
                "config_unresolved",
                format!("the policy in force cannot be resolved: {error}; no config was changed"),
                "resolve the repository config and keep exactly one readable format",
            ));
        }
    };
    if crate::collaboration_submit::mentions_collaboration(&existing)
        || crate::collaboration_submit::mentions_collaboration(&in_force)
    {
        return Err(Failure::refused(
            "already_configured",
            format!(
                "{} or the policy in force already mentions collaboration, so nothing is \
                 appended",
                config.display()
            ),
            "add or fix the printed [collaboration] section by hand",
        ));
    }
    let separator = if existing.is_empty() || existing.ends_with("\n\n") {
        ""
    } else if existing.ends_with('\n') {
        "\n"
    } else {
        "\n\n"
    };
    std::fs::create_dir_all(&directory)
        .map_err(|error| Failure::domain("io", format!("{}: {error}", directory.display())))?;
    let bytes = format!("{existing}{separator}{section}");
    crate::atomic_file::with_synced_temporary(&config, bytes.as_bytes(), |temporary| {
        std::fs::rename(temporary, &config)
    })
    .map_err(|error| Failure::domain("io", format!("{}: {error}", config.display())))?;
    Ok(config)
}

// ---------------------------------------------------------------- capture

fn capture_recover(main_root: &Path) -> Result<Output, Failure> {
    let mut store = open(main_root)?;
    let report =
        recover(&mut store).map_err(|error| Failure::domain(error.code(), error.to_string()))?;
    let recovered: Vec<Value> = report
        .recovered
        .iter()
        .map(|r| json!({ "operation_id": r.operation_id.as_str(), "from": r.from, "to": r.to }))
        .collect();
    let errors: Vec<Value> = report
        .errors
        .iter()
        .map(|(id, error)| {
            json!({ "operation_id": id.as_str(), "code": error.code(), "message": error.to_string() })
        })
        .collect();
    let mut text = format!(
        "recovered {} capture(s); {} could not be resolved\n",
        recovered.len(),
        errors.len()
    );
    for r in &report.recovered {
        text.push_str(&format!(
            "  {} {} -> {}\n",
            r.operation_id.as_str(),
            r.from,
            r.to
        ));
    }
    for (id, error) in &report.errors {
        text.push_str(&format!("  {} {}: {error}\n", id.as_str(), error.code()));
    }
    Ok((
        json!({ "schema": CAPTURE_RECOVER_SCHEMA, "recovered": recovered, "errors": errors }),
        text,
    ))
}

fn capture_abort(main_root: &Path, rest: &[&str]) -> Result<Output, Failure> {
    let operation_id = operation(rest)?;
    let mut store = open(main_root)?;
    abort(&mut store, &operation_id)
        .map_err(|error| Failure::domain(error.code(), error.to_string()))?;
    Ok((
        json!({ "schema": CAPTURE_ABORT_SCHEMA, "operation_id": operation_id.as_str(), "aborted": true }),
        format!("aborted {}\n", operation_id.as_str()),
    ))
}

fn capture_receipt(main_root: &Path, rest: &[&str]) -> Result<Output, Failure> {
    let operation_id = operation(rest)?;
    let mut store = open(main_root)?;
    let found = receipt(&mut store, &operation_id)
        .map_err(|error| Failure::domain(error.code(), error.to_string()))?;
    let Some(receipt) = found else {
        return Err(Failure::refused(
            "no_receipt",
            format!("{} has no committed receipt", operation_id.as_str()),
            "run `aethyme collab status` to see its state",
        ));
    };
    let value = json!({
        "schema": CAPTURE_RECEIPT_SCHEMA,
        "operation_id": receipt.operation_id.as_str(),
        "status": receipt.status,
        "durability": receipt.durability,
        "contribution": receipt.contribution.as_str(),
        "base": receipt.base.as_str(),
        "result": receipt.result.as_str(),
        "retention": receipt.retention.describe(),
        "receipt_record_id": receipt.receipt_record_id.as_str(),
    });
    let text = format!(
        "{} {} ({})\n  contribution {}\n  base {}\n  result {}\n  retention {}\n",
        receipt.operation_id.as_str(),
        receipt.status,
        receipt.durability,
        receipt.contribution.as_str(),
        receipt.base.as_str(),
        receipt.result.as_str(),
        receipt.retention.describe()
    );
    Ok((value, text))
}

// --------------------------------------------------------------------- gc

fn with_schema(schema: &str, value: impl serde::Serialize) -> Value {
    let mut value = serde_json::to_value(value).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        map.insert("schema".into(), json!(schema));
    }
    value
}

fn gc_plan_command(main_root: &Path) -> Result<Output, Failure> {
    let mut store = open(main_root)?;
    let plan = gc_plan(&mut store, &GcOptions::default())
        .map_err(|error| Failure::domain(error.code(), error.to_string()))?;
    let mut text = format!(
        "plan {}: {} item(s), {} bytes reclaimable; {} bytes protected\n",
        plan.digest,
        plan.reclaimable.len(),
        plan.reclaimable_bytes,
        plan.protected_bytes
    );
    for blocker in &plan.blockers {
        text.push_str(&format!("  blocker {}: {}\n", blocker.kind, blocker.detail));
    }
    if !plan.unknown.is_empty() {
        text.push_str(&format!(
            "  {} unknown file(s) are kept and not judged\n",
            plan.unknown.len()
        ));
    }
    text.push_str(&format!("  next: {}\n", plan.next_action));
    // A plan is recorded, and can be applied, only when nothing blocks it
    // and it names something to reclaim.
    let recorded =
        plan.blockers.is_empty() && !(plan.reclaimable.is_empty() && plan.entities.is_empty());
    let mut value = with_schema(GC_PLAN_SCHEMA, &plan);
    value["recorded"] = json!(recorded);
    Ok((value, text))
}

fn gc_apply_command(main_root: &Path, rest: &[&str]) -> Result<Output, Failure> {
    no_extra(rest, &["--confirm"], 0)?;
    let digest = flag_value(rest, "--confirm")?.ok_or_else(|| {
        Failure::usage("--confirm <digest> is required; `aethyme collab gc plan` prints it")
    })?;
    let mut store = open(main_root)?;
    let report = gc_apply(&mut store, digest, &GcOptions::default())
        .map_err(|error| Failure::domain(error.code(), error.to_string()))?;
    let mut text = format!(
        "reclaimed {} item(s), {} bytes; skipped {}\n",
        report.reclaimed.len(),
        report.reclaimed_bytes,
        report.skipped.len()
    );
    for (item, reason) in &report.skipped {
        text.push_str(&format!("  skipped {} ({reason})\n", item.relpath));
    }
    Ok((with_schema(GC_APPLY_SCHEMA, &report), text))
}

fn gc_resume_command(main_root: &Path) -> Result<Output, Failure> {
    let mut store = open(main_root)?;
    let resumed = gc_resume(&mut store, &GcOptions::default())
        .map_err(|error| Failure::domain(error.code(), error.to_string()))?;
    let mut text = format!("finished {} interrupted apply(s)\n", resumed.len());
    for generation in &resumed {
        text.push_str(&format!(
            "  generation {}: {} removed, {} restored\n",
            generation.generation, generation.removed, generation.restored
        ));
    }
    Ok((
        json!({ "schema": GC_RESUME_SCHEMA, "resumed": resumed }),
        text,
    ))
}

// ---------------------------------------------------------------- context

fn number(rest: &[&str], name: &str, default: usize) -> Result<usize, Failure> {
    match flag_value(rest, name)? {
        None => Ok(default),
        Some(text) => text
            .parse()
            .map_err(|_| Failure::usage(format!("{name} needs a whole number, not {text:?}"))),
    }
}

fn context(main_root: &Path, rest: &[&str]) -> Result<Output, Failure> {
    const FLAGS: &[&str] = &[
        "--path",
        "--revision-pin",
        "--source",
        "--analysis",
        "--max-items",
        "--max-brief-tokens",
        "--max-matched-paths",
        "--max-bytes",
    ];
    no_extra(rest, FLAGS, 0)?;
    let pins = context_revisions::pins(rest)?;
    let mut scope = Vec::new();
    let mut analysis = Vec::new();
    let mut index = 0;
    while index < rest.len() {
        let value = rest.get(index + 1).copied();
        match rest[index] {
            "--path" => scope.push(
                value
                    .ok_or_else(|| Failure::usage("--path needs a value"))?
                    .as_bytes()
                    .to_vec(),
            ),
            "--analysis" => {
                let file = value.ok_or_else(|| Failure::usage("--analysis needs a file"))?;
                let bytes = read_capped(file, MAX_ANALYSIS_FILE_BYTES, "analysis envelope")?;
                analysis.push(AnalysisEnvelope::from_record(&bytes).map_err(|error| {
                    Failure::refused(
                        "invalid_analysis",
                        format!("{file}: {error}"),
                        "pass an aethyme.analysis-result record",
                    )
                })?);
            }
            _ => {}
        }
        index += 2;
    }
    if scope.is_empty() {
        return Err(Failure::usage("at least one --path is required"));
    }
    let source = flag_value(rest, "--source")?
        .map(|text| {
            SourceSnapshotId::parse(text).map_err(|error| {
                Failure::refused(
                    "invalid_source",
                    format!("{text}: {error}"),
                    "pass a sha256:<64 hex> source snapshot ID",
                )
            })
        })
        .transpose()?;
    let defaults = Budget::default();
    let budget = Budget {
        max_items: number(rest, "--max-items", defaults.max_items)?,
        max_brief_tokens: number(rest, "--max-brief-tokens", defaults.max_brief_tokens)?,
        max_matched_paths: number(rest, "--max-matched-paths", defaults.max_matched_paths)?,
        max_bytes: number(rest, "--max-bytes", defaults.max_bytes)?,
    };
    let mut store = open(main_root)?;
    let query = ContextQuery {
        scope,
        source,
        analysis,
        budget,
    };
    if !pins.is_empty() {
        return context_revisions::retrieve(&store, &query, &pins);
    }
    let answer = crate::collaboration_context::retrieve_cached(&mut store, &query, &LocalProject)
        .map_err(|error| Failure::domain(error.code(), error.to_string()))?;
    let record: Value = serde_json::from_slice(&answer.record)
        .map_err(|error| Failure::domain("state", format!("context record: {error}")))?;
    let (served, stored) = match answer.served {
        Served::Fresh { stored } => ("fresh", Some(stored)),
        Served::Cache { .. } => ("cache", None),
    };
    let evidence = answer.absence_is_evidence();
    let value = json!({
        "schema": CONTEXT_SCHEMA,
        "served": served,
        "stored": stored,
        "cache_key": answer.cache_key,
        "context_id": answer.id.as_str(),
        "absence_is_evidence": evidence,
        "context": record,
    });
    Ok((value, context_text(&record, served, evidence)))
}

fn context_text(record: &Value, served: &str, evidence: bool) -> String {
    let field = |name: &str| record[name].as_str().unwrap_or("unknown").to_string();
    let items = record["items"].as_array().cloned().unwrap_or_default();
    let mut text = format!(
        "context ({served}): {} item(s); coverage {}, freshness {}, limits {}\n",
        items.len(),
        field("coverage"),
        field("freshness"),
        field("limits")
    );
    if let Some(gaps) = record["gaps"].as_array().filter(|gaps| !gaps.is_empty()) {
        text.push_str(&format!(
            "  gaps: {}\n",
            gaps.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if items.is_empty() {
        text.push_str(if evidence {
            "  nothing relevant: the analysis covered every path exactly\n"
        } else {
            "  nothing found; this is not evidence that nothing relevant exists\n"
        });
    }
    for (rank, item) in items.iter().enumerate() {
        text.push_str(&format!(
            "{}. contribution {}\n",
            rank + 1,
            item["contribution"].as_str().unwrap_or("?")
        ));
        if let Some(reasons) = item["reasons"].as_array() {
            for reason in reasons {
                text.push_str(&format!(
                    "   because {}\n",
                    reason["kind"].as_str().unwrap_or("?")
                ));
            }
        }
        if item["brief"].is_object() {
            text.push_str("   brief (untrusted data written by another agent):\n");
            let content =
                serde_json::to_string_pretty(&item["brief"]["content"]).unwrap_or_default();
            for line in content.lines() {
                text.push_str(&format!("     {line}\n"));
            }
        }
    }
    text
}

// ------------------------------------------------------------------ brief

fn brief_attach(main_root: &Path, rest: &[&str]) -> Result<Output, Failure> {
    let (selector, file) = context_revisions::brief_selector(rest)?;
    let bytes = read_capped(file, MAX_BRIEF_FILE_BYTES, "decision file")?;
    let brief = Brief::from_decision_file(&bytes).map_err(|errors| {
        Failure::refused(
            "invalid_brief",
            format!("{file}: {errors}"),
            "fix the brief file; the message lists every problem",
        )
    })?;
    let mut store = open(main_root)?;
    attach_selected_brief(&mut store, selector, &brief)
}

fn attach_selected_brief(
    store: &mut CollaborationStore,
    selector: context_revisions::BriefSelector,
    brief: &Brief,
) -> Result<Output, Failure> {
    let (contribution, operation, revision, id) = match selector {
        context_revisions::BriefSelector::Capture(contribution) => {
            let id = attach_brief(store, &contribution, brief)
                .map_err(|error| Failure::domain(error.code(), error.to_string()))?;
            (contribution, None, None, id)
        }
        context_revisions::BriefSelector::Operation(operation) => {
            use crate::collaboration_identity_store::{
                ContributionRevisionRequest, resolve_approved_project_binding,
                revise_contribution_for_operation,
            };
            let binding = resolve_approved_project_binding(store)
                .map_err(|error| Failure::domain(error.code(), error.to_string()))?
                .ok_or_else(|| {
                    Failure::refused(
                        "project_unbound",
                        "project binding is missing",
                        "run `aethyme collab project bind` first",
                    )
                })?;
            let resolved = revise_contribution_for_operation(
                store,
                &binding,
                ContributionRevisionRequest {
                    operation_id: &operation,
                    file_operations: None,
                    brief: Some(brief),
                    depends_on: None,
                },
            )
            .map_err(|error| Failure::domain(error.code(), error.to_string()))?;
            let id = resolved
                .revision()
                .brief()
                .ok_or_else(|| {
                    Failure::domain(
                        "revision_identity",
                        "committed revision has no requested Brief".to_string(),
                    )
                })?
                .record_id
                .clone();
            (
                resolved.capture_record_id().clone(),
                Some(operation),
                Some(resolved.record_id().clone()),
                id,
            )
        }
    };
    let mut value = json!({
        "schema": BRIEF_ATTACH_SCHEMA,
        "contribution": contribution.as_str(),
        "brief_record_id": id.as_str(),
        "tokens": brief.token_count(),
    });
    if let Some(operation) = operation {
        value["operation"] = json!(operation.as_str());
        value["contribution_revision"] = json!(revision.as_ref().map(RecordId::as_str));
    }
    Ok((
        value,
        format!(
            "attached brief {} ({} tokens) to {}\n",
            id.as_str(),
            brief.token_count(),
            contribution.as_str()
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base32_matches_rfc_4648_vectors() {
        // RFC 4648 §10, lowercased and unpadded.
        for (input, expected) in [
            ("", ""),
            ("f", "my"),
            ("fo", "mzxq"),
            ("foo", "mzxw6"),
            ("foob", "mzxw6yq"),
            ("fooba", "mzxw6ytb"),
            ("foobar", "mzxw6ytboi"),
        ] {
            assert_eq!(base32_lower(input.as_bytes()), expected, "{input}");
        }
        assert_eq!(base32_lower(&[0xff; 16]).len(), 26);
    }

    #[test]
    fn native_gate_retention_route_is_named_and_documented() {
        let command = [
            "target".to_string(),
            "native-gates".to_string(),
            "release".to_string(),
        ];
        assert_eq!(command_name(&command), "target native-gates");
        for verb in ["list", "status", "release"] {
            let route = format!("aethyme collab target native-gates {verb}");
            assert!(USAGE.contains(route.as_str()));
        }
    }

    #[test]
    fn bootstrap_routes_are_named_and_documented() {
        for (verbs, command, usage) in [
            (
                &["target", "budget", "project"][..],
                "target budget",
                "aethyme collab target budget project",
            ),
            (
                &["target", "budget", "objective"][..],
                "target budget",
                "aethyme collab target budget objective",
            ),
            (
                &["target", "budget", "status"][..],
                "target budget",
                "aethyme collab target budget status",
            ),
            (
                &["target", "verifier", "prepare"][..],
                "target verifier",
                "aethyme collab target verifier prepare",
            ),
            (
                &["target", "verifier", "configure"][..],
                "target verifier",
                "aethyme collab target verifier configure",
            ),
            (
                &["target", "verifier", "status"][..],
                "target verifier",
                "aethyme collab target verifier status",
            ),
            (
                &["target", "verifier", "revoke"][..],
                "target verifier",
                "aethyme collab target verifier revoke",
            ),
            (
                &["target", "capture-notice", "ingest"][..],
                "target capture-notice",
                "aethyme collab target capture-notice ingest",
            ),
            (
                &["target", "capture-notice", "list"][..],
                "target capture-notice",
                "aethyme collab target capture-notice list",
            ),
            (
                &["target", "run", "status"][..],
                "target run",
                "aethyme collab target run status",
            ),
        ] {
            let args: Vec<String> = verbs.iter().map(|verb| (*verb).into()).collect();
            assert_eq!(command_name(&args), command, "{verbs:?}");
            assert!(USAGE.contains(usage), "missing help route {usage}");
        }
    }

    #[test]
    fn a_minted_project_key_is_a_valid_directory_key() {
        let encoded = mint_project_id().unwrap();
        assert_eq!(encoded.len(), 26);
        ProjectKey::parse(&format!("proj-{encoded}")).unwrap();
        assert_ne!(encoded, mint_project_id().unwrap());
    }

    /// Exit 3 means "refused, do not retry unchanged"; an integrity or local
    /// failure is exit 1.
    #[test]
    fn integrity_failures_exit_1_and_refusals_exit_3() {
        for code in [
            "io",
            "sqlite",
            "corrupt_receipt",
            "corrupt_object",
            "missing_object",
            "failed",
        ] {
            assert_eq!(
                Failure::domain(code, String::new()).exit,
                exit_status::FAILED,
                "{code}"
            );
        }
        for code in [
            "archive_in_use",
            "already_committed",
            "not_retained",
            "blocked",
        ] {
            assert_eq!(
                Failure::domain(code, String::new()).exit,
                exit_status::REFUSED,
                "{code}"
            );
        }
    }

    /// The error object's `command` is known verbs only.
    #[test]
    fn the_command_name_never_echoes_arguments() {
        let words = |list: &[&str]| list.iter().map(|w| w.to_string()).collect::<Vec<_>>();
        assert_eq!(
            command_name(&words(&["gc", "apply", "--confirm", "x"])),
            "gc apply"
        );
        assert_eq!(
            command_name(&words(&["context", "--path", "/secret"])),
            "context"
        );
        assert_eq!(command_name(&words(&["/secret/path"])), "unknown");
        assert_eq!(command_name(&words(&["--repo", "/secret", "status"])), "");
        assert_eq!(command_name(&words(&["status", "/secret"])), "unknown");
    }

    #[test]
    fn every_refusal_code_with_a_remedy_names_one() {
        for code in [
            "overlaps_cleanup_root",
            "insecure_permissions",
            "archive_in_use",
            "blocked",
            "stale_plan",
            "invalid_brief",
        ] {
            assert!(next_action_for(code).is_some(), "{code}");
        }
    }
}
