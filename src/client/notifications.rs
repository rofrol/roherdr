use std::io;

use tracing::{debug, warn};

use crate::protocol::NotifyKind;

use super::shell;

#[cfg(not(windows))]
use crate::platform::show_desktop_notification as show_untargeted_system_notification;

#[cfg(windows)]
fn queue_system_notification(
    task: impl FnOnce() -> io::Result<bool> + Send + 'static,
) -> io::Result<bool> {
    type Task = Box<dyn FnOnce() -> io::Result<bool> + Send>;
    static QUEUE: std::sync::OnceLock<tokio::sync::mpsc::UnboundedSender<Task>> =
        std::sync::OnceLock::new();
    let queue = QUEUE.get_or_init(|| {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<Task>();
        // Preserve replacement order while keeping native waits off the client loop.
        tokio::spawn(async move {
            while let Some(task) = receiver.recv().await {
                let result = tokio::task::spawn_blocking(task)
                    .await
                    .unwrap_or_else(|err| Err(io::Error::other(err)));
                if let Err(err) = result {
                    warn!(err = %err, "failed to emit system notification");
                }
            }
        });
        sender
    });
    queue.send(Box::new(task)).map_err(|_| {
        io::Error::new(
            io::ErrorKind::BrokenPipe,
            "notification delivery queue closed",
        )
    })?;
    Ok(true)
}

#[cfg(windows)]
fn show_untargeted_system_notification(title: &str, body: Option<&str>) -> io::Result<bool> {
    let title = title.to_owned();
    let body = body.map(str::to_owned);
    queue_system_notification(move || {
        crate::platform::show_desktop_notification(&title, body.as_deref())
    })
}

pub(super) fn handle_shell_notification_effects(
    effects: Vec<shell::ClientShellNotificationEffect>,
    sound_config: &crate::config::SoundConfig,
    #[cfg(windows)] event_tx: &tokio::sync::mpsc::Sender<super::events::ClientLoopEvent>,
) {
    for effect in effects {
        match effect {
            shell::ClientShellNotificationEffect::Sound { sound, agent } => {
                let agent = agent.as_deref().and_then(crate::detect::parse_agent_label);
                if sound_config.allows(agent) {
                    crate::sound::play(sound, sound_config);
                }
            }
            shell::ClientShellNotificationEffect::Terminal { title, body } => {
                if let Err(err) = crate::terminal_notify::show_notification(&title, body.as_deref())
                {
                    warn!(err = %err, "failed to emit terminal notification");
                }
            }
            shell::ClientShellNotificationEffect::System {
                title,
                subtitle,
                body,
                click_target,
                #[cfg(windows)]
                target,
            } => {
                #[cfg(windows)]
                let result = {
                    // Windows clicks go through the endpoint target; the subtitle
                    // is folded into the body.
                    let _ = click_target;
                    let details = crate::platform::DesktopNotificationDetails {
                        subtitle,
                        ..Default::default()
                    };
                    let body = details.flattened_body(body.as_deref());
                    show_system_notification(&title, body.as_deref(), target, event_tx)
                };
                #[cfg(not(windows))]
                let result = {
                    let details =
                        system_notification_details(subtitle, click_target, local_click_context());
                    crate::platform::show_desktop_notification_with_details(
                        &title,
                        body.as_deref(),
                        &details,
                    )
                };
                if let Err(err) = result {
                    warn!(err = %err, "failed to emit system notification");
                }
            }
        }
    }
}

