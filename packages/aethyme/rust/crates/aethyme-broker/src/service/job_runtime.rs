//! Explicit, repository-bound GC planning. Nothing applies a cleanup plan.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::job_protocol::{self as wire, Error, ErrorCode, Method, Reply, Status};
use super::jobs::{JobCompletion, JobError, JobOperation, JobRequest, JobStore};
use super::transport::{DirectoryIdentity, RuntimeDirectory, Server, owner};
use crate::Broker;

const DATABASE: &str = "broker-service-jobs.sqlite";
const CONTEXT: &str = "broker-service-jobs-context.json";
const MAX_JOB_MS: i64 = 30 * 60 * 1000;
const MAX_OUTSTANDING: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

fn identity(metadata: &fs::Metadata) -> FileIdentity {
    FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

fn private_file(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != owner()
        || metadata.mode() & 0o077 != 0
        || metadata.nlink() != 1
        || identity(&metadata) != identity(&fs::symlink_metadata(path)?)
    {
        return Err(io::Error::other("unsafe service job file"));
    }
    Ok(file)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Repository {
    root: PathBuf,
    common_dir: PathBuf,
    broker_database: PathBuf,
    uuid: String,
    root_identity: FileIdentity,
    common_identity: FileIdentity,
}

impl Repository {
    fn open(path: &Path) -> io::Result<(Self, Broker)> {
        let mut broker = Broker::open_snapshot(path).map_err(other)?;
        let binding = broker
            .store()
            .repository_worktree_binding()
            .map_err(other)?;
        let scope = broker
            .store()
            .binding_scope
            .clone()
            .ok_or_else(|| io::Error::other("repository identity unavailable"))?;
        // Reuse the selected-owner snapshot validation, including canonical
        // repository/common/database paths, rather than treating a UUID alone
        // as proof that this database belongs to the selected repository.
        let verified = crate::BrokerStore::open_snapshot_at_in_repo(
            &scope.repository_root,
            &scope.broker_db_path,
        )
        .map_err(other)?;
        if verified.repository_worktree_binding().map_err(other)? != binding {
            return Err(io::Error::other("repository binding changed during open"));
        }
        let uuid = scope
            .repository_uuid
            .filter(|uuid| Some(uuid) == binding.repository_uuid.as_ref())
            .ok_or_else(|| io::Error::other("job service requires a verified repository UUID"))?;
        let repository = Self {
            root_identity: identity(&fs::metadata(&scope.repository_root)?),
            common_identity: identity(&fs::metadata(&scope.git_common_dir)?),
            root: scope.repository_root,
            common_dir: scope.git_common_dir,
            broker_database: scope.broker_db_path,
            uuid,
        };
        Ok((repository, broker))
    }

    fn key(&self) -> io::Result<String> {
        Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(self)?)))
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Context {
    schema_version: u32,
    directory: DirectoryIdentity,
    repository: Repository,
    database: FileIdentity,
    worker_generation: String,
}

type Planner =
    dyn Fn(&Repository, &JobOperation, Instant) -> Result<serde_json::Value, String> + Send + Sync;

pub(crate) struct JobRuntime {
    directory: RuntimeDirectory,
    database: FileIdentity,
    repository: Repository,
    repository_key: String,
    generation: String,
    store: Mutex<JobStore>,
    planner: Arc<Planner>,
}

impl JobRuntime {
    pub(crate) fn open(server: &Server, repository: &Path) -> io::Result<Arc<Self>> {
        Self::open_with_planner(server, repository, Arc::new(plan))
    }

    fn open_with_planner(
        server: &Server,
        path: &Path,
        planner: Arc<Planner>,
    ) -> io::Result<Arc<Self>> {
        let directory = server.runtime_directory()?;
        let (repository, _) = Repository::open(path)?;
        let context_path = directory.path().join(CONTEXT);
        let database_path = directory.path().join(DATABASE);
        let previous = match private_file(&context_path) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(65537).read_to_end(&mut bytes)?;
                if bytes.len() > 65536 {
                    return Err(io::Error::other("oversized service job context"));
                }
                let context: Context = serde_json::from_slice(&bytes)?;
                if context.schema_version != 1
                    || context.directory != directory.identity()
                    || context.repository != repository
                    || context.worker_generation.len() != 32
                    || !context
                        .worker_generation
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit())
                {
                    return Err(io::Error::other("service job context identity changed"));
                }
                if identity(&private_file(&database_path)?.metadata()?) != context.database {
                    return Err(io::Error::other("service job database was replaced"));
                }
                Some(context)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                // A crash before context publication cannot have accepted a
                // request. Preserve any leftover database for explicit review.
                let file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(&database_path)?;
                file.sync_all()?;
                None
            }
            Err(error) => return Err(error),
        };
        for suffix in ["-wal", "-shm", "-journal"] {
            let sidecar = directory.path().join(format!("{DATABASE}{suffix}"));
            match private_file(&sidecar) {
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        let database = identity(&private_file(&database_path)?.metadata()?);
        let mut store = JobStore::open_private(&database_path).map_err(other)?;
        directory.validate()?;
        if identity(&private_file(&database_path)?.metadata()?) != database {
            return Err(io::Error::other("service job database changed during open"));
        }
        // The retained directory capability holds the exact permanent lock;
        // context matching above refuses replacement of that lock or directory.
        if let Some(previous) = previous {
            store
                .interrupt_worker(&previous.worker_generation, now_ms()?)
                .map_err(other)?;
        }
        let generation = store.new_worker_generation().map_err(other)?;
        let context = Context {
            schema_version: 1,
            directory: directory.identity(),
            repository: repository.clone(),
            database,
            worker_generation: generation.clone(),
        };
        let bytes = serde_json::to_vec(&context)?;
        let mut temporary = tempfile::NamedTempFile::new_in(directory.path())?;
        temporary.write_all(&bytes)?;
        temporary.as_file().sync_all()?;
        directory.validate()?;
        temporary
            .persist(&context_path)
            .map_err(|error| error.error)?;
        File::open(directory.path())?.sync_all()?;
        let mut verified = Vec::new();
        private_file(&context_path)?
            .take(65537)
            .read_to_end(&mut verified)?;
        if verified != bytes {
            return Err(io::Error::other("service context readback differs"));
        }
        Ok(Arc::new(Self {
            repository_key: repository.key()?,
            directory,
            database,
            repository,
            generation,
            store: Mutex::new(store),
            planner,
        }))
    }

    fn store(&self) -> io::Result<MutexGuard<'_, JobStore>> {
        let store = self
            .store
            .lock()
            .map_err(|_| io::Error::other("job store worker panicked"))?;
        self.directory.validate()?;
        if identity(&private_file(&self.directory.path().join(DATABASE))?.metadata()?)
            != self.database
        {
            return Err(io::Error::other("service job database was replaced"));
        }
        Ok(store)
    }

    pub(crate) fn handle(&self, method: &Method) -> Result<Reply, Error> {
        let mut store = self.store().map_err(|e| error(ErrorCode::Unavailable, e))?;
        match method {
            Method::Submit {
                idempotency_key,
                operation,
                deadline_ms,
            } => {
                let now = now_ms().map_err(|e| error(ErrorCode::Unavailable, e))?;
                let previous = store
                    .find_request(&self.repository_key, idempotency_key)
                    .map_err(job_error)?;
                if previous.is_none() {
                    if *deadline_ms <= now || *deadline_ms > now.saturating_add(MAX_JOB_MS) {
                        return Err(error(
                            ErrorCode::InvalidRequest,
                            "job deadline must be within the next 30 minutes",
                        ));
                    }
                    if store
                        .outstanding_count(&self.repository_key)
                        .map_err(job_error)?
                        >= MAX_OUTSTANDING
                    {
                        return Err(error(
                            ErrorCode::Unavailable,
                            "outstanding job limit reached",
                        ));
                    }
                }
                let submission = store
                    .submit(
                        &JobRequest {
                            repository: self.repository_key.clone(),
                            idempotency_key: idempotency_key.clone(),
                            operation: operation.clone(),
                            deadline_ms: *deadline_ms,
                        },
                        now,
                    )
                    .map_err(job_error)?;
                Ok(Reply::Submitted {
                    job: Status::from(&submission.job),
                    joined: submission.joined,
                })
            }
            Method::Lookup { idempotency_key } => Ok(Reply::Lookup {
                job: store
                    .find_request(&self.repository_key, idempotency_key)
                    .map_err(job_error)?
                    .as_ref()
                    .map(Status::from),
            }),
            Method::Status { job_id }
            | Method::Cancel { job_id }
            | Method::ResultChunk { job_id, .. } => {
                let job = store.get(*job_id).map_err(job_error)?;
                if job.repository != self.repository_key {
                    return Err(error(
                        ErrorCode::NotFound,
                        "job is outside this service repository",
                    ));
                }
                match method {
                    Method::Status { .. } => Ok(Reply::Status {
                        job: Status::from(&job),
                    }),
                    Method::Cancel { .. } => Ok(Reply::CancelRequested {
                        job: Status::from(
                            &store
                                .request_cancel(
                                    *job_id,
                                    now_ms().map_err(|e| error(ErrorCode::Unavailable, e))?,
                                )
                                .map_err(job_error)?,
                        ),
                    }),
                    Method::ResultChunk { offset, .. } => match job.completion {
                        Some(JobCompletion::Succeeded { result }) => wire::result_chunk(
                            *job_id,
                            &serde_json::to_string(&result)
                                .map_err(|e| error(ErrorCode::Internal, e))?,
                            *offset,
                        ),
                        _ => Err(error(ErrorCode::NotReady, "job has no successful result")),
                    },
                    _ => unreachable!(),
                }
            }
        }
    }

    pub(crate) fn run(&self, stop: &AtomicBool, local_stop: &AtomicBool) -> io::Result<()> {
        while !stop.load(Ordering::Acquire) && !local_stop.load(Ordering::Acquire) {
            let claim = self
                .store()?
                .claim_next(&self.generation, now_ms()?)
                .map_err(other)?;
            let Some(claim) = claim else {
                std::thread::sleep(Duration::from_millis(50));
                continue;
            };
            let job = self.store()?.get(claim.job().id).map_err(other)?;
            let completion = if job.repository != self.repository_key {
                JobCompletion::Failed {
                    message: "job repository does not match this service".into(),
                }
            } else if job.cancellation_requested
                || stop.load(Ordering::Acquire)
                || local_stop.load(Ordering::Acquire)
            {
                JobCompletion::Cancelled
            } else {
                let remaining = job.deadline_ms.saturating_sub(now_ms()?).min(MAX_JOB_MS);
                if remaining <= 0 {
                    JobCompletion::Failed {
                        message: "job deadline elapsed before execution".into(),
                    }
                } else {
                    let deadline = Instant::now() + Duration::from_millis(remaining as u64);
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        (self.planner)(&self.repository, &job.operation, deadline)
                    }))
                    .unwrap_or_else(|_| Err("GC planner panicked; work was not retried".into()));
                    if self
                        .store()?
                        .get(job.id)
                        .map_err(other)?
                        .cancellation_requested
                    {
                        JobCompletion::Cancelled
                    } else {
                        match result {
                            Ok(result) => JobCompletion::Succeeded { result },
                            Err(message) => JobCompletion::Failed {
                                message: bounded(&message),
                            },
                        }
                    }
                }
            };
            let mut store = self.store()?;
            match store.complete(&claim, &completion, now_ms()?) {
                Ok(_) => {}
                Err(JobError::Invalid(message)) => {
                    store
                        .complete(
                            &claim,
                            &JobCompletion::Failed {
                                message: bounded(&message),
                            },
                            now_ms()?,
                        )
                        .map_err(other)?;
                }
                Err(error) => return Err(other(error)),
            }
        }
        Ok(())
    }
}

