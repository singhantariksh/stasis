use std::time::{Duration, Instant};
use tokio::task::JoinHandle;

use crate::{
    config::model::{IdleAction, IdleActionBlock, StasisConfig},
    log::{log_debug_message, log_message},
};

/// Manages the action queue, progression, and notification state
#[derive(Debug)]
pub struct ActionQueue {
    // Action sets for different power states
    pub default_actions: Vec<IdleActionBlock>,
    pub ac_actions: Vec<IdleActionBlock>,
    pub battery_actions: Vec<IdleActionBlock>,

    // Current state
    pub current_block: String,
    pub action_index: usize,
    pub instants_triggered: bool,

    // Resume queue for actions that need resume commands
    pub resume_queue: Vec<IdleActionBlock>,
    pub resume_commands_fired: bool,

    // Notification state
    pub pending_notification_task: Option<JoinHandle<()>>,
    pub notification_sent_for_action: Option<usize>,
}

impl Default for ActionQueue {
    fn default() -> Self {
        Self {
            default_actions: Vec::new(),
            ac_actions: Vec::new(),
            battery_actions: Vec::new(),
            current_block: "default".to_string(),
            action_index: 0,
            instants_triggered: false,
            resume_queue: Vec::new(),
            resume_commands_fired: false,
            pending_notification_task: None,
            notification_sent_for_action: None,
        }
    }
}

impl ActionQueue {
    pub fn new(cfg: &StasisConfig, is_laptop: bool) -> Self {
        let default_actions: Vec<_> = cfg
            .actions
            .iter()
            .filter(|a| !a.name.starts_with("ac.") && !a.name.starts_with("battery."))
            .cloned()
            .collect();

        let ac_actions: Vec<_> = cfg
            .actions
            .iter()
            .filter(|a| a.name.starts_with("ac."))
            .cloned()
            .collect();

        let battery_actions: Vec<_> = cfg
            .actions
            .iter()
            .filter(|a| a.name.starts_with("battery."))
            .cloned()
            .collect();

        let current_block = if is_laptop { "ac" } else { "default" }.to_string();

        Self {
            default_actions,
            ac_actions,
            battery_actions,
            current_block,
            action_index: 0,
            instants_triggered: false,
            resume_queue: Vec::new(),
            resume_commands_fired: false,
            pending_notification_task: None,
            notification_sent_for_action: None,
        }
    }

    /// Get the currently active action set (immutable)
    pub fn get_active_actions(&self) -> &[IdleActionBlock] {
        match self.current_block.as_str() {
            "ac" => &self.ac_actions,
            "battery" => &self.battery_actions,
            "default" => &self.default_actions,
            _ => &self.default_actions,
        }
    }

    /// Get the currently active action set (mutable)
    pub fn get_active_actions_mut(&mut self) -> &mut Vec<IdleActionBlock> {
        match self.current_block.as_str() {
            "ac" => &mut self.ac_actions,
            "battery" => &mut self.battery_actions,
            "default" => &mut self.default_actions,
            _ => &mut self.default_actions,
        }
    }

    /// Get all instant actions from the current block
    pub fn get_active_instant_actions(&self) -> Vec<IdleActionBlock> {
        self.get_active_actions()
            .iter()
            .filter(|a| a.is_instant())
            .cloned()
            .collect()
    }

    /// Switch the active action block (ac/battery/default)
    pub fn switch_block(&mut self, new_block: String) -> bool {
        if new_block == self.current_block {
            return false;
        }

        let old_block = self.current_block.clone();
        self.current_block = new_block;

        log_message(&format!(
            "Switched active block: {} -> {}",
            old_block, self.current_block
        ));

        // Reset state when switching blocks
        self.action_index = 0;
        self.instants_triggered = false;
        self.pending_notification_task = None;
        self.notification_sent_for_action = None;

        true
    }

    /// Determine which block should be active based on power state
    pub fn determine_block(&self, on_battery: bool) -> String {
        if on_battery {
            if !self.battery_actions.is_empty() {
                "battery".to_string()
            } else {
                "default".to_string()
            }
        } else {
            if !self.ac_actions.is_empty() {
                "ac".to_string()
            } else {
                "default".to_string()
            }
        }
    }

    /// Reset instant action trigger state
    pub fn reset_instant_actions(&mut self) {
        self.instants_triggered = false;
        log_debug_message("Instant actions reset; they can trigger again");
    }

