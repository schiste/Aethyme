//! Private opt-in socket server. No repository is opened by a health request.

use super::protocol::{self, Pong, Request, Response};
use super::{job_protocol, job_runtime::JobRuntime};
use crate::file_lock::ExclusiveFileLock;
use std::fs::{self, OpenOptions};
use std::io::{self, BufReader};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const IO_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_CLIENTS: usize = 16;

fn refusal(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

pub(crate) fn owner() -> u32 {
    // SAFETY: geteuid takes no pointers and has no preconditions.
    unsafe { libc::geteuid() }
}

pub(crate) fn private_directory(path: &Path) -> io::Result<fs::Metadata> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.uid() != owner() || metadata.mode() & 0o077 != 0 {
        return Err(refusal(
            "service directory must be owned by this user with mode 0700",
        ));
    }
    if path.canonicalize()? != path {
        return Err(refusal(
            "service directory must be an absolute canonical path",
        ));
    }
    Ok(metadata)
}

/// A foreground service. Dropping it removes only the socket inode it bound;
/// the permanent lock file remains to avoid splitting singleton ownership.
pub struct Server {
    listener: UnixListener,
    socket: PathBuf,
    socket_identity: (u64, u64),
    directory: RuntimeDirectory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DirectoryIdentity {
    device: u64,
    inode: u64,
    lock_device: u64,
    lock_inode: u64,
}

/// Keeps the singleton lock held until every job and client has drained.
#[derive(Clone)]
pub(crate) struct RuntimeDirectory {
    path: PathBuf,
    identity: DirectoryIdentity,
    _lock: Arc<ExclusiveFileLock>,
}

impl RuntimeDirectory {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
    pub(crate) fn identity(&self) -> DirectoryIdentity {
        self.identity
    }
    pub(crate) fn validate(&self) -> io::Result<()> {
        let directory = private_directory(&self.path)?;
        let lock = fs::symlink_metadata(self.path.join("broker-service.lock"))?;
        if !lock.is_file()
            || lock.uid() != owner()
            || lock.mode() & 0o077 != 0
            || lock.nlink() != 1
            || self.identity
                != (DirectoryIdentity {
                    device: directory.dev(),
                    inode: directory.ino(),
                    lock_device: lock.dev(),
                    lock_inode: lock.ino(),
                })
        {
            return Err(refusal("service directory or singleton lock was replaced"));
        }
        Ok(())
    }
}

impl Server {
    /// The operator creates the private runtime directory explicitly. A stale
    /// socket can be removed only after acquiring the permanent singleton lock.
    pub fn bind(directory: &Path) -> io::Result<Self> {
        let directory_metadata = private_directory(directory)?;
        let lock_path = directory.join("broker-service.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&lock_path)?;
        let opened = file.metadata()?;
        if !opened.is_file()
            || opened.uid() != owner()
            || opened.mode() & 0o077 != 0
            || opened.nlink() != 1
        {
            return Err(refusal("unsafe service lock file"));
        }
        let lock = ExclusiveFileLock::try_acquire(file)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::AddrInUse,
                "broker service already owns this directory",
            )
        })?;
        let named = fs::symlink_metadata(&lock_path)?;
        let current_directory = private_directory(directory)?;
        if (opened.dev(), opened.ino()) != (named.dev(), named.ino())
            || (directory_metadata.dev(), directory_metadata.ino())
                != (current_directory.dev(), current_directory.ino())
        {
            return Err(refusal("service directory or lock changed during startup"));
        }
        let socket = directory.join("broker-service.sock");
        match fs::symlink_metadata(&socket) {
            Ok(metadata) if metadata.file_type().is_socket() && metadata.uid() == owner() => {
                // Do not unlink a responsive endpoint, even if its lock was
                // replaced by an operator or created by an older service.
                match UnixStream::connect(&socket) {
                    Ok(_) => {
                        return Err(io::Error::new(
                            io::ErrorKind::AddrInUse,
                            "broker service socket is live",
                        ));
                    }
                    Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {}
                    Err(error) => return Err(error),
                }
                fs::remove_file(&socket)?;
            }
            Ok(_) => return Err(refusal("service socket path is not an owned socket")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let listener = UnixListener::bind(&socket)?;
        let metadata = fs::symlink_metadata(&socket)?;
        let server = Self {
            listener,
            socket,
            socket_identity: (metadata.dev(), metadata.ino()),
            directory: RuntimeDirectory {
                path: directory.into(),
                identity: DirectoryIdentity {
                    device: directory_metadata.dev(),
                    inode: directory_metadata.ino(),
                    lock_device: opened.dev(),
                    lock_inode: opened.ino(),
                },
                _lock: Arc::new(lock),
            },
        };
        fs::set_permissions(&server.socket, fs::Permissions::from_mode(0o600))?;
        server.listener.set_nonblocking(true)?;
        Ok(server)
    }

    pub(crate) fn runtime_directory(&self) -> io::Result<RuntimeDirectory> {
        self.directory.validate()?;
        Ok(self.directory.clone())
    }

    /// Bounded independent clients keep a partial frame from blocking every
    /// request. On orderly stop, drain all workers before releasing ownership.
    pub fn serve_until(&self, stop: &AtomicBool) -> io::Result<()> {
        self.serve_requests_until(stop, None, None)
    }

    pub(crate) fn serve_jobs_until(
        &self,
        stop: &AtomicBool,
        jobs: Arc<JobRuntime>,
    ) -> io::Result<()> {
        let worker_stop = AtomicBool::new(false);
        struct StopOnDrop<'a>(&'a AtomicBool);
        impl Drop for StopOnDrop<'_> {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        thread::scope(|scope| {
            let worker = scope.spawn(|| {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    jobs.run(stop, &worker_stop)
                }))
                .unwrap_or_else(|_| Err(io::Error::other("job worker panicked")));
                worker_stop.store(true, Ordering::Release);
                result
            });
            let drain = StopOnDrop(&worker_stop);
            let result = self.serve_requests_until(stop, Some(&worker_stop), Some(&jobs));
            drop(drain);
            let worker_result = worker
                .join()
                .map_err(|_| io::Error::other("job worker panicked"))?;
            result.and(worker_result)
        })
    }

    fn serve_requests_until(
        &self,
        stop: &AtomicBool,
        worker_stop: Option<&AtomicBool>,
        jobs: Option<&Arc<JobRuntime>>,
    ) -> io::Result<()> {
        let mut workers: Vec<JoinHandle<()>> = Vec::new();
        let result = (|| {
            while !stop.load(Ordering::Acquire)
                && !worker_stop.is_some_and(|stop| stop.load(Ordering::Acquire))
            {
                let mut active = Vec::new();
                for worker in workers.drain(..) {
                    if worker.is_finished() {
                        join_worker(worker);
                    } else {
                        active.push(worker);
                    }
                }
                workers = active;
                match self.listener.accept() {
                    Ok((stream, _)) if workers.len() < MAX_CLIENTS => {
                        let jobs = jobs.cloned();
                        workers.push(thread::spawn(move || {
                            if let Err(error) = respond(stream, jobs.as_deref()) {
                                eprintln!("broker service: client request failed: {error}");
                            }
                        }));
                    }
                    Ok(_) => {} // Bounded overload: close without claiming execution.
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(20));
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error),
                }
            }
            Ok(())
        })();
        for worker in workers {
            join_worker(worker);
        }
        result
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Ok(metadata) = fs::symlink_metadata(&self.socket)
            && (metadata.dev(), metadata.ino()) == self.socket_identity
            && let Err(error) = fs::remove_file(&self.socket)
        {
            eprintln!("broker service: socket cleanup failed: {error}");
        }
    }
}

