//! The update check the Settings window offers, and the daily one behind it.
//!
//! The work itself is in [`crate::platform::updater`]; this holds the answer
//! the interface draws and the task that is producing it. A task is kept only
//! so a second click cannot start a second download: dropping it would not
//! cancel the check, and two installers racing over one directory is a way to
//! make a broken update.

use std::time::Duration;

use gpui::{Context, Task, WeakEntity};

use super::WhatsAppApp;
use crate::platform::updater;

/// How often a running window looks for a new nightly on its own.
const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// What the last check learned.
pub(crate) enum UpdateState {
    /// No check has finished in this window yet.
    Unknown,
    Checking,
    /// The published nightly is this build.
    Current,
    /// A different build is published and may be installed.
    Available(updater::Update),
    Downloading,
    /// The installer is running and this window is on its way out.
    Restarting,
    Failed(String),
}

pub(crate) struct Updates {
    pub(crate) state: UpdateState,
    /// When the last check finished, for the daily cadence.
    checked_at: Option<chrono::DateTime<chrono::Utc>>,
    /// The check or download in flight.
    task: Option<Task<()>>,
    /// The loop that checks once a day while the window is open.
    daily: Option<Task<()>>,
}

impl Updates {
    pub(crate) fn new() -> Self {
        Self {
            state: UpdateState::Unknown,
            checked_at: None,
            task: None,
            daily: None,
        }
    }

    fn busy(&self) -> bool {
        matches!(self.state, UpdateState::Checking | UpdateState::Downloading)
    }

    fn ready(&self) -> bool {
        matches!(self.state, UpdateState::Available(_))
    }
}

impl WhatsAppApp {
    /// Check on a person's click, whether or not a check is due.
    pub(crate) fn check_for_updates(&mut self, cx: &mut Context<Self>) {
        if !updater::supported() || self.updates.busy() {
            return;
        }
        let client = cx.http_client();
        self.updates.state = UpdateState::Checking;
        self.updates.checked_at = Some(velo_core::now());
        self.updates.task = Some(cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let answer = updater::check(&client).await;
            let _ = this.update(cx, |app, cx| {
                app.updates.state = match answer {
                    Ok(Some(update)) => UpdateState::Available(update),
                    Ok(None) => UpdateState::Current,
                    Err(error) => UpdateState::Failed(error),
                };
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Download the offered nightly, verify it, and hand it the installer.
    pub(crate) fn install_update(&mut self, cx: &mut Context<Self>) {
        let UpdateState::Available(update) = &self.updates.state else {
            return;
        };
        if !updater::supported() {
            return;
        }
        let update = update.clone();
        let client = cx.http_client();
        self.updates.state = UpdateState::Downloading;
        cx.notify();
        self.updates.task = Some(cx.spawn(async move |this: WeakEntity<Self>, cx| {
            let answer = updater::install(&client, &update).await;
            let _ = this.update(cx, |app, cx| match answer {
                Ok(()) => {
                    app.updates.state = UpdateState::Restarting;
                    cx.notify();
                    // The installer stops this process; quitting first is
                    // what leaves it a window to replace rather than one to
                    // fight with.
                    cx.quit();
                }
                Err(error) => {
                    app.updates.state = UpdateState::Failed(error);
                    cx.notify();
                }
            });
        }));
    }

    /// Start the daily check, once, if this build can update.
    ///
    /// Called when the window is ready rather than at construction: a window
    /// that is only ever a pairing screen should not be asking GitHub
    /// anything.
    pub(crate) fn watch_for_updates(&mut self, cx: &mut Context<Self>) {
        if !updater::supported() || self.updates.daily.is_some() {
            return;
        }
        self.maybe_check_for_updates(cx);
        self.updates.daily = Some(cx.spawn(async move |this: WeakEntity<Self>, cx| {
            loop {
                crate::platform::sleep(CHECK_EVERY).await;
                if this
                    .update(cx, |app, cx| app.maybe_check_for_updates(cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    fn maybe_check_for_updates(&mut self, cx: &mut Context<Self>) {
        if !updater::supported() || self.updates.busy() {
            return;
        }
        let due = self.updates.checked_at.is_none_or(|checked| {
            velo_core::now().signed_duration_since(checked)
                >= chrono::TimeDelta::seconds(CHECK_EVERY.as_secs() as i64)
        });
        if due {
            self.check_for_updates(cx);
        }
    }

    /// The player's view of the last check, for Settings.
    pub(crate) fn update_status(&self) -> String {
        match &self.updates.state {
            UpdateState::Unknown => crate::tr!(NotCheckedYet).to_string(),
            UpdateState::Checking => crate::tr!(CheckingForUpdates).to_string(),
            UpdateState::Current => crate::tr!(UpToDate).to_string(),
            UpdateState::Available(update) => format!(
                "{} ({})",
                crate::tr!(NightlyRevisionIsAvailable),
                update.revision
            ),
            UpdateState::Downloading => crate::tr!(DownloadingAndVerifying).to_string(),
            UpdateState::Restarting => crate::tr!(InstallingAndRestarting).to_string(),
            UpdateState::Failed(error) => error.clone(),
        }
    }

    /// Whether the button installs rather than checks.
    pub(crate) fn update_ready(&self) -> bool {
        self.updates.ready()
    }

    /// Whether a check or download is already running.
    pub(crate) fn update_busy(&self) -> bool {
        self.updates.busy()
    }

    /// The next check's button is the only one drawn; the label says which
    /// action it will take.
    pub(crate) fn update_action_label(&self) -> &'static str {
        if self.updates.ready() {
            crate::tr!(InstallAndRestart)
        } else {
            crate::tr!(CheckForUpdates)
        }
    }
}
