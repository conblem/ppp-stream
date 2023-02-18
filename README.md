[![crates.io](https://img.shields.io/crates/v/ppp-stream)](https://crates.io/crates/ppp-stream)
# ppp-stream

This is a simple library that adds helper functions to [AsyncRead](https://docs.rs/tokio/latest/tokio/io/trait.AsyncRead.html) streams for [HAProxy Protocol](https://www.haproxy.org/download/1.8/doc/proxy-protocol.txt) extraction.
The Proxy Protocol parsing is implemented by the great [ppp](https://crates.io/crates/ppp) library, many thanks to [@misalcedo](https://github.com/misalcedo) and all the other [contributors](https://github.com/misalcedo/ppp/graphs/contributors).

Also, big thanks to [@simao](https://github.com/simao) for his contribution to this project.