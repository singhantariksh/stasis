use std::{sync::Arc, time::Instant};

use tokio::sync::Notify;

use crate::{
    config::model::StasisConfig,
    log::log_message,
    core::manager::actions::ProcessInfo,
    core::manager::action_queue::ActionQueue,
};
use crate::core::utils::{detect_chassis, ChassisKind};
use crate::media_bridge::*;

#[derive(Debug)]
pub struct ManagerState {
    // Configuration
    pub cfg: Option<Arc<StasisConfig>>,

    // Action management (extracted into separate struct)
    pub action_queue: ActionQueue,

    // Timing
    pub last_activity: Instant,
    pub debounce: Option<Instant>,
    pub start_time: Instant,

    // Chassis and power state
    pub chassis: ChassisType,

    // Lock state
    pub lock_state: LockState,
    pub lock_notify: Arc<Notify>,

    // Pause/inhibit state
    pub paused: bool,
    pub manually_paused: bool,
    pub active_inhibitor_count: u32,
    pub dbus_inhibit_active: bool,
    pub app_inhibit_debounce: Option<Instant>,

    // Media state
    pub media_blocking: bool,
    pub media_playing: bool,
    pub media_bridge: MediaBridgeState,

    // Brightness
    pub brightness_device: Option<String>,
    pub max_brightness: Option<u32>,
    pub previous_brightness: Option<u32>,

    // Suspend
    pub pre_suspend_command: Option<String>,
    pub suspend_occured: bool,

    // Active flags
    pub active_flags: ActiveFlags,

    // Notifications
    pub notify: Arc<Notify>,
    pub shutdown_flag: Arc<Notify>,
}

impl Default for ManagerState {
    fn default() -> Self {
        let now = Instant::now();

        Self {
            cfg: None,
            action_queue: ActionQueue::default(),
            last_activity: now,
            debounce: None,
            start_time: now,
            chassis: ChassisType::Desktop(DesktopState),
            lock_state: LockState::default(),
            lock_notify: Arc::new(Notify::new()),
            paused: false,
            manually_paused: false,
            active_inhibitor_count: 0,
            dbus_inhibit_active: false,
            app_inhibit_debounce: None,
            media_blocking: false,
            media_playing: false,
            media_bridge: MediaBridgeState::new(),
            brightness_device: None,
            max_brightness: None,
            previous_brightness: None,
            pre_suspend_command: None,
            suspend_occured: false,
            active_flags: ActiveFlags::default(),
            notify: Arc::new(Notify::new()),
            shutdown_flag: Arc::new(Notify::new()),
        }
    }
}

impl ManagerState {
    pub fn new(cfg: Arc<StasisConfig>) -> Self {
        let now = Instant::now();
        let debounce = Some(now + std::time::Duration::from_secs(cfg.debounce_seconds as u64));

        let chassis = match detect_chassis() {
            ChassisKind::Laptop => ChassisType::Laptop(LaptopState { on_battery: false }),
            ChassisKind::Desktop => ChassisType::Desktop(DesktopState),
        };

        let is_laptop = matches!(chassis, ChassisType::Laptop(_));
        let action_queue = ActionQueue::new(&cfg, is_laptop);

        Self {
            cfg: Some(cfg.clone()),
            action_queue,
            last_activity: now,
            debounce,
            start_time: now,
            chassis,
            lock_state: LockState::from_config(&cfg),
            lock_notify: Arc::new(Notify::new()),
            paused: false,
            manually_paused: false,
            active_inhibitor_count: 0,
            dbus_inhibit_active: false,
            app_inhibit_debounce: None,
            media_blocking: false,
            media_playing: false,
            media_bridge: MediaBridgeState::new(),
            brightness_device: None,
            max_brightness: None,
            previous_brightness: None,
            pre_suspend_command: cfg.pre_suspend_command.clone(),
            suspend_occured: false,
            active_flags: ActiveFlags::default(),
            notify: Arc::new(Notify::new()),
            shutdown_flag: Arc::new(Notify::new()),
        }
    }

    pub fn is_laptop(&self) -> bool {
        matches!(self.chassis, ChassisType::Laptop(_))
    }

    pub fn on_battery(&self) -> Option<bool> {
        match &self.chassis {
            ChassisType::Laptop(l) => Some(l.on_battery),
            ChassisType::Desktop(_) => None,
        }
    }

    pub fn set_on_battery(&mut self, value: bool) {
        if let ChassisType::Laptop(l) = &mut self.chassis {
            l.on_battery = value;
            self.update_current_block();
        }
    }

    pub fn update_current_block(&mut self) {
        let new_block = match &self.chassis {
            ChassisType::Desktop(_) => "default".to_string(),
            ChassisType::Laptop(state) => self.action_queue.determine_block(state.on_battery),
        };

        if self.action_queue.switch_block(new_block) {
            self.notify.notify_one();
        }
    }

    // Convenience accessors that delegate to action_queue
    pub fn get_active_actions(&self) -> &[crate::config::model::IdleActionBlock] {
        self.action_queue.get_active_actions()
    }

    pub fn get_active_actions_mut(&mut self) -> &mut Vec<crate::config::model::IdleActionBlock> {
        self.action_queue.get_active_actions_mut()
    }

    pub fn get_active_instant_actions(&self) -> Vec<crate::config::model::IdleActionBlock> {
        self.action_queue.get_active_instant_actions()
    }

