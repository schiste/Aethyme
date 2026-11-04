//! Explicit, exact-namespace launchd control. No provider polling or retries.
use super::*;
use crate::scheduled_service::launchd::Observation;
#[cfg(target_os = "macos")]
use crate::scheduled_service::launchd::launchctl;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

fn hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

pub(crate) fn checked_file(path: &Path, uid: u32) -> io::Result<Vec<u8>> {
    let (bytes, metadata) = existing(path, uid)?
        .ok_or_else(|| invalid("service file is missing; inspect installation first"))?;
    if metadata.nlink() != 1 {
        return Err(invalid("service file still has unresolved hard links"));
    }
    Ok(bytes)
}

// Read-only traversal: status and stop must never create an installation.
pub(crate) fn check_directories(
    home: &Path,
    path: &Path,
    uid: u32,
    private: bool,
) -> io::Result<()> {
    directory(home, uid, false)?;
    let mut current = home.to_path_buf();
    for part in path
        .strip_prefix(home)
        .map_err(|_| invalid("service escaped home"))?
        .components()
    {
        let Component::Normal(name) = part else {
            return Err(invalid("invalid service path"));
        };
        current.push(name);
        directory(&current, uid, false)?;
    }
    directory(path, uid, private)
}

fn load(home: &Path, uuid: &str) -> io::Result<(Plan, Vec<u8>, Value)> {
    if uuid.len() != 36
        || !uuid.bytes().enumerate().all(|(i, c)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                c == b'-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
    {
        return Err(invalid("invalid repository service UUID"));
    }
    // SAFETY: geteuid reads this process identity without pointer arguments.
    let uid = unsafe { libc::geteuid() };
    if uid == 0 || !home.is_absolute() {
        return Err(invalid("service requires non-root absolute HOME"));
    }
    let home = home.canonicalize()?;
    let label = format!("com.aethyme.pr-watch.{uuid}");
    let root = home
        .join("Library/Application Support/Aethyme/watch-services")
        .join(uuid);
    check_directories(&home, &root, uid, true)?;
    let bytes = checked_file(&root.join("plan.json"), uid)?;
    let plan: Plan = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    let value: Value = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    if plan.schema != "aethyme.pr-watch-service-plan/v1"
        || plan.repository_uuid != uuid
        || plan.uid != uid
        || plan.home != home
        || plan.directory != root
        || plan.label != label
        || plan.unit_path
            != home
                .join("Library/LaunchAgents")
                .join(format!("{label}.plist"))
        || !(120..=3600).contains(&plan.interval_seconds)
        || !hex(&plan.digest)
    {
        return Err(invalid(
            "installed service namespace does not match this repository and user",
        ));
    }
    // Verify the original producer's exact bytes, including older manifests
    // without optional fields. Re-serializing a newer struct changes that input.
    let text = std::str::from_utf8(&bytes).map_err(io::Error::other)?;
    let needle = format!("\"digest\":\"{}\"", plan.digest);
    if text.matches(&needle).count() != 1 {
        return Err(invalid(
            "service manifest is not the exact producer encoding",
        ));
    }
    let unsigned = text.replacen(&needle, "\"digest\":\"\"", 1);
    if format!("{:x}", Sha256::digest(unsigned.as_bytes())) != plan.digest {
        return Err(invalid(
            "service manifest digest does not match its exact bytes",
        ));
    }
    check_directories(&home, &home.join("Library/LaunchAgents"), uid, false)?;
    if checked_file(&plan.unit_path, uid)? != plan.unit.as_bytes() {
        return Err(invalid("installed service unit changed"));
    }
    Ok((plan, bytes, value))
}

fn health(root: &Path, uid: u32, generation: Option<&str>, interval: u64, now_ms: u64) -> Value {
    let observed = checked_file(&root.join("health.json"), uid)
        .ok()
        .filter(|b| b.len() <= 4096)
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    let Some(value) = observed else {
        return json!({"state":"unavailable"});
    };
    let stamp = value.get("finished_unix_ms").and_then(Value::as_u64);
    let bound = generation.is_some_and(hex)
        && value.get("generation").and_then(Value::as_str) == generation
        && value.get("identity").and_then(Value::as_str) == Some("verified")
        && value.get("schema").and_then(Value::as_str) == Some("aethyme.pr-monitor-health/v1");
    let state = if !bound {
        "unbound"
    } else if stamp.is_some_and(|s| s <= now_ms && now_ms - s <= (interval + 120) * 1000) {
        "bound_recent"
    } else {
        "bound_stale_or_clock_unknown"
    };
    // These are observations, never a readiness or notification-delivery claim.
    let allowed = |key: &str, choices: &[&str]| -> Option<String> {
        value
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| choices.contains(s))
            .map(str::to_owned)
    };
    json!({"state":state,"finished_unix_ms":stamp,
        "tick":allowed("tick", &["completed","failed","unknown"]),
        "outbox":allowed("outbox", &["empty","pending","unknown"]),
        "delivery":allowed("delivery", &["attempted","not_started"]),"delivery_proven":false})
}

