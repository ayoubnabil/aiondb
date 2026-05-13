use super::*;

const MAX_PASSWORD_RESPONSE_BYTES: usize = 64 * 1024;

impl<E, R, W> Connection<E, R, W>
where
    E: PgWireEngine + 'static,
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    pub(super) async fn handle_startup(&mut self) -> Result<(), DbError> {
        if self.startup_deadline.is_none() {
            self.startup_deadline =
                checked_deadline_after(self.startup_timeout, "connection startup timeout");
        }

        if let Some(deadline) = self.startup_deadline {
            match tokio::time::timeout_at(deadline, self.handle_startup_inner()).await {
                Ok(result) => result,
                Err(_) => {
                    let error = self.startup_timeout_error();
                    self.record_startup_failure_metrics(&error);
                    Err(error)
                }
            }
        } else {
            self.handle_startup_inner().await
        }
    }

    fn startup_timeout_error(&self) -> DbError {
        if self.startup_timeout.is_zero() {
            DbError::protocol("startup timeout exceeded")
        } else {
            DbError::protocol(format!(
                "startup timeout exceeded after {} ms",
                self.startup_timeout.as_millis()
            ))
        }
    }

    pub(super) async fn read_frontend_message_during_startup(
        &mut self,
    ) -> Result<codec::RawFrontendMessage, DbError> {
        if let Some(deadline) = self.startup_deadline {
            match tokio::time::timeout_at(deadline, codec::read_frontend_message(&mut self.reader))
                .await
            {
                Ok(result) => result,
                Err(_) => Err(self.startup_timeout_error()),
            }
        } else {
            codec::read_frontend_message(&mut self.reader).await
        }
    }

    async fn handle_startup_inner(&mut self) -> Result<(), DbError> {
        loop {
            let startup_result = codec::read_startup(&mut self.reader).await;
            let payload = match startup_result {
                Ok(payload) => payload,
                Err(error) => {
                    self.record_startup_failure_metrics(&error);
                    self.write_startup_error_response(&error).await?;
                    return Err(error);
                }
            };
            match payload {
                StartupPayload::SslRequest => {
                    self.writer
                        .write_all(b"N")
                        .await
                        .map_err(|e| DbError::protocol(format!("write SSL decline: {e}")))?;
                    self.writer
                        .flush()
                        .await
                        .map_err(|e| DbError::protocol(format!("flush SSL decline: {e}")))?;
                }
                StartupPayload::CancelRequest(target_pid, target_key) => {
                    if let Some(session) = self.cancel_registry.lookup(target_pid, target_key) {
                        debug!(
                            pid = self.pid,
                            target_pid, "forwarding cancel request to session"
                        );
                        if let Err(error) = self
                            .run_engine(move |engine| engine.cancel_session(&session))
                            .await
                        {
                            warn!(
                                pid = self.pid,
                                target_pid,
                                error = %error,
                                "cancel request forwarding failed"
                            );
                        }
                    } else {
                        debug!(
                            pid = self.pid,
                            target_pid, "cancel request for unknown pid/key pair"
                        );
                    }
                    return Ok(());
                }
                StartupPayload::Startup(params) => {
                    let user = params.get("user").cloned().unwrap_or_default();
                    if user.trim().is_empty() {
                        let error = DbError::invalid_authorization(
                            "startup parameter \"user\" must not be empty",
                        );
                        self.record_startup_failure_metrics(&error);
                        self.write_startup_error_response(&error).await?;
                        return Err(error);
                    }
                    const MAX_STARTUP_PARAMS: usize = 32;
                    const MAX_STARTUP_PARAM_NAME_LEN: usize = 64;
                    const MAX_STARTUP_PARAM_LEN: usize = 256;
                    const MAX_STARTUP_OPTIONS_LEN: usize = 1024;
                    if params.len() > MAX_STARTUP_PARAMS {
                        let error = DbError::protocol(format!(
                            "too many startup parameters ({}, maximum is {MAX_STARTUP_PARAMS})",
                            params.len()
                        ));
                        self.record_startup_failure_metrics(&error);
                        self.write_startup_error_response(&error).await?;
                        return Err(error);
                    }
                    for (name, value) in &params {
                        if name.len() > MAX_STARTUP_PARAM_NAME_LEN {
                            let error = DbError::protocol(format!(
                                "startup parameter name exceeds maximum length of {MAX_STARTUP_PARAM_NAME_LEN} bytes"
                            ));
                            self.record_startup_failure_metrics(&error);
                            self.write_startup_error_response(&error).await?;
                            return Err(error);
                        }
                        let max_len = if name == "options" {
                            MAX_STARTUP_OPTIONS_LEN
                        } else {
                            MAX_STARTUP_PARAM_LEN
                        };
                        if value.len() > max_len {
                            let error = DbError::protocol(format!(
                                "startup parameter \"{name}\" exceeds maximum length of {max_len} bytes"
                            ));
                            self.record_startup_failure_metrics(&error);
                            self.write_startup_error_response(&error).await?;
                            return Err(error);
                        }
                    }
                    let database = params
                        .get("database")
                        .cloned()
                        .unwrap_or_else(|| aiondb_core::COMPAT_DEFAULT_DATABASE_NAME.to_owned());
                    if database.trim().is_empty() {
                        let error = DbError::parse_error(
                            SqlState::InvalidCatalogName,
                            "startup parameter \"database\" must not be empty",
                        );
                        self.record_startup_failure_metrics(&error);
                        self.write_startup_error_response(&error).await?;
                        return Err(error);
                    }
                    let application_name = params.get("application_name").cloned();
                    let replication_mode = match startup_requests_replication(&params) {
                        Ok(replication_mode) => replication_mode,
                        Err(error) => {
                            self.record_startup_failure_metrics(&error);
                            self.write_startup_error_response(&error).await?;
                            return Err(error);
                        }
                    };
                    let transport = if self.peer_addr.is_some() {
                        TransportInfo {
                            kind: TransportKind::Network {
                                tls: self.tls,
                                peer_addr: self.peer_addr.clone(),
                            },
                        }
                    } else {
                        TransportInfo::in_process()
                    };
                    if let Err(error) = self
                        .run_engine({
                            let principal = user.clone();
                            let transport = transport.clone();
                            move |engine| engine.startup_rate_limit_check(&principal, &transport)
                        })
                        .await
                    {
                        self.record_startup_failure_metrics(&error);
                        self.write_startup_error_response(&error).await?;
                        return Err(error);
                    }
                    let startup_auth = match self
                        .run_engine({
                            let user = user.clone();
                            let database = database.clone();
                            let transport = transport.clone();
                            move |engine| {
                                engine.startup_authentication(&user, &database, &transport)
                            }
                        })
                        .await
                    {
                        Ok(auth) => auth,
                        Err(error) => {
                            self.record_startup_failure_metrics(&error);
                            self.write_startup_error_response(&error).await?;
                            return Err(error);
                        }
                    };
                    let credential = match startup_auth {
                        StartupAuthentication::Trust => {
                            Credential::Anonymous { user: user.clone() }
                        }
                        StartupAuthentication::CleartextPassword => {
                            if matches!(transport.kind, TransportKind::Network { .. }) && !self.tls
                            {
                                let error = DbError::invalid_authorization(
                                    "cleartext password authentication requires a TLS connection",
                                );
                                self.record_startup_failure_metrics(&error);
                                self.write_startup_error_response(&error).await?;
                                return Err(error);
                            }
                            let mut w = MessageWriter::new();
                            messages::write_auth_cleartext_password(&mut w);
                            w.flush(&mut self.writer).await?;

                            let raw = match self.read_frontend_message_during_startup().await {
                                Ok(raw) => raw,
                                Err(error) => {
                                    self.record_startup_failure_metrics(&error);
                                    self.write_startup_error_response(&error).await?;
                                    return Err(error);
                                }
                            };
                            if raw.payload.len() > MAX_PASSWORD_RESPONSE_BYTES {
                                let error = DbError::protocol(
                                    "password response exceeds maximum permitted size",
                                );
                                self.record_startup_failure_metrics(&error);
                                self.write_startup_error_response(&error).await?;
                                return Err(error);
                            }
                            match FrontendMessage::parse(raw.tag, raw.payload) {
                                Ok(FrontendMessage::Password(password)) => {
                                    Credential::CleartextPassword {
                                        user: user.clone(),
                                        password: SecretString::new(password),
                                    }
                                }
                                Ok(_) => {
                                    let error =
                                        DbError::protocol("expected password response from client");
                                    self.record_startup_failure_metrics(&error);
                                    self.write_startup_error_response(&error).await?;
                                    return Err(error);
                                }
                                Err(error) => {
                                    self.record_startup_failure_metrics(&error);
                                    self.write_startup_error_response(&error).await?;
                                    return Err(error);
                                }
                            }
                        }
                        StartupAuthentication::ScramSha256 {
                            verifier,
                            proof_token,
                        } => {
                            if let Err(error) = self.scram_authenticate(&verifier).await {
                                let rate_limit_error = self
                                    .run_engine({
                                        let principal = user.clone();
                                        let transport = transport.clone();
                                        move |engine| {
                                            engine.startup_rate_limit_record_failure(
                                                &principal, &transport,
                                            )
                                        }
                                    })
                                    .await
                                    .err();
                                let error = rate_limit_error.unwrap_or(error);
                                self.record_startup_failure_metrics(&error);
                                self.write_startup_error_response(&error).await?;
                                return Err(error);
                            }
                            Credential::Token {
                                user: user.clone(),
                                token: proof_token,
                            }
                        }
                    };

                    let replication_application_name = if replication_mode {
                        application_name
                            .clone()
                            .filter(|value| !value.trim().is_empty())
                    } else {
                        None
                    };
                    let startup_params = StartupParams {
                        database: database.clone(),
                        application_name,
                        options: params.into_iter().collect(),
                        credential,
                        transport,
                    };
                    let (session, info) = match self
                        .run_engine(move |engine| engine.startup(startup_params))
                        .await
                    {
                        Ok(started) => started,
                        Err(error) => {
                            self.record_startup_failure_metrics(&error);
                            self.write_startup_error_response(&error).await?;
                            return Err(error);
                        }
                    };

                    if replication_mode {
                        if let Err(error) = self
                            .run_engine({
                                let session = session.clone();
                                let info = info.clone();
                                move |engine| {
                                    engine.authorize_replication_connection(&session, &info)
                                }
                            })
                            .await
                        {
                            if let Err(terminate_error) = self
                                .run_engine({
                                    let session = session.clone();
                                    move |engine| engine.terminate(session)
                                })
                                .await
                            {
                                warn!(
                                    pid = self.pid,
                                    error = %terminate_error,
                                    "failed to terminate unauthorized replication session"
                                );
                            }
                            self.record_startup_failure_metrics(&error);
                            self.write_startup_error_response(&error).await?;
                            return Err(error);
                        }
                    }

                    self.cancel_registry
                        .register(self.pid, self.secret_key, session.clone());
                    self.session = Some(session);
                    self.replication_mode = replication_mode;
                    self.replication_database = if replication_mode {
                        Some(database)
                    } else {
                        None
                    };
                    self.replication_application_name = replication_application_name;

                    let mut w = MessageWriter::new();
                    messages::write_auth_ok(&mut w);
                    messages::write_parameter_status(
                        &mut w,
                        "server_version",
                        COMPAT_SERVER_VERSION,
                    );
                    let server_version_num = compat_server_version_num_string();
                    messages::write_parameter_status(
                        &mut w,
                        "server_version_num",
                        &server_version_num,
                    );
                    messages::write_parameter_status(
                        &mut w,
                        "server_encoding",
                        COMPAT_SERVER_ENCODING,
                    );
                    messages::write_parameter_status(
                        &mut w,
                        "client_encoding",
                        COMPAT_CLIENT_ENCODING,
                    );
                    messages::write_parameter_status(&mut w, "DateStyle", COMPAT_DATE_STYLE);
                    messages::write_parameter_status(
                        &mut w,
                        "integer_datetimes",
                        COMPAT_INTEGER_DATETIMES,
                    );
                    messages::write_parameter_status(
                        &mut w,
                        "standard_conforming_strings",
                        COMPAT_STANDARD_CONFORMING_STRINGS,
                    );
                    messages::write_parameter_status(
                        &mut w,
                        "IntervalStyle",
                        COMPAT_INTERVAL_STYLE,
                    );
                    let timezone = compat_timezone();
                    messages::write_parameter_status(&mut w, "TimeZone", &timezone);
                    messages::write_parameter_status(
                        &mut w,
                        "default_transaction_read_only",
                        COMPAT_DEFAULT_TRANSACTION_READ_ONLY,
                    );
                    messages::write_parameter_status(
                        &mut w,
                        "in_hot_standby",
                        COMPAT_DEFAULT_TRANSACTION_READ_ONLY,
                    );
                    messages::write_parameter_status(
                        &mut w,
                        "is_superuser",
                        if info.is_superuser { "on" } else { "off" },
                    );
                    messages::write_parameter_status(&mut w, "session_authorization", &user);
                    messages::write_parameter_status(&mut w, "application_name", "");
                    messages::write_backend_key_data(&mut w, self.pid, self.secret_key);
                    messages::write_ready_for_query(&mut w, TransactionStatus::Idle);
                    w.flush(&mut self.writer).await?;
                    self.record_startup_success_metrics();
                    return Ok(());
                }
            }
        }
    }

    async fn write_startup_error_response(&mut self, error: &DbError) -> Result<(), DbError> {
        if matches!(
            error.sqlstate(),
            SqlState::InvalidAuthorizationSpecification | SqlState::TooManyAuthenticationFailures
        ) && !self.auth_failure_backoff.is_zero()
        {
            tokio::time::sleep(self.auth_failure_backoff).await;
        }

        let mut w = MessageWriter::new();
        messages::write_error_response(&mut w, error);
        w.flush(&mut self.writer).await?;
        Ok(())
    }

    fn record_startup_success_metrics(&self) {
        if let Some(metrics) = &self.metrics {
            metrics.record_startup_success();
        }
    }

    fn record_startup_failure_metrics(&self, error: &DbError) {
        if let Some(metrics) = &self.metrics {
            metrics.record_startup_failure(error);
        }
    }
}

fn startup_requests_replication(
    params: &std::collections::BTreeMap<String, String>,
) -> Result<bool, DbError> {
    let Some(value) = params.get("replication") else {
        return Ok(false);
    };
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "on" | "yes" | "database" => Ok(true),
        "false" | "0" | "off" | "no" => Ok(false),
        _ => Err(DbError::parse_error(
            SqlState::InvalidParameterValue,
            "startup parameter \"replication\" has invalid value",
        )),
    }
}
