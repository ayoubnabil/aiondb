#![allow(clippy::match_same_arms, clippy::unused_self)]

use std::sync::Arc;

use aiondb_catalog::{CatalogReader, RoleDescriptor};
use aiondb_config::SecurityConfig;
use aiondb_core::{DatabaseId, DbError, DbResult, TxnId};
use aiondb_security::{
    crypto::{hmac_sha256, sha256},
    AuthenticatedIdentity, Authenticator, Credential, ScramVerifier, SecretBytes, TransportInfo,
    TransportKind,
};
use subtle::ConstantTimeEq;

use crate::engine::StartupAuthentication;

const SCRAM_PROOF_VERSION: &[u8] = b"aiondb-scram-proof-v1";
const SCRAM_PROOF_MAC_LEN: usize = 32;

#[derive(Debug)]
pub(crate) struct CatalogAuthPolicy {
    catalog: Arc<dyn CatalogReader>,
    security: SecurityConfig,
    token_secret: [u8; 32],
}

impl CatalogAuthPolicy {
    pub(crate) fn new(catalog: Arc<dyn CatalogReader>, security: SecurityConfig) -> DbResult<Self> {
        let mut token_secret = [0u8; 32];
        getrandom::fill(&mut token_secret).map_err(|error| {
            DbError::internal(format!(
                "failed to generate auth token secret securely: {error} — \
                 refusing to start with a predictable secret"
            ))
        })?;
        Ok(Self {
            catalog,
            security,
            token_secret,
        })
    }

    pub(crate) fn startup_authentication(
        &self,
        user: &str,
        database: &str,
        transport: &TransportInfo,
    ) -> DbResult<StartupAuthentication> {
        let role = self.lookup_role(user)?;
        match role {
            Some(role) => self.startup_authentication_for_role(&role, user, database, transport),
            None => self.startup_authentication_for_missing_role(user, database, transport),
        }
    }

    fn startup_authentication_for_missing_role(
        &self,
        user: &str,
        database: &str,
        transport: &TransportInfo,
    ) -> DbResult<StartupAuthentication> {
        if self.security.allow_ephemeral_users && matches!(transport.kind, TransportKind::InProcess)
        {
            return Ok(StartupAuthentication::Trust);
        }

        if matches!(transport.kind, TransportKind::Network { .. }) {
            return self.startup_authentication_network_decoy(user, database, transport);
        }

        Err(DbError::invalid_authorization(format!(
            "role \"{user}\" does not exist"
        )))
    }

    fn startup_authentication_for_role(
        &self,
        role: &RoleDescriptor,
        user: &str,
        database: &str,
        transport: &TransportInfo,
    ) -> DbResult<StartupAuthentication> {
        // For network transports, converge non-authenticable role states to a
        // decoy SCRAM challenge to avoid role-state enumeration.
        if matches!(transport.kind, TransportKind::Network { .. }) {
            if !role.login {
                return self.startup_authentication_network_decoy(user, database, transport);
            }
            match role.password_hash.as_deref() {
                Some(password_hash) => {
                    if let Ok(verifier) = ScramVerifier::from_password_hash_string(password_hash) {
                        return Ok(StartupAuthentication::ScramSha256 {
                            verifier,
                            proof_token: self.issue_scram_proof_token(
                                user,
                                database,
                                transport,
                                password_hash,
                            )?,
                        });
                    }
                    return self.startup_authentication_network_decoy(user, database, transport);
                }
                None => {
                    return self.startup_authentication_network_decoy(user, database, transport);
                }
            }
        }

        if !role.login {
            return Err(login_not_allowed());
        }

        match role.password_hash.as_deref() {
            Some(password_hash) => {
                if let Ok(verifier) = ScramVerifier::from_password_hash_string(password_hash) {
                    return Ok(StartupAuthentication::ScramSha256 {
                        verifier,
                        proof_token: self.issue_scram_proof_token(
                            user,
                            database,
                            transport,
                            password_hash,
                        )?,
                    });
                }
                Err(invalid_stored_password_hash())
            }
            None => match transport.kind {
                TransportKind::InProcess => Ok(StartupAuthentication::Trust),
                _ => Err(DbError::invalid_authorization(
                    "role does not have a password configured",
                )),
            },
        }
    }

