use aiondb_config::env::{env_bool, env_optional_string, env_string, env_u16};
use aiondb_dashboard::{build_dashboard_engine, BootstrapAdmin, DashboardConfig, DashboardServer};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use tokio::sync::watch;
use tracing::{error, info, warn};

fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
}

fn bind_address_is_loopback(bind_address: &str) -> bool {
    bind_address.eq_ignore_ascii_case("localhost")
        || bind_address
            .parse::<std::net::IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false)
}

fn generate_bootstrap_password() -> Result<String, String> {
    let mut bytes = [0u8; 18];
    getrandom::fill(&mut bytes)
        .map_err(|error| format!("failed to generate bootstrap admin password: {error}"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn resolve_bootstrap_admin_from_values(
    config: &DashboardConfig,
    admin_user: Option<String>,
    admin_pass: Option<String>,
) -> Result<(BootstrapAdmin, bool), String> {
    if !bind_address_is_loopback(&config.bind_address) {
        return Err(
            "dashboard only supports loopback binds until transport encryption is configured; bind to 127.0.0.1/::1 and terminate TLS in front of it for remote access"
                .to_owned(),
        );
    }

    match (admin_user, admin_pass) {
        (Some(username), Some(password)) => Ok((BootstrapAdmin { username, password }, false)),
        (None, None) => {
            let password = generate_bootstrap_password()?;
            Ok((
                BootstrapAdmin {
                    username: "admin".to_owned(),
                    password,
                },
                true,
            ))
        }
        (Some(_), None) | (None, Some(_)) => {
            Err("set both AIONDB_ADMIN_USER and AIONDB_ADMIN_PASSWORD together".to_owned())
        }
    }
}

fn resolve_bootstrap_admin(config: &DashboardConfig) -> Result<(BootstrapAdmin, bool), String> {
    resolve_bootstrap_admin_from_values(
        config,
        env_optional_string("AIONDB_ADMIN_USER"),
        env_optional_string("AIONDB_ADMIN_PASSWORD"),
    )
}

fn emit_generated_bootstrap_credentials(user: &str, password: &str) {
    use std::io::{self, IsTerminal as _, Write as _};

    let mut stderr = io::stderr();
    if stderr.is_terminal() {
        let _ = writeln!(
            stderr,
            "AionDB dashboard bootstrap credentials\nusername: {user}\npassword: {password}"
        );
    } else {
        warn!(
            user = %user,
            "generated dashboard bootstrap credentials were not written because stderr is not a TTY; set AIONDB_ADMIN_USER/AIONDB_ADMIN_PASSWORD to use fixed credentials"
        );
    }
}

#[tokio::main]
async fn main() {
    init_tracing();

    let config = DashboardConfig {
        bind_address: env_string("AIONDB_DASHBOARD_BIND", "127.0.0.1"),
        port: env_u16("AIONDB_DASHBOARD_PORT", 8080),
        allow_unauthenticated_metrics_prometheus: env_bool(
            "AIONDB_DASHBOARD_PROMETHEUS_UNAUTHENTICATED",
            false,
        ),
        trust_proxy_tls_headers: env_bool("AIONDB_DASHBOARD_TRUST_PROXY_TLS_HEADERS", false),
        ..DashboardConfig::default()
    };

    let (admin, generated_password) = resolve_bootstrap_admin(&config).unwrap_or_else(|err| {
        error!(%err, "failed to resolve dashboard bootstrap credentials");
        std::process::exit(1);
    });

    let engine = build_dashboard_engine().unwrap_or_else(|err| {
        error!(%err, "failed to build dashboard engine");
        std::process::exit(1);
    });
    let server = DashboardServer::new(engine.clone(), config.clone());

    // Bootstrap: create the admin role so the user can log in.
    match server.bootstrap_admin(&admin) {
        Ok(()) => {}
        Err(err) => {
            // If the role already exists, that's fine - just warn.
            warn!(%err, "bootstrap admin (may already exist)");
        }
    }

    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    tokio::spawn(async move {
        if let Err(error) = tokio::signal::ctrl_c().await {
            error!(%error, "failed to listen for Ctrl+C signal");
            return;
        }
        let _ = shutdown_tx.send(true);
    });

    info!(
        address = %format!("{}:{}", config.bind_address, config.port),
        "dashboard ready"
    );
    if generated_password {
        info!(
            user = %admin.username,
            "generated local-only bootstrap admin credentials"
        );
        emit_generated_bootstrap_credentials(&admin.username, &admin.password);
    } else {
        info!(
            user = %admin.username,
            "using configured dashboard admin credentials"
        );
    }

    if let Err(error) = server.start(shutdown_rx).await {
        error!(%error, "dashboard server exited with error");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_detection_accepts_local_bindings() {
        assert!(bind_address_is_loopback("127.0.0.1"));
        assert!(bind_address_is_loopback("::1"));
        assert!(bind_address_is_loopback("localhost"));
        assert!(!bind_address_is_loopback("0.0.0.0"));
    }

    #[test]
    fn remote_bind_is_rejected_even_with_explicit_bootstrap_credentials() {
        let config = DashboardConfig {
            bind_address: "0.0.0.0".to_owned(),
            ..DashboardConfig::default()
        };
        let error = resolve_bootstrap_admin_from_values(
            &config,
            Some("admin".to_owned()),
            Some("s3cret".to_owned()),
        )
        .expect_err("remote bind must be rejected until transport is encrypted");
        assert!(error.contains("loopback"));
    }

    #[test]
    fn loopback_bind_generates_bootstrap_password() {
        let config = DashboardConfig::default();
        let (admin, generated) =
            resolve_bootstrap_admin_from_values(&config, None, None).expect("generated admin");
        assert!(generated);
        assert_eq!(admin.username, "admin");
        assert!(!admin.password.is_empty());
    }

    #[test]
    fn explicit_bootstrap_credentials_are_used_verbatim() {
        let config = DashboardConfig::default();
        let (admin, generated) = resolve_bootstrap_admin_from_values(
            &config,
            Some("root".to_owned()),
            Some("s3cret".to_owned()),
        )
        .expect("explicit admin");
        assert!(!generated);
        assert_eq!(admin.username, "root");
        assert_eq!(admin.password, "s3cret");
    }
}
