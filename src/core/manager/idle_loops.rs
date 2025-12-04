use std::{
  sync::Arc,
  time::{Duration, Instant},
};
use tokio::{
  sync::Mutex,
  task::JoinHandle,
  time::{Instant as TokioInstant, sleep_until},
};

use crate::{
  core::manager::{
    Manager,
    actions::{is_process_active, is_process_running, is_session_locked_via_logind, run_command_detached},
  },
  log::log_message,
};

pub fn spawn_idle_task(manager: Arc<Mutex<Manager>>) -> JoinHandle<()> {
  tokio::spawn(async move {
    loop {
      // Grab both the next timeout and the notify handles
      let (next_instant, notify, shutdown) = {
        let mgr = manager.lock().await;
        (
          mgr.next_action_instant(),
          mgr.state.notify.clone(),
          mgr.state.shutdown_flag.clone(),
        )
      };

      // Compute how long we should sleep
      let sleep_deadline = match next_instant {
        Some(instant) => {
          let now = Instant::now();
          let max_sleep = Duration::from_secs(60); // wake periodically
          if instant <= now {
            now + Duration::from_millis(50)
          } else if instant - now > max_sleep {
            now + max_sleep
          } else {
            instant
          }
        }
        None => Instant::now() + Duration::from_secs(60),
      };

      tokio::select! {
          _ = sleep_until(TokioInstant::from_std(sleep_deadline)) => {},
          _ = notify.notified() => {
              // Woken up by external event (reset, AC change, playback)
              continue; // recalc immediately
          }
          _ = shutdown.notified() => {
              break; // exit loop cleanly
          }
      }

      // Now check timeouts only once after wake
      let mut mgr = manager.lock().await;
      if !mgr.state.paused && !mgr.state.manually_paused {
        mgr.check_timeouts().await;
      }
    }

    log_message("Idle loop shutting down...");
  })
}

pub async fn spawn_lock_watcher(
  manager: std::sync::Arc<tokio::sync::Mutex<crate::core::manager::Manager>>,
) -> tokio::task::JoinHandle<()> {
  use std::time::Duration;
  use tokio::time::sleep;

  tokio::spawn(async move {
    loop {
      let shutdown = {
        let mgr = manager.lock().await;
        mgr.state.shutdown_flag.clone()
      };

      // Wait until lock becomes active
      {
        let mut mgr = manager.lock().await;
        while !mgr.state.lock_state.is_locked {
          let lock_notify = mgr.state.lock_notify.clone();
          drop(mgr);
          tokio::select! {
              _ = lock_notify.notified() => {},
              _ = shutdown.notified() => {
                  log_message("Lock watcher shutting down...");
                  return;
              }
          }
          mgr = manager.lock().await;
        }
      }

      log_message("Lock detected — entering lock watcher");

      // Give the lock screen time to signal logind before first check
      // This avoids race condition where we check before logind is updated
      sleep(Duration::from_millis(500)).await;

      // Monitor lock until it ends
      let mut last_state = true; // We know we're locked when entering
      let mut check_count = 0u32;
      
      loop {
        let (process_info, maybe_cmd, was_locked, shutdown, lock_notify) = {
          let mgr = manager.lock().await;
          (
            mgr.state.lock_state.process_info.clone(),
            mgr.state.lock_state.command.clone(),
            mgr.state.lock_state.is_locked,
            mgr.state.shutdown_flag.clone(),
            mgr.state.lock_notify.clone(),
          )
        };

        if !was_locked {
          break;
        }

        // Check if lock is still active using logind (primary) or process check (fallback)
        let logind_result = is_session_locked_via_logind().await;
        let still_active = match logind_result {
            Some(true) => {
                // Definitely locked
                true
            }
            _ => {
                // Either logind returned false or query failed
                if let Some(ref info) = process_info {
                    is_process_active(info).await
                } else if let Some(cmd) = maybe_cmd {
                    is_process_running(&cmd).await
                } else {
                    // Conservative default: assume still locked
                    if check_count == 0 {
                        log_message("No process info or command available for lock fallback, assuming locked");
                    }
                    true
                }
            }
        };

        // Only log on state change or every 20 checks (10 seconds)
        check_count += 1;
        if still_active != last_state || check_count % 20 == 0 {
            let logind_str = match logind_result {
                Some(true) => "locked",
                Some(false) => "unlocked", 
                None => "unavailable",
            };
            log_message(&format!(
                "Lock check #{}: active={} (logind={})",
                check_count, still_active, logind_str
            ));
            last_state = still_active;
        }

        if !still_active {
          let mut mgr = manager.lock().await;

          if !mgr.state.lock_state.is_locked {
            break;
          }

          // Fire resume command if configured
          use crate::config::model::IdleAction;
          if let Some(lock_action) = mgr
            .state
            .default_actions
            .iter()
            .chain(mgr.state.ac_actions.iter())
            .chain(mgr.state.battery_actions.iter())
            .find(|a| matches!(a.kind, IdleAction::LockScreen))
          {
            if let Some(resume_cmd) = &lock_action.resume_command {
              log_message("Firing lockscreen resume command");
              if let Err(e) = run_command_detached(resume_cmd).await {
                log_message(&format!("Failed to run lock resume command: {}", e));
              }
            }
          }

          mgr.state.lock_state.process_info = None;
          mgr.state.lock_state.post_advanced = false;
          mgr.state.action_index = 0;
          mgr.state.lock_state.is_locked = false;

          mgr.reset().await;

          log_message("Lockscreen ended — exiting lock watcher");
          break;
        }

        tokio::select! {
            _ = lock_notify.notified() => {
                check_count = 0; // Reset on manual notification
            },
            _ = sleep(Duration::from_millis(500)) => {},
            _ = shutdown.notified() => {
                log_message("Lock watcher shutting down during active lock...");
                return;
            }
        }
      }
    }
  })
}
