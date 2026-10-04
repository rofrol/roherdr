use super::*;

const MAX_QUEUED_NOTIFICATIONS: usize = 8;
const COMPLETION_EVIDENCE_GRACE: std::time::Duration = std::time::Duration::from_secs(1);
pub(super) const COMPLETION_RECHECK_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(50);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NotificationValidation {
    Current,
    AwaitingSnapshot,
    Stale,
}

fn notification_duration(kind: SemanticNotificationKind) -> std::time::Duration {
    std::time::Duration::from_secs(match kind {
        SemanticNotificationKind::NeedsAttention => 12,
        SemanticNotificationKind::Finished => 8,
        SemanticNotificationKind::UpdateInstalled => 5,
        SemanticNotificationKind::Custom => 8,
    })
}

impl ClientShellState {
    pub(super) fn retire_endpoint_notifications(&mut self, endpoint_id: &ClientEndpointId) {
        self.pending_notifications
            .retain(|pending| &pending.endpoint_id != endpoint_id);
        self.queued_notifications
            .retain(|queued| &queued.endpoint_id != endpoint_id);
        if self
            .visible_notification
            .as_ref()
            .is_some_and(|visible| &visible.endpoint_id == endpoint_id)
        {
            self.visible_notification = None;
            self.promote_queued_notification(std::time::Instant::now());
        }
    }

    fn queue_visible_notification(
        &mut self,
        mut notification: ClientVisibleNotification,
        now: std::time::Instant,
    ) {
        if self.visible_notification.is_none() {
            notification.deadline = now + notification_duration(notification.event.kind);
            self.visible_notification = Some(notification);
            return;
        }
        if self.queued_notifications.len() == MAX_QUEUED_NOTIFICATIONS {
            self.queued_notifications.pop_front();
        }
        self.queued_notifications.push_back(notification);
    }

    fn promote_queued_notification(&mut self, now: std::time::Instant) -> bool {
        let Some(mut notification) = self.queued_notifications.pop_front() else {
            return false;
        };
        notification.deadline = now + notification_duration(notification.event.kind);
        self.visible_notification = Some(notification);
        true
    }

    pub(super) fn focus_visible_notification(&mut self, outcome: &mut ClientShellInput) {
        let Some(notification) = self.visible_notification.as_ref() else {
            return;
        };
        if notification.event.pane_id.is_some()
            && !self.endpoint_is_online(&notification.endpoint_id)
        {
            let label = self.endpoint_label(&notification.endpoint_id).to_owned();
            self.receive_endpoint_unavailable(format!("{label} is unavailable"));
            outcome.repaint = true;
            return;
        }
        let notification = self
            .visible_notification
            .take()
            .expect("checked visible notification");
        self.promote_queued_notification(std::time::Instant::now());
        outcome.repaint = true;
        let Some(pane_id) = notification.event.pane_id else {
            return;
        };
        if notification.endpoint_id == self.active_endpoint_id {
            self.push_endpoint_method(
                crate::api::schema::Method::PaneFocus(crate::api::schema::PaneTarget { pane_id }),
                outcome,
            );
        } else if self.endpoint_is_online(&notification.endpoint_id) {
            outcome.actions.push(ClientShellAction::ActivateEndpoint {
                endpoint_id: notification.endpoint_id,
                target: Some(ClientEndpointFocusTarget::Pane(pane_id)),
            });
        }
    }