#[cfg(target_os = "macos")]
pub(crate) fn control(
    home: &Path,
    uuid: &str,
    action: &str,
    confirmation: Option<&str>,
) -> io::Result<Value> {
    control_with(home, uuid, action, confirmation, launchctl)
}

fn control_with(
    home: &Path,
    uuid: &str,
    action: &str,
    confirmation: Option<&str>,
    mut run: impl FnMut(&[String]) -> io::Result<Observation>,
) -> io::Result<Value> {
    if !matches!(action, "status" | "activate" | "deactivate") {
        return Err(invalid("unknown service control"));
    }
    let (plan, bytes, value) = load(home, uuid)?;
    if action != "status" && confirmation != Some(plan.digest.as_str()) {
        return Err(invalid(
            "service control requires the exact installed plan digest",
        ));
    }
    let _claim = if action == "status" {
        None
    } else {
        Some(claim_installer(&plan.directory, plan.uid)?)
    };
    if action != "status" && load(home, uuid)?.1 != bytes {
        return Err(invalid(
            "service installation changed while claiming control",
        ));
    }
    if action == "activate" {
        let fresh = Plan::new(
            &plan.repository,
            uuid,
            &plan.home,
            &plan.broker,
            &plan.python,
            plan.interval_seconds,
            plan.uid,
        )?;
        if serde_json::to_vec(&fresh).map_err(io::Error::other)? != bytes {
            return Err(invalid(
                "service generation is obsolete; review a new plan before activation",
            ));
        }
        for (name, content) in script_contents() {
            if checked_file(&plan.directory.join(name), plan.uid)? != content.as_bytes() {
                return Err(invalid("installed service helper changed"));
            }
        }
    }
    let target = format!("gui/{}/{}", plan.uid, plan.label);
    let args = match action {
        "activate" => vec![
            "bootstrap".into(),
            format!("gui/{}", plan.uid),
            plan.unit_path.to_string_lossy().into_owned(),
        ],
        "deactivate" => vec!["bootout".into(), target],
        _ => vec!["print".into(), target],
    };
    // One call only. Unknown outcomes require observation, never blind retries.
    let observation = run(&args)?;
    let scheduler = match (action, &observation) {
        ("deactivate", Observation::Success) => "unloaded",
        (_, Observation::Success) => "loaded",
        _ => "unknown",
    };
    let result = match observation {
        Observation::Success => "acknowledged",
        Observation::Refused => "refused",
        Observation::Unknown => "unknown",
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_millis();
    let generation = value.get("generation").and_then(Value::as_str);
    Ok(
        json!({"schema":"aethyme.pr-watch-service-control/v1", "action":action,
        "plan_digest":plan.digest,"generation":generation,"scheduler_state":scheduler,
        "operation_result":result,"last_pass":health(&plan.directory,plan.uid,generation,plan.interval_seconds,u64::try_from(now).unwrap_or(u64::MAX)),
        "provider_polled_by_control":false,"delivery_proven":false}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pr_watch_service_install::tests::fixture;
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn installed() -> (tempfile::TempDir, Plan) {
        let (temp, plan) = fixture();
        install(&plan, &plan.digest).unwrap();
        (temp, plan)
    }

    #[test]
    fn exact_confirmation_and_helpers_gate_activation() {
        let (_temp, plan) = installed();
        let mut calls = 0;
        assert!(
            control_with(
                &plan.home,
                &plan.repository_uuid,
                "activate",
                Some("wrong"),
                |_| {
                    calls += 1;
                    Ok(Observation::Success)
                }
            )
            .is_err()
        );
        fs::write(plan.directory.join("aethyme-pr-monitor.py"), "changed").unwrap();
        assert!(
            control_with(
                &plan.home,
                &plan.repository_uuid,
                "activate",
                Some(&plan.digest),
                |_| {
                    calls += 1;
                    Ok(Observation::Success)
                }
            )
            .is_err()
        );
        assert_eq!(calls, 0);
    }

    #[test]
    fn activation_dispatches_only_exact_namespace_once() {
        let (_temp, plan) = installed();
        let mut calls = Vec::new();
        let result = control_with(
            &plan.home,
            &plan.repository_uuid,
            "activate",
            Some(&plan.digest),
            |args| {
                calls.push(args.to_vec());
                Ok(Observation::Success)
            },
        )
        .unwrap();
        assert_eq!(
            calls,
            vec![vec![
                "bootstrap".to_owned(),
                format!("gui/{}", plan.uid),
                plan.unit_path.to_string_lossy().into_owned()
            ]]
        );
        assert_eq!(result["scheduler_state"], "loaded");
        assert_eq!(result["delivery_proven"], false);
    }

    #[test]
    fn retired_runtime_can_be_stopped_without_hashing_or_executing_it() {
        let (_temp, plan) = installed();
        fs::remove_file(&plan.broker).unwrap();
        let mut calls = Vec::new();
        let result = control_with(
            &plan.home,
            &plan.repository_uuid,
            "deactivate",
            Some(&plan.digest),
            |args| {
                calls.push(args.to_vec());
                Ok(Observation::Success)
            },
        )
        .unwrap();
        assert_eq!(
            calls,
            vec![vec![
                "bootout".to_owned(),
                format!("gui/{}/{}", plan.uid, plan.label)
            ]]
        );
        assert_eq!(result["scheduler_state"], "unloaded");
    }

    #[test]
    fn scheduler_errors_are_unknown_and_never_retried() {
        let (_temp, plan) = installed();
        for action in ["status", "activate", "deactivate"] {
            for outcome in [Observation::Refused, Observation::Unknown] {
                let mut count = 0;
                let result = control_with(
                    &plan.home,
                    &plan.repository_uuid,
                    action,
                    Some(&plan.digest),
                    |_| {
                        count += 1;
                        Ok(outcome.clone())
                    },
                )
                .unwrap();
                assert_eq!(count, 1);
                assert_eq!(result["scheduler_state"], "unknown");
                assert_eq!(result["last_pass"]["state"], "unavailable");
            }
        }
    }

    #[test]
    fn forged_names_and_reencoded_manifests_never_reach_launchctl() {
        let (_temp, plan) = installed();
        let manifest = plan.directory.join("plan.json");
        let original = fs::read(&manifest).unwrap();
        let mut forged: Value = serde_json::from_slice(&original).unwrap();
        forged["label"] = json!("other.service");
        // A valid content digest must not authorize a different service target.
        forged["digest"] = json!("");
        let signed = format!("{:x}", Sha256::digest(serde_json::to_vec(&forged).unwrap()));
        forged["digest"] = json!(signed);
        fs::write(&manifest, serde_json::to_vec(&forged).unwrap()).unwrap();
        assert!(
            control_with(
                &plan.home,
                &plan.repository_uuid,
                "deactivate",
                Some(&signed),
                |_| panic!("must refuse forged target")
            )
            .unwrap_err()
            .to_string()
            .contains("namespace")
        );
        fs::write(
            &manifest,
            serde_json::to_vec_pretty(&serde_json::from_slice::<Value>(&original).unwrap())
                .unwrap(),
        )
        .unwrap();
        assert!(
            control_with(
                &plan.home,
                &plan.repository_uuid,
                "status",
                None,
                |_| panic!("must refuse reencoding")
            )
            .is_err()
        );
    }

    #[test]
    fn lock_contention_and_linked_helpers_do_not_dispatch() {
        let (_temp, plan) = installed();
        let claim = claim_installer(&plan.directory, plan.uid).unwrap();
        assert!(
            control_with(
                &plan.home,
                &plan.repository_uuid,
                "deactivate",
                Some(&plan.digest),
                |_| panic!("must refuse busy installer")
            )
            .is_err()
        );
        drop(claim);
        let monitor = plan.directory.join("aethyme-pr-monitor.py");
        fs::remove_file(&monitor).unwrap();
        symlink(&plan.broker, &monitor).unwrap();
        assert!(
            control_with(
                &plan.home,
                &plan.repository_uuid,
                "activate",
                Some(&plan.digest),
                |_| panic!("must refuse symlink")
            )
            .is_err()
        );
    }

    #[test]
    fn health_requires_generation_identity_and_fresh_clock_without_claiming_delivery() {
        let (_temp, plan) = installed();
        let path = plan.directory.join("health.json");
        let generation = "a".repeat(64);
        let mut value = json!({"schema":"aethyme.pr-monitor-health/v1","generation":generation,"identity":"verified","finished_unix_ms":1000000,"delivery":"attempted","tick":"completed","outbox":"empty"});
        let write = |v: &Value| {
            fs::write(&path, serde_json::to_vec(v).unwrap()).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        };
        write(&value);
        let report = health(&plan.directory, plan.uid, Some(&generation), 300, 1000001);
        assert_eq!(report["state"], "bound_recent");
        assert_eq!(report["delivery_proven"], false);
        assert_eq!(
            health(&plan.directory, plan.uid, Some(&generation), 300, 2000000)["state"],
            "bound_stale_or_clock_unknown"
        );
        assert_eq!(
            health(&plan.directory, plan.uid, Some(&generation), 300, 999999)["state"],
            "bound_stale_or_clock_unknown"
        );
        value["identity"] = json!("failed");
        write(&value);
        assert_eq!(
            health(&plan.directory, plan.uid, Some(&generation), 300, 1000001)["state"],
            "unbound"
        );
        assert_eq!(
            health(&plan.directory, plan.uid, None, 300, 1000001)["state"],
            "unbound"
        );
    }
}