    pub async fn update_from_config(&mut self, cfg: &StasisConfig) {
        self.active_flags = ActiveFlags::default();
        self.previous_brightness = None;
        self.pre_suspend_command = cfg.pre_suspend_command.clone();

        let is_laptop = self.is_laptop();
        self.action_queue.update_from_config(cfg, is_laptop);

        let debounce = std::time::Duration::from_secs(cfg.debounce_seconds as u64);
        self.debounce = Some(Instant::now() + debounce);

        self.cfg = Some(Arc::new(cfg.clone()));
        self.lock_state = LockState::from_config(cfg);
        self.last_activity = Instant::now();
        self.notify.notify_one();

        log_message(&format!(
            "Idle timers reloaded from config (active block: {})",
            self.action_queue.current_block
        ));
    }

    /// Get the effective media playing state accounting for both MPRIS and browser
    pub fn is_any_media_playing(&self) -> bool {
        if self.media_bridge.active {
            self.media_bridge.browser_playing || self.media_playing
        } else {
            self.media_playing
        }
    }

    /// Get the total number of media inhibitors currently active
    pub fn get_media_inhibitor_count(&self) -> usize {
        self.media_bridge.inhibitor_count()
            + if self.media_playing && !self.media_bridge.active {
                1
            } else {
                0
            }
    }

    /// Log current media state for debugging
    pub fn log_media_state(&self) {
        self.media_bridge
            .log_state(self.media_playing, self.active_inhibitor_count);
    }

    pub fn get_manual_inhibit(&self) -> bool {
        self.manually_paused
    }

    pub fn update_lock_state(&mut self, locked: bool) {
        self.lock_state.is_locked = locked;
    }


    pub fn instants_triggered(&self) -> bool {
        self.action_queue.instants_triggered
    }

    pub fn set_instants_triggered(&mut self, value: bool) {
        self.action_queue.instants_triggered = value;
    }

    pub fn action_index(&self) -> usize {
        self.action_queue.action_index
    }

    pub fn set_action_index(&mut self, value: usize) {
        self.action_queue.action_index = value;
    }

    pub fn pending_notification_task(&self) -> &Option<tokio::task::JoinHandle<()>> {
        &self.action_queue.pending_notification_task
    }

    pub fn pending_notification_task_mut(&mut self) -> &mut Option<tokio::task::JoinHandle<()>> {
        &mut self.action_queue.pending_notification_task
    }

    pub fn notification_sent_for_action(&self) -> Option<usize> {
        self.action_queue.notification_sent_for_action
    }

    pub fn set_notification_sent_for_action(&mut self, value: Option<usize>) {
        self.action_queue.notification_sent_for_action = value;
    }

    pub fn resume_queue(&self) -> &Vec<crate::config::model::IdleActionBlock> {
        &self.action_queue.resume_queue
    }

    pub fn resume_queue_mut(&mut self) -> &mut Vec<crate::config::model::IdleActionBlock> {
        &mut self.action_queue.resume_queue
    }

    pub fn resume_commands_fired(&self) -> bool {
        self.action_queue.resume_commands_fired
    }

    pub fn set_resume_commands_fired(&mut self, value: bool) {
        self.action_queue.resume_commands_fired = value;
    }

    pub fn default_actions(&self) -> &Vec<crate::config::model::IdleActionBlock> {
        &self.action_queue.default_actions
    }

    pub fn ac_actions(&self) -> &Vec<crate::config::model::IdleActionBlock> {
        &self.action_queue.ac_actions
    }

    pub fn battery_actions(&self) -> &Vec<crate::config::model::IdleActionBlock> {
        &self.action_queue.battery_actions
    }

    pub fn current_block(&self) -> &str {
        &self.action_queue.current_block
    }
}

#[derive(Debug)]
pub enum ChassisType {
    Laptop(LaptopState),
    Desktop(DesktopState),
}

#[derive(Debug)]
pub struct LaptopState {
    pub on_battery: bool,
}

#[derive(Debug)]
pub struct DesktopState;

impl Default for ChassisType {
    fn default() -> Self {
        ChassisType::Desktop(DesktopState)
    }
}

#[derive(Debug, Clone)]
pub struct LockState {
    pub is_locked: bool,
    pub process_info: Option<ProcessInfo>,
    pub command: Option<String>,
    pub last_advanced: Option<std::time::Instant>,
    pub post_advanced: bool,
}

impl Default for LockState {
    fn default() -> Self {
        Self {
            is_locked: false,
            process_info: None,
            command: None,
            last_advanced: None,
            post_advanced: false,
        }
    }
}

impl LockState {
    pub fn from_config(cfg: &crate::config::model::StasisConfig) -> Self {
        use crate::config::model::IdleAction;

        let lock_action = cfg
            .actions
            .iter()
            .find(|a| a.kind == IdleAction::LockScreen);

        let command = lock_action.map(|a| {
            if let Some(ref lock_cmd) = a.lock_command {
                lock_cmd.clone()
            } else {
                a.command.clone()
            }
        });

        Self {
            is_locked: false,
            process_info: None,
            command,
            last_advanced: None,
            post_advanced: false,
        }
    }
}

#[derive(Debug)]
pub struct ActiveFlags {
    pub pre_suspend_triggered: bool,
    pub brightness_captured: bool,
}

impl Default for ActiveFlags {
    fn default() -> Self {
        Self {
            pre_suspend_triggered: false,
            brightness_captured: false,
        }
    }
}