    pub(crate) fn receive_notification(
        &mut self,
        endpoint_id: &ClientEndpointId,
        event: SemanticNotification,
        now: std::time::Instant,
    ) -> (Vec<ClientShellNotificationEffect>, bool) {
        if endpoint_id == &self.active_endpoint_id {
            self.notification_log_received(event.tab_id.as_deref());
        }
        let delay = if event.kind == SemanticNotificationKind::Custom {
            0
        } else {
            self.config.toast_delay_seconds
        };
        let deadline = now
            .checked_add(std::time::Duration::from_secs(delay))
            .unwrap_or(now);
        let cleared_visible = event.pane_id.as_deref().is_some_and(|pane_id| {
            self.visible_notification.as_ref().is_some_and(|visible| {
                visible.endpoint_id == *endpoint_id
                    && visible.event.pane_id.as_deref() == Some(pane_id)
            })
        });
        if let Some(pane_id) = event.pane_id.as_deref() {
            self.pending_notifications.retain(|pending| {
                pending.endpoint_id != *endpoint_id
                    || pending.event.pane_id.as_deref() != Some(pane_id)
            });
            self.queued_notifications.retain(|queued| {
                queued.endpoint_id != *endpoint_id
                    || queued.event.pane_id.as_deref() != Some(pane_id)
            });
            if cleared_visible {
                self.visible_notification = None;
                self.promote_queued_notification(now);
            }
        }
        // A finished turn asks nothing: by default it only leaves a quiet row
        // in the history list (the server records it) and the unread dot, with
        // no sound, toast or system notification.
        if event.kind == SemanticNotificationKind::Finished && !self.config.toast_alert_on_finished
        {
            return (Vec::new(), cleared_visible);
        }
        // Completion evidence is advisory. A Finished effect is valid only while the
        // client-projected pane remains Done, even when delivery is immediate.
        let validate_state = delay > 0 || event.kind == SemanticNotificationKind::Finished;
        self.pending_notifications.push(ClientPendingNotification {
            endpoint_id: endpoint_id.clone(),
            event,
            deadline,
            expires_at: now.checked_add(COMPLETION_EVIDENCE_GRACE).unwrap_or(now),
            validate_state,
        });
        let (effects, repaint) = self.tick_notifications(now);
        (effects, repaint || cleared_visible)
    }

    pub(crate) fn tick_notifications(
        &mut self,
        now: std::time::Instant,
    ) -> (Vec<ClientShellNotificationEffect>, bool) {
        let mut repaint = false;
        if self
            .visible_notification
            .as_ref()
            .is_some_and(|visible| now >= visible.deadline)
        {
            self.visible_notification = None;
            self.promote_queued_notification(now);
            repaint = true;
        }
        if self
            .visible_endpoint_notice
            .as_ref()
            .is_some_and(|visible| now >= visible.deadline)
        {
            self.visible_endpoint_notice = None;
            repaint = true;
        }

        let pending = std::mem::take(&mut self.pending_notifications);
        let mut effects = Vec::new();
        for pending in pending {
            if pending.deadline > now {
                self.pending_notifications.push(pending);
                continue;
            }
            if pending.validate_state {
                match self.notification_validation(&pending.endpoint_id, &pending.event) {
                    NotificationValidation::Current => {}
                    NotificationValidation::AwaitingSnapshot if now < pending.expires_at => {
                        let mut pending = pending;
                        pending.deadline = now
                            .checked_add(COMPLETION_RECHECK_INTERVAL)
                            .unwrap_or(now)
                            .min(pending.expires_at);
                        self.pending_notifications.push(pending);
                        continue;
                    }
                    NotificationValidation::AwaitingSnapshot | NotificationValidation::Stale => {
                        continue;
                    }
                }
            }
            let target_active =
                self.notification_target_is_active(&pending.endpoint_id, &pending.event);
            let suppress_external = target_active && self.outer_focused != Some(false);
            if let Some(sound) = pending.event.sound {
                let suppress_sound =
                    pending.event.kind == SemanticNotificationKind::Finished && suppress_external;
                if !suppress_sound {
                    effects.push(ClientShellNotificationEffect::Sound {
                        sound: match sound {
                            SemanticNotificationSound::Done => crate::sound::Sound::Done,
                            SemanticNotificationSound::Request => crate::sound::Sound::Request,
                        },
                        agent: pending.event.agent.clone(),
                    });
                }
            }

            // While the herdr window has focus, herdr's own toast shows
            // instead of the system one: the system toast would cover the
            // window being looked at. A terminal that does not report focus
            // keeps the system toast.
            let herdr_toast = match self.config.toast_delivery {
                crate::config::ToastDelivery::Herdr => true,
                crate::config::ToastDelivery::System => self.outer_focused == Some(true),
                _ => false,
            };
            match self.config.toast_delivery {
                crate::config::ToastDelivery::Off => {}
                _ if herdr_toast && !target_active => {
                    self.queue_visible_notification(
                        ClientVisibleNotification {
                            endpoint_id: pending.endpoint_id,
                            event: pending.event,
                            deadline: now,
                        },
                        now,
                    );
                    repaint = true;
                }
                _ if herdr_toast => {}
                crate::config::ToastDelivery::Terminal if !suppress_external => {
                    effects.push(ClientShellNotificationEffect::Terminal {
                        title: pending.event.title,
                        body: pending.event.body,
                    });
                }
                crate::config::ToastDelivery::System if !suppress_external => {
                    effects
                        .push(self.system_notification_effect(&pending.endpoint_id, pending.event));
                }
                crate::config::ToastDelivery::Herdr
                | crate::config::ToastDelivery::Terminal
                | crate::config::ToastDelivery::System => {}
            }
        }
        (effects, repaint)
    }

