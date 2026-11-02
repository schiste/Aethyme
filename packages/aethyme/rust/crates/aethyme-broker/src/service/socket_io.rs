//! Absolute socket I/O deadlines without changing options after peer closure.

use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

pub(super) struct DeadlineIo<'a> {
    stream: &'a UnixStream,
    deadline: Instant,
}

impl<'a> DeadlineIo<'a> {
    pub(super) fn new(stream: &'a UnixStream, timeout: Duration) -> io::Result<Self> {
        stream.set_nonblocking(true)?;
        Ok(Self {
            stream,
            deadline: Instant::now() + timeout,
        })
    }

    fn perform(&self, readiness: i16, mut operation: impl FnMut() -> isize) -> io::Result<usize> {
        loop {
            let remaining = self
                .deadline
                .checked_duration_since(Instant::now())
                .filter(|remaining| !remaining.is_zero())
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::TimedOut, "service frame deadline expired")
                })?;
            let result = operation();
            if result >= 0 {
                return Ok(result as usize);
            }
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            if error.kind() != io::ErrorKind::WouldBlock {
                return Err(error);
            }
            let mut descriptor = libc::pollfd {
                fd: self.stream.as_raw_fd(),
                events: readiness,
                revents: 0,
            };
            let timeout = (remaining.as_millis() + 1).min(i32::MAX as u128) as i32;
            // SAFETY: descriptor is one valid initialized pollfd for this call.
            let result = unsafe { libc::poll(&mut descriptor, 1, timeout) };
            if result < 0 {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::Interrupted {
                    return Err(error);
                }
            }
            // Recheck the absolute deadline and retry nonblocking I/O, including
            // after hangup: a closed peer can leave a complete buffered frame.
        }
    }
}

impl Read for DeadlineIo<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        self.perform(libc::POLLIN, || {
            // SAFETY: buffer is writable for its length and the stream owns fd.
            unsafe {
                libc::recv(
                    self.stream.as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                    libc::MSG_DONTWAIT,
                )
            }
        })
    }
}

impl Write for DeadlineIo<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        self.perform(libc::POLLOUT, || {
            // SAFETY: buffer is readable for its length and the stream owns fd.
            // MSG_NOSIGNAL makes peer closure an error, never a process signal.
            unsafe {
                libc::send(
                    self.stream.as_raw_fd(),
                    buffer.as_ptr().cast(),
                    buffer.len(),
                    libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
                )
            }
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffered_frame_survives_peer_close() {
        let (reader, mut writer) = UnixStream::pair().unwrap();
        writer.write_all(b"complete frame\n").unwrap();
        drop(writer);
        let mut bytes = Vec::new();
        DeadlineIo::new(&reader, Duration::from_secs(1))
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        assert_eq!(bytes, b"complete frame\n");
    }

    #[test]
    fn silent_peer_exhausts_read_budget() {
        let (reader, _writer) = UnixStream::pair().unwrap();
        let error = DeadlineIo::new(&reader, Duration::from_millis(20))
            .unwrap()
            .read(&mut [0; 1])
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn trickling_peer_does_not_reset_frame_deadline() {
        let (reader, mut writer) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            for _ in 0..128 {
                if writer.write_all(b"x").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        let result = DeadlineIo::new(&reader, Duration::from_millis(50))
            .unwrap()
            .read_exact(&mut [0; 128]);
        drop(reader);
        worker.join().unwrap();
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn closed_peer_write_returns_an_error_without_a_signal() {
        let (writer, reader) = UnixStream::pair().unwrap();
        drop(reader);
        assert!(
            DeadlineIo::new(&writer, Duration::from_secs(1))
                .unwrap()
                .write_all(b"frame\n")
                .is_err()
        );
    }

    #[test]
    fn stalled_peer_exhausts_write_budget() {
        let (writer, _reader) = UnixStream::pair().unwrap();
        let size: libc::c_int = 4096;
        // SAFETY: size points to an initialized integer of the stated length.
        let result = unsafe {
            libc::setsockopt(
                writer.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_SNDBUF,
                (&size as *const libc::c_int).cast(),
                std::mem::size_of_val(&size) as libc::socklen_t,
            )
        };
        assert_eq!(result, 0);
        let error = DeadlineIo::new(&writer, Duration::from_millis(20))
            .unwrap()
            .write_all(&vec![0; 256 * 1024])
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }
}
