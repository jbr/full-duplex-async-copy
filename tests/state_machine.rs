//! Deterministic state-machine tests.
//!
//! These use a scripted mock transport that never returns `Poll::Pending`, so the whole copy
//! drives to completion inside a single `block_on` with no threads and no timing — every run is
//! identical. The mock is `!Send` (`Rc<RefCell<_>>`), which is fine now that `full_duplex_copy`
//! no longer requires `Send + Sync`, and lets a handle inspect what was written/closed afterwards.

use full_duplex_async_copy::{Close, full_duplex_copy};
use futures_lite::{
    future::block_on,
    io::{AsyncRead, AsyncWrite},
};
use std::{
    cell::RefCell,
    collections::VecDeque,
    io,
    pin::Pin,
    rc::Rc,
    task::{Context, Poll},
};

#[derive(Default)]
struct MockState {
    /// Bytes still to be yielded by reads; once empty, reads report EOF.
    to_read: VecDeque<u8>,
    /// If set, the next read fails with this error kind (consumed once).
    read_error: Option<io::ErrorKind>,
    /// Everything written into this mock.
    written: Vec<u8>,
    /// Cap on bytes accepted per `poll_write`, to force partial writes.
    max_write: Option<usize>,
    /// If set, every `poll_write` reports `Ok(0)`.
    write_zero: bool,
    close_calls: usize,
    flush_calls: usize,
}

#[derive(Clone, Default)]
struct Mock(Rc<RefCell<MockState>>);

impl Mock {
    fn reader(bytes: &[u8]) -> Self {
        Mock::default().with(|s| s.to_read = bytes.iter().copied().collect())
    }

    fn with(self, f: impl FnOnce(&mut MockState)) -> Self {
        f(&mut self.0.borrow_mut());
        self
    }

    fn written(&self) -> Vec<u8> {
        self.0.borrow().written.clone()
    }

    fn close_calls(&self) -> usize {
        self.0.borrow().close_calls
    }

    fn flush_calls(&self) -> usize {
        self.0.borrow().flush_calls
    }
}

impl AsyncRead for Mock {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        let mut state = self.0.borrow_mut();
        if let Some(kind) = state.read_error.take() {
            return Poll::Ready(Err(kind.into()));
        }
        let n = buf.len().min(state.to_read.len());
        for slot in buf.iter_mut().take(n) {
            *slot = state.to_read.pop_front().unwrap();
        }
        Poll::Ready(Ok(n)) // n == 0 signals EOF
    }
}

impl AsyncWrite for Mock {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let mut state = self.0.borrow_mut();
        if state.write_zero {
            return Poll::Ready(Ok(0));
        }
        let n = state.max_write.map_or(buf.len(), |max| max.min(buf.len()));
        state.written.extend_from_slice(&buf[..n]);
        Poll::Ready(Ok(n))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.0.borrow_mut().flush_calls += 1;
        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.0.borrow_mut().close_calls += 1;
        Poll::Ready(Ok(()))
    }
}

#[test]
fn partial_writes_deliver_all_bytes() {
    let a = Mock::reader(b"hello world");
    let b = Mock::default().with(|s| s.max_write = Some(1)); // one byte accepted per write
    let written = b.clone();

    let counts = block_on(full_duplex_copy(Close::Contain(a), Close::Contain(b))).unwrap();

    assert_eq!(counts, (11, 0));
    assert_eq!(written.written(), b"hello world");
}

#[test]
fn write_zero_is_an_error() {
    let a = Mock::reader(b"data");
    let b = Mock::default().with(|s| s.write_zero = true);

    let err = block_on(full_duplex_copy(Close::Contain(a), Close::Contain(b))).unwrap_err();

    assert_eq!(err.kind(), io::ErrorKind::WriteZero);
}

#[test]
fn read_error_propagates() {
    let a = Mock::default().with(|s| s.read_error = Some(io::ErrorKind::ConnectionReset));
    let b = Mock::reader(b"whatever");

    let err = block_on(full_duplex_copy(Close::Contain(a), Close::Contain(b))).unwrap_err();

    assert_eq!(err.kind(), io::ErrorKind::ConnectionReset);
}

#[test]
fn close_propagate_shuts_down_each_peer_once() {
    let a = Mock::default(); // immediate EOF
    let b = Mock::default(); // immediate EOF
    let (a_handle, b_handle) = (a.clone(), b.clone());

    let counts = block_on(full_duplex_copy(Close::Propagate(a), Close::Propagate(b))).unwrap();

    assert_eq!(counts, (0, 0));
    // Each side reaching EOF closes the *other* side's write half exactly once.
    assert_eq!(a_handle.close_calls(), 1);
    assert_eq!(b_handle.close_calls(), 1);
}

#[test]
fn close_contain_never_shuts_down_a_peer() {
    let a = Mock::default();
    let b = Mock::default();
    let (a_handle, b_handle) = (a.clone(), b.clone());

    let counts = block_on(full_duplex_copy(Close::Contain(a), Close::Contain(b))).unwrap();

    assert_eq!(counts, (0, 0));
    assert_eq!(a_handle.close_calls(), 0);
    assert_eq!(b_handle.close_calls(), 0);
    // EOF still flushes the peer, even when it isn't closed.
    assert!(a_handle.flush_calls() >= 1);
    assert!(b_handle.flush_calls() >= 1);
}

/// A payload that is relayed only after the sender has already hit EOF must still arrive: EOF on
/// one direction must not curtail the still-active opposite direction.
#[test]
fn opposite_direction_drains_after_first_eof() {
    let a = Mock::default(); // EOF immediately (a -> b sends nothing)
    let b = Mock::reader(b"late payload"); // b -> a still has data
    let delivered = a.clone();

    let counts = block_on(full_duplex_copy(Close::Contain(a), Close::Contain(b))).unwrap();

    assert_eq!(counts, (0, 12));
    assert_eq!(delivered.written(), b"late payload");
}
