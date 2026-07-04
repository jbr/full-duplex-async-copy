# full-duplex-async-copy

[![ci][ci-badge]][ci]
[![crates.io version][version-badge]][crate]
[![docs.rs][docs-badge]][docs]
[![codecov][codecov-badge]][codecov]

[ci]: https://github.com/jbr/full-duplex-async-copy/actions?query=workflow%3ACI
[ci-badge]: https://github.com/jbr/full-duplex-async-copy/workflows/CI/badge.svg
[version-badge]: https://img.shields.io/crates/v/full-duplex-async-copy.svg?style=flat-square
[crate]: https://crates.io/crates/full-duplex-async-copy
[docs-badge]: https://img.shields.io/badge/docs-latest-blue.svg?style=flat-square
[docs]: https://docs.rs/full-duplex-async-copy
[codecov-badge]: https://codecov.io/gh/jbr/full-duplex-async-copy/graph/badge.svg
[codecov]: https://codecov.io/gh/jbr/full-duplex-async-copy

Concurrently copy bytes in both directions between two `AsyncRead + AsyncWrite` transports until
both reach EOF, resolving to the number of bytes copied in each direction. Think
[`tokio::io::copy_bidirectional`](https://docs.rs/tokio/latest/tokio/io/fn.copy_bidirectional.html),
but for [`futures-io`](https://docs.rs/futures-io) / [`futures-lite`](https://docs.rs/futures-lite)
transports and with explicit, per-endpoint control over half-close behavior.

When one direction reaches EOF, you choose whether that close is propagated to the peer's write
half:

- `Close::Propagate` — shut the peer's write half down, forwarding the close as a FIN (the behavior
  of a plain byte-stream proxy, and of `tokio::io::copy_bidirectional`).
- `Close::Contain` — leave the peer's write half open, so a one-sided close is not escalated into a
  full teardown. Required for peers that treat an incoming FIN as a full close.

There is deliberately no default: the caller chooses a policy for each side.

## Example

```rust
use full_duplex_async_copy::{Close, full_duplex_copy};

// `a` and `b` are two transports to relay between — e.g. two TCP connections to proxy.
// Each side decides whether its close propagates to the peer. Here both propagate, tokio-style:
let (a_to_b, b_to_a) = full_duplex_copy(Close::Propagate(a), Close::Propagate(b)).await?;

// To preserve a half-open connection instead (e.g. one side is an upgraded protocol that
// treats a FIN as a full close), contain that side's close:
let (a_to_b, b_to_a) = full_duplex_copy(Close::Contain(a), Close::Propagate(b)).await?;
```

## Safety

This crate uses `#![forbid(unsafe_code)]`.

## License

<sup>
Licensed under either of <a href="LICENSE-APACHE">Apache License, Version
2.0</a> or <a href="LICENSE-MIT">MIT license</a> at your option.
</sup>

<br/>

<sub>
Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this crate by you, as defined in the Apache-2.0 license, shall
be dual licensed as above, without any additional terms or conditions.
</sub>
