#!/usr/bin/env python3
"""
Native messaging host for Firefox Per-Tab MPRIS Bridge
Creates MPRIS D-Bus players for each tab with media
Provides a UNIX socket API for querying tab state
"""

import sys
import json
import struct
import threading
import socket
import os
from typing import Dict, Optional
from pathlib import Path

# Check if pydbus is available
try:
    from pydbus import SessionBus
    from gi.repository import GLib
    DBUS_AVAILABLE = True
except ImportError:
    print("Warning: pydbus not installed. Install with: pip install pydbus", file=sys.stderr)
    DBUS_AVAILABLE = False

# Single socket for all communication
QUERY_SOCKET = os.path.expanduser("/tmp/media_bridge.sock")

def debug_print(msg):
    """Print debug message with timestamp"""
    import datetime
    timestamp = datetime.datetime.now().strftime("%H:%M:%S.%f")[:-3]
    print(f"[{timestamp}] {msg}", file=sys.stderr, flush=True)

class MPRISPlayer:
    """Represents a single MPRIS player for one tab"""
    def __init__(self, tab_id: int, send_callback):
        self.tab_id = tab_id
        self.send = send_callback
        self.metadata = {}
        self.playback_status = "Stopped"
        self.position = 0
        self.bus_name = None
        
        if DBUS_AVAILABLE:
            self.setup_dbus()
    
    def setup_dbus(self):
        try:
            bus = SessionBus()
            self.bus_name = f"org.mpris.MediaPlayer2.firefox_tab_{self.tab_id}"
            debug_print(f"Would register {self.bus_name}")
        except Exception as e:
            debug_print(f"D-Bus setup error: {e}")
    
    def update_state(self, state: dict):
        old_status = self.playback_status
        self.playback_status = "Playing" if state.get("playing") else "Paused"
        self.position = state.get("position", 0)
        
        self.metadata = {
            "xesam:title": state.get("title", "Unknown"),
            "xesam:artist": [state.get("artist", "Unknown")],
            "xesam:album": state.get("album", ""),
            "mpris:length": int(state.get("duration", 0) * 1000000),
        }
        
        # Only log if status actually changed
        if old_status != self.playback_status:
            status_emoji = "▶️" if self.playback_status == "Playing" else "⏸️"
            debug_print(f"{status_emoji} Tab {self.tab_id}: {old_status} → {self.playback_status}")
    
    def is_playing(self) -> bool:
        return self.playback_status == "Playing"
    
    def play(self):
        self.send({"command": "play", "tabId": self.tab_id})
    
    def pause(self):
        self.send({"command": "pause", "tabId": self.tab_id})
    
    def play_pause(self):
        self.send({"command": "playPause", "tabId": self.tab_id})
    
    def stop(self):
        self.send({"command": "stop", "tabId": self.tab_id})
    
    def next(self):
        self.send({"command": "next", "tabId": self.tab_id})
    
    def previous(self):
        self.send({"command": "previous", "tabId": self.tab_id})
    
    def cleanup(self):
        if DBUS_AVAILABLE and self.bus_name:
            debug_print(f"Cleaning up {self.bus_name}")

