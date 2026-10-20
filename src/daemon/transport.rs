//! Control-plane transport: Unix-domain sockets on Unix, named pipes on Windows.
//!
//! The JSON-RPC layer only sees [`DaemonStream`] (an `AsyncRead + AsyncWrite`
//! value) and [`DaemonListener`]. On Windows the "socket path" keeps its role
//! as the session identity: the listener creates a marker file at that path
//! and derives the pipe name from it, so every lifecycle routine that scans,
//! probes, or removes `*.sock` files works unchanged on both platforms.

use std::io;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::time::{Duration, Instant};

#[cfg(unix)]
pub use unix_impl::{connect, peer_is_same_user, DaemonListener, DaemonStream};
#[cfg(windows)]
pub use windows_impl::{connect, peer_is_same_user, pipe_name, DaemonListener, DaemonStream};

/// Restrict a filesystem artifact to its owner. Unix applies `mode`; Windows
/// relies on the per-user ACL that `%LOCALAPPDATA%` already carries.
#[cfg_attr(not(unix), allow(clippy::unused_async))] // only the Unix branch awaits
pub async fn restrict_to_owner(path: &Path, mode: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).await
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        Ok(())
    }
}

/// Lock file that serializes daemons claiming `socket`.
pub fn lock_path(socket: &Path) -> PathBuf {
    let mut name = socket.file_name().map(std::ffi::OsStr::to_os_string).unwrap_or_default();
    name.push(".lock");
    socket.with_file_name(name)
}

/// How long a claim keeps retrying a busy lock before giving up. A lock can
/// stay held for a few milliseconds after its owner released it: `flock`
/// follows the open file description, and a child forked while the lock was
/// open keeps that description until it execs. A genuine second daemon is
/// still refused, just a quarter second later.
#[cfg(unix)]
const CLAIM_RETRY_WINDOW: Duration = Duration::from_millis(250);

/// Open `path` owner-only and take the exclusive advisory lock, failing with
/// `AddrInUse` when another process still holds it after the retry window.
#[cfg(unix)]
async fn claim_lock(path: &Path) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let deadline = Instant::now() + CLAIM_RETRY_WINDOW;
    loop {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path)?;
        if crate::os::unix::try_lock_exclusive(&file)? {
            return Ok(file);
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                format!("another daemon is starting or listening (lock {})", path.display()),
            ));
        }
        drop(file);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[cfg(unix)]
mod unix_impl {
    use std::io;
    use std::path::Path;

    use tokio::net::{UnixListener, UnixStream};

    pub type DaemonStream = UnixStream;

    pub struct DaemonListener {
        inner: UnixListener,
        /// Held for the listener's lifetime; see [`DaemonListener::bind`].
        _lock: std::fs::File,
    }

    impl DaemonListener {
        /// Claim `path` for this process, replacing a stale socket left by a
        /// dead daemon but never stealing a live one.
        ///
        /// Two daemons started at the same moment both see the same stale
        /// socket; without coordination the second one unlinks the first
        /// one's freshly bound socket and both end up listening, one of them
        /// unreachable. An exclusive advisory lock on `<path>.lock` makes the
        /// existence check and the bind one atomic step: the loser fails with
        /// `AddrInUse` and its launcher simply connects to the winner.
        pub async fn bind(path: &Path) -> io::Result<Self> {
            let lock = super::claim_lock(&super::lock_path(path)).await?;
            if tokio::fs::try_exists(path).await? {
                if connect(path).await.is_ok() {
                    return Err(io::Error::new(
                        io::ErrorKind::AddrInUse,
                        format!("a daemon is already listening at {}", path.display()),
                    ));
                }
                tokio::fs::remove_file(path).await?;
            }
            Ok(Self { inner: UnixListener::bind(path)?, _lock: lock })
        }

        pub async fn accept(&mut self) -> io::Result<DaemonStream> {
            self.inner.accept().await.map(|(stream, _)| stream)
        }
    }

    pub async fn connect(path: &Path) -> io::Result<DaemonStream> {
        UnixStream::connect(path).await
    }