    fn startup_authentication_network_decoy(
        &self,
        user: &str,
        database: &str,
        transport: &TransportInfo,
    ) -> DbResult<StartupAuthentication> {
        let verifier = ScramVerifier::from_password("aiondb-network-role-scram-decoy")?;
        let decoy_password_hash = verifier.to_password_hash_string();
        Ok(StartupAuthentication::ScramSha256 {
            verifier,
            proof_token: self.issue_scram_proof_token(
                user,
                database,
                transport,
                &decoy_password_hash,
            )?,
        })
    }

    fn authenticate_existing_role(
        &self,
        role: &RoleDescriptor,
        credential: &Credential,
        database: &str,
        transport: &TransportInfo,
    ) -> DbResult<AuthenticatedIdentity> {
        if !role.login {
            if matches!(transport.kind, TransportKind::Network { .. }) {
                return Err(invalid_password_auth());
            }
            return Err(login_not_allowed());
        }

        match role.password_hash.as_deref() {
            Some(password_hash) => self.authenticate_passworded_role(
                role,
                password_hash,
                credential,
                database,
                transport,
            ),
            None => match transport.kind {
                TransportKind::InProcess => match credential {
                    Credential::Token { .. } => Err(invalid_password_auth()),
                    _ => Ok(identity_for_role(role)),
                },
                _ => Err(invalid_password_auth()),
            },
        }
    }

    fn authenticate_passworded_role(
        &self,
        role: &RoleDescriptor,
        password_hash: &str,
        credential: &Credential,
        database: &str,
        transport: &TransportInfo,
    ) -> DbResult<AuthenticatedIdentity> {
        if let Ok(verifier) = ScramVerifier::from_password_hash_string(password_hash) {
            if self.security.require_tls_for_password
                && !transport.tls_enabled()
                && matches!(credential, Credential::CleartextPassword { .. })
            {
                return Err(tls_required_for_password_auth());
            }
            return match credential {
                Credential::CleartextPassword { password, .. } => {
                    if verifier.verify_password(password.as_str()) {
                        Ok(identity_for_role(role))
                    } else {
                        Err(invalid_password_auth())
                    }
                }
                Credential::Token { token, .. } => {
                    if self.verify_scram_proof_token(
                        credential.user(),
                        database,
                        transport,
                        password_hash,
                        token,
                    )? {
                        Ok(identity_for_role(role))
                    } else {
                        Err(invalid_password_auth())
                    }
                }
                Credential::Anonymous { .. } => Err(invalid_password_auth()),
                _ => Err(invalid_password_auth()),
            };
        }

        if matches!(transport.kind, TransportKind::Network { .. }) {
            return Err(invalid_password_auth());
        }

        // The stored password_hash is not a valid SCRAM verifier. Reject
        // authentication rather than falling back to a cleartext comparison,
        // which would allow passwords stored in plaintext to be used.
        Err(invalid_stored_password_hash())
    }

    fn authenticate_ephemeral_user(
        &self,
        credential: &Credential,
        transport: &TransportInfo,
    ) -> DbResult<AuthenticatedIdentity> {
        if !self.security.allow_ephemeral_users {
            if matches!(transport.kind, TransportKind::Network { .. }) {
                return match credential {
                    Credential::Anonymous { .. } => Err(DbError::invalid_authorization(
                        "anonymous authentication is not permitted over the network",
                    )),
                    _ => Err(invalid_password_auth()),
                };
            }
            return Err(DbError::invalid_authorization(format!(
                "role \"{}\" does not exist",
                credential.user()
            )));
        }

        match credential {
            Credential::Token { .. } => Err(invalid_password_auth()),
            _ if matches!(transport.kind, TransportKind::InProcess) => {
                Ok(identity_for_user(credential.user()))
            }
            // for ephemeral users -- only in-process connections (handled above)
            // are allowed without authentication.
            Credential::Anonymous { .. } => Err(DbError::invalid_authorization(
                "anonymous authentication is not permitted over the network",
            )),
            _ => Err(invalid_password_auth()),
        }
    }