class MPRISBridge:
    """Main bridge coordinating between Firefox and external queries"""
    def __init__(self):
        self.players: Dict[int, MPRISPlayer] = {}
        self.lock = threading.Lock()
        self._start_query_server()
    
    def _start_query_server(self):
        """Start a background UNIX socket server for external queries"""
        threading.Thread(target=self._run_query_server, daemon=True).start()
    
    def _run_query_server(self):
        if Path(QUERY_SOCKET).exists():
            os.unlink(QUERY_SOCKET)
        
        server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        server.bind(QUERY_SOCKET)
        server.listen(5)
        debug_print(f"Query server listening on {QUERY_SOCKET}")
        
        while True:
            try:
                conn, _ = server.accept()
                with conn:
                    data = conn.recv(1024)
                    if not data:
                        continue
                    cmd = data.decode().strip()
                    response = self._handle_query(cmd)
                    conn.sendall(json.dumps(response).encode())
            except Exception as e:
                debug_print(f"Query server error: {e}")
    
    def _handle_query(self, cmd: str):
        """Handle queries from Rust or other clients"""
        with self.lock:
            parts = cmd.split()
            if not parts:
                return {"error": "empty command"}
            
            if parts[0] == "status":
                # Return overall status
                any_playing = any(p.is_playing() for p in self.players.values())
                tab_count = len(self.players)
                playing_tabs = [tid for tid, p in self.players.items() if p.is_playing()]
                return {
                    "playing": any_playing,
                    "tab_count": tab_count,
                    "playing_tabs": playing_tabs
                }
            
            elif parts[0] == "list_tabs":
                # Return detailed info about all tabs
                return {
                    str(tid): {
                        "playing": p.is_playing(),
                        "title": p.metadata.get("xesam:title", "Unknown"),
                        "artist": p.metadata.get("xesam:artist", ["Unknown"])[0],
                        "status": p.playback_status
                    }
                    for tid, p in self.players.items()
                }
            
            elif parts[0] == "tab_action" and len(parts) >= 3:
                # Control a specific tab
                try:
                    tab_id = int(parts[1])
                    action = parts[2]
                    player = self.players.get(tab_id)
                    if not player:
                        return {"error": "tab not found"}
                    getattr(player, action, lambda: None)()
                    return {"ok": True}
                except Exception as e:
                    return {"error": str(e)}
            
            else:
                return {"error": "unknown command"}
    
    def read_msg(self) -> Optional[dict]:
        try:
            raw_len = sys.stdin.buffer.read(4)
            if not raw_len or len(raw_len) < 4:
                return None
            length = struct.unpack("I", raw_len)[0]
            data = sys.stdin.buffer.read(length)
            if len(data) < length:
                debug_print(f"Warning: incomplete message")
                return None
            return json.loads(data.decode("utf-8"))
        except Exception as e:
            debug_print(f"Error reading message: {e}")
            return None
    
    def send_msg(self, obj: dict):
        try:
            encoded = json.dumps(obj).encode("utf-8")
            sys.stdout.buffer.write(struct.pack("I", len(encoded)))
            sys.stdout.buffer.write(encoded)
            sys.stdout.buffer.flush()
        except Exception as e:
            debug_print(f"Error sending message: {e}")
    
    def handle_message(self, msg: dict):
        msg_type = msg.get("type")
        tab_id = msg.get("tabId")
        
        if msg_type == "mediaState" and tab_id is not None:
            with self.lock:
                if tab_id not in self.players:
                    self.players[tab_id] = MPRISPlayer(tab_id, self.send_msg)
                    debug_print(f"Created player for tab {tab_id}")
                self.players[tab_id].update_state(msg)
        
        elif msg_type == "tabClosed" and tab_id is not None:
            debug_print(f"← Tab {tab_id} closed")
            with self.lock:
                if tab_id in self.players:
                    self.players[tab_id].cleanup()
                    del self.players[tab_id]
                    debug_print(f"➖ Removed player for tab {tab_id}")
        
        else:
            debug_print(f"Unknown message type: {msg_type}")
        
        self.send_msg({"ok": True})
    
    def run(self):
        debug_print("=" * 60)
        debug_print("MPRIS Bridge started")
        debug_print(f"Socket: {QUERY_SOCKET}")
        debug_print("=" * 60)
        
        while True:
            msg = self.read_msg()
            if msg is None:
                debug_print("⚠ No message received, exiting")
                break
            self.handle_message(msg)
        
        debug_print("MPRIS Bridge stopped")

def main():
    bridge = MPRISBridge()
    try:
        bridge.run()
    except KeyboardInterrupt:
        debug_print("Interrupted")
    except Exception as e:
        debug_print(f"✗ Fatal error: {e}")
        import traceback
        traceback.print_exc(file=sys.stderr)
        sys.exit(1)
    finally:
        # Cleanup socket on exit
        if Path(QUERY_SOCKET).exists():
            os.unlink(QUERY_SOCKET)

if __name__ == "__main__":
    main()