    /// Only the daemon's own uid may drive it; `SO_PEERCRED` is authoritative.
    pub fn peer_is_same_user(stream: &DaemonStream) -> io::Result<bool> {
        Ok(stream.peer_cred()?.uid() == crate::os::unix::effective_uid())
    }
}

#[cfg(windows)]
mod windows_impl {
    use std::io;
    use std::path::{Path, PathBuf};
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use std::time::{Duration, Instant};

    use sha2::{Digest as _, Sha256};
    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
    use tokio::net::windows::named_pipe::{
        ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
    };
    use windows_sys::Win32::Foundation::ERROR_PIPE_BUSY;

    use crate::os::windows::PipeSecurity;

    /// How long a client keeps retrying while every pipe instance is busy.
    /// The listener always keeps a spare instance, so this only covers the
    /// instant between one client's `connect` and the next instance's creation.
    const PIPE_BUSY_RETRY_WINDOW: Duration = Duration::from_secs(2);

    /// Deterministic pipe name for a socket path: the same path always maps to
    /// the same pipe, so clients need nothing but the path, and two different
    /// paths (two runtime directories) never collide.
    pub fn pipe_name(path: &Path) -> String {
        let normalized = path.to_string_lossy().to_lowercase();
        let digest = Sha256::digest(normalized.as_bytes());
        let mut tag = String::with_capacity(24);
        for byte in &digest[..12] {
            use std::fmt::Write as _;
            let _ = write!(tag, "{byte:02x}");
        }
        format!(r"\\.\pipe\winx-{tag}")
    }

    /// One accepted or dialed pipe connection.
    #[derive(Debug)]
    pub enum DaemonStream {
        Server(NamedPipeServer),
        Client(NamedPipeClient),
    }

    impl AsyncRead for DaemonStream {
        fn poll_read(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            match self.get_mut() {
                Self::Server(pipe) => Pin::new(pipe).poll_read(cx, buf),
                Self::Client(pipe) => Pin::new(pipe).poll_read(cx, buf),
            }
        }
    }

    impl AsyncWrite for DaemonStream {
        fn poll_write(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            data: &[u8],
        ) -> Poll<io::Result<usize>> {
            match self.get_mut() {
                Self::Server(pipe) => Pin::new(pipe).poll_write(cx, data),
                Self::Client(pipe) => Pin::new(pipe).poll_write(cx, data),
            }
        }

        fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            match self.get_mut() {
                Self::Server(pipe) => Pin::new(pipe).poll_flush(cx),
                Self::Client(pipe) => Pin::new(pipe).poll_flush(cx),
            }
        }

        fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            match self.get_mut() {
                Self::Server(pipe) => Pin::new(pipe).poll_shutdown(cx),
                Self::Client(pipe) => Pin::new(pipe).poll_shutdown(cx),
            }
        }
    }

    impl DaemonStream {
        /// Non-blocking read used by idle health connections to notice a peer
        /// that went away.
        pub fn try_read(&self, buf: &mut [u8]) -> io::Result<usize> {
            match self {
                Self::Server(pipe) => pipe.try_read(buf),
                Self::Client(pipe) => pipe.try_read(buf),
            }
        }
    }

    /// Named-pipe listener that owns the marker file at the socket path.
    pub struct DaemonListener {
        name: String,
        marker: PathBuf,
        security: PipeSecurity,
        spare: Option<NamedPipeServer>,
    }

    impl DaemonListener {
        /// Claim the pipe named after `path`. `first_pipe_instance` already
        /// makes the kernel refuse a second daemon, so a stale marker file
        /// needs no check: it is simply rewritten by the winner.
        // Signature parity with the Unix listener, which awaits the stale-socket check.
        #[allow(clippy::unused_async, clippy::unused_async_trait_impl)]
        pub async fn bind(path: &Path) -> io::Result<Self> {
            let name = pipe_name(path);
            // Every instance carries an owner-only DACL plus an integrity
            // label (see `PipeSecurity`): other local accounts and
            // lower-integrity processes cannot open the pipe at all.
            // `first_pipe_instance` makes a second daemon fail to bind instead
            // of silently sharing the name; `reject_remote_clients` keeps the
            // pipe off the network.
            let security = PipeSecurity::owner_only()?;
            let spare = security.create_pipe_instance(
                ServerOptions::new().first_pipe_instance(true).reject_remote_clients(true),
                &name,
            )?;
            std::fs::write(path, format!("{name}\n{}\n", std::process::id()))?;
            Ok(Self { name, marker: path.to_path_buf(), security, spare: Some(spare) })
        }

        fn new_instance(&self) -> io::Result<NamedPipeServer> {
            self.security
                .create_pipe_instance(ServerOptions::new().reject_remote_clients(true), &self.name)
        }

        pub async fn accept(&mut self) -> io::Result<DaemonStream> {
            let server = match self.spare.take() {
                Some(server) => server,
                None => self.new_instance()?,
            };
            server.connect().await?;
            // Create the next instance before handing this one out so a client
            // racing in finds a free instance instead of `ERROR_PIPE_BUSY`.
            self.spare = self.new_instance().ok();
            Ok(DaemonStream::Server(server))
        }

        pub fn marker_path(&self) -> &Path {
            &self.marker
        }
    }

    /// Dial the daemon behind `path`. A missing server surfaces as
    /// `NotFound`, which callers already treat like a refused Unix socket.
    pub async fn connect(path: &Path) -> io::Result<DaemonStream> {
        let name = pipe_name(path);
        let deadline = Instant::now() + PIPE_BUSY_RETRY_WINDOW;
        loop {
            match ClientOptions::new().open(&name) {
                Ok(client) => return Ok(DaemonStream::Client(client)),
                Err(error)
                    if error.raw_os_error() == Some(ERROR_PIPE_BUSY.cast_signed())
                        && Instant::now() < deadline =>
                {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// See [`DaemonListener::bind`]: the owner-only DACL and integrity label
    /// on every pipe instance stop other accounts and lower-integrity
    /// processes from opening it, so an accepted connection is same-user.
    pub fn peer_is_same_user(_stream: &DaemonStream) -> io::Result<bool> {
        Ok(true)
    }

    #[cfg(test)]
    mod tests {
        use super::pipe_name;
        use std::path::Path;

        #[test]
        fn pipe_names_are_stable_and_case_insensitive() {
            let lower = pipe_name(Path::new(r"c:\users\x\winx\winxd.sock"));
            let upper = pipe_name(Path::new(r"C:\Users\X\winx\winxd.sock"));
            assert_eq!(lower, upper);
            assert!(lower.starts_with(r"\\.\pipe\winx-"));
            assert_ne!(lower, pipe_name(Path::new(r"C:\Users\X\winx\other.sock")));
        }
    }
}

#[cfg(all(test, unix))]
mod unix_claim_tests {
    #![allow(clippy::expect_used)]
    use super::{connect, lock_path, DaemonListener};

    #[tokio::test]
    async fn second_bind_fails_while_the_first_listens_and_the_first_stays_reachable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("winxd.sock");
        let first = DaemonListener::bind(&socket).await.expect("first bind");
        let error = DaemonListener::bind(&socket)
            .await
            .err()
            .expect("second bind must not steal the socket");
        assert_eq!(error.kind(), std::io::ErrorKind::AddrInUse, "{error}");
        assert!(lock_path(&socket).is_file(), "lock file beside the socket");
        // The loser did not unlink the winner's socket.
        assert!(connect(&socket).await.is_ok(), "first listener still reachable");
        drop(first);
        DaemonListener::bind(&socket).await.expect("bind after the first listener is gone");
    }

    #[tokio::test]
    async fn stale_socket_from_a_dead_daemon_is_replaced() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("winxd.sock");
        let stale = tokio::net::UnixListener::bind(&socket).expect("stale listener");
        drop(stale);
        assert!(socket.exists(), "unix sockets outlive their listener");
        let listener = DaemonListener::bind(&socket).await.expect("replace stale socket");
        assert!(connect(&socket).await.is_ok());
        drop(listener);
    }

    #[test]
    fn lock_path_sits_beside_the_socket() {
        assert_eq!(
            lock_path(std::path::Path::new("/run/u/winx/winxd.sock")),
            std::path::PathBuf::from("/run/u/winx/winxd.sock.lock")
        );
    }
}
