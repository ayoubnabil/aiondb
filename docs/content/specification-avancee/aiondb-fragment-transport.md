---
title: aiondb-fragment-transport
order: 63
---

# aiondb-fragment-transport

Remote fragment execution transport. Provides client and server components that ship `PhysicalPlan` fragments between cluster nodes over framed TCP, with shared-secret authentication and optional rustls-based TLS. The wire protocol is a length-prefixed JSON envelope; the current `PROTOCOL_VERSION` is `1` and payloads are capped at 64 MiB.

## cargo

```toml
[dependencies]
aiondb-fragment-transport = { path = "../aiondb-fragment-transport" }
```

## modules

| module | purpose |
|---|---|
| `protocol` | wire types and codec: `FragmentRequest`, `FragmentResponse`, `CancelRequest`, `FragmentSnapshot`, `TransportEnvelope`, `TransportPayload`, version and message-type constants. |
| `auth` | `AuthToken`: shared-secret token, redacted in `Debug`. |
| `tls` | `TlsClientConfig`, `TlsServerConfig`: rustls connector and acceptor builders. |
| `client` | `FragmentClient`, `ConnectionPool`: pooled async client. |
| `server` | `FragmentServer`, `FragmentExecutor`: TCP listener executing fragments locally. |

## wire protocol

```text
msg_type (u8) | payload_len (u32 LE) | payload (JSON)
```

| message type | byte |
|---|---|
| `MSG_EXECUTE_FRAGMENT` | `0x01` |
| `MSG_CANCEL_FRAGMENT` | `0x02` |
| `MSG_FRAGMENT_RESULT` | `0x81` |
| `MSG_FRAGMENT_ERROR` | `0x82` |
| `MSG_CANCEL_ACK` | `0x83` |

Servers accept envelopes whose `version` is in `MIN_PROTOCOL_VERSION..=PROTOCOL_VERSION` (currently both are `1`). The coordinator embeds a random `cancel_key` in `FragmentRequest`; subsequent `CancelRequest` messages must replay it for the cancel to be honoured.

## key types

| type | role |
|---|---|
| `AuthToken` | shared-secret authentication token. |
| `FragmentRequest` | execute-fragment envelope: plan, txn id, isolation, resource caps, optional snapshot, optional shard id, deadline, cancel key. |
| `FragmentSnapshot` | serialised MVCC snapshot (`xmin`, `xmax`, `active`). |
| `CancelRequest` | cancel envelope carrying `request_id` and `cancel_key`. |
| `FragmentResponse` | `Success`, `Error`, or `CancelAck`. |
| `TransportEnvelope`, `TransportPayload` | top-level wrapper types. |
| `TlsClientConfig`, `TlsServerConfig` | PEM file paths for rustls. |
| `ConnectionPool` | per-host pool of idle TLS or plaintext connections. |
| `FragmentClient` | high-level client for sending requests and cancels. |
| `FragmentServer`, `FragmentExecutor` | accept loop and per-request executor trait. |

## status

The transport is in active development. The server enforces protocol-version bounds, payload-size caps, cancel-authorization keys, and a 30-second drain on shutdown. TLS is optional and configured via PEM files; when absent the connection runs over plain TCP and authentication is the only barrier.

## example

```rust
use aiondb_fragment_transport::{AuthToken, FragmentRequest};

let token = AuthToken::new("shared-secret-from-config");
token.require_non_empty().expect("auth token must be set");

// Building a real FragmentRequest requires a PhysicalPlan from
// aiondb-plan; clients normally construct these via the higher-level
// query coordinator rather than by hand. The struct fields are:
//
//   request_id, plan, txn_id, isolation, max_result_rows,
//   max_result_bytes, max_memory_bytes, max_temp_bytes,
//   snapshot, deadline_epoch_ms, shard_id, cancel_key
let _ = std::any::type_name::<FragmentRequest>();
```
