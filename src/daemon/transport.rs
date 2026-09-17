//! Control-plane transport: Unix-domain sockets on Unix, named pipes on Windows.
//!
//! The JSON-RPC layer only sees [`DaemonStream`] (an `AsyncRead + AsyncWrite`
//! value) and [`DaemonListener`]. On Windows the "socket path" keeps its role
//! as the session identity: the listener creates a marker file at that path
//! and derives the pipe name from it, so every lifecycle routine that scans,
//! probes, or removes `*.sock` files works unchanged on both platforms.

use std::io;
use std::path::Path;

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

#[cfg(unix)]
mod unix_impl {
    use std::io;
    use std::path::Path;

    use tokio::net::{UnixListener, UnixStream};

    pub type DaemonStream = UnixStream;

    pub struct DaemonListener {
        inner: UnixListener,
    }

    impl DaemonListener {
        pub fn bind(path: &Path) -> io::Result<Self> {
            Ok(Self { inner: UnixListener::bind(path)? })
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
        spare: Option<NamedPipeServer>,
    }

    impl DaemonListener {
        pub fn bind(path: &Path) -> io::Result<Self> {
            let name = pipe_name(path);
            // `first_pipe_instance` makes a second daemon fail to bind instead
            // of silently sharing the name; `reject_remote_clients` keeps the
            // pipe off the network. The default DACL grants other local users
            // read-only access at most, and every Winx client opens read/write,
            // so a foreign user cannot complete a connection.
            let spare = ServerOptions::new()
                .first_pipe_instance(true)
                .reject_remote_clients(true)
                .create(&name)?;
            std::fs::write(path, format!("{name}\n{}\n", std::process::id()))?;
            Ok(Self { name, marker: path.to_path_buf(), spare: Some(spare) })
        }

        pub async fn accept(&mut self) -> io::Result<DaemonStream> {
            let server = match self.spare.take() {
                Some(server) => server,
                None => ServerOptions::new().reject_remote_clients(true).create(&self.name)?,
            };
            server.connect().await?;
            // Create the next instance before handing this one out so a client
            // racing in finds a free instance instead of `ERROR_PIPE_BUSY`.
            self.spare = ServerOptions::new().reject_remote_clients(true).create(&self.name).ok();
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

    /// See [`DaemonListener::bind`]: the pipe ACL already excludes other users
    /// from read/write connections, so an accepted connection is same-user.
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