#[cfg(windows)]
fn show_system_notification(
    title: &str,
    body: Option<&str>,
    target: Option<shell::ClientSystemNotificationTarget>,
    event_tx: &tokio::sync::mpsc::Sender<super::events::ClientLoopEvent>,
) -> io::Result<bool> {
    let Some(target) = target else {
        return show_untargeted_system_notification(title, body);
    };
    let key = serde_json::to_string(&(
        &target.endpoint_id.storage_key(),
        &target.boot_id,
        &target.pane_id,
    ))
    .map_err(io::Error::other)?;
    let event_tx = event_tx.clone();
    let runtime = tokio::runtime::Handle::current();
    let title = title.to_owned();
    let body = body.map(str::to_owned);
    queue_system_notification(move || {
        crate::platform::show_actionable_desktop_notification(
            &title,
            body.as_deref(),
            key,
            std::sync::Arc::new(move || {
                let event_tx = event_tx.clone();
                let target = target.clone();
                // Never block a native callback: WinRT can dispatch it while Show is awaited.
                runtime.spawn(async move {
                    let _ = event_tx
                        .send(super::events::ClientLoopEvent::NotificationActivated(
                            target,
                        ))
                        .await;
                });
            }),
        )
    })
}


/// How a notification click reaches the local server: this executable and
/// the API socket this client process resolved.
#[cfg(not(windows))]
struct LocalClickContext {
    herdr_exe: std::path::PathBuf,
    api_socket: std::path::PathBuf,
}

#[cfg(not(windows))]
fn local_click_context() -> Option<LocalClickContext> {
    let herdr_exe = std::env::current_exe().ok()?;
    let api_socket = std::path::absolute(crate::api::socket_path()).ok()?;
    Some(LocalClickContext {
        herdr_exe,
        api_socket,
    })
}

// Clicks only move focus (agent, tab, workspace) through this client's own
// herdr binary; a server never chooses a command to run on the client's machine.
#[cfg(not(windows))]
fn system_notification_details(
    subtitle: Option<String>,
    click_target: Option<shell::ClientNotificationClickTarget>,
    context: Option<LocalClickContext>,
) -> crate::platform::DesktopNotificationDetails {
    let Some((target, context)) = click_target.zip(context) else {
        return crate::platform::DesktopNotificationDetails {
            subtitle,
            ..Default::default()
        };
    };
    let herdr = context.herdr_exe.into_os_string();
    // Agent focus selects the exact pane; the tab fallback covers panes whose
    // agent was released since the notification was shown.
    let mut commands = vec![vec![
        herdr.clone(),
        "agent".into(),
        "focus".into(),
        target.pane_id.clone().into(),
    ]];
    if let Some(tab_id) = target.tab_id {
        commands.push(vec![
            herdr.clone(),
            "tab".into(),
            "focus".into(),
            tab_id.into(),
        ]);
    }
    if let Some(workspace_id) = target.workspace_id {
        commands.push(vec![
            herdr,
            "workspace".into(),
            "focus".into(),
            workspace_id.into(),
        ]);
    }
    crate::platform::DesktopNotificationDetails {
        subtitle,
        // Namespaced by socket so panes of different sessions do not replace
        // each other's notifications.
        group: Some(format!(
            "herdr:{}:{}",
            context.api_socket.display(),
            target.pane_id
        )),
        on_click: Some(crate::platform::NotificationClickAction {
            env: vec![(
                crate::api::SOCKET_PATH_ENV_VAR.to_owned(),
                context.api_socket.into_os_string(),
            )],
            commands,
        }),
    }
}

pub(super) fn handle_notify(
    kind: NotifyKind,
    message: &str,
    body: Option<&str>,
    sound_config: &crate::config::SoundConfig,
) {
    handle_notify_with_notifiers(
        kind,
        message,
        body,
        sound_config,
        crate::terminal_notify::show_notification,
        show_untargeted_system_notification,
    );
}

pub(super) fn handle_notify_with_notifiers(
    kind: NotifyKind,
    message: &str,
    body: Option<&str>,
    sound_config: &crate::config::SoundConfig,
    mut show_terminal_notification: impl FnMut(&str, Option<&str>) -> io::Result<bool>,
    mut show_system_notification: impl FnMut(&str, Option<&str>) -> io::Result<bool>,
) {
    match kind {
        NotifyKind::Sound => {
            let Some(sound) = sound_from_notify_message(message) else {
                warn!(
                    message = message,
                    "received unknown sound notification from server"
                );
                return;
            };
            if sound_config.enabled {
                crate::sound::play(sound, sound_config);
            }
        }
        NotifyKind::Toast => {
            debug!(
                message = message,
                "received terminal toast notification from server"
            );
            if let Err(err) = show_terminal_notification(message, body) {
                warn!(err = %err, "failed to emit terminal notification");
            }
        }
        NotifyKind::SystemToast => {
            debug!(
                message = message,
                "received system toast notification from server"
            );
            if let Err(err) = show_system_notification(message, body) {
                warn!(err = %err, "failed to emit system notification");
            }
        }
    }
}

