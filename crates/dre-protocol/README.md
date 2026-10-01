# dre-protocol

The plugin protocol of [DRE](https://github.com/get-dre/dre), the Declarative Reporting Engine:
the frames and messages core and a plugin exchange, core's side (spawning a plugin and talking to
it), an SDK for writing a plugin in Rust, and a conformance suite that checks a plugin binary.

Every DRE source, format and destination is a plugin: a separate program that speaks this
protocol over stdin/stdout, in any language. See the
[protocol specification](https://github.com/get-dre/dre/blob/master/docs/protocol.md).

This crate has its own version. The wire protocol has an integer version too, which changes
only when the messages do; DRE and a plugin agree on it in the handshake.