    #[cfg(windows)]
    pub(crate) fn notification_target_is_current(
        &self,
        endpoint_id: &ClientEndpointId,
        target: &ClientEndpointFocusTarget,
    ) -> bool {
        let ClientEndpointFocusTarget::Notification { pane_id, boot_id } = target else {
            return true;
        };
        self.endpoint_is_online(endpoint_id)
            && self
                .endpoints
                .iter()
                .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
                .and_then(|endpoint| endpoint.snapshot.as_deref())
                .is_some_and(|snapshot| {
                    snapshot.boot_id == *boot_id
                        && snapshot.panes.iter().any(|pane| pane.pane_id == *pane_id)
                })
    }

    #[cfg(windows)]
    pub(crate) fn activate_system_notification(
        &mut self,
        target: ClientSystemNotificationTarget,
    ) -> ClientShellInput {
        let focus = ClientEndpointFocusTarget::Notification {
            pane_id: target.pane_id,
            boot_id: target.boot_id,
        };
        let mut outcome = ClientShellInput::default();
        if self.notification_target_is_current(&target.endpoint_id, &focus) {
            outcome.actions.push(ClientShellAction::ActivateEndpoint {
                endpoint_id: target.endpoint_id,
                target: Some(focus),
            });
        }
        outcome
    }

    /// Agent notifications show the agent's task (its terminal title) as the
    /// message and the workspace/tab context as the subtitle, and focus the
    /// agent's pane on click when the endpoint is local.
    fn system_notification_effect(
        &self,
        endpoint_id: &ClientEndpointId,
        event: SemanticNotification,
    ) -> ClientShellNotificationEffect {
        let task = event.pane_id.as_deref().and_then(|pane_id| {
            let snapshot = self
                .endpoints
                .iter()
                .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
                .and_then(|endpoint| endpoint.snapshot.as_deref())?;
            let agent = snapshot
                .agents
                .iter()
                .find(|agent| agent.pane_id == pane_id)?;
            agent
                .task
                .as_deref()
                .or(agent.terminal_title_stripped.as_deref())
                .and_then(notification_detail_text)
        });
        let click_target = event
            .pane_id
            .clone()
            .filter(|_| endpoint_id.is_local())
            .map(|pane_id| ClientNotificationClickTarget {
                pane_id,
                tab_id: event.tab_id.clone(),
                workspace_id: event.workspace_id.clone(),
            });
        let (subtitle, body) = match task {
            Some(task) => (event.body, Some(task)),
            None => (None, event.body),
        };
        #[cfg(windows)]
        let target = event.pane_id.as_ref().and_then(|pane_id| {
            self.endpoint_boot_id(endpoint_id)
                .map(|boot_id| ClientSystemNotificationTarget {
                    endpoint_id: endpoint_id.clone(),
                    boot_id: boot_id.to_owned(),
                    pane_id: pane_id.clone(),
                })
        });
        ClientShellNotificationEffect::System {
            title: event.title,
            subtitle,
            body,
            click_target,
            #[cfg(windows)]
            target,
        }
    }

