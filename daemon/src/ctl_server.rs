//! Control socket: one JSON object per line, one request per line.
//!
//! `status` is answered straight from the store so it stays responsive while
//! the supervisor is busy dialing; everything else is forwarded to the
//! supervisor, which owns the socket.

use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::{mpsc, oneshot, Semaphore},
    time::{timeout_at, Instant},
};
use tracing::{debug, warn};

use crate::{
    ctl_proto::{Request, Response},
    store::{ConnectionContext, Store},
};

/// Longest request line accepted. Real requests are well under 200 bytes;
/// anything past this is a bug or an attempt to grow the daemon's heap.
const MAX_LINE: usize = 64 * 1024;
/// Most control clients that may be served at once. `auris` connects,
/// asks one thing and leaves, so this is generous.
const MAX_CLIENTS: usize = 16;
/// Pause after a transient accept error, so a descriptor shortage does not
/// turn into a busy loop.
const ACCEPT_RETRY: Duration = Duration::from_millis(200);
/// Leave a little room below the CLI's five-second I/O timeout. Commands that
/// sat behind a dial this long must become inert, not run after the client has
/// already given up or the selected device changed.
const COMMAND_DEADLINE: Duration = Duration::from_secs(4);

/// A request plus the channel its answer must go back on.
#[derive(Debug)]
pub struct Command {
    /// What was asked.
    pub request: Request,
    /// Where the answer goes.
    pub reply: oneshot::Sender<Response>,
    /// Identity/link context captured before queueing.
    pub context: ConnectionContext,
    /// Queue and execution deadline. Dropping `reply` at this point makes a
    /// later queued command observable as inert to the supervisor.
    pub deadline: Instant,
}

/// Bind the control socket, clearing a stale one left by a crash.
///
/// Fails if another daemon is already listening on it.
pub fn bind(path: &Path) -> anyhow::Result<UnixListener> {
    if path.exists() {
        match std::os::unix::net::UnixStream::connect(path) {
            Ok(_) => anyhow::bail!("another aurisd is already listening on {}", path.display()),
            Err(_) => {
                debug!(path = %path.display(), "removing stale control socket");
                std::fs::remove_file(path)?;
            }
        }
    }
    // Create the socket node already owner-only rather than tightening it
    // afterwards, so there is no window at umask permissions.
    let old = unsafe { libc::umask(0o077) };
    let bound = UnixListener::bind(path);
    unsafe { libc::umask(old) };
    let listener = bound?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Remove the control socket. Called on shutdown; failures are not fatal.
pub fn unbind(path: &Path) {
    if let Err(e) = std::fs::remove_file(path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            warn!(path = %path.display(), error = %e, "could not remove control socket");
        }
    }
}

/// Is this accept error transient enough to keep serving through?
///
/// Running out of file descriptors or having a client hang up mid-handshake
/// must not take the control socket down for the life of the daemon.
fn accept_is_transient(e: &std::io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        Some(libc::EMFILE | libc::ENFILE | libc::ECONNABORTED | libc::EINTR | libc::EAGAIN)
    ) || matches!(
        e.kind(),
        std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::Interrupted
            | std::io::ErrorKind::WouldBlock
    )
}

/// Accept connections until the listener dies.
pub async fn serve(listener: UnixListener, store: Arc<Store>, tx: mpsc::Sender<Command>) {
    let slots = Arc::new(Semaphore::new(MAX_CLIENTS));
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let Ok(permit) = Arc::clone(&slots).try_acquire_owned() else {
                    warn!(
                        max = MAX_CLIENTS,
                        "control socket busy; dropping connection"
                    );
                    drop(stream);
                    continue;
                };
                let store = Arc::clone(&store);
                let tx = tx.clone();
                tokio::spawn(async move {
                    handle(stream, store, tx).await;
                    drop(permit);
                });
            }
            Err(e) if accept_is_transient(&e) => {
                warn!(error = %e, "transient control socket accept error; retrying");
                tokio::time::sleep(ACCEPT_RETRY).await;
            }
            Err(e) => {
                warn!(error = %e, "control socket accept failed");
                return;
            }
        }
    }
}

