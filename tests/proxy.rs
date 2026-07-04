//! End-to-end proxy tests.
//!
//! Each test wires two [`TestTransport`] pairs through [`full_duplex_copy`] and drives them like a
//! real bidirectional proxy: an outer `client` and `server` on the ends, and the two inner ends
//! handed to the copy. Whatever a transport writes is readable from its peer, and either half of
//! either connection can be closed independently — enough to exercise half-open states.

use full_duplex_async_copy::{Close, full_duplex_copy};
use futures_lite::io::{AsyncRead, AsyncReadExt, AsyncWrite};
use std::{
    io,
    pin::Pin,
    sync::{Arc, RwLock},
    task::{Context, Poll, Waker},
};

// ---------------------------------------------------------------------------
// A minimal in-memory duplex transport, adapted (dependency-free) from
// trillium-testing's TestTransport.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct CursorInner {
    data: Vec<u8>,
    cursor: usize,
    waker: Option<Waker>,
    closed: bool,
}

/// A shared, closeable byte buffer with a read cursor. Writers append; a reader drains from the
/// cursor and parks until more data arrives or the buffer is closed (EOF).
#[derive(Default)]
struct CloseableCursor(RwLock<CursorInner>);

impl CloseableCursor {
    fn close(&self) {
        let mut inner = self.0.write().unwrap();
        inner.closed = true;
        if let Some(waker) = inner.waker.take() {
            waker.wake();
        }
    }

    fn push(&self, bytes: &[u8]) {
        let mut inner = self.0.write().unwrap();
        inner.data.extend_from_slice(bytes);
        if let Some(waker) = inner.waker.take() {
            waker.wake();
        }
    }

    fn snapshot(&self) -> Vec<u8> {
        self.0.read().unwrap().data.clone()
    }
}

