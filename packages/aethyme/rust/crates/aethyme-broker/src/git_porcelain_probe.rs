//! Shared Git output integrity probe for the broker and contract gate.
use std::path::Path;
use std::process::Command;

/// What one candidate did when asked for porcelain it could not embellish.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PorcelainProbe {
    Undecorated,
    Decorated {
        bytes: usize,
    },
    /// Said the right bytes and the wrong thing: an exit code that does not
    /// match the repository state the probe built. Separate from
    /// [`PorcelainProbe::Decorated`] only so the rejection can say which
    /// kind of dishonesty it found; both refuse the candidate.
    StatusRewritten {
        detail: String,
    },
    Unusable {
        reason: String,
    },
}

/// Run `program` against a freshly created empty repository and require
/// `git status --porcelain` to say nothing at all.
///
/// The repository is built with an empty template and an empty global config
/// so the answer describes the binary rather than the machine's hooks or
/// aliases: a `status.showUntrackedFiles` setting or a template hook that
/// prints would otherwise be indistinguishable from a wrapper.
pub(crate) fn porcelain_probe(program: &Path) -> PorcelainProbe {
    // A recognisable name: a process killed mid-probe (or one that replaces
    // itself) leaves this directory behind, and the background worker removes
    // stale ones by this prefix and shape (`git_probe_cache`).
    let probe = match tempfile::Builder::new()
        .prefix(crate::git_probe_cache::PROBE_DIRECTORY_PREFIX)
        .tempdir()
    {
        Ok(probe) => probe,
        Err(err) => {
            return PorcelainProbe::Unusable {
                reason: format!("could not create a probe directory ({err})"),
            };
        }
    };
    let template = probe.path().join("empty-template");
    let repo = probe.path().join("repo");
    let config = probe.path().join("empty-gitconfig");
    if let Err(err) = std::fs::create_dir(&template)
        .and_then(|()| std::fs::create_dir(&repo))
        .and_then(|()| std::fs::write(&config, []))
    {
        return PorcelainProbe::Unusable {
            reason: format!("could not prepare the probe repository ({err})"),
        };
    }

    let isolated = |program: &Path| {
        let mut command = Command::new(program);
        command.current_dir(&repo);
        crate::git::remove_git_environment(&mut command);
        command
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", &config)
            .env("GIT_AUTHOR_NAME", "aethyme probe")
            .env("GIT_AUTHOR_EMAIL", "probe@aethyme.invalid")
            .env("GIT_COMMITTER_NAME", "aethyme probe")
            .env("GIT_COMMITTER_EMAIL", "probe@aethyme.invalid");
        command
    };

    match isolated(program)
        .args(["init", "-q"])
        .arg(format!("--template={}", template.display()))
        .output()
    {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            return PorcelainProbe::Unusable {
                reason: format!("git init exited {}", output.status),
            };
        }
        Err(err) => {
            return PorcelainProbe::Unusable {
                reason: format!("could not execute it ({err})"),
            };
        }
    }

    // `-z` so the answer cannot be a trailing newline argument: a clean
    // repository's NUL-separated porcelain is exactly zero bytes.
    match isolated(program)
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .output()
    {
        Ok(output) if !output.status.success() => {
            return PorcelainProbe::Unusable {
                reason: format!("git status exited {}", output.status),
            };
        }
        Ok(output) if output.stdout.is_empty() => {}
        Ok(output) => {
            return PorcelainProbe::Decorated {
                bytes: output.stdout.len(),
            };
        }
        Err(err) => {
            return PorcelainProbe::Unusable {
                reason: format!("could not execute git status ({err})"),
            };
        }
    }

    // Everything above proves a property of stdout, and an exit code is not
    // stdout. The wrapper behind #178 returns 0 from `git diff --quiet` where
    // real git returns 1 -- emitting no bytes at all, so the emptiness check
    // above sees a perfectly honest binary. `paths_equal` read that status and
    // reported differing content as identical, which is the sole automatic
    // evidence an unattended cleanup uses to discard local work.
    //
    // So ask the same binary a question whose answer this function already
    // knows, in both directions: a committed file that matches, then the same
    // file modified. A candidate that cannot tell those apart is refused for
    // the same reason a decorating one is.
    let tracked = repo.join("probe.txt");
    if std::fs::write(&tracked, "before\n").is_err() {
        return PorcelainProbe::Unusable {
            reason: "could not write the probe file".to_string(),
        };
    }
    for args in [
        &["add", "probe.txt"][..],
        &["commit", "-q", "-m", "probe"][..],
    ] {
        match isolated(program).args(args).output() {
            Ok(output) if output.status.success() => {}
            Ok(output) => {
                return PorcelainProbe::Unusable {
                    reason: format!("git {} exited {}", args.join(" "), output.status),
                };
            }
            Err(err) => {
                return PorcelainProbe::Unusable {
                    reason: format!("could not execute git {} ({err})", args.join(" ")),
                };
            }
        }
    }

    // `--` so a file named like a revision cannot change what is compared.
    //
    // `output()` rather than `status()`, which every other probe command here
    // already uses. `status()` hands the child this process's own stdout, so a
    // wrapper that gets the exit codes right but prints while doing it writes
    // its banner straight into the broker's stdout -- and the first
    // `aethyme ... --json` of the run then emits those bytes ahead of its
    // JSON. The probe is the first thing any command does, so it is the worst
    // possible place to inherit a stream.
    //
    // Capturing also makes the check stricter for free: a `--quiet` that
    // prints anything is not quiet, whatever it exits.
    // `Err` carries the verdict rather than a string: printing on `--quiet` is
    // decoration and marks the candidate dishonest, while failing to spawn is
    // merely unusable, and only the first makes the dirtiness predicates
    // refuse the machine. Collapsing them into one message would file a lying
    // wrapper as a broken one.
    let quiet_exit = |program: &Path| match isolated(program)
        .args(["diff", "--quiet", "--", "probe.txt"])
        .output()
    {
        Ok(output) if !output.stdout.is_empty() => Err(PorcelainProbe::Decorated {
            bytes: output.stdout.len(),
        }),
        Ok(output) => output
            .status
            .code()
            .ok_or_else(|| PorcelainProbe::Unusable {
                reason: format!(
                    "git diff --quiet was terminated without an exit code ({})",
                    output.status
                ),
            }),
        Err(err) => Err(PorcelainProbe::Unusable {
            reason: format!("could not execute git diff --quiet ({err})"),
        }),
    };

    match quiet_exit(program) {
        Ok(0) => {}
        Ok(code) => {
            return PorcelainProbe::StatusRewritten {
                detail: format!(
                    "git diff --quiet exited {code} on an unmodified tracked file, \
                     where git exits 0"
                ),
            };
        }
        Err(verdict) => return verdict,
    }

    if std::fs::write(&tracked, "after\n").is_err() {
        return PorcelainProbe::Unusable {
            reason: "could not modify the probe file".to_string(),
        };
    }
    match quiet_exit(program) {
        Ok(1) => {}
        // The dangerous direction, and the reason this check exists: 0 here
        // means "identical", for a file this function just changed.
        Ok(code) => {
            return PorcelainProbe::StatusRewritten {
                detail: format!(
                    "git diff --quiet exited {code} on a modified tracked file, \
                     where git exits 1"
                ),
            };
        }
        Err(verdict) => return verdict,
    }

    // Everything above proves a property of `status` and of exit codes.
    // `paths_equal` reads neither: it reads the stdout of
    // `git diff --name-only -z <left> <right> -- <paths>`, and treats empty
    // output as proof that the two commits agree. A wrapper that answers
    // `status` honestly, exits correctly from `diff --quiet`, and discards
    // stdout for `diff --name-only` alone passes every check above, is filed
    // `Undecorated`, and then makes `paths_equal` report differing commits as
    // identical -- the sole automatic evidence for classifying a promotion
    // `SupersededUpstream`, on the strength of which an unattended cleanup
    // discards local work.
    //
    // So ask this binary the exact question, in the shape the caller parses,
    // with an answer this function already knows. The probe file is modified
    // on disk; commit it and the two commits differ in exactly one path.
    for args in [
        &["add", "probe.txt"][..],
        &["commit", "-q", "-m", "probe-changed"][..],
    ] {
        match isolated(program).args(args).output() {
            Ok(output) if output.status.success() => {}
            Ok(output) => {
                return PorcelainProbe::Unusable {
                    reason: format!("git {} exited {}", args.join(" "), output.status),
                };
            }
            Err(err) => {
                return PorcelainProbe::Unusable {
                    reason: format!("could not execute git {} ({err})", args.join(" ")),
                };
            }
        }
    }

    let name_only = |program: &Path, left: &str, right: &str| match isolated(program)
        .args(["diff", "--name-only", "-z", left, right, "--", "probe.txt"])
        .output()
    {
        Ok(output) if !output.status.success() => Err(PorcelainProbe::Unusable {
            reason: format!(
                "git diff --name-only exited {} comparing {left} and {right}",
                output.status
            ),
        }),
        Ok(output) => Ok(output.stdout),
        Err(err) => Err(PorcelainProbe::Unusable {
            reason: format!("could not execute git diff --name-only ({err})"),
        }),
    };

    // The load-bearing direction: two commits that differ in `probe.txt` and
    // nothing else. Real git answers with exactly one NUL-terminated path.
    match name_only(program, "HEAD~1", "HEAD") {
        Ok(out) if out == b"probe.txt\0" => {}
        Ok(out) => {
            return PorcelainProbe::StatusRewritten {
                detail: format!(
                    "git diff --name-only -z reported {:?} for two commits differing \
                     only in probe.txt, where git reports \"probe.txt\\0\"; a wrapper \
                     that empties this output makes differing commits look identical",
                    String::from_utf8_lossy(&out)
                ),
            };
        }
        Err(verdict) => return verdict,
    }

    // And the direction whose emptiness is trusted, so that "empty" has been
    // observed to mean "equal" for this binary rather than assumed to.
    match name_only(program, "HEAD", "HEAD") {
        Ok(out) if out.is_empty() => PorcelainProbe::Undecorated,
        Ok(out) => PorcelainProbe::Decorated { bytes: out.len() },
        Err(verdict) => verdict,
    }
}
