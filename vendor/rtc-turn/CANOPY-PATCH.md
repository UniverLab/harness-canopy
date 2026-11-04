# Canopy patch vs upstream rtc-turn 0.21.1

Upstream frees a channel binding on any `CHANNEL-BIND` error, including the
`438 Stale Nonce` it asks the client to retry. The TURN server does not free the
channel on that path, so the client picks a fresh number for the same peer on
the next send. RFC 8656 §12 requires the server to answer `400` to a peer that
already has a different channel, or to a number already bound to another peer,
so Cloudflare `turn.cloudflare.com` rejects every retry (`error 400: `, 88
times on 2026-11-02). Incoming `ChannelData` on the number the server still
holds is then dropped with `ErrChannelBindNotFound`, which loses the ICE
consent responses and takes the pair `connected → disconnected → failed`
within 6–35 s.

The patch keeps the channel number alive for the server's 10-minute channel
lifetime:

- `src/client/binding.rs` — a `Binding` records `sent_at` (the last
  `CHANNEL-BIND` transmitted to the server), `bind_retries`, and whether a
  failure has already been logged. A channel number already present in
  `chan_map` is never assigned to another peer. Bindings older than
  `CHANNEL_LIFETIME` are pruned when a new one is created.
- `src/client/relay.rs` — a `CHANNEL-BIND` error never deletes the binding: the
  binding keeps its number and peer and goes to `Failed`. A `438` takes the new
  nonce from the error and retries the same number immediately, at most twice
  per bind exchange (a fresh bind starts a fresh budget). A `400` for a peer
  that still has a binding means the server holds a channel for it, so the
  binding is treated as `Ready`. The error code and reason are logged at `WARN`
  once per binding, and later repeats at `DEBUG`.
- `src/client/mod.rs` — `handle_channel_data` resolves a channel number through
  every binding sent in the last `CHANNEL_LIFETIME`, including `Failed` ones,
  so relayed data is never dropped for a channel the server may still hold.

Everything else is upstream 0.21.1. This vendored copy, and the
`[patch.crates-io]` entry that wires it, are removed once rtc-turn ships the
fix upstream.