    /// Clear last_triggered for actions before or equal to current stage
    pub fn clear_triggered_before_lock(&mut self, is_locked: bool) {
        for actions in [
            &mut self.default_actions,
            &mut self.ac_actions,
            &mut self.battery_actions,
        ] {
            let mut past_lock = false;
            for a in actions.iter_mut() {
                if matches!(a.kind, IdleAction::LockScreen) {
                    past_lock = true;
                }

                // If locked, preserve stages past lock (so dpms/suspend remain offset correctly)
                if is_locked && past_lock {
                    continue;
                }

                if a.is_instant() {
                    continue;
                }

                a.last_triggered = None;
            }
        }
    }

    /// Find the index of the lock screen action in current block
    pub fn find_lock_index(&self) -> Option<usize> {
        self.get_active_actions()
            .iter()
            .position(|a| matches!(a.kind, IdleAction::LockScreen))
    }

    /// Add an action to the resume queue
    pub fn queue_resume(&mut self, action: IdleActionBlock) {
        if !matches!(action.kind, IdleAction::LockScreen) && action.resume_command.is_some() {
            self.resume_queue.push(action);
        }
    }

    /// Clear the resume queue
    pub fn clear_resume_queue(&mut self) {
        self.resume_queue.clear();
    }

    /// Advance to the next action in the sequence
    pub fn advance_index(&mut self) {
        let actions_len = self.get_active_actions().len();
        self.action_index += 1;

        if self.action_index < actions_len {
            self.resume_commands_fired = false;
        } else {
            self.action_index = actions_len.saturating_sub(1);
        }
    }

    /// Reset action index to start
    pub fn reset_index(&mut self) {
        self.action_index = 0;
    }

    /// Set action index to specific value
    pub fn set_index(&mut self, index: usize) {
        self.action_index = index;
    }

    /// Clear notification state
    pub fn clear_notification_state(&mut self) {
        self.pending_notification_task = None;
        self.notification_sent_for_action = None;
    }

    /// Mark notification as sent for current action
    pub fn mark_notification_sent(&mut self, action_index: usize) {
        self.notification_sent_for_action = Some(action_index);
    }

    /// Check if notification was sent for an action
    pub fn notification_sent_for(&self, action_index: usize) -> bool {
        self.notification_sent_for_action == Some(action_index)
    }

    /// Update from new config
    pub fn update_from_config(&mut self, cfg: &StasisConfig, is_laptop: bool) {
        self.default_actions = cfg
            .actions
            .iter()
            .filter(|a| !a.name.starts_with("ac.") && !a.name.starts_with("battery."))
            .cloned()
            .collect();

        self.ac_actions = cfg
            .actions
            .iter()
            .filter(|a| a.name.starts_with("ac."))
            .cloned()
            .collect();

        self.battery_actions = cfg
            .actions
            .iter()
            .filter(|a| a.name.starts_with("battery."))
            .cloned()
            .collect();

        // Clear triggered state for all actions
        for actions in [
            &mut self.default_actions,
            &mut self.ac_actions,
            &mut self.battery_actions,
        ] {
            for a in actions.iter_mut() {
                a.last_triggered = None;
            }
        }

        // Determine and switch to appropriate block
        let new_block = if is_laptop {
            self.determine_block(false) // Start on AC for laptops
        } else {
            "default".to_string()
        };

        self.switch_block(new_block);
        self.instants_triggered = false;
        self.action_index = 0;
        self.clear_notification_state();
    }

    /// Calculate when the next action should fire
    pub fn next_action_time(
        &self,
        is_locked: bool,
        last_activity: Instant,
        debounce: Option<Instant>,
        notify_enabled: bool,
        notify_seconds: u32,
    ) -> Option<Instant> {
        let actions = self.get_active_actions();

        if actions.is_empty() {
            return None;
        }

        let mut min_time: Option<Instant> = None;

        for (i, action) in actions.iter().enumerate() {
            // Skip lock if already locked
            if matches!(action.kind, IdleAction::LockScreen) && is_locked {
                continue;
            }

            // Calculate the original fire time (where notification would fire)
            let timeout = Duration::from_secs(action.timeout as u64);
            let original_fire_time = if let Some(last_trig) = action.last_triggered {
                last_trig + timeout
            } else if i > 0 {
                if let Some(prev_trig) = actions[i - 1].last_triggered {
                    prev_trig + timeout
                } else {
                    last_activity + timeout
                }
            } else {
                let base = debounce.unwrap_or(last_activity);
                base + timeout
            };

            // Determine when we need to wake up
            let next_time = if notify_enabled && action.notification.is_some() {
                if self.notification_sent_for(i) {
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
}
