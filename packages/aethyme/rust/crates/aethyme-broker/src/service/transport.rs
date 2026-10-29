//! Private opt-in socket server. No repository is opened by a health request.

use super::protocol::{self, Pong, Request, Response};
use crate::file_lock::ExclusiveFileLock;
use std::fs::{self, OpenOptions};
use std::io::{self, BufReader, Read};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const IO_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_CLIENTS: usize = 16;

fn refusal(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

fn owner() -> u32 {
    // SAFETY: geteuid takes no pointers and has no preconditions.
    unsafe { libc::geteuid() }
}

fn private_directory(path: &Path) -> io::Result<fs::Metadata> {
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
    _lock: ExclusiveFileLock,
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
            _lock: lock,
        };
        fs::set_permissions(&server.socket, fs::Permissions::from_mode(0o600))?;
        server.listener.set_nonblocking(true)?;
        Ok(server)
    }

    /// Bounded independent clients keep a partial frame from blocking every
    /// request. On orderly stop, drain all workers before releasing ownership.
    pub fn serve_until(&self, stop: &AtomicBool) -> io::Result<()> {
        let mut workers: Vec<JoinHandle<()>> = Vec::new();
        let result = (|| {
            while !stop.load(Ordering::Acquire) {
                let mut active = Vec::new();
                for worker in workers.drain(..) {
                    if worker.is_finished() {
                        let _ = worker.join();
                    } else {
                        active.push(worker);
                    }
                }
                workers = active;
                match self.listener.accept() {
                    Ok((stream, _)) if workers.len() < MAX_CLIENTS => {
                        workers.push(thread::spawn(move || {
                            let _ = respond(stream);
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
            let _ = worker.join();
        }
        result
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Ok(metadata) = fs::symlink_metadata(&self.socket)
            && (metadata.dev(), metadata.ino()) == self.socket_identity
        {
            let _ = fs::remove_file(&self.socket);
        }
    }
}

fn respond(mut stream: UnixStream) -> io::Result<()> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let request: Request = read_message(&mut stream)?;
    let result = request.validate().map(|()| Pong {
        pid: std::process::id(),
        version: env!("CARGO_PKG_VERSION").into(),
    });
    protocol::write_frame(
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
    private_directory(directory)?;
    let socket = directory.join("broker-service.sock");
    let metadata = fs::symlink_metadata(&socket)?;
    if !metadata.file_type().is_socket()
        || metadata.uid() != owner()
        || metadata.mode() & 0o077 != 0
    {
        return Err(refusal("unsafe service socket"));
    }
    let mut stream = UnixStream::connect(&socket)?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let request = Request::ping(1);
    protocol::write_frame(&mut stream, &request)?;
    let response: Response = read_message(&mut stream)?;
    response.validate_for(&request)?;
    Ok(response)
}

// Resetting SO_RCVTIMEO after each successful byte would allow a trickling
// client to retain a worker indefinitely. Bound the entire frame instead.
fn read_message<T: serde::de::DeserializeOwned>(stream: &mut UnixStream) -> io::Result<T> {
    struct DeadlineReader<'a> {
        stream: &'a mut UnixStream,
        deadline: Instant,
    }
    impl Read for DeadlineReader<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let remaining = self
                .deadline
                .checked_duration_since(Instant::now())
                .filter(|duration| !duration.is_zero())
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::TimedOut, "service frame deadline expired")
                })?;
            self.stream.set_read_timeout(Some(remaining))?;
            self.stream.read(buffer)
        }
    }
    protocol::read_frame(&mut BufReader::new(DeadlineReader {
        stream,
        deadline: Instant::now() + IO_TIMEOUT,
    }))
}
