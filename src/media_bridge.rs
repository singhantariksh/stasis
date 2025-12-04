use std::sync::Arc;
use tokio::sync::Notify;

/// Encapsulates all state related to the browser media bridge
#[derive(Debug)]
pub struct MediaBridgeState {
    /// Whether the media bridge (browser extension) is currently active
    pub active: bool,
    
    /// Whether any browser tabs are currently playing media
    pub browser_playing: bool,
    
    /// Number of browser tabs currently playing media
    pub playing_tab_count: usize,
    
    /// Notify signal for browser media state changes
    pub notify: Arc<Notify>,
}

impl Default for MediaBridgeState {
    fn default() -> Self {
        Self {
            active: false,
            browser_playing: false,
            playing_tab_count: 0,
            notify: Arc::new(Notify::new()),
        }
    }
}

impl MediaBridgeState {
    /// Create a new MediaBridgeState
    pub fn new() -> Self {
        Self::default()
    }
    
    /// Check if the bridge is active and browser media is playing
    pub fn is_playing(&self) -> bool {
        self.active && self.browser_playing
    }
    
    /// Get the number of media inhibitors from the browser
    pub fn inhibitor_count(&self) -> usize {
        if self.active {
            self.playing_tab_count
        } else {
            0
        }
    }
    
    /// Reset the bridge state (called when stopping the monitor)
    pub fn reset(&mut self) {
        self.active = false;
        self.browser_playing = false;
        self.playing_tab_count = 0;
    }
    
    /// Activate the bridge and initialize tracking
    pub fn activate(&mut self) {
        self.active = true;
        self.playing_tab_count = 0;
        self.browser_playing = false;
    }
    
    /// Update the playing state from browser
    pub fn update_playing_state(&mut self, _playing: bool, tab_count: usize) -> i32 {
        let prev_count = self.playing_tab_count;
        let delta = tab_count as i32 - prev_count as i32;
        
        self.playing_tab_count = tab_count;
        self.browser_playing = tab_count > 0;
        
        delta
    }
    
    /// Log current state for debugging
    pub fn log_state(&self, mpris_playing: bool, total_inhibitors: u32) {
        crate::log::log_message(&format!(
            "Media State: bridge_active={}, browser_playing={} (tabs={}), mpris_playing={}, total_inhibitors={}",
            self.active,
            self.browser_playing,
            self.playing_tab_count,
            mpris_playing,
            total_inhibitors
        ));
    }
}