    fn lookup_role(&self, user: &str) -> DbResult<Option<RoleDescriptor>> {
        self.catalog.get_role(TxnId::default(), user)
    }

    fn issue_scram_proof_token(
        &self,
        user: &str,
        database: &str,
        transport: &TransportInfo,
        password_hash: &str,
    ) -> DbResult<SecretBytes> {
        let mut payload = self.scram_proof_payload(user, database, transport, password_hash);
        let mac = hmac_sha256(&self.token_secret, &payload)?;
        payload.extend_from_slice(&mac);
        Ok(SecretBytes::new(payload))
    }

    fn verify_scram_proof_token(
        &self,
        user: &str,
        database: &str,
        transport: &TransportInfo,
        password_hash: &str,
        token: &SecretBytes,
    ) -> DbResult<bool> {
        let token = token.as_bytes();
        if token.len() < SCRAM_PROOF_MAC_LEN {
            return Ok(false);
        }

        let split_at = token.len() - SCRAM_PROOF_MAC_LEN;
        let (payload, mac) = token.split_at(split_at);
        let expected_payload = self.scram_proof_payload(user, database, transport, password_hash);
        if payload.ct_eq(&expected_payload).unwrap_u8() != 1 {
            return Ok(false);
        }

        let expected_mac = hmac_sha256(&self.token_secret, payload)?;
        Ok(expected_mac.ct_eq(mac).unwrap_u8() == 1)
    }

    fn scram_proof_payload(
        &self,
        user: &str,
        database: &str,
        transport: &TransportInfo,
        password_hash: &str,
    ) -> Vec<u8> {
        let capacity = SCRAM_PROOF_VERSION
            .len()
            .saturating_add(user.len())
            .saturating_add(database.len())
            .saturating_add(64);
        let mut payload = Vec::with_capacity(capacity);
        payload.extend_from_slice(SCRAM_PROOF_VERSION);
        payload.push(0);
        payload.extend_from_slice(user.as_bytes());
        payload.push(0);
        payload.extend_from_slice(database.as_bytes());
        payload.push(0);
        payload.push(transport_marker(transport));
        payload.push(u8::from(transport.tls_enabled()));
        payload.extend_from_slice(&sha256(password_hash.as_bytes()));
        payload
    }
}

impl Authenticator for CatalogAuthPolicy {
    fn authenticate(
        &self,
        credential: &Credential,
        database: &str,
        transport: &TransportInfo,
    ) -> DbResult<AuthenticatedIdentity> {
        let role = self.lookup_role(credential.user())?;
        match role {
            Some(role) => self.authenticate_existing_role(&role, credential, database, transport),
            None => self.authenticate_ephemeral_user(credential, transport),
        }
    }
}

fn identity_for_role(role: &RoleDescriptor) -> AuthenticatedIdentity {
    AuthenticatedIdentity {
        user: role.name.clone(),
        database_id: DatabaseId::new(0),
        roles: vec![role.name.clone()],
    }
}

fn identity_for_user(user: &str) -> AuthenticatedIdentity {
    AuthenticatedIdentity {
        user: user.to_owned(),
        database_id: DatabaseId::new(0),
        roles: vec![user.to_owned()],
    }
}

fn login_not_allowed() -> DbError {
    DbError::invalid_authorization("role is not permitted to log in")
}

fn invalid_password_auth() -> DbError {
    DbError::invalid_authorization("invalid user name or password")
}

fn invalid_stored_password_hash() -> DbError {
    DbError::invalid_authorization(
        "stored password format is invalid — re-set the password with ALTER ROLE",
    )
}

fn tls_required_for_password_auth() -> DbError {
    DbError::invalid_authorization("TLS is required for password authentication")
}

fn transport_marker(transport: &TransportInfo) -> u8 {
    match transport.kind {
        TransportKind::InProcess => 0,
        _ => 1,
    }
}