pub(super) fn sound_from_notify_message(message: &str) -> Option<crate::sound::Sound> {
    match message {
        "agent done" => Some(crate::sound::Sound::Done),
        "agent attention" => Some(crate::sound::Sound::Request),
        _ => None,
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn native_delivery_keeps_client_responsive_and_preserves_replacement_order() {
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        queue_system_notification(move || {
            let _ = started.send(());
            wait.recv_timeout(std::time::Duration::from_secs(5))
                .map_err(io::Error::other)?;
            Ok(true)
        })
        .expect("queued first notification");
        ready.await.expect("notification worker started");
        let (finished, mut second) = tokio::sync::oneshot::channel();
        queue_system_notification(move || {
            let _ = finished.send(());
            Ok(true)
        })
        .expect("queued replacement");
        tokio::task::yield_now().await;
        assert_eq!(
            second.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        );
        // This send must run while native delivery is still waiting.
        release.send(()).expect("client loop remained responsive");
        second.await.expect("replacement delivered after original");

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> LocalClickContext {
        LocalClickContext {
            herdr_exe: "/opt/herdr/bin/herdr".into(),
            api_socket: "/Users/me/.config/herdr/sessions/work/herdr.sock".into(),
        }
    }

    #[test]
    fn click_target_focuses_agent_then_tab_on_the_resolved_socket() {
        let details = system_notification_details(
            Some("repo · 1".into()),
            Some(shell::ClientNotificationClickTarget {
                pane_id: "w1:p2".into(),
                tab_id: Some("w1:t1".into()),
                workspace_id: None,
            }),
            Some(context()),
        );
        assert_eq!(details.subtitle.as_deref(), Some("repo · 1"));
        assert_eq!(
            details.group.as_deref(),
            Some("herdr:/Users/me/.config/herdr/sessions/work/herdr.sock:w1:p2")
        );
        let action = details.on_click.expect("click action");
        assert_eq!(
            action.env,
            vec![(
                "HERDR_SOCKET_PATH".to_owned(),
                "/Users/me/.config/herdr/sessions/work/herdr.sock".into()
            )]
        );
        let argv = |args: &[&str]| {
            args.iter()
                .map(std::ffi::OsString::from)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            action.commands,
            vec![
                argv(&["/opt/herdr/bin/herdr", "agent", "focus", "w1:p2"]),
                argv(&["/opt/herdr/bin/herdr", "tab", "focus", "w1:t1"]),
            ]
        );
    }

    #[test]
    fn click_target_falls_back_to_the_workspace_once_the_tab_is_gone() {
        let details = system_notification_details(
            None,
            Some(shell::ClientNotificationClickTarget {
                pane_id: "w1:p7".into(),
                tab_id: Some("w1:t4".into()),
                workspace_id: Some("w1".into()),
            }),
            Some(context()),
        );
        let commands = details.on_click.expect("click action").commands;
        let verbs = commands
            .iter()
            .map(|argv| argv[1..].join(std::ffi::OsStr::new(" ")))
            .collect::<Vec<_>>();
        assert_eq!(
            verbs,
            ["agent focus w1:p7", "tab focus w1:t4", "workspace focus w1"]
        );
    }

    #[test]
    fn notification_without_click_target_has_no_group_or_action() {
        let details = system_notification_details(None, None, Some(context()));
        assert_eq!(
            details,
            crate::platform::DesktopNotificationDetails::default()
        );
    }
}
