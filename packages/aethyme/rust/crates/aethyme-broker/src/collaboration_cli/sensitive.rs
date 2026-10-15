use super::*;
use crate::collaboration_sensitive::{self as response, ReasonCode};

fn response_error(error: response::SensitiveError) -> Failure {
    let code = match &error {
        response::SensitiveError::Quarantined => "sensitive_response_quarantined",
        response::SensitiveError::Mismatch => "sensitive_request_mismatch",
        response::SensitiveError::NotFrozen => "authority_not_frozen",
        response::SensitiveError::Busy => "sensitive_work_active",
        response::SensitiveError::UnknownWork => "sensitive_work_unknown",
        response::SensitiveError::UnclassifiedFiles => "sensitive_residuals",
        response::SensitiveError::Io(_) => "io",
        response::SensitiveError::Sqlite(_) => "sqlite",
        response::SensitiveError::Json(_) => "invalid_sensitive_state",
        response::SensitiveError::State(_) => "state",
        response::SensitiveError::Authority(_) => "authority_unavailable",
        response::SensitiveError::Gc(_) => "archive_unavailable",
        response::SensitiveError::Invalid(_) => "invalid_sensitive_state",
    };
    Failure::domain(code, error.to_string())
}

fn reason(value: &str) -> Result<ReasonCode, Failure> {
    match value {
        "suspected_test_secret_exposure" => Ok(ReasonCode::SuspectedTestSecretExposure),
        "misrouted_context" => Ok(ReasonCode::MisroutedContext),
        "operator_requested_purge" => Ok(ReasonCode::OperatorRequestedPurge),
        _ => Err(Failure::usage(
            "--reason must be a supported fixed reason code",
        )),
    }
}

pub(super) fn command(root: &Path, verb: &str, rest: &[&str]) -> Result<Output, Failure> {
    let mut store = open(root)?;
    match verb {
        "quarantine" => {
            let flags = Flags::parse(
                rest,
                &["--incident", "--issuer", "--epoch", "--reason", "--scope"],
                &[],
            )?;
            let scope = flags.one("--scope")?;
            if scope != "project" {
                return Err(Failure::usage("--scope must be exactly `project`"));
            }
            let result = response::quarantine(
                &mut store,
                flags.one("--incident")?,
                flags.one("--issuer")?,
                flags.one("--epoch")?,
                reason(flags.one("--reason")?)?,
            )
            .map_err(response_error)?;
            let digest = result.request_digest.clone();
            Ok((
                json!({
                    "schema":"aethyme.collab-sensitive-quarantine/experimental-v0",
                    "status":result,
                    "confirmation_digest":digest,
                    "authority":"trusted local OS operator; issuer label is audit text",
                    "purge_command":format!("aethyme collab sensitive purge --incident {} --confirm {}", flags.one("--incident")?, digest),
                }),
                format!(
                    "Project permanently quarantined under incident {}; confirm managed purge with digest {}\n",
                    flags.one("--incident")?,
                    digest
                ),
            ))
        }
        "status" => {
            let flags = Flags::parse(rest, &["--incident"], &[])?;
            let result =
                response::status(&store, flags.one("--incident")?).map_err(response_error)?;
            Ok((
                json!({"schema":"aethyme.collab-sensitive-status/experimental-v0","status":result}),
                format!("Sensitive response state: {}\n", result.state),
            ))
        }
        "purge" | "resume" => {
            let flags = Flags::parse(rest, &["--incident", "--confirm"], &[])?;
            let result = response::purge(
                &mut store,
                flags.one("--incident")?,
                flags.one("--confirm")?,
            )
            .map_err(response_error)?;
            let state = result.status.state.clone();
            let completed = result.completed;
            let schema = if verb == "resume" {
                "aethyme.collab-sensitive-resume/experimental-v0"
            } else {
                "aethyme.collab-sensitive-purge/experimental-v0"
            };
            Ok((
                json!({"schema":schema,"completed":completed,"status":result.status}),
                if completed {
                    format!(
                        "Managed collaboration-store purge completed; state {state}. External residuals remain listed.\n"
                    )
                } else {
                    format!(
                        "Managed purge did not complete; the project remains quarantined ({state}). Run status and reconcile listed blockers.\n"
                    )
                },
            ))
        }
        _ => Err(Failure::usage("unknown sensitive-response command")),
    }
}
