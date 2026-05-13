use super::*;

impl<E, R, W> Connection<E, R, W>
where
    E: PgWireEngine + 'static,
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    pub(super) async fn handle_replication_query(&mut self, sql: &str) -> Result<(), DbError> {
        let Some(command) = ReplicationCommand::parse(sql) else {
            let error = DbError::protocol("invalid replication command");
            self.write_replication_error_and_ready(&error).await?;
            return Ok(());
        };

        match command {
            ReplicationCommand::IdentifySystem => self.handle_identify_system().await,
            ReplicationCommand::CreateReplicationSlot {
                slot_name,
                reserve_wal,
            } => {
                self.handle_create_replication_slot(&slot_name, reserve_wal)
                    .await
            }
            ReplicationCommand::ReadReplicationSlot { slot_name } => {
                self.handle_read_replication_slot(&slot_name).await
            }
            ReplicationCommand::StartReplication {
                slot_name,
                start_lsn,
                timeline,
            } => {
                self.handle_start_replication(slot_name.as_deref(), start_lsn, timeline)
                    .await
            }
            ReplicationCommand::TimelineHistory { timeline } => {
                self.handle_timeline_history(timeline).await
            }
            ReplicationCommand::BaseBackup => self.handle_base_backup().await,
        }
    }

    async fn handle_base_backup(&mut self) -> Result<(), DbError> {
        let Some(manager) = self.engine.replication_manager() else {
            let error = DbError::feature_not_supported(
                "BASE_BACKUP is unavailable: replication manager not exposed",
            );
            self.write_replication_error_and_ready(&error).await?;
            return Ok(());
        };
        let Some(identity) = self.engine.replication_identity() else {
            let error = DbError::feature_not_supported(
                "BASE_BACKUP is unavailable: replication identity not exposed",
            );
            self.write_replication_error_and_ready(&error).await?;
            return Ok(());
        };

        let state = manager.state();
        let wal_dir = state.wal_dir().to_path_buf();
        if !wal_dir.exists() {
            let error = DbError::internal(format!(
                "BASE_BACKUP cannot stream: wal_dir {} does not exist",
                wal_dir.display()
            ));
            self.write_replication_error_and_ready(&error).await?;
            return Ok(());
        }
        let wal_start_lsn = state.wal_notifier().current_lsn();
        let timeline = u32::try_from(state.timeline()).unwrap_or(1).max(1);
        let header = aiondb_replication::base_backup::BaseBackupHeader {
            wal_start_lsn,
            timeline,
            system_identifier: identity.system_identifier.clone(),
        };

        let mut w = MessageWriter::new();
        messages::write_copy_out_response(&mut w, 0)?;
        w.flush(&mut self.writer).await?;

        let header_frame = match aiondb_replication::base_backup::encode_header(&header) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.write_replication_error_and_ready(&error).await?;
                return Ok(());
            }
        };
        let mut w = MessageWriter::new();
        messages::write_copy_data(&mut w, &header_frame)?;
        w.flush(&mut self.writer).await?;

        let entries = match collect_base_backup_files(&wal_dir).await {
            Ok(entries) => entries,
            Err(error) => {
                self.write_replication_error_and_ready(&error).await?;
                return Ok(());
            }
        };
        for entry in entries {
            if let Err(error) = self.stream_base_backup_file(&wal_dir, &entry).await {
                self.write_replication_error_and_ready(&error).await?;
                return Ok(());
            }
        }

        let end_frame = aiondb_replication::base_backup::encode_backup_end()?;
        let mut w = MessageWriter::new();
        messages::write_copy_data(&mut w, &end_frame)?;
        messages::write_copy_done(&mut w);
        messages::write_command_complete(&mut w, "BASE_BACKUP");
        messages::write_ready_for_query(&mut w, TransactionStatus::Idle);
        w.flush(&mut self.writer).await?;
        Ok(())
    }

    async fn stream_base_backup_file(
        &mut self,
        wal_dir: &std::path::Path,
        rel_path: &str,
    ) -> Result<(), DbError> {
        let full_path = wal_dir.join(rel_path);
        let metadata = tokio::fs::metadata(&full_path).await.map_err(|err| {
            DbError::internal(format!(
                "BASE_BACKUP cannot stat {}: {err}",
                full_path.display()
            ))
        })?;
        if !metadata.is_file() {
            return Ok(());
        }
        let size = metadata.len();
        let start = aiondb_replication::base_backup::encode_file_start(rel_path, size)?;
        let mut w = MessageWriter::new();
        messages::write_copy_data(&mut w, &start)?;
        w.flush(&mut self.writer).await?;

        let mut file = tokio::fs::File::open(&full_path).await.map_err(|err| {
            DbError::internal(format!(
                "BASE_BACKUP cannot open {}: {err}",
                full_path.display()
            ))
        })?;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = tokio::io::AsyncReadExt::read(&mut file, &mut buf)
                .await
                .map_err(|err| {
                    DbError::internal(format!(
                        "BASE_BACKUP read failed for {}: {err}",
                        full_path.display()
                    ))
                })?;
            if n == 0 {
                break;
            }
            let chunk = aiondb_replication::base_backup::encode_file_data(&buf[..n])?;
            let mut w = MessageWriter::new();
            messages::write_copy_data(&mut w, &chunk)?;
            w.flush(&mut self.writer).await?;
        }
        let end = aiondb_replication::base_backup::encode_file_end()?;
        let mut w = MessageWriter::new();
        messages::write_copy_data(&mut w, &end)?;
        w.flush(&mut self.writer).await?;
        Ok(())
    }

    async fn handle_identify_system(&mut self) -> Result<(), DbError> {
        let Some(identity) = self.engine.replication_identity() else {
            let error = DbError::feature_not_supported(
                "replication protocol is unavailable for this engine",
            );
            self.write_replication_error_and_ready(&error).await?;
            return Ok(());
        };
        let Some(manager) = self.engine.replication_manager() else {
            let error = DbError::feature_not_supported(
                "replication protocol is unavailable for this engine",
            );
            self.write_replication_error_and_ready(&error).await?;
            return Ok(());
        };

        let timeline = identity.timeline.to_string();
        let current_lsn = format_pg_lsn(manager.state().wal_notifier().current_lsn());
        let dbname = self.replication_database.as_deref().unwrap_or_default();
        let fields = [
            FieldDescription {
                name: "systemid".to_owned(),
                table_oid: 0,
                column_attr: 0,
                type_oid: 25,
                type_size: -1,
                type_modifier: -1,
                format_code: 0,
            },
            FieldDescription {
                name: "timeline".to_owned(),
                table_oid: 0,
                column_attr: 0,
                type_oid: 23,
                type_size: 4,
                type_modifier: -1,
                format_code: 0,
            },
            FieldDescription {
                name: "xlogpos".to_owned(),
                table_oid: 0,
                column_attr: 0,
                type_oid: 25,
                type_size: -1,
                type_modifier: -1,
                format_code: 0,
            },
            FieldDescription {
                name: "dbname".to_owned(),
                table_oid: 0,
                column_attr: 0,
                type_oid: 25,
                type_size: -1,
                type_modifier: -1,
                format_code: 0,
            },
        ];

        let mut w = MessageWriter::new();
        messages::write_row_description(&mut w, &fields)?;
        messages::write_data_row(
            &mut w,
            &[
                Some(identity.system_identifier.as_bytes()),
                Some(timeline.as_bytes()),
                Some(current_lsn.as_bytes()),
                if dbname.is_empty() {
                    None
                } else {
                    Some(dbname.as_bytes())
                },
            ],
        )?;
        messages::write_command_complete(&mut w, "IDENTIFY_SYSTEM");
        messages::write_ready_for_query(&mut w, TransactionStatus::Idle);
        w.flush(&mut self.writer).await?;
        Ok(())
    }

    async fn handle_timeline_history(&mut self, timeline: Option<u32>) -> Result<(), DbError> {
        let Some(identity) = self.engine.replication_identity() else {
            let error = DbError::feature_not_supported(
                "replication protocol is unavailable for this engine",
            );
            self.write_replication_error_and_ready(&error).await?;
            return Ok(());
        };

        let requested_timeline = timeline.unwrap_or(identity.timeline);
        let history = self
            .engine
            .replication_timeline_history(requested_timeline)?;
        let Some(history) = history else {
            let error = DbError::protocol(format!(
                "timeline history for timeline {requested_timeline} is unavailable"
            ));
            self.write_replication_error_and_ready(&error).await?;
            return Ok(());
        };

        let filename = format!("{requested_timeline:08X}.history");
        let fields = [
            FieldDescription {
                name: "filename".to_owned(),
                table_oid: 0,
                column_attr: 0,
                type_oid: 25,
                type_size: -1,
                type_modifier: -1,
                format_code: 0,
            },
            FieldDescription {
                name: "content".to_owned(),
                table_oid: 0,
                column_attr: 0,
                type_oid: 25,
                type_size: -1,
                type_modifier: -1,
                format_code: 0,
            },
        ];

        let mut w = MessageWriter::new();
        messages::write_row_description(&mut w, &fields)?;
        messages::write_data_row(
            &mut w,
            &[Some(filename.as_bytes()), Some(history.as_bytes())],
        )?;
        messages::write_command_complete(&mut w, "TIMELINE_HISTORY");
        messages::write_ready_for_query(&mut w, TransactionStatus::Idle);
        w.flush(&mut self.writer).await?;
        Ok(())
    }

    async fn handle_create_replication_slot(
        &mut self,
        slot_name: &str,
        reserve_wal: bool,
    ) -> Result<(), DbError> {
        let Some(manager) = self.engine.replication_manager() else {
            let error = DbError::feature_not_supported(
                "replication protocol is unavailable for this engine",
            );
            self.write_replication_error_and_ready(&error).await?;
            return Ok(());
        };

        let slot = match manager.create_physical_slot(slot_name, reserve_wal) {
            Ok(slot) => slot,
            Err(error) => {
                self.write_replication_error_and_ready(&error).await?;
                return Ok(());
            }
        };
        let consistent_point = format_pg_lsn(
            slot.restart_lsn
                .unwrap_or_else(|| manager.state().wal_notifier().current_lsn()),
        );

        let fields = [
            FieldDescription {
                name: "slot_name".to_owned(),
                table_oid: 0,
                column_attr: 0,
                type_oid: 25,
                type_size: -1,
                type_modifier: -1,
                format_code: 0,
            },
            FieldDescription {
                name: "consistent_point".to_owned(),
                table_oid: 0,
                column_attr: 0,
                type_oid: 25,
                type_size: -1,
                type_modifier: -1,
                format_code: 0,
            },
            FieldDescription {
                name: "snapshot_name".to_owned(),
                table_oid: 0,
                column_attr: 0,
                type_oid: 25,
                type_size: -1,
                type_modifier: -1,
                format_code: 0,
            },
            FieldDescription {
                name: "output_plugin".to_owned(),
                table_oid: 0,
                column_attr: 0,
                type_oid: 25,
                type_size: -1,
                type_modifier: -1,
                format_code: 0,
            },
        ];

        let mut w = MessageWriter::new();
        messages::write_row_description(&mut w, &fields)?;
        messages::write_data_row(
            &mut w,
            &[
                Some(slot.name.as_bytes()),
                Some(consistent_point.as_bytes()),
                None,
                None,
            ],
        )?;
        messages::write_command_complete(&mut w, "CREATE_REPLICATION_SLOT");
        messages::write_ready_for_query(&mut w, TransactionStatus::Idle);
        w.flush(&mut self.writer).await?;
        Ok(())
    }

    async fn handle_read_replication_slot(&mut self, slot_name: &str) -> Result<(), DbError> {
        let Some(manager) = self.engine.replication_manager() else {
            let error = DbError::feature_not_supported(
                "replication protocol is unavailable for this engine",
            );
            self.write_replication_error_and_ready(&error).await?;
            return Ok(());
        };

        let slot = match manager.read_physical_slot(slot_name) {
            Ok(slot) => slot,
            Err(error) => {
                self.write_replication_error_and_ready(&error).await?;
                return Ok(());
            }
        };

        let fields = [
            FieldDescription {
                name: "slot_type".to_owned(),
                table_oid: 0,
                column_attr: 0,
                type_oid: 25,
                type_size: -1,
                type_modifier: -1,
                format_code: 0,
            },
            FieldDescription {
                name: "restart_lsn".to_owned(),
                table_oid: 0,
                column_attr: 0,
                type_oid: 25,
                type_size: -1,
                type_modifier: -1,
                format_code: 0,
            },
            FieldDescription {
                name: "restart_tli".to_owned(),
                table_oid: 0,
                column_attr: 0,
                type_oid: 20,
                type_size: 8,
                type_modifier: -1,
                format_code: 0,
            },
        ];

        let mut w = MessageWriter::new();
        messages::write_row_description(&mut w, &fields)?;
        if let Some(slot) = slot {
            let restart_lsn = slot.restart_lsn.map(format_pg_lsn);
            let restart_tli = slot.restart_tli.to_string();
            messages::write_data_row(
                &mut w,
                &[
                    Some(b"physical"),
                    restart_lsn.as_deref().map(str::as_bytes),
                    Some(restart_tli.as_bytes()),
                ],
            )?;
        } else {
            messages::write_data_row(&mut w, &[None, None, None])?;
        }
        messages::write_command_complete(&mut w, "READ_REPLICATION_SLOT");
        messages::write_ready_for_query(&mut w, TransactionStatus::Idle);
        w.flush(&mut self.writer).await?;
        Ok(())
    }

    async fn handle_start_replication(
        &mut self,
        slot_name: Option<&str>,
        start_lsn: aiondb_wal::Lsn,
        timeline: Option<u32>,
    ) -> Result<(), DbError> {
        let Some(manager) = self.engine.replication_manager() else {
            let error = DbError::feature_not_supported(
                "replication protocol is unavailable for this engine",
            );
            self.write_replication_error_and_ready(&error).await?;
            return Ok(());
        };

        let Some(identity) = self.engine.replication_identity() else {
            let error = DbError::feature_not_supported(
                "replication identity is unavailable for this engine",
            );
            self.write_replication_error_and_ready(&error).await?;
            return Ok(());
        };

        let requested_timeline = timeline.unwrap_or(identity.timeline);
        if requested_timeline != identity.timeline {
            let error = DbError::feature_not_supported(format!(
                "requested timeline {} does not match current timeline {}; fetch TIMELINE_HISTORY and restart replication on the latest timeline",
                requested_timeline, identity.timeline
            ));
            self.write_replication_error_and_ready(&error).await?;
            return Ok(());
        }

        let (sender, replica_id) = match manager.create_wal_sender_for_slot(start_lsn, slot_name) {
            Ok(sender) => sender,
            Err(error) => {
                self.write_replication_error_and_ready(&error).await?;
                return Ok(());
            }
        };
        if let Some(application_name) = &self.replication_application_name {
            manager
                .state()
                .replica_registry()
                .set_application_name(replica_id, application_name.clone());
        }
        let handler = ReplicationStreamHandler::new(
            sender,
            Arc::clone(manager.state().replica_registry()),
            Arc::clone(manager.state().wal_notifier()),
        );

        let stream_result = self.run_replication_stream(handler).await;
        manager.disconnect_replica(replica_id);
        stream_result
    }

    async fn run_replication_stream(
        &mut self,
        mut handler: ReplicationStreamHandler,
    ) -> Result<(), DbError> {
        let mut w = MessageWriter::new();
        messages::write_copy_both_response(&mut w, 0)?;
        w.flush(&mut self.writer).await?;

        loop {
            if let Some(frame) = handler.poll_next_message()? {
                let mut w = MessageWriter::new();
                messages::write_copy_data(&mut w, &frame)?;
                w.flush(&mut self.writer).await?;
                continue;
            }

            tokio::select! {
                () = handler.wait_for_activity_or_keepalive() => {}
                read_result = codec::read_frontend_message(&mut self.reader) => match read_result {
                    Ok(raw) => {
                        let message = FrontendMessage::parse(raw.tag, raw.payload)?;
                        match message {
                            FrontendMessage::CopyData(payload) => handler.handle_copy_data(&payload)?,
                            FrontendMessage::Terminate => {
                                self.close_requested = true;
                                return Ok(());
                            }
                            FrontendMessage::CopyDone => {
                                return Ok(());
                            }
                            FrontendMessage::CopyFail(reason) => {
                                return Err(DbError::protocol(
                                    format!("replica cancelled replication stream: {reason}"),
                                ));
                            }
                            _ => {
                                return Err(DbError::protocol(
                                    "unexpected frontend message during replication stream",
                                ));
                            }
                        }
                    }
                    Err(_) => {
                        self.close_requested = true;
                        return Ok(());
                    }
                }
            }
        }
    }

    async fn write_replication_error_and_ready(&mut self, error: &DbError) -> Result<(), DbError> {
        let mut w = MessageWriter::new();
        messages::write_error_response(&mut w, error);
        messages::write_ready_for_query(&mut w, TransactionStatus::Idle);
        w.flush(&mut self.writer).await?;
        Ok(())
    }
}