// The trait impls live on `&CloseableCursor` so a shared `Arc<CloseableCursor>` (held by both ends
// of a connection) can be read and written concurrently through interior mutability.
impl AsyncRead for &CloseableCursor {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        let mut inner = self.0.write().unwrap();
        if inner.cursor < inner.data.len() {
            let n = buf.len().min(inner.data.len() - inner.cursor);
            buf[..n].copy_from_slice(&inner.data[inner.cursor..inner.cursor + n]);
            inner.cursor += n;
            Poll::Ready(Ok(n))
        } else if inner.closed {
            Poll::Ready(Ok(0))
        } else {
            inner.waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

impl AsyncWrite for &CloseableCursor {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let mut inner = self.0.write().unwrap();
        if inner.closed {
            Poll::Ready(Ok(0))
        } else {
            inner.data.extend_from_slice(buf);
            if let Some(waker) = inner.waker.take() {
                waker.wake();
            }
            Poll::Ready(Ok(buf.len()))
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.close();
        Poll::Ready(Ok(()))
    }
}

/// One end of a duplex connection. Anything written to it is readable from its peer.
#[derive(Clone)]
struct TestTransport {
    read: Arc<CloseableCursor>,
    write: Arc<CloseableCursor>,
}

impl TestTransport {
    /// Two connected ends. Writing to one is readable from the other.
    fn pair() -> (TestTransport, TestTransport) {
        let a = Arc::new(CloseableCursor::default());
        let b = Arc::new(CloseableCursor::default());
        (
            TestTransport {
                read: a.clone(),
                write: b.clone(),
            },
            TestTransport { read: b, write: a },
        )
    }

    /// Deliver bytes to the peer's reader.
    fn send(&self, bytes: &[u8]) {
        self.write.push(bytes);
    }

    /// EOF the peer's reader (as if we shut down our write half / hung up).
    fn close_write(&self) {
        self.write.close();
    }

    /// Cause the peer's writes to us to return `Ok(0)` (as if we shut down our read half).
    fn close_read(&self) {
        self.read.close();
    }

    /// Everything delivered to our reader so far.
    fn received(&self) -> Vec<u8> {
        self.read.snapshot()
    }

    /// Read exactly `n` bytes.
    async fn recv_exact(&self, n: usize) -> Vec<u8> {
        let mut buf = vec![0; n];
        self.clone().read_exact(&mut buf).await.unwrap();
        buf
    }

    /// Read to EOF.
    async fn recv_to_end(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        self.clone().read_to_end(&mut buf).await.unwrap();
        buf
    }

    /// A single read; returns the number of bytes read (`0` means EOF).
    async fn read_once(&self) -> usize {
        let mut buf = [0; 64];
        self.clone().read(&mut buf).await.unwrap()
    }
}

impl AsyncRead for TestTransport {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        let mut read: &CloseableCursor = &self.read;
        Pin::new(&mut read).poll_read(cx, buf)
    }
}

impl AsyncWrite for TestTransport {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let mut write: &CloseableCursor = &self.write;
        Pin::new(&mut write).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let mut write: &CloseableCursor = &self.write;
        Pin::new(&mut write).poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let mut write: &CloseableCursor = &self.write;
        Pin::new(&mut write).poll_close(cx)
    }
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[test]
fn bidirectional_roundtrip() {
    smol::block_on(async {
        let (client, proxy_a) = TestTransport::pair();
        let (proxy_b, server) = TestTransport::pair();
        let copy = smol::spawn(full_duplex_copy(
            Close::Propagate(proxy_a),
            Close::Propagate(proxy_b),
        ));

        client.send(b"ping");
        assert_eq!(server.recv_exact(4).await, b"ping");

        server.send(b"pong");
        assert_eq!(client.recv_exact(4).await, b"pong");

        client.close_write();
        server.close_write();
        assert_eq!(copy.await.unwrap(), (4, 4));
    });
}

#[test]
fn empty_both_sides_completes() {
    smol::block_on(async {
        let (client, proxy_a) = TestTransport::pair();
        let (proxy_b, server) = TestTransport::pair();
        let copy = smol::spawn(full_duplex_copy(
            Close::Propagate(proxy_a),
            Close::Propagate(proxy_b),
        ));

        client.close_write();
        server.close_write();
        assert_eq!(copy.await.unwrap(), (0, 0));
    });
}

#[test]
fn single_direction_transfer() {
    smol::block_on(async {
        let (client, proxy_a) = TestTransport::pair();
        let (proxy_b, server) = TestTransport::pair();
        let copy = smol::spawn(full_duplex_copy(
            Close::Propagate(proxy_a),
            Close::Propagate(proxy_b),
        ));

        client.send(b"only one way");
        client.close_write();
        server.close_write(); // server sends nothing

        assert_eq!(server.recv_to_end().await, b"only one way");
        assert_eq!(copy.await.unwrap(), (12, 0));
    });
}

#[test]
fn large_payloads_both_directions() {
    smol::block_on(async {
        let (client, proxy_a) = TestTransport::pair();
        let (proxy_b, server) = TestTransport::pair();
        let copy = smol::spawn(full_duplex_copy(
            Close::Propagate(proxy_a),
            Close::Propagate(proxy_b),
        ));

        // Both well past the 8 KiB internal BufReader, so many fill/write cycles are exercised.
        let up: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let down: Vec<u8> = (0..300_000u32).map(|i| (i % 253) as u8).collect();

        client.send(&up);
        client.close_write();
        server.send(&down);
        server.close_write();

        assert_eq!(server.recv_to_end().await, up);
        assert_eq!(client.recv_to_end().await, down);
        assert_eq!(copy.await.unwrap(), (up.len() as u64, down.len() as u64));
    });
}

/// `Close::Contain`: when one side stops sending, the peer's write half stays open, so the opposite
/// direction keeps relaying. The copy completes only once *both* directions have reached EOF.
#[test]
fn half_close_keeps_peer_writable() {
    smol::block_on(async {
        let (client, proxy_a) = TestTransport::pair();
        let (proxy_b, server) = TestTransport::pair();
        let copy = smol::spawn(full_duplex_copy(
            Close::Contain(proxy_a),
            Close::Contain(proxy_b),
        ));

        client.send(b"hello");
        assert_eq!(server.recv_exact(5).await, b"hello");

        // The client half-closes its send side. With `Close::Contain` this must NOT tear down the
        // server, so the server can still reach the client.
        client.close_write();
        server.send(b"world");
        assert_eq!(client.recv_exact(5).await, b"world");

        // Nothing was propagated to the server's reader.
        assert_eq!(server.received(), b"hello");

        // Now the server finishes too; the copy completes with a byte count for each direction.
        server.close_write();
        assert_eq!(copy.await.unwrap(), (5, 5));
    });
}

/// `Close::Propagate`: when one side reaches EOF, the peer's write half is shut down, propagating
/// the close (the reader on the far end observes EOF).
#[test]
fn full_close_propagates_eof_to_peer() {
    smol::block_on(async {
        let (client, proxy_a) = TestTransport::pair();
        let (proxy_b, server) = TestTransport::pair();
        let copy = smol::spawn(full_duplex_copy(
            Close::Propagate(proxy_a),
            Close::Propagate(proxy_b),
        ));

        client.send(b"hello");
        assert_eq!(server.recv_exact(5).await, b"hello");

        // Client hangs up. `Close::Propagate` closes the server-facing write half, so the server
        // now sees EOF on its reader.
        client.close_write();
        assert_eq!(server.read_once().await, 0);

        server.close_write();
        assert_eq!(copy.await.unwrap(), (5, 0));
    });
}

/// If the peer we're relaying *into* has shut down its read half, the write returns `Ok(0)` and
/// the copy surfaces a `WriteZero` error rather than silently dropping data.
#[test]
fn write_to_closed_peer_errors() {
    smol::block_on(async {
        let (client, proxy_a) = TestTransport::pair();
        let (proxy_b, server) = TestTransport::pair();
        let copy = smol::spawn(full_duplex_copy(
            Close::Propagate(proxy_a),
            Close::Propagate(proxy_b),
        ));

        client.close_read(); // the client will no longer accept relayed bytes
        server.send(b"data"); // ...but the server sends some, which we must relay to the client
        server.close_write();

        assert_eq!(copy.await.unwrap_err().kind(), io::ErrorKind::WriteZero);
    });
}
