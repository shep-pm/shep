//! The run loop, and what a `Start` does.
//!
//! `run` drains the mailbox until a shutdown resolves, and `handle_command`
//! is the one dispatch point every command passes through. `do_start` is
//! here too, because starting a sheep is the path the rest of the actor is
//! shaped around.

use super::*;

impl<R: ProcessRunner> Actor<R> {
    /// Runs the actor to completion: every mailbox message until a `Shutdown`
    /// fully resolves. Dropping `rx` then closes the mailbox, so later
    /// [`SupervisorHandle`] calls see [`SupervisorError::EngineStopped`].
    pub(super) async fn run(mut self, mut rx: mpsc::Receiver<Msg>) {
        while let Some(msg) = rx.recv().await {
            let should_break = match msg {
                // Synchronous: nothing in the command path awaits, so the
                // actor cannot park on a busy sheep task.
                Msg::Command(cmd) => self.handle_command(cmd),
                Msg::Exited { id, outcome } => self.handle_exited(id, outcome),
                Msg::RestartDue { id, epoch } => {
                    self.handle_restart_due(id, epoch);
                    false
                }
                Msg::Ready { id } => {
                    self.handle_ready_signal(id);
                    false
                }
                Msg::ReloadDeadline { name, stamp } => {
                    self.handle_reload_deadline(&name, stamp);
                    false
                }
                Msg::ReloadVerified {
                    name,
                    new_id,
                    readiness,
                } => {
                    self.handle_reload_verified(&name, new_id, readiness);
                    false
                }
                Msg::ActionReply {
                    id,
                    action,
                    body,
                    stamp,
                } => {
                    self.handle_action_reply(id, &action, body, stamp);
                    false
                }
                Msg::ActionResult { id, stamp, outcome } => {
                    self.handle_action_result(id, stamp, outcome);
                    false
                }
                Msg::ReadyResult {
                    id,
                    epoch,
                    manually,
                    readiness,
                } => {
                    self.handle_ready_result(id, epoch, manually, readiness);
                    false
                }
            };
            if should_break {
                break;
            }
        }
    }

    pub(super) fn handle_command(&mut self, cmd: Command) -> bool {
        match cmd {
            // Rejected once shutdown has begun: it would spawn a child the
            // shutdown aggregation, fixed when it ran, cannot know to kill.
            Command::Start {
                apps,
                policy,
                gate,
                reply,
            } => {
                let result = if self.shutting_down {
                    Err(SupervisorError::EngineStopped)
                } else {
                    self.do_start(apps, None, policy, &gate)
                };
                let _ = reply.send(result);
                false
            }
            Command::RegisterAtRest { apps, reply } => {
                let registered = apps.iter().map(|app| self.register_at_rest(app)).collect();
                let _ = reply.send(Ok(registered));
                false
            }
            Command::ConfigDrift { apps, reply } => {
                let _ = reply.send(Ok(self.config_drift(&apps)));
                false
            }
            // Rejected while shutting down: a dog spawns. The check is inside
            // `do_start_dog`.
            Command::StartDog { app, source, reply } => {
                let result = self.do_start_dog(*app, source);
                let _ = reply.send(result);
                false
            }
            Command::Scale { name, count, reply } => {
                self.handle_scale(&name, count, reply);
                false
            }
            // Answered during a shutdown: `handle_apply_config` declines to
            // route the instance count while `shutting_down`.
            Command::ApplyConfig { apps, reset, reply } => {
                let report = self.handle_apply_config(apps, reset);
                let _ = reply.send(Ok(report));
                false
            }
            // All three are answered during a shutdown: one reads memory,
            // and the others park a config for a spawn a shutdown will not
            // reach.
            Command::SheepConfig { name, reply } => {
                let _ = reply.send(self.handle_sheep_config(&name));
                false
            }
            Command::SetSheepEnv {
                name,
                key,
                value,
                reply,
            } => {
                let _ = reply.send(self.handle_set_sheep_env(
                    &name,
                    &key,
                    value.as_ref().map(EnvValue::as_str),
                ));
                false
            }
            Command::SetSheepEnvBatch {
                name,
                entries,
                force,
                dry_run,
                reply,
            } => {
                let entries: BTreeMap<String, String> = entries
                    .into_iter()
                    .map(|(key, value)| (key, value.as_str().to_string()))
                    .collect();
                let _ =
                    reply.send(self.handle_set_sheep_env_batch(&name, &entries, force, dry_run));
                false
            }
            Command::SetSheepField {
                name,
                key,
                value,
                reply,
            } => {
                let _ = reply.send(self.handle_set_sheep_field(&name, &key, &value));
                false
            }
            Command::SetSmit {
                conn,
                sheep,
                smit,
                reply,
            } => {
                let _ = reply.send(self.handle_set_smit(conn, &sheep, smit));
                false
            }
            Command::ForgetSmits { conn, reply } => {
                self.smits.retain(|_, (painter, _)| *painter != conn);
                let _ = reply.send(());
                false
            }
            Command::List { reply } => {
                let _ = reply.send(self.snapshot_all());
                false
            }
            Command::Reopen { selector, reply } => {
                self.handle_reopen(&selector, reply);
                false
            }
            Command::Flush { selector, reply } => {
                self.handle_flush(&selector, reply);
                false
            }
            #[cfg(unix)]
            Command::HandoverSnapshot { fds, reply } => {
                self.handle_handover_snapshot(fds, reply);
                false
            }
            #[cfg(unix)]
            Command::HandoverFitness { reply } => {
                self.handle_handover_fitness(reply);
                false
            }
            Command::Trigger {
                selector,
                action,
                params,
                reply,
            } => {
                self.begin_action(&selector, action, params, reply);
                false
            }
            Command::Signal {
                selector,
                sig,
                reply,
            } => {
                self.begin_signal(&selector, sig, reply);
                false
            }
            Command::SendLine {
                selector,
                line,
                reply,
            } => {
                self.begin_send_line(&selector, line, reply);
                false
            }
            Command::Stop { selector, reply } => {
                self.begin_manual(
                    selector,
                    ManualKind::Stop,
                    CommandOrigin::Operator,
                    ReplyKind::Info(reply),
                );
                false
            }
            Command::Restart {
                selector,
                origin,
                reply,
            } => {
                if self.shutting_down {
                    send_reply(ReplyKind::Info(reply), Err(SupervisorError::EngineStopped));
                } else {
                    self.begin_manual(
                        selector,
                        ManualKind::Restart,
                        origin,
                        ReplyKind::Info(reply),
                    );
                }
                false
            }
            Command::ExtraRestart {
                id,
                pid,
                epoch,
                observed,
            } => {
                self.handle_extra_restart(id, pid, epoch, observed);
                false
            }
            Command::Reload { selector, reply } => {
                if self.shutting_down {
                    let _ = reply.send(Err(SupervisorError::EngineStopped));
                } else {
                    self.handle_reload(&selector, reply);
                }
                false
            }
            Command::Delete { selector, reply } => {
                self.begin_manual(
                    selector,
                    ManualKind::Delete,
                    CommandOrigin::Operator,
                    ReplyKind::Ids(reply),
                );
                false
            }
            Command::Shutdown { reply } => self.begin_shutdown(reply),
        }
    }

