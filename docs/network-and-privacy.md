---
title: Network and Privacy
description: Every outbound connection canopy makes, and how to turn each one off.
order: 19
---

# Network and Privacy

Every outbound connection canopy makes, who makes it, what is sent, when, and the setting or command that turns it off. A default install holds no always-on connection: announcements stay off until you say yes at setup. Everything else is off, manual, or a clearly labelled automatic check.

| destination | made by | purpose | data sent | when | how to turn it off | default |
|---|---|---|---|---|---|---|
| `wss://announcements.univerlab.org/ws` | daemon | upgrade and tip announcements | nothing; the daemon only reads messages and keeps seen IDs in its local database | connected while the daemon runs, only once enabled at setup | `announcements_enabled` (takes effect on the next daemon run) | Off (asked at setup) |
| `https://github.com/UniverLab/harness-canopy/releases/latest` and `https://api.github.com/repos/UniverLab/harness-canopy/releases` | TUI at start | read-only update notice | version check requests; no body | at TUI start, at most every `update.check_interval_hours` | `update.check_enabled` | On, every 24 hours |
| `https://github.com/UniverLab/harness-canopy/releases/download/<tag>/<asset>` (plus `SHA256SUMS.txt` and `SHA256SUMS.txt.minisig` in the same release) | `canopy update` | download and verify the release asset | version, asset, checksums and signature requests; the downloaded archive | only when typed | only runs when you type `canopy update` | Manual |
| `https://raw.githubusercontent.com/UniverLab/canopy-registry/main/` | TUI at start, `canopy setup`, `canopy registry refresh`, `harness refresh_registry` | platform registry (index, servers, platforms) | registry file requests; no body | TUI at start when the last refresh is over 24 hours old, and during setup; the commands run only when typed | no switch | On for the TUI check; manual otherwise |
| `https://api.github.com/repos/UniverLab/skills/contents` (and its file `download_url`s) | `canopy setup` (or the TUI on a first launch that runs setup), `canopy skill install`, `canopy update` | check legacy `~/.agents/skills/` copies against the upstream Essential Pack during cleanup | directory listing and skill file requests; no body | during legacy skill-copy cleanup | only runs during `canopy setup`, `canopy skill install`, or `canopy update` | Manual |
| `https://models.dev/api.json` | TUI model pickers, `harness models`, `canopy models refresh` | shared model catalogue | catalogue request; no body | whenever the cache is older than `models.catalog_ttl_minutes` (the TUI refreshes it in the background; the commands fetch on demand) | `models.catalog_ttl_minutes` (raise it to slow background refreshes) | Automatic when stale; 24 hour cache |
| `https://graphs.univerlab.org` (the default for `graphs.catalogue_url`) | `canopy graph install` and the catalogue actions | ready-made graphs | catalogue requests; no body | only when typed | only runs when you type `canopy graph install` | Manual |
| the relay at `remote.relay_url` (default `https://remote.univerlab.org`) | daemon, only when remote control is on | owner remote-control socket | GitHub token, host id and label on connect; signalling envelopes after that (see below) | while remote control is on | `remote.enabled` | Off |
| `https://github.com/login/device/code`, `https://github.com/login/oauth/access_token`, `https://api.github.com/user` | `canopy remote login` and the TUI remote panel | GitHub OAuth device flow sign-in | device code request, token exchange, one user lookup | only on explicit sign-in | only runs when you type `canopy remote login` | Manual |
| `https://api.github.com` | daemon while remote is on, `canopy remote invite` | resolve guest GitHub logins to user ids | the login name; the user id comes back | daemon on relay join and when the guest list changes; one lookup when inviting | `remote.enabled` (daemon lookups stop when remote is off; inviting resolves one login only when typed) | Off unless remote is on |
| the configured STUN fallback server (previously `stun:stun.l.google.com:19302`) | daemon, remote path only | fallback ICE server | STUN binding requests that reveal this host's addresses to the fallback server | only when the relay returns no ICE servers | `remote.stun_fallback` (empty means no fallback; takes effect on the next connection) | Empty on a fresh install (no fallback) |
| `https://api.openai.com/v1` | daemon RAG | cloud embeddings | text chunks and the API key | only when `embeddings_model` names an OpenAI model | `embeddings_model` | Off unless configured |
| `https://generativelanguage.googleapis.com` | daemon RAG | cloud embeddings | text chunks and the API key | only when `embeddings_model` names a Gemini model | `embeddings_model` | Off unless configured |
| `https://huggingface.co` (or `$HF_ENDPOINT`) | local-embeddings builds | local embedding model download | model file requests; no body | when a local embeddings model is selected | `embeddings_model` | Off unless configured |
| `https://github.com/microsoft/onnxruntime/releases/download/v1.24.2/...` | local-embeddings builds | ONNX Runtime shared library, checked against a pinned SHA-256 | archive request; no body | first use of a local embeddings model | `embeddings_model` (only a local model triggers the download) | Local-embeddings builds only |
| The vendor of each harness CLI (never canopy's servers) | every harness CLI canopy launches (claude, codex, and the rest) | the agent's own work | the prompt and arguments you or the graph supplied | whenever an agent runs | no switch | Whenever an agent runs |

## What the relay receives

On connect the host sends its GitHub OAuth token as an HTTP Authorization Bearer header, together with its host id in X-Canopy-Host-Id and its label in X-Canopy-Host-Label (`request_for` in `src/remote/relay.rs`). TURN credentials come from a GET to `<relay_url>/turn` authenticated with the same token as a bearer (`fetch_ice_servers` in `src/remote/peer_ice.rs`). Signalling travels as JSON envelopes, relay to host `{from, body}` and host to relay `{to, body}` (module docs in `src/remote/relay.rs`). Local ICE candidates are forwarded to the other side verbatim, and the host answers only after gathering, so the answer SDP carries the host candidates, which include the host's LAN addresses, and the server-reflexive candidates collected in that window (`on_ice_candidate` and `answer_with_gathered_sdp` in `src/remote/peer.rs`; host and server-reflexive candidates land inside the gather grace in `ANSWER_GATHER_GRACE`). Once the data channel labelled `canopy-session-v1` is up, session bytes travel over the direct SCTP data channel authenticated end to end by the DTLS fingerprint inside the signed SDP; the relay only ever forwards signalling envelopes, never session content (module docs and `CHANNEL_LABEL` in `src/remote/peer.rs`).

## Connections with no switch

These rows have no setting and no command that turns them off:

- `https://raw.githubusercontent.com/UniverLab/canopy-registry/main/`
- The vendor of each harness CLI (never canopy's servers)
