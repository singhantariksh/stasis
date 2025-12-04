use crate::config::model::{IdleAction, IdleActionBlock};
use crate::log::{log_message, log_debug_message};
use eyre::Result;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

/// Check if the session is locked via systemd logind
/// Returns Some(true) if locked, Some(false) if unlocked, None if unable to query
pub async fn is_session_locked_via_logind() -> Option<bool> {
  match Command::new("busctl")
    .arg("get-property")
    .arg("--system")
    .arg("--")
    .arg("org.freedesktop.login1")
    .arg("/org/freedesktop/login1/session/auto")
    .arg("org.freedesktop.login1.Session")
    .arg("LockedHint")
    .output()
    .await
  {
    Ok(output) => {
      let output_str = String::from_utf8_lossy(&output.stdout);
      let is_locked = output_str.trim() == "b true";
      Some(is_locked)
    }
    Err(_) => None,
  }
}

#[derive(Debug, Clone)]
pub enum ActionRequest {
  RunCommand(String),
  Skip(String),
}

/// Information about a spawned process
#[derive(Debug, Clone)]
pub struct ProcessInfo {
  pub pid: u32,
  pub pgid: u32,
  pub command: String,
  pub expected_process_name: Option<String>,
}

/// Prepare action for execution
pub async fn prepare_action(action: &IdleActionBlock) -> Vec<ActionRequest> {
  let cmd = action.command.clone();
  match action.kind {
    IdleAction::Suspend => {
      if !cmd.trim().is_empty() {
        vec![ActionRequest::RunCommand(cmd)]
      } else {
        vec![]
      }
    }

    IdleAction::LockScreen => {
      // Check lock state using systemd logind DBus
      let logind_result = is_session_locked_via_logind().await;

      let is_locked = match logind_result {
        Some(true) => {
          log_message("Using logind detection: locked=true");
          true
        }
        _ => {
          log_message("logind returned false or unavailable, falling back to process check");

          let is_locked_process = if let Some(ref lock_cmd) = action.lock_command {
            is_process_running(lock_cmd).await
          } else {
            is_process_running(&action.command).await
          };

          log_message(&format!("Process check result: locked={}", is_locked_process));
          is_locked_process
        }
      };

      if is_locked {
        log_message("Lockscreen already active, skipping action.");
        let probe_cmd = action.lock_command.as_ref().unwrap_or(&action.command);
        vec![ActionRequest::Skip(probe_cmd.to_string())]
      } else {
        vec![ActionRequest::RunCommand(action.command.clone())]
      }
    }

    _ => {
      if cmd.trim().is_empty() {
        vec![]
      } else {
        vec![ActionRequest::RunCommand(cmd)]
      }
    }
  }
}

/// Run a shell command silently (log to /tmp/stasis.log)
pub async fn run_command_silent(cmd: &str) -> Result<()> {
  let log_file = "/tmp/stasis.log";
  let fut = async {
    let mut child = Command::new("sh")
      .arg("-c")
      .arg(format!("{cmd} >> {log_file} 2>&1"))
      .envs(std::env::vars())
      .stdin(Stdio::null())
      .stdout(Stdio::null())
      .stderr(Stdio::null())
      .spawn()?;
    let status = child.wait().await?;
    if !status.success() {
      eyre::bail!("Command '{}' exited with status {:?}", cmd, status.code());
    }
    Ok::<(), eyre::Report>(())
  };
  tokio::time::timeout(Duration::from_secs(30), fut).await??;
  Ok(())
}

/// Run a command detached and return comprehensive process info
pub async fn run_command_detached(command: &str) -> Result<ProcessInfo, Box<dyn std::error::Error>> {
  if command.trim().is_empty() {
    return Err("Empty command".into());
  }

  // Create a new process group by using setsid
  let child = Command::new("sh")
    .arg("-c")
    .arg(command)
    .envs(std::env::vars())
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .process_group(0) // Create new process group
    .spawn()?;

  let pid = child.id().ok_or("Failed to get child PID")?;

  // Get the process group ID (usually same as PID for process group leader)
  let pgid = get_pgid(pid).await.unwrap_or(pid);

  // Extract expected process name from command for later verification
  let expected_name = extract_expected_process_name(command);

  log_debug_message(&format!(
    "Spawned process: PID={}, PGID={}, expected_name={:?}",
    pid, pgid, expected_name
  ));

  Ok(ProcessInfo {
    pid,
    pgid,
    command: command.to_string(),
    expected_process_name: expected_name,
  })
}

