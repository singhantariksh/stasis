use std::sync::Arc;
use eyre::Result;
use mpris::{PlayerFinder, PlaybackStatus};
use tokio::task;
use crate::core::manager::Manager;

const IGNORED_PLAYERS: &[&str] = &[
    "KDE Connect", "kdeconnect", "Chromecast", "chromecast",
    "Spotify Connect", "spotifyd", "vlc-http", "plexamp", "bluez",
];

// Event-driven media monitoring using D-Bus signals
use zbus::{Connection, MatchRule, MessageStream};
use futures_util::stream::StreamExt;

pub async fn spawn_media_monitor_dbus(
    manager: Arc<tokio::sync::Mutex<Manager>>,
    ignore_remote_media: bool,
) -> Result<()> {
    task::spawn(async move {
        let conn = match Connection::session().await {
            Ok(c) => c,
            Err(e) => {
                crate::log::log_error_message(&format!("Failed to connect to D-Bus: {}", e));
                return;
            }
        };
        
        // Create match rule for MPRIS PropertiesChanged signals
        let rule = MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .interface("org.freedesktop.DBus.Properties")
            .unwrap()
            .member("PropertiesChanged")
            .unwrap()
            .path_namespace("/org/mpris/MediaPlayer2")
            .unwrap()
            .build();
        
        // Subscribe to matching signals
        let mut stream = MessageStream::for_match_rule(
            rule,
            &conn,
            None, // No message queue size limit
        ).await.unwrap();
        
        let mut media_playing = false;
        
        // Also do an initial check
        let any_playing = check_media_playing(ignore_remote_media);
        if any_playing {
            let mut mgr = manager.lock().await;
            mgr.pause(false).await;
            media_playing = true;
        }
        
        loop {
            // Wait for D-Bus signal - 0% CPU while waiting!
            if let Some(_msg) = stream.next().await {
                // Check all players when we get a PropertiesChanged signal
                let any_playing = check_media_playing(ignore_remote_media);
                
                let mut mgr = manager.lock().await;
                if any_playing && !media_playing {
                    mgr.pause(false).await;
                    media_playing = true;
                } else if !any_playing && media_playing {
                    mgr.resume(false).await;
                    media_playing = false;
                }
            }
        }
    });
    Ok(())
}

fn check_media_playing(ignore_remote_media: bool) -> bool {
    match PlayerFinder::new() {
        Ok(finder) => match finder.find_all() {
            Ok(players) => players.iter().any(|player| {
                let identity = player.identity();
                let bus_name = player.bus_name().to_string();
                let is_playing = player.get_playback_status()
                    .map(|s| s == PlaybackStatus::Playing)
                    .unwrap_or(false);
                
                if !is_playing { return false; }
                
                if ignore_remote_media {
                    !IGNORED_PLAYERS.iter().any(|s| identity.contains(s) || bus_name.contains(s))
                } else {
                    true
                }
            }),
            Err(_) => false,
        },
        Err(_) => false,
    }
}