fn format_pg_lsn(lsn: aiondb_wal::Lsn) -> String {
    let raw = lsn.get();
    let lower = u32::try_from(raw & u64::from(u32::MAX)).unwrap_or(u32::MAX);
    format!("{:X}/{:08X}", raw >> 32, lower)
}

async fn collect_base_backup_files(root: &std::path::Path) -> Result<Vec<String>, DbError> {
    let mut entries = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let mut read_dir = tokio::fs::read_dir(&dir).await.map_err(|err| {
            DbError::internal(format!(
                "BASE_BACKUP cannot enumerate {}: {err}",
                dir.display()
            ))
        })?;
        while let Some(entry) = read_dir.next_entry().await.map_err(|err| {
            DbError::internal(format!(
                "BASE_BACKUP enumerate failed for {}: {err}",
                dir.display()
            ))
        })? {
            let path = entry.path();
            let metadata = entry.metadata().await.map_err(|err| {
                DbError::internal(format!(
                    "BASE_BACKUP stat failed for {}: {err}",
                    path.display()
                ))
            })?;
            if metadata.is_dir() {
                stack.push(path);
                continue;
            }
            // Skip transient temp files left around by atomic writes.
            if let Some(ext) = path.extension().and_then(std::ffi::OsStr::to_str) {
                if ext == "tmp" {
                    continue;
                }
            }
            let rel = path.strip_prefix(root).map_err(|err| {
                DbError::internal(format!(
                    "BASE_BACKUP relative path failed for {}: {err}",
                    path.display()
                ))
            })?;
            entries.push(rel.to_string_lossy().into_owned());
        }
    }
    entries.sort();
    Ok(entries)
}
