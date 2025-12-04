pub mod actions;
pub mod brightness;
pub mod helpers;
pub mod idle_loops;
pub mod inhibitors;
pub mod state;
pub mod tasks;

use std::{sync::Arc, time::{Duration, Instant}};
use tokio::{
    time::sleep
};

pub use self::state::ManagerState;
use crate::{
    config::model::{IdleAction, StasisConfig}, 
    core::manager::{
        actions::{is_process_running, run_command_detached, run_command_silent},
        brightness::restore_brightness,
        helpers::run_action,
        inhibitors::{decr_active_inhibitor, incr_active_inhibitor}, 
        tasks::TaskHandles,
    }, 
    log::{log_debug_message, log_message},
};

pub struct Manager {
    pub state: ManagerState,
    pub tasks: TaskHandles,

}

impl Manager {
    pub fn new(cfg: Arc<StasisConfig>) -> Self {
        Self {
            state: ManagerState::new(cfg),
            tasks: TaskHandles::new(), 
        }
    }

    pub async fn trigger_instant_actions(&mut self) {
        if self.state.instants_triggered {
            return;
        }

        let instant_actions = self.state.get_active_instant_actions();

        log_debug_message("Triggering instant actions at startup...");
        for action in instant_actions {
            run_action(self, &action).await;
        }

        self.state.instants_triggered = true;
    }

    pub fn reset_instant_actions(&mut self) {
        self.state.instants_triggered = false;
        log_debug_message("Instant actions reset; they can trigger again");
    }

    // Called when libinput service resets (on user activity)
    pub async fn reset(&mut self) {
        let cfg = match &self.state.cfg {
            Some(cfg) => Arc::clone(cfg),
            None => {
                log_debug_message("No configuration available, skipping reset");
                return;
            }
        };

        // Restore brightness if needed
        if self.state.previous_brightness.is_some() {
            if let Err(e) = restore_brightness(&mut self.state).await {
                log_message(&format!("Failed to restore brightness: {}", e));
            }
        }
        
        let now = Instant::now();
        let debounce = Duration::from_secs(cfg.debounce_seconds as u64);
        self.state.debounce = Some(now + debounce);
        self.state.last_activity = now;

        // Cancel any pending notifications
        self.state.pending_notification_task = None;
        self.state.notification_sent_for_action = None;

        // Store values we need before borrowing
        let is_locked = self.state.lock_state.is_locked;
        let cmd_to_check = self.state.lock_state.command.clone();

        // Clear only actions that are before or equal to the current stage
        for actions in [&mut self.state.default_actions, &mut self.state.ac_actions, &mut self.state.battery_actions] {
            let mut past_lock = false;
            for a in actions.iter_mut() {
                if matches!(a.kind, crate::config::model::IdleAction::LockScreen) {
                    past_lock = true;
                }
                // if locked, preserve stages past lock (so dpms/suspend remain offset correctly)
                if is_locked && past_lock {
                    continue;
                }


                if a.is_instant() {
                    continue;
                }
          
                a.last_triggered = None;
            }
        }

        // Use the helper method to get active actions
        let (is_instant, lock_index) = {
            let actions = self.state.get_active_actions_mut();

            // Skip instant actions here. handled elsewhere
            let index = actions.iter()
                .position(|a| a.last_triggered.is_none())
                .unwrap_or(actions.len().saturating_sub(1));

            let is_instant = !actions.is_empty() && actions[index].is_instant();

            // Find lock index if needed
            let lock_index = if is_locked {
                actions.iter().position(|a| matches!(a.kind, crate::config::model::IdleAction::LockScreen))
            } else {
                None
            };

            (is_instant, lock_index)
        }; // Borrow ends here

        // Reset action_index
        if !is_locked {
            self.state.action_index = 0;
        }

        if is_instant {
            return;
        }

        if is_locked {
            if let Some(lock_index) = lock_index {
                // Check if lock process is still running
                let still_active = if let Some(cmd) = cmd_to_check {
                    is_process_running(&cmd).await
                } else {
                    true // Assume lock is active if no command is specified
                };

                if still_active {
                    // Always advance to one past lock when locked
                    self.state.action_index = lock_index.saturating_add(1);
                    
                    let debounce_end = now + debounce;
                    let new_action_index = self.state.action_index;
                    let actions = self.state.get_active_actions_mut();
                    if new_action_index < actions.len() {
                        actions[new_action_index].last_triggered = Some(debounce_end); 
                    } else {
                        // If at the end, reset last_triggered for the last action
                        if lock_index < actions.len() {
                            actions[lock_index].last_triggered = Some(debounce_end);
                        } 
                    }
                    
                    self.state.lock_state.post_advanced = true;
                } 
            } 
        }
        
        self.fire_resume_queue().await;
        self.state.notify.notify_one();
    }