/// Outcome of one bounded line read.
enum Line {
    /// A complete line, newline stripped.
    Got(String),
    /// The peer hung up.
    Eof,
    /// The line ran past [`MAX_LINE`].
    TooLong,
    /// Read failure.
    Failed(std::io::Error),
}

/// Read one newline-terminated line, refusing to buffer more than
/// [`MAX_LINE`] bytes. `tokio`'s `read_line` has no such bound.
async fn read_line_capped<R: tokio::io::AsyncBufRead + Unpin>(r: &mut R) -> Line {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let (done, used) = {
            let available = match r.fill_buf().await {
                Ok(b) => b,
                Err(e) => return Line::Failed(e),
            };
            if available.is_empty() {
                return if buf.is_empty() {
                    Line::Eof
                } else {
                    Line::Got(String::from_utf8_lossy(&buf).into_owned())
                };
            }
            match available.iter().position(|&b| b == b'\n') {
                Some(i) => {
                    if buf.len() + i > MAX_LINE {
                        return Line::TooLong;
                    }
                    buf.extend_from_slice(&available[..i]);
                    (true, i + 1)
                }
                None => {
                    if buf.len() + available.len() > MAX_LINE {
                        return Line::TooLong;
                    }
                    buf.extend_from_slice(available);
                    (false, available.len())
                }
            }
        };
        r.consume(used);
        if done {
            return Line::Got(String::from_utf8_lossy(&buf).into_owned());
        }
    }
}