fn plan(
    expected: &Repository,
    operation: &JobOperation,
    deadline: Instant,
) -> Result<serde_json::Value, String> {
    let (current, mut broker) = Repository::open(&expected.root).map_err(|e| e.to_string())?;
    if &current != expected {
        return Err("repository identity changed before execution".into());
    }
    let JobOperation::GcPlan {
        include_active_gate_cache,
        full_scan,
    } = operation;
    let plan = broker
        .gc_plan_for_service(*include_active_gate_cache, *full_scan, deadline)
        .map_err(|e| e.to_string())?;
    serde_json::to_value(plan).map_err(|e| e.to_string())
}

fn now_ms() -> io::Result<i64> {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(other)?
            .as_millis(),
    )
    .map_err(other)
}

fn other(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}
fn bounded(message: &str) -> String {
    message.chars().take(1024).collect()
}
fn error(code: ErrorCode, message: impl std::fmt::Display) -> Error {
    Error {
        code,
        message: bounded(&message.to_string()),
    }
}
fn job_error(value: JobError) -> Error {
    let code = match &value {
        JobError::Invalid(_) => ErrorCode::InvalidRequest,
        JobError::IdempotencyConflict => ErrorCode::IdempotencyConflict,
        JobError::NotFound(_) => ErrorCode::NotFound,
        _ => ErrorCode::Unavailable,
    };
    error(code, value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::{jobs::JobState, transport};
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::process::Command;
    use std::sync::mpsc;
    use std::thread;

    struct Fixture {
        _owner: tempfile::TempDir,
        root: PathBuf,
        repository: PathBuf,
        runtime: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let owner = tempfile::Builder::new()
                .prefix("sj")
                .tempdir_in("/tmp")
                .unwrap();
            let root = owner.path().canonicalize().unwrap();
            let repository = root.join("repo");
            let runtime = root.join("run");
            fs::create_dir(&repository).unwrap();
            fs::create_dir(&runtime).unwrap();
            fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
            for args in [
                vec!["init", "-q", "-b", "main"],
                vec![
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.test",
                    "commit",
                    "--allow-empty",
                    "-qm",
                    "fixture",
                ],
            ] {
                let mut git = Command::new("git");
                for (name, _) in std::env::vars_os()
                    .filter(|(name, _)| name.to_string_lossy().starts_with("GIT_"))
                {
                    git.env_remove(name);
                }
                let output = git
                    .args(args)
                    .current_dir(&repository)
                    .env("HOME", &root)
                    .env("GIT_CONFIG_GLOBAL", "/dev/null")
                    .env("GIT_CONFIG_NOSYSTEM", "1")
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            let preparation = root.join("prepare");
            fs::create_dir(&preparation).unwrap();
            let state = root.join("state");
            let options = crate::BrokerOpenOptions::default()
                .with_host_state_dir(&state)
                .with_host_cache_dir(root.join("cache"))
                .with_preparation_cache_root(preparation)
                .with_worktree_root(root.join("worktrees"))
                .with_host_operation_database(state.join("operations.db"))
                .with_collaboration_state(state.join("collaboration"));
            let mut broker = Broker::open_with_options(&repository, options).unwrap();
            broker.start_worktree("service fixture", None).unwrap();
            drop(broker);
            Self {
                _owner: owner,
                root,
                repository,
                runtime,
            }
        }
        fn open(&self, planner: Arc<Planner>) -> (Server, Arc<JobRuntime>) {
            let server = Server::bind(&self.runtime).unwrap();
            let jobs = JobRuntime::open_with_planner(&server, &self.repository, planner).unwrap();
            (server, jobs)
        }
    }
    fn operation() -> JobOperation {
        JobOperation::GcPlan {
            include_active_gate_cache: false,
            full_scan: false,
        }
    }
    fn submit(jobs: &JobRuntime, key: &str, deadline: i64) -> (i64, bool) {
        match jobs
            .handle(&Method::Submit {
                idempotency_key: key.into(),
                operation: operation(),
                deadline_ms: deadline,
            })
            .unwrap()
        {
            Reply::Submitted { job, joined } => (job.id, joined),
            _ => panic!("unexpected submit reply"),
        }
    }
    fn status(jobs: &JobRuntime, job_id: i64) -> Status {
        match jobs.handle(&Method::Status { job_id }).unwrap() {
            Reply::Status { job } => job,
            _ => panic!("unexpected status reply"),
        }
    }
    fn wait_terminal(jobs: &JobRuntime, job_id: i64) -> Status {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let job = status(jobs, job_id);
            if matches!(
                job.state,
                JobState::Succeeded | JobState::Failed | JobState::Cancelled
            ) {
                return job;
            }
            assert!(Instant::now() < deadline, "job did not finish: {job:?}");
            thread::sleep(Duration::from_millis(10));
        }
    }
    struct Running {
        stop: Arc<AtomicBool>,
        thread: Option<thread::JoinHandle<io::Result<()>>>,
    }
    impl Running {
        fn start(server: Server, jobs: Arc<JobRuntime>) -> Self {
            let stop = Arc::new(AtomicBool::new(false));
            let thread_stop = stop.clone();
            Self {
                stop,
                thread: Some(thread::spawn(move || {
                    server.serve_jobs_until(&thread_stop, jobs)
                })),
            }
        }
    }
    impl Drop for Running {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            self.thread.take().unwrap().join().unwrap().unwrap();
        }
    }

    #[test]
    fn slow_shared_job_preserves_health_and_cancellation_acknowledges_only_after_work() {
        let fixture = Fixture::new();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let (server, jobs) = fixture.open(Arc::new(move |_, _, _| {
            entered_tx.send(()).unwrap();
            release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10))
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!({"large": "é".repeat(10000)}))
        }));
        let deadline = now_ms().unwrap() + 60000;
        let (job_id, joined) = submit(&jobs, "first", deadline);
        assert!(!joined);
        let running = Running::start(server, jobs.clone());
        entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(submit(&jobs, "second", deadline), (job_id, true));
        assert!(transport::ping(&fixture.runtime).unwrap().result.is_ok());
        // A client can disappear without cancelling the shared request.
        let lookup = transport::job_request(
            &fixture.runtime,
            Method::Lookup {
                idempotency_key: "first".into(),
            },
        )
        .unwrap();
        assert!(
            matches!(lookup.result.unwrap(), Reply::Lookup { job: Some(job) } if job.id == job_id)
        );
        jobs.handle(&Method::Cancel { job_id }).unwrap();
        let pending = status(&jobs, job_id);
        assert_eq!(pending.state, JobState::Running);
        assert!(pending.cancellation_requested);
        assert!(pending.completion.is_none());
        release_tx.send(()).unwrap();
        assert_eq!(wait_terminal(&jobs, job_id).state, JobState::Cancelled);
        drop(running);
        // Runtime clones retain the permanent singleton lock after socket drain.
        assert!(Server::bind(&fixture.runtime).is_err());
        drop(jobs);
        drop(Server::bind(&fixture.runtime).unwrap());
    }

    #[test]
    fn queued_cancel_prevents_dispatch_and_panicking_job_does_not_wedge_next_job() {
        let fixture = Fixture::new();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = calls.clone();
        let (server, jobs) = fixture.open(Arc::new(move |_, _, _| {
            if count.fetch_add(1, Ordering::SeqCst) == 0 {
                panic!("injected planner failure");
            }
            Ok(serde_json::json!({"planned": true}))
        }));
        let deadline = now_ms().unwrap() + 60000;
        let (cancelled, _) = submit(&jobs, "cancelled", deadline);
        jobs.handle(&Method::Cancel { job_id: cancelled }).unwrap();
        let (failed, _) = submit(&jobs, "failed", deadline);
        let running = Running::start(server, jobs.clone());
        assert_eq!(wait_terminal(&jobs, failed).state, JobState::Failed);
        let (success, _) = submit(&jobs, "success", deadline);
        assert_eq!(wait_terminal(&jobs, success).state, JobState::Succeeded);
        assert_eq!(status(&jobs, cancelled).state, JobState::Cancelled);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(transport::ping(&fixture.runtime).unwrap().result.is_ok());
        let chunk = jobs
            .handle(&Method::ResultChunk {
                job_id: success,
                offset: 0,
            })
            .unwrap();
        assert!(
            matches!(chunk, Reply::ResultChunk { complete: true, data, .. } if data == "{\"planned\":true}")
        );
        drop(running);
    }

    #[test]
    fn restart_fences_previous_generation_without_retrying_interrupted_work() {
        let fixture = Fixture::new();
        let planner: Arc<Planner> = Arc::new(|_, _, _| Ok(serde_json::json!({})));
        let (server, jobs) = fixture.open(planner.clone());
        let deadline = now_ms().unwrap() + 60000;
        let (job_id, _) = submit(&jobs, "lost-reply", deadline);
        let claim = jobs
            .store()
            .unwrap()
            .claim_next(&jobs.generation, now_ms().unwrap())
            .unwrap()
            .unwrap();
        drop(jobs);
        drop(server);
        let (server, recovered) = fixture.open(planner);
        assert_eq!(status(&recovered, job_id).state, JobState::Interrupted);
        assert_eq!(submit(&recovered, "lost-reply", deadline), (job_id, true));
        assert_eq!(
            submit(&recovered, "join-interrupted", deadline),
            (job_id, true)
        );
        let mut store = recovered.store().unwrap();
        assert!(matches!(
            store.complete(&claim, &JobCompletion::Cancelled, now_ms().unwrap()),
            Err(JobError::ClaimLost(_))
        ));
        assert!(
            store
                .claim_next(&recovered.generation, now_ms().unwrap())
                .unwrap()
                .is_none()
        );
        drop(store);
        drop(server);
    }

    #[test]
    fn uuid_does_not_authorize_a_foreign_binding_or_incomplete_bootstrap() {
        let fixture = Fixture::new();
        let server = Server::bind(&fixture.runtime).unwrap();
        let database = crate::broker_db_path(&fixture.repository).unwrap();
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute(
                "UPDATE repository_worktree_binding SET repository_key = 'foreign-owner'",
                [],
            )
            .unwrap();
        drop(connection);
        let planner: Arc<Planner> = Arc::new(|_, _, _| panic!("refused startup must not plan"));
        assert!(
            JobRuntime::open_with_planner(&server, &fixture.repository, planner.clone()).is_err()
        );
        assert!(!fixture.runtime.join(DATABASE).exists());
        assert!(!fixture.runtime.join(CONTEXT).exists());
        drop(server);

        let fresh = Fixture::new();
        let server = Server::bind(&fresh.runtime).unwrap();
        let orphan = fresh.runtime.join(DATABASE);
        fs::write(&orphan, b"preserved incomplete bootstrap").unwrap();
        fs::set_permissions(&orphan, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(JobRuntime::open_with_planner(&server, &fresh.repository, planner).is_err());
        assert_eq!(
            fs::read(&orphan).unwrap(),
            b"preserved incomplete bootstrap"
        );
        assert!(!fresh.runtime.join(CONTEXT).exists());
    }

    #[test]
    fn replaced_lock_or_database_and_foreign_repository_preserve_context_and_rows() {
        let fixture = Fixture::new();
        let planner: Arc<Planner> = Arc::new(|_, _, _| Ok(serde_json::json!({})));
        let (server, jobs) = fixture.open(planner.clone());
        let (job_id, _) = submit(&jobs, "preserve", now_ms().unwrap() + 60000);
        let context = fs::read(fixture.runtime.join(CONTEXT)).unwrap();
        // Renaming leaves the old held descriptor and a different named inode.
        let lock = fixture.runtime.join("broker-service.lock");
        fs::rename(&lock, fixture.root.join("held-lock")).unwrap();
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&lock)
            .unwrap();
        assert!(jobs.handle(&Method::Status { job_id }).is_err());
        drop(jobs);
        drop(server);
        let replacement_server = Server::bind(&fixture.runtime).unwrap();
        assert!(
            JobRuntime::open_with_planner(
                &replacement_server,
                &fixture.repository,
                planner.clone()
            )
            .is_err()
        );
        assert_eq!(fs::read(fixture.runtime.join(CONTEXT)).unwrap(), context);
        drop(replacement_server);
        fs::remove_file(&lock).unwrap();
        fs::rename(fixture.root.join("held-lock"), &lock).unwrap();
        let server = Server::bind(&fixture.runtime).unwrap();
        let other = Fixture::new();
        assert!(
            JobRuntime::open_with_planner(&server, &other.repository, planner.clone()).is_err()
        );
        assert_eq!(fs::read(fixture.runtime.join(CONTEXT)).unwrap(), context);
        let database = fixture.runtime.join(DATABASE);
        fs::rename(&database, fixture.root.join("original.sqlite")).unwrap();
        symlink(fixture.root.join("original.sqlite"), &database).unwrap();
        assert!(
            JobRuntime::open_with_planner(&server, &fixture.repository, planner.clone()).is_err()
        );
        fs::remove_file(&database).unwrap();
        fs::write(&database, b"replacement bytes").unwrap();
        fs::set_permissions(&database, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(JobRuntime::open_with_planner(&server, &fixture.repository, planner).is_err());
        assert_eq!(fs::read(&database).unwrap(), b"replacement bytes");
        assert_eq!(fs::read(fixture.runtime.join(CONTEXT)).unwrap(), context);
    }
}