    fn notification_target_is_active(
        &self,
        endpoint_id: &ClientEndpointId,
        event: &SemanticNotification,
    ) -> bool {
        if endpoint_id != &self.active_endpoint_id {
            return false;
        }
        let Some(snapshot) = self
            .endpoints
            .iter()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
            .and_then(|endpoint| endpoint.snapshot.as_deref())
        else {
            return false;
        };
        if let Some(tab_id) = event.tab_id.as_deref() {
            return snapshot.focused_tab_id.as_deref() == Some(tab_id);
        }
        event.workspace_id.as_deref().is_some_and(|workspace_id| {
            snapshot.focused_workspace_id.as_deref() == Some(workspace_id)
        })
    }

    fn notification_validation(
        &self,
        endpoint_id: &ClientEndpointId,
        event: &SemanticNotification,
    ) -> NotificationValidation {
        // Custom notifications (the socket API) may target any pane, not only an
        // agent's, and carry no agent state to verify.
        if event.kind == SemanticNotificationKind::Custom {
            return NotificationValidation::Current;
        }
        let Some(pane_id) = event.pane_id.as_deref() else {
            // Finished notifications carry no independently trustworthy completion state. Without
            // a projected pane to verify as Done, do not emit completion chrome or sound.
            return if event.kind == SemanticNotificationKind::Finished {
                NotificationValidation::Stale
            } else {
                NotificationValidation::Current
            };
        };
        let Some(agent) = self
            .endpoints
            .iter()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
            .and_then(|endpoint| endpoint.snapshot.as_deref())
            .and_then(|snapshot| {
                snapshot
                    .agents
                    .iter()
                    .find(|agent| agent.pane_id == pane_id)
            })
        else {
            return NotificationValidation::AwaitingSnapshot;
        };
        match event.kind {
            SemanticNotificationKind::NeedsAttention
                if agent.agent_status == crate::api::schema::AgentStatus::Blocked =>
            {
                NotificationValidation::Current
            }
            SemanticNotificationKind::Finished
                if agent.agent_status == crate::api::schema::AgentStatus::Done =>
            {
                NotificationValidation::Current
            }
            SemanticNotificationKind::Finished
                if agent.agent_status == crate::api::schema::AgentStatus::Working =>
            {
                NotificationValidation::AwaitingSnapshot
            }
            SemanticNotificationKind::UpdateInstalled | SemanticNotificationKind::Custom => {
                NotificationValidation::Current
            }
            SemanticNotificationKind::NeedsAttention | SemanticNotificationKind::Finished => {
                NotificationValidation::Stale
            }
        }
    }
}

const MAX_NOTIFICATION_DETAIL_CHARS: usize = 160;

/// Single-line, bounded text for a notification line; `None` when blank.
pub(super) fn notification_detail_text(text: &str) -> Option<String> {
    let text = text
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect::<String>();
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return None;
    }
    if text.chars().count() <= MAX_NOTIFICATION_DETAIL_CHARS {
        return Some(text);
    }
    let mut truncated = text
        .chars()
        .take(MAX_NOTIFICATION_DETAIL_CHARS - 1)
        .collect::<String>();
    truncated.push('…');
    Some(truncated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_detail_text_is_single_line_and_bounded() {
        assert_eq!(notification_detail_text(" \t\n "), None);
        assert_eq!(
            notification_detail_text("a\x1b[31mb\nc").as_deref(),
            Some("a [31mb c")
        );
        let long = "x".repeat(MAX_NOTIFICATION_DETAIL_CHARS + 10);
        let text = notification_detail_text(&long).expect("text");
        assert_eq!(text.chars().count(), MAX_NOTIFICATION_DETAIL_CHARS);
        assert!(text.ends_with('…'));
    }
}