    /// Registers + spawns one dog, or reports the one already registered
    /// under that name.
    ///
    /// The name lookup reads names rather than markers: what it rules out is
    /// two live processes under one name, dog or not.
    pub(super) fn do_start_dog(
        &mut self,
        app: ResolvedApp,
        source: DogSource,
    ) -> Result<ProcessInfo, SupervisorError> {
        if self.shutting_down {
            return Err(SupervisorError::EngineStopped);
        }
        if let Some(slot) = self
            .sheep
            .values()
            .find(|slot| slot.entry.spec.config().name == app.config().name)
        {
            return Ok(to_info(&slot.entry, &self.smits));
        }
        // `PerApp`: a dog that cannot start must land in the dogs table as
        // `Errored`, which `dogs::spawn_dog_watch` subscribes to.
        let started = self.do_start(
            vec![app],
            Some(source),
            BatchPolicy::PerApp,
            &BTreeSet::new(),
        )?;
        started
            .into_iter()
            .next()
            .ok_or_else(|| SupervisorError::SpawnFailed("the dog registered no instance".into()))
    }

    /// Expands each app through `instance_slots` + `assemble`, spawning one
    /// instance per slot, after checking every app in the batch.
    ///
    /// Under [`BatchPolicy::AllOrNothing`] nothing is registered if any app
    /// fails that check, and the error names every one that did. A spawn that
    /// fails anyway still leaves the batch part-registered: only exec knows
    /// for certain.
    ///
    /// `dog` is written onto every entry this registers, and is `None` for
    /// every caller but [`Self::do_start_dog`]; see [`ProcessEntry::dog`].
    pub(super) fn do_start(
        &mut self,
        apps: Vec<ResolvedApp>,
        dog: Option<DogSource>,
        policy: BatchPolicy,
        gate: &BTreeSet<String>,
    ) -> Result<Vec<ProcessInfo>, SupervisorError> {
        // One sequence rather than two: a zip against `apps` misaligns the
        // moment a failure is skipped.
        let mut ready: Vec<(ResolvedApp, Option<Credentials>)> = Vec::with_capacity(apps.len());
        // `AllOrNothing` only: a non-empty `refusals` returns before anything
        // is registered.
        let mut refusals = Vec::new();
        // `PerApp` only, joined into one error at the end.
        let mut failures = Vec::new();
        let total = apps.len();
        for app in apps {
            let name = app.config().name.clone();
            // Once per app: every instance shares one identity, and it is
            // ahead of the registering loop so a lookup failing on the fourth
            // app leaves none of the first three registered.
            let credentials = match privilege::resolve(app.config()) {
                Ok(resolved) => resolved,
                Err(err) => {
                    // One refusal per app, not one per failed check: the
                    // summary below counts apps.
                    match policy {
                        // No row: one `Errored` row alone is the half-state
                        // this policy prevents.
                        BatchPolicy::AllOrNothing => refusals.push(format!("{name}: {err}")),
                        // A row, since the rest of the batch goes on. Safe
                        // only because it carries `SpawnIdentity::Unresolved`,
                        // so a later restart resolves through
                        // `credentials_for_spawn` and meets this same refusal.
                        BatchPolicy::PerApp => {
                            failures.push(format!("{name}: {err}"));
                            // `Fresh` only: the emit announces a transition,
                            // and a repeat restore transitions nothing.
                            if let Registration::Fresh(info) = self.register_without_spawning(
                                &app,
                                ProcStatus::Errored,
                                dog.clone(),
                            ) {
                                self.emit(ProcessEventKind::Errored, info, true);
                            }
                        }
                    }
                    continue;
                }
            };
            // Instance 0 and no credentials: neither changes which file exec
            // names, which is all `preflight` reads.
            let view = self.secret_view(&app);
            let described = match assemble(&app, 0, &self.paths, None, &view) {
                Ok(spec) => spec,
                // A reference nobody has set is as knowable here as a missing
                // binary is, and here is the last point at which refusing
                // costs nothing: no app in the batch is registered yet.
                Err(err) if !err.is_retriable() => match policy {
                    BatchPolicy::AllOrNothing => {
                        refusals.push(format!("{name}: {err}"));
                        continue;
                    }
                    // Reported, never refused: the spawn below meets the same
                    // error and leaves an `Errored` row an operator can read.
                    BatchPolicy::PerApp => {
                        tracing::warn!(sheep = %name, "{err}");
                        describe(&app, 0, &self.paths, None, &view)
                    }
                },
                // Decided nowhere but at the spawn: a provider dog that is
                // merely late is what `spawn_fresh` routes to the ordinary
                // backoff, per instance.
                Err(_) => describe(&app, 0, &self.paths, None, &view),
            };
            match self.runner.preflight(&described) {
                Preflight::Unknown => {}
                Preflight::Impossible(reason) if policy == BatchPolicy::AllOrNothing => {
                    refusals.push(format!("{name}: {reason}"));
                    continue;
                }
                // Reported, never refused: the spawn fails on its own and
                // names the same program.
                Preflight::Impossible(reason) | Preflight::Doubtful(reason) => {
                    tracing::warn!(sheep = %name, "{reason}");
                }
            }
            ready.push((app, credentials));
        }
        if !refusals.is_empty() {
            return Err(SupervisorError::CannotStart(format!(
                "nothing in this batch was registered; {} of {} apps cannot start: {}",
                refusals.len(),
                total,
                refusals.join("; "),
            )));
        }

        let mut results = Vec::new();
        'apps: for (app, credentials) in ready {
            let name = app.config().name.clone();
            let mut existing: Vec<u32> = self
                .sheep
                .values()
                .filter(|slot| slot.entry.spec.config().name == name)
                .map(|slot| slot.entry.instance)
                .collect();
            existing.sort_unstable();
            let slots = instance_slots(&existing, app.config().instances);

            for instance in slots {
                match self.spawn_fresh(&app, instance, credentials, dog.clone(), gate) {
                    Ok(info) => results.push(info),
                    Err(message) => {
                        let failure = format!("{name}: {message}");
                        match policy {
                            BatchPolicy::AllOrNothing => {
                                return Err(SupervisorError::SpawnFailed(failure));
                            }
                            // Every remaining app still gets its turn.
                            // `spawn_fresh` registered this one `Errored`.
                            BatchPolicy::PerApp => {
                                failures.push(failure);
                                // The rest of this app's instances share its
                                // binary and cwd, and would fail the same.
                                continue 'apps;
                            }
                        }
                    }
                }
            }
        }
        // An `Err` rather than a partial `Ok`, so `snapshot::muster`'s log and
        // `do_start_dog`'s refusal still fire.
        if !failures.is_empty() {
            return Err(SupervisorError::SpawnFailed(failures.join("; ")));
        }
        // Built so far in the caller's order, which is the Flockfile's rather
        // than the one every other listing takes.
        sort_flock(&mut results);
        Ok(results)
    }
}