/// Extract the expected process name from a command
fn extract_expected_process_name(command: &str) -> Option<String> {
  let first_word = command.split_whitespace().next()?;

  // Get just the binary name (last component of the path)
  let binary_name = std::path::Path::new(first_word)
    .file_name()
    .and_then(|s| s.to_str())?
    .to_string();

  Some(binary_name)
}

/// Get process group ID for a PID
async fn get_pgid(pid: u32) -> Option<u32> {
  let stat_path = format!("/proc/{}/stat", pid);
  let contents = tokio::fs::read_to_string(&stat_path).await.ok()?;

  // Parse /proc/[pid]/stat - PGID is the 5th field
  let fields: Vec<&str> = contents.split_whitespace().collect();
  if fields.len() > 4 { fields[4].parse().ok() } else { None }
}

/// Check if a process or its descendants are still running
pub async fn is_process_active(info: &ProcessInfo) -> bool {
  // Strategy 1: Check if original PID still exists
  if std::path::Path::new(&format!("/proc/{}", info.pid)).exists() {
    return true;
  }

  // Strategy 2: Check process group for any surviving members
  if let Some(pids) = get_process_group_members(info.pgid).await {
    if !pids.is_empty() {
      return true;
    }
  }

  // Strategy 3: If we know the expected process name, search for it
  if let Some(ref name) = info.expected_process_name {
    if is_process_running(name).await {
      return true;
    }
  }

  false
}

/// Get all PIDs in a process group
async fn get_process_group_members(pgid: u32) -> Option<Vec<u32>> {
  let output = Command::new("ps").arg("-eo").arg("pid,pgid").output().await.ok()?;

  if output.stdout.is_empty() {
    return Some(Vec::new());
  }

  let pids: Vec<u32> = String::from_utf8_lossy(&output.stdout)
    .lines()
    .skip(1) // Skip header
    .filter_map(|line| {
      let parts: Vec<&str> = line.split_whitespace().collect();
      if parts.len() >= 2 {
        let pid: u32 = parts[0].parse().ok()?;
        let proc_pgid: u32 = parts[1].parse().ok()?;
        if proc_pgid == pgid { Some(pid) } else { None }
      } else {
        None
      }
    })
    .collect();

  Some(pids)
}

/// Check if a process matching `cmd` is running (by name)
pub async fn is_process_running(cmd: &str) -> bool {
  if cmd.trim().is_empty() {
    return false;
  }

  // Extract the actual binary name from the command
  let first_word = cmd.split_whitespace().next().unwrap_or("");
  if first_word.is_empty() {
    return false;
  }

  // Get just the binary name (last component of the path)
  let binary_name = std::path::Path::new(first_word)
    .file_name()
    .and_then(|s| s.to_str())
    .unwrap_or(first_word);

  match Command::new("pgrep").arg("-x").arg(binary_name).output().await {
    Ok(output) => !output.stdout.is_empty(),
    Err(_) => false,
  }
}

/// Forcefully kill a process and its entire process group
pub async fn kill_process_group(info: &ProcessInfo) -> Result<()> {
  log_message(&format!(
    "Killing process group {} (original PID: {})",
    info.pgid, info.pid
  ));

  // Kill entire process group
  let _ = Command::new("kill")
    .arg("-TERM")
    .arg(format!("-{}", info.pgid)) // Negative PGID kills the group
    .output()
    .await;

  // Give processes time to terminate gracefully
  tokio::time::sleep(Duration::from_millis(500)).await;

  // Force kill if still alive
  if is_process_active(info).await {
    log_message(&format!("Process group {} still alive, sending SIGKILL", info.pgid));
    let _ = Command::new("kill")
      .arg("-KILL")
      .arg(format!("-{}", info.pgid))
      .output()
      .await;
  }

  Ok(())
}