    // Check whether we have been idle enough to elapse one of the timeouts
    pub async fn check_timeouts(&mut self) {
        if self.state.paused || self.state.manually_paused {
            return;
        }

        let now = Instant::now();

        // Store values we need before borrowing actions
        let action_index = self.state.action_index;
        let is_locked = self.state.lock_state.is_locked;
        let last_activity = self.state.last_activity;
        let debounce = self.state.debounce;

        // Get notification settings from config early
        let (notify_before_enabled, notify_seconds) = if let Some(cfg) = &self.state.cfg {
            (cfg.notify_before_action, cfg.notify_seconds_before)
        } else {
            (false, 0)
        };

        // Get reference to the right actions Vec using helper method
        let actions = self.state.get_active_actions_mut();

        if actions.is_empty() {
            return;
        }

        let index = action_index.min(actions.len() - 1);

        // Skip lock if already locked
        if matches!(actions[index].kind, IdleAction::LockScreen) && is_locked {
            return;
        }

        // Calculate the ORIGINAL fire time (without notification delay)
        let timeout = Duration::from_secs(actions[index].timeout as u64);
        let original_fire_time = if let Some(last_trig) = actions[index].last_triggered {
            // Already triggered: timeout from when it last fired
            last_trig + timeout
        } else if index > 0 {
            // Not first action: fire relative to previous action
            if let Some(prev_trig) = actions[index - 1].last_triggered {
                prev_trig + timeout
            } else {
                // Previous hasn't fired yet, shouldn't happen but fallback
                last_activity + timeout
            }
        } else {
            // First action: apply debounce + timeout from last_activity
            let base = debounce.unwrap_or(last_activity);
            base + timeout
        };

        // Extract notification data before spawning task
        let notification_opt = actions[index].notification.as_ref().map(|n| (n.clone(), actions[index].name.clone()));

        // Check if we should schedule notification
        // Timeline: [idle] -> [debounce] -> [timeout] -> [NOTIFY at original_fire_time] -> [wait notify_seconds] -> [ACTION]
        if notify_before_enabled && notify_seconds > 0 {
            if let Some((notification_msg, action_name)) = notification_opt {
                let notify_duration = Duration::from_secs(notify_seconds as u64);
                let already_notified = self.state.notification_sent_for_action == Some(index);
                
                // Notification fires at the ORIGINAL fire time
                // Action fires AFTER the notification delay
                let actual_action_time = original_fire_time + notify_duration;
                
                // If we've reached the original fire time but haven't notified yet
                if now >= original_fire_time && !already_notified {
                    let action_time = actual_action_time;
                    let notify = Arc::clone(&self.state.notify);
                    
                    log_message(&format!(
                        "Notification time reached for action '{}': sending '{}' (action will fire in {}s)",
                        action_name, notification_msg, notify_seconds
                    ));
                    
                    // Mark that we've sent notification for this action
                    self.state.notification_sent_for_action = Some(index);
                    
                    // Spawn notification task
                    let task = tokio::spawn(async move {
                        // Send notification immediately
                        let notify_cmd = format!("notify-send -a Stasis '{}'", notification_msg);
                        if let Err(e) = run_command_silent(&notify_cmd).await {
                            log_message(&format!("Failed to send notification: {}", e));
                        } else {
                            log_message(&format!("Notification sent: {}", notification_msg));
                        }
                        
                        // Wait for the notification delay period
                        let wait_duration = action_time.checked_duration_since(Instant::now())
                            .unwrap_or(Duration::ZERO);
                        if !wait_duration.is_zero() {
                            tokio::time::sleep(wait_duration).await;
                        }
                        
                        // Wake the idle task to fire the action
                        notify.notify_one();
                    });
                    
                    self.state.pending_notification_task = Some(task);
                    return; // Don't fire action yet
                }
                
                // If notification was sent, check if it's time to fire the action
                if already_notified && now >= actual_action_time {
                    // Fall through to fire the action
                } else {
                    // Not ready yet
                    return;
                }
            }
        } else {
            // No notification - check original fire time
            if now < original_fire_time {
                return;
            }
        }

        // Action is ready: clone and mark triggered
        let (action_clone, actions_len) = {
            let actions = self.state.get_active_actions_mut();
            let action_clone = actions[index].clone();
            actions[index].last_triggered = Some(now);
            (action_clone, actions.len())
        }; // Borrow ends here

        // Clear notification task since action is firing
        self.state.pending_notification_task = None;
        self.state.notification_sent_for_action = None;

        // Advance index
        self.state.action_index += 1;
        if self.state.action_index < actions_len {
            // Only mark next action triggered after it actually fires
            self.state.resume_commands_fired = false;
        } else {
            self.state.action_index = actions_len - 1;
        }

        // Add to resume queue if needed
        if !matches!(action_clone.kind, IdleAction::LockScreen) && action_clone.resume_command.is_some() {
            self.state.resume_queue.push(action_clone.clone());
        }

        // Fire the action (without blocking notification)
        run_action(self, &action_clone).await;
    }