/// Serve one client connection.
pub async fn handle(stream: UnixStream, store: Arc<Store>, tx: mpsc::Sender<Command>) {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    loop {
        let line = match read_line_capped(&mut reader).await {
            Line::Got(l) => l,
            Line::Eof => return,
            Line::TooLong => {
                warn!(
                    max = MAX_LINE,
                    "control request line too long; closing connection"
                );
                let body = serde_json::to_vec(&Response::error("request too long"))
                    .unwrap_or_else(|_| b"{\"ok\":false,\"error\":\"request too long\"}".to_vec());
                let _ = write.write_all(&body).await;
                let _ = write.write_all(b"\n").await;
                let _ = write.flush().await;
                return;
            }
            Line::Failed(e) => {
                debug!(error = %e, "control client read failed");
                return;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(Request::Status) => Response::Status(Box::new(store.snapshot())),
            Ok(Request::Subscribe) => return stream_snapshots(write, &store).await,
            Ok(request) => dispatch(&tx, &store, request).await,
            Err(e) => Response::error(format!("bad request: {e}")),
        };
        let mut body = match serde_json::to_vec(&response) {
            Ok(b) => b,
            Err(e) => {
                warn!(error = %e, "could not encode response");
                return;
            }
        };
        body.push(b'\n');
        if let Err(e) = write.write_all(&body).await {
            debug!(error = %e, "control client write failed");
            return;
        }
        let _ = write.flush().await;
    }
}

/// Write the current snapshot, then one per change until the client hangs up.
///
/// `watch` keeps only the newest value, so a slow reader is never a memory
/// problem: it just skips the states it was too late for and gets the current
/// one. The connection stops reading requests, so a subscriber that also wants
/// to send commands opens a second one.
async fn stream_snapshots(mut write: tokio::net::unix::OwnedWriteHalf, store: &Arc<Store>) {
    let mut rx = store.subscribe();
    loop {
        // borrow_and_update marks this value seen, so the changed() below waits
        // for the next one instead of returning immediately.
        let snapshot = rx.borrow_and_update().clone();
        let mut body = match serde_json::to_vec(&Response::Status(Box::new(snapshot))) {
            Ok(b) => b,
            Err(e) => {
                warn!(error = %e, "could not encode snapshot for subscriber");
                return;
            }
        };
        body.push(b'\n');
        if let Err(e) = write.write_all(&body).await {
            debug!(error = %e, "subscriber write failed");
            return;
        }
        if write.flush().await.is_err() {
            return;
        }
        if rx.changed().await.is_err() {
            // The store is gone, which only happens as the daemon shuts down.
            return;
        }
    }
}

async fn dispatch(tx: &mpsc::Sender<Command>, store: &Store, request: Request) -> Response {
    let deadline = Instant::now() + COMMAND_DEADLINE;
    dispatch_until(tx, store, request, deadline).await
}

async fn dispatch_until(
    tx: &mpsc::Sender<Command>,
    store: &Store,
    request: Request,
    deadline: Instant,
) -> Response {
    let (reply, rx) = oneshot::channel();
    let command = Command {
        request,
        reply,
        context: store.connection_context(),
        deadline,
    };
    match timeout_at(deadline, tx.send(command)).await {
        Ok(Ok(())) => {}
        Ok(Err(_)) => return Response::error("daemon is shutting down"),
        Err(_) => return Response::error("request expired before queueing"),
    }
    match timeout_at(deadline, rx).await {
        Ok(Ok(r)) => r,
        Ok(Err(_)) => Response::error("daemon dropped the request"),
        Err(_) => Response::error(
            "request timed out; an already-sent operation may still complete; no automatic retry",
        ),
    }
}

/// Path helper used by both binaries.
pub fn default_socket(runtime_dir: &Path) -> PathBuf {
    crate::config::socket_path(runtime_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::PrimaryBud, state::Snapshot, store::Update};

    /// A socket path under the test temp dir, unique per call.
    fn temp_socket() -> PathBuf {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("aurisd-test-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("ctl.sock")
    }

    /// Bind, serve, and hand back the path plus the store driving it.
    fn serving() -> (PathBuf, Arc<Store>) {
        let path = temp_socket();
        let listener = bind(&path).unwrap();
        let store = Store::new(Snapshot::initial(""), PrimaryBud::Auto);
        let (tx, _rx) = mpsc::channel(4);
        tokio::spawn(serve(listener, Arc::clone(&store), tx));
        (path, store)
    }

    #[tokio::test]
    async fn subscribe_sends_the_snapshot_then_one_per_change() {
        let (path, store) = serving();
        let stream = UnixStream::connect(&path).await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut lines = BufReader::new(read).lines();

        write.write_all(b"{\"cmd\":\"subscribe\"}\n").await.unwrap();

        let first: serde_json::Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(first["schema"], 2, "first line must be the snapshot");
        assert_eq!(first["device"]["connected"], false);

        store.apply(Update::AclConnected(true));
        let second: serde_json::Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(
            second["device"]["connected"], true,
            "a change must be pushed without being asked for"
        );

        // A no-op update must not produce a line; the next real one must.
        store.apply(Update::AclConnected(true));
        store.apply(Update::AapLink(true));
        let third: serde_json::Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(third["device"]["aap_link"], true);

        unbind(&path);
    }

    #[tokio::test]
    async fn status_still_answers_one_line_per_request() {
        let (path, _store) = serving();
        let stream = UnixStream::connect(&path).await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut lines = BufReader::new(read).lines();

        for _ in 0..2 {
            write.write_all(b"{\"cmd\":\"status\"}\n").await.unwrap();
            let v: serde_json::Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(v["schema"], 2);
        }

        unbind(&path);
    }

    #[tokio::test]
    async fn dispatch_deadline_drops_a_queued_reply_receiver() {
        let store = Store::new(Snapshot::initial(""), PrimaryBud::Auto);
        let (tx, mut rx) = mpsc::channel(1);
        let deadline = Instant::now() + Duration::from_millis(100);
        let task = tokio::spawn({
            let store = Arc::clone(&store);
            async move { dispatch_until(&tx, &store, Request::Reconnect, deadline).await }
        });
        let command = rx.recv().await.expect("dispatch queued command");
        let response = task.await.unwrap();
        assert!(matches!(response, Response::Ack { ok: false, .. }));
        assert!(
            command.reply.is_closed(),
            "expired dispatch must make the queued command inert"
        );
    }
}
