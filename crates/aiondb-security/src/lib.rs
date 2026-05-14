pub mod authn;
pub mod authz;
pub mod crypto;
pub mod identity;
// `policy` is not `pub`: only the curated `Allow*Authorizer` and `DenyAllAuthorizer`
// re-exports are part of the supported API. Hiding the module also keeps
// `AllowAllAuthenticator` (a "trust-everything" identity issuer kept for
// internal tests only) out of reach for downstream embedders.
mod policy;
pub mod rate_limit;
pub mod scram;
pub mod secrets;

pub use authn::{AuthenticatedIdentity, Authenticator, Credential, TransportInfo, TransportKind};
pub use authz::{AccessRequest, AccessTarget, Action, Authorizer};
pub use identity::IdentityExt;
pub use policy::{AllowAllAuthorizer, DenyAllAuthorizer};
pub use rate_limit::{
    AuthRateLimiter, FileBackedAuthRateLimiter, InMemoryAuthRateLimiter, NoopAuthRateLimiter,
};
pub use scram::ScramVerifier;
pub use secrets::{SecretBytes, SecretString};