fn join_worker(worker: JoinHandle<()>) {
    if worker.join().is_err() {
        eprintln!("broker service: client handler panicked");
    }
}

fn respond(mut stream: UnixStream, jobs: Option<&JobRuntime>) -> io::Result<()> {
    let bytes = read_bytes(&mut stream)?;
    #[derive(serde::Deserialize)]
    struct Header {
        protocol_version: u32,
        request_id: u64,
    }
    let header: Header = serde_json::from_slice(&bytes)?;
    if header.protocol_version == job_protocol::VERSION {
        let result = match serde_json::from_slice::<job_protocol::Request>(&bytes) {
            Ok(request) => match jobs {
                Some(jobs) => {
                    jobs.handle_scoped(&request.method, request.repository_key.as_deref())
                }
                None => Err(job_protocol::Error {
                    code: job_protocol::ErrorCode::JobsDisabled,
                    message: "job service is not enabled".into(),
                }),
            },
            Err(_) => Err(job_protocol::Error {
                code: job_protocol::ErrorCode::InvalidRequest,
                message: "invalid job request".into(),
            }),
        };
        return write_message(
            &mut stream,
            &job_protocol::Response {
                protocol_version: job_protocol::VERSION,
                request_id: header.request_id,
                result,
            },
        );
    }
    let request: Request = if header.protocol_version == protocol::PROTOCOL_VERSION {
        serde_json::from_slice(&bytes)?
    } else {
        Request {
            protocol_version: header.protocol_version,
            ..Request::ping(header.request_id)
        }
    };
    let result = request.validate().map(|()| Pong {
        pid: std::process::id(),
        version: env!("CARGO_PKG_VERSION").into(),
    });
    write_message(
        &mut stream,
        &Response {
            protocol_version: protocol::PROTOCOL_VERSION,
            request_id: request.request_id,
            result,
        },
    )
}

