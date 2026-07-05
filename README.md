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
both reach EOF, resolving to the number of bytes copied in each direction. It supports **half-closed**
connections: when one direction reaches EOF, that read half stops and the peer's write half is shut
down (forwarding the close as a FIN), while the opposite direction keeps relaying until it too
reaches EOF.

Built for [`futures-io`](https://docs.rs/futures-io) /
[`futures-lite`](https://docs.rs/futures-lite) transports.

## Example

```rust
use full_duplex_async_copy::full_duplex_copy;

// `a` and `b` are two transports to relay between — e.g. two TCP connections to proxy.
let (a_to_b, b_to_a) = full_duplex_copy(a, b).await?;
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
