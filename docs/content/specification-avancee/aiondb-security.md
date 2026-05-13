---
title: aiondb-security
order: 12
---

# aiondb-security

Authentication, authorization, and credential handling primitives. Defines the `Authenticator` and `Authorizer` traits, the `AuthenticatedIdentity` returned to the engine, SCRAM-SHA-256 verifier crypto, secret wrappers that zeroize on drop, and rate limiters used to back off failed authentication attempts.

## cargo

```toml
[dependencies]
aiondb-security = { path = "../aiondb-security" }
```

## modules

| module | purpose |
|---|---|
| `authn` | `Authenticator` trait, `Credential`, `TransportInfo`, `TransportKind`, `AuthenticatedIdentity`. |
| `authz` | `Authorizer` trait, `Action`, `AccessTarget`, `AccessRequest`. |
| `identity` | `IdentityExt` extension trait (`has_role`). |
| `policy` | Default trait implementations: `AllowAllAuthenticator`, `AllowAllAuthorizer`, `DenyAllAuthorizer`. |
| `rate_limit` | `AuthRateLimiter` trait and three backends: `NoopAuthRateLimiter`, `InMemoryAuthRateLimiter`, `FileBackedAuthRateLimiter`. |
| `scram` | `ScramVerifier` (RFC 5802 / 7677 server-side state). |
| `secrets` | `SecretString`, `SecretBytes` wrappers with redacted `Debug` and constant-time equality. |

## key types

- `Credential` - `Anonymous`, `CleartextPassword`, or `Token` form. `Debug` redacts the secret material.
- `TransportInfo` / `TransportKind` - describes whether the request arrived in-process or over the network, with TLS and optional peer address.
- `AuthenticatedIdentity` - principal returned on success. Carries `user`, `database_id`, and `roles`.
- `Authenticator` - trait `fn authenticate(&self, &Credential, db: &str, &TransportInfo) -> DbResult<AuthenticatedIdentity>`.
- `Action` - `Connect`, `Select`, `Insert`, `Update`, `Delete`, `Create`, `Drop`, `Alter`, `Execute`, `Usage`.
- `AccessTarget` - `Database`, `Schema`, `Relation`, `Index`, `Sequence` keyed by the matching id type from `aiondb-core`.
- `AccessRequest` - pair of `Action` plus optional `AccessTarget`.
- `Authorizer` - trait `fn authorize(&self, &AuthenticatedIdentity, &AccessRequest) -> DbResult<()>`.
- `AllowAllAuthenticator`, `AllowAllAuthorizer` - permissive defaults intended for tests.
- `DenyAllAuthorizer` - production-safe default that returns `insufficient_privilege` for every request.
- `AuthRateLimiter` - trait with `check`, `record_success`, `record_failure`. `InMemoryAuthRateLimiter` keys by principal and transport scope; `FileBackedAuthRateLimiter` persists state to a file.
- `ScramVerifier` - SCRAM-SHA-256 verifier with `iterations`, `salt`, `stored_key`, `server_key`. Built from a cleartext password via `from_password` or `from_password_with_salt`.
- `SecretString`, `SecretBytes` - secret wrappers; `Debug` prints `**redacted**`, equality is constant time, contents are zeroized on drop.
- `IdentityExt` - extension trait providing `has_role(&str) -> bool` on `AuthenticatedIdentity`.

## example

```rust
use aiondb_security::{
    AccessRequest, Action, AllowAllAuthenticator, Authenticator, Credential,
    DenyAllAuthorizer, Authorizer, IdentityExt, SecretString, TransportInfo,
};

let auth = AllowAllAuthenticator;
let cred = Credential::CleartextPassword {
    user: "alice".to_string(),
    password: SecretString::new("hunter2".to_string()),
};
let transport = TransportInfo::in_process();

let identity = auth
    .authenticate(&cred, "postgres", &transport)
    .expect("anonymous authenticator always succeeds");
assert_eq!(identity.user, "alice");
assert!(identity.has_role("alice"));

let authz = DenyAllAuthorizer;
let request = AccessRequest {
    action: Action::Select,
    target: None,
};
assert!(authz.authorize(&identity, &request).is_err());
```

## scram

```rust
use aiondb_security::ScramVerifier;

let verifier = ScramVerifier::from_password("hunter2").expect("derive verifier");
assert!(!verifier.salt.is_empty());
assert_eq!(verifier.stored_key.len(), 32);
assert_eq!(verifier.server_key.len(), 32);
```

## secrets

`SecretString` and `SecretBytes` never leak through `Debug` and clear their backing buffer on drop:

```rust
use aiondb_security::SecretString;

let s = SecretString::new("hunter2".to_string());
assert_eq!(format!("{s:?}"), "SecretString(**redacted**)");
assert_eq!(s, SecretString::new("hunter2".to_string()));
```