/// No fallback or automatic service startup: failure remains visible to callers.
pub fn ping(directory: &Path) -> io::Result<Response> {
    let mut stream = connect(directory)?;
    let request = Request::ping(1);
    write_message(&mut stream, &request)?;
    let response: Response = read_message(&mut stream)?;
    response.validate_for(&request)?;
    Ok(response)
}

/// Submit exactly once. A lost reply can be reconciled with Lookup using the
/// same durable request key; the client never starts work through a fallback.
pub fn job_request(
    directory: &Path,
    method: job_protocol::Method,
) -> io::Result<job_protocol::Response> {
    send_job_request(directory, method, None)
}

/// Pin every request to a previously verified repository identity. An older
/// server may refuse the new field; this client never downgrades or falls back.
pub fn scoped_job_request(
    directory: &Path,
    repository_key: &str,
    method: job_protocol::Method,
) -> io::Result<job_protocol::Response> {
    if repository_key.len() != 64
        || !repository_key
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid repository identity key",
        ));
    }
    send_job_request(directory, method, Some(repository_key.to_owned()))
}

fn send_job_request(
    directory: &Path,
    method: job_protocol::Method,
    repository_key: Option<String>,
) -> io::Result<job_protocol::Response> {
    let mut stream = connect(directory)?;
    let request = job_protocol::Request {
        protocol_version: job_protocol::VERSION,
        request_id: 1,
        repository_key,
        method,
    };
    write_message(&mut stream, &request)?;
    let response: job_protocol::Response = read_message(&mut stream)?;
    if response.protocol_version != request.protocol_version
        || response.request_id != request.request_id
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "job response version or correlation differs",
        ));
    }
    Ok(response)
}

fn connect(directory: &Path) -> io::Result<UnixStream> {
    private_directory(directory)?;
    let socket = directory.join("broker-service.sock");
    let metadata = fs::symlink_metadata(&socket)?;
    if !metadata.file_type().is_socket()
        || metadata.uid() != owner()
        || metadata.mode() & 0o077 != 0
    {
        return Err(refusal("unsafe service socket"));
    }
    let stream = UnixStream::connect(&socket)?;
    Ok(stream)
}

// Resetting SO_RCVTIMEO after each successful byte would allow a trickling
// client to retain a worker indefinitely. Bound the entire frame instead.
fn write_message<T: serde::Serialize>(stream: &mut UnixStream, value: &T) -> io::Result<()> {
    protocol::write_frame(
        &mut super::socket_io::DeadlineIo::new(stream, IO_TIMEOUT)?,
        value,
    )
}

fn read_message<T: serde::de::DeserializeOwned>(stream: &mut UnixStream) -> io::Result<T> {
    serde_json::from_slice(&read_bytes(stream)?)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn read_bytes(stream: &mut UnixStream) -> io::Result<Vec<u8>> {
    protocol::read_frame_bytes(&mut BufReader::new(super::socket_io::DeadlineIo::new(
        stream, IO_TIMEOUT,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    #[test]
    fn nonblocking_client_completes_a_bounded_frame() {
        let (server, mut client) = UnixStream::pair().unwrap();
        server.set_nonblocking(true).unwrap();
        let inspection = server.try_clone().unwrap();
        let worker = thread::spawn(move || respond(server, None));
        let flags = || {
            // SAFETY: inspection owns this live descriptor; F_GETFL takes no pointer.
            let flags = unsafe { libc::fcntl(inspection.as_raw_fd(), libc::F_GETFL) };
            assert!(flags >= 0);
            flags
        };
        // The deadline adapter keeps nonblocking I/O even on platforms where
        // MSG_DONTWAIT alone does not bound a partially buffered send.
        assert_ne!(flags() & libc::O_NONBLOCK, 0);
        protocol::write_frame(&mut client, &Request::ping(91)).unwrap();
        let response: Response = read_message(&mut client).unwrap();
        worker.join().unwrap().unwrap();
        assert_ne!(flags() & libc::O_NONBLOCK, 0);
        assert_eq!(response.request_id, 91);
        assert!(response.result.is_ok());
    }
}