    pub async fn fire_resume_queue(&mut self) {
        if self.state.resume_queue.is_empty() {
            return;
        }

        log_message(&format!("Firing {} queued resume command(s)...", self.state.resume_queue.len()));

        for action in self.state.resume_queue.drain(..) {
            if let Some(resume_cmd) = &action.resume_command {
                log_message(&format!("Running resume command for action: {}", action.name));
                if let Err(e) = run_command_detached(resume_cmd).await {
                    log_message(&format!("Failed to run resume command '{}': {}", resume_cmd, e));
                }
            }
        }

        self.state.resume_queue.clear();
    }

    pub fn next_action_instant(&self) -> Option<Instant> {
        if self.state.paused || self.state.manually_paused {
            return None;
        }

        // Use helper method to get active actions
        let actions = self.state.get_active_actions();

        if actions.is_empty() {
            return None;
        }

        // Get notification settings from config
        let (notify_before_enabled, notify_seconds) = if let Some(cfg) = &self.state.cfg {
            (cfg.notify_before_action, cfg.notify_seconds_before)
        } else {
            (false, 0)
        };

        let mut min_time: Option<Instant> = None;

        for (i, action) in actions.iter().enumerate() {
            // Skip lock if already locked
            if matches!(action.kind, IdleAction::LockScreen) && self.state.lock_state.is_locked {
                continue;
            }

            // Calculate the ORIGINAL fire time (where notification would fire)
            let timeout = Duration::from_secs(action.timeout as u64);
            let original_fire_time = if let Some(last_trig) = action.last_triggered {
                // Already triggered: timeout from when it last fired
                last_trig + timeout
            } else if i > 0 {
                // Not first action: fire relative to previous action
                if let Some(prev_trig) = actions[i - 1].last_triggered {
                    prev_trig + timeout
                } else {
                    // Previous hasn't fired yet, shouldn't happen but fallback
                    self.state.last_activity + timeout
                }
            } else {
                // First action: use debounce + timeout
                let base = self.state.debounce.unwrap_or(self.state.last_activity);
                base + timeout
            };

            // Determine when we need to wake up
            let next_time = if notify_before_enabled && action.notification.is_some() {
                // If notification not yet sent for this action, wake at original fire time
                // If already sent, wake at actual action time (original + notify delay)
                if self.state.notification_sent_for_action == Some(i) {
                    let notify_duration = Duration::from_secs(notify_seconds as u64);
                    original_fire_time + notify_duration
                } else {
                    original_fire_time
                }
            } else {
                original_fire_time
            };

            min_time = Some(match min_time {
                None => next_time,
                Some(current_min) => current_min.min(next_time),
            });
        }

        min_time
    }

    pub async fn advance_past_lock(&mut self) {
        log_message("Advancing state past lock stage...");
        self.state.lock_state.post_advanced = true;
        self.state.lock_state.last_advanced = Some(Instant::now());
    }

    pub async fn pause(&mut self, manual: bool) {
        if manual {
            self.state.manually_paused = true;
            log_message("Idle timers manually paused");
        } else if !self.state.manually_paused {
            self.state.paused = true;
            log_message("Idle timers automatically paused");
        }
    }

    pub async fn resume(&mut self, manually: bool) {
        if manually {
            if self.state.manually_paused {
                self.state.manually_paused = false;
                
                if self.state.active_inhibitor_count == 0 {
                    self.state.paused = false;
                    log_message("Idle timers manually resumed");
                } else {
                    log_message(&format!(
                        "Manual pause cleared, but {} inhibitor(s) still active - timers remain paused",
                        self.state.active_inhibitor_count
                    ));
                }
            }
        } else if !self.state.manually_paused && self.state.paused {
            // This is called by decr_active_inhibitor when count reaches 0
            self.state.paused = false;
            log_message("Idle timers automatically resumed");
        }
    }

    pub async fn toggle_state(&mut self, inhibit: bool) {
        if inhibit {
            self.pause(true).await;
        } else {
            self.resume(true).await;
        }
    }

    pub async fn recheck_media(&mut self) {
        // read ignore_remote_media + media blacklist from cfg
        let (ignore_remote, media_blacklist) = match &self.state.cfg {
            Some(cfg) => (cfg.ignore_remote_media, cfg.media_blacklist.clone()),
            None => (false, Vec::new()),
        };

        // sync check (pactl + mpris).
        let playing = crate::core::services::media::check_media_playing(ignore_remote, &media_blacklist, false, );

        // Only change state via the helpers so behaviour stays consistent:
        if playing && !self.state.media_playing {
            // call the same helper the monitor uses
            incr_active_inhibitor(self).await;
            self.state.media_playing = true;
        } else if !playing && self.state.media_playing {
            decr_active_inhibitor(self).await;
            self.state.media_playing = false;
        }
    }

    pub async fn shutdown(&mut self) {
        self.state.shutdown_flag.notify_waiters();

        sleep(Duration::from_millis(200)).await;

        self.tasks.abort_all(); 
    }
}
