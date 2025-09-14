#!/usr/bin/env python3
"""
Event emitter for communication between Python executor and Tauri.
"""

import json
import sys
import time
import base64
from typing import Any, Dict, Optional
from io import BytesIO
from pathlib import Path

# Try to import PIL for screenshot handling
try:
    from PIL import Image
    HAS_PIL = True
except ImportError:
    HAS_PIL = False


class EventEmitter:
    """Handles event emission to Tauri backend."""
    
    def __init__(self, debug: bool = False):
        self.debug = debug
        self.event_count = 0
    
    def emit(self, event_type: str, data: Dict[str, Any]):
        """Emit an event to Tauri."""
        self.event_count += 1
        event = {
            "type": "event",
            "event": event_type,
            "timestamp": time.time(),
            "sequence": self.event_count,
            "data": data
        }
        
        if self.debug:
            # In debug mode, also write to stderr
            print(f"[EVENT] {event_type}: {data}", file=sys.stderr)
        
        # Write to stdout for Tauri to capture
        print(json.dumps(event), flush=True)
    
    def log(self, level: str, message: str, details: Optional[Dict] = None):
        """Emit a log event."""
        log_data = {
            "level": level,
            "message": message
        }
        if details:
            log_data["details"] = details
        
        self.emit("log", log_data)
    
    def error(self, message: str, exception: Optional[Exception] = None):
        """Emit an error event."""
        error_data = {
            "message": message
        }
        
        if exception:
            error_data["exception"] = {
                "type": type(exception).__name__,
                "message": str(exception)
            }
            
            # Include traceback if available
            import traceback
            error_data["traceback"] = traceback.format_exc()
        
        self.emit("error", error_data)
    
    def progress(self, current: int, total: int, message: str = ""):
        """Emit a progress event."""
        self.emit("progress", {
            "current": current,
            "total": total,
            "percentage": (current / total * 100) if total > 0 else 0,
            "message": message
        })
    
    def screenshot(self, image_path: Optional[str] = None, image_data: Optional[bytes] = None, 
                  metadata: Optional[Dict] = None):
        """Emit a screenshot event with base64 encoded image."""
        if not HAS_PIL:
            self.log("warning", "PIL not available, cannot send screenshot")
            return
        
        screenshot_data = {}
        
        try:
            if image_path:
                # Load image from file
                with open(image_path, 'rb') as f:
                    image_data = f.read()
                screenshot_data["source"] = str(image_path)
            
            if image_data:
                # Encode as base64
                encoded = base64.b64encode(image_data).decode('utf-8')
                screenshot_data["data"] = f"data:image/png;base64,{encoded}"
                screenshot_data["size"] = len(image_data)
            
            if metadata:
                screenshot_data.update(metadata)
            
            self.emit("screenshot", screenshot_data)
            
        except Exception as e:
            self.error(f"Failed to emit screenshot: {str(e)}", e)
    
    def state_change(self, from_state: Optional[str], to_state: str, 
                    transition: Optional[str] = None):
        """Emit a state change event."""
        self.emit("state_changed", {
            "from_state": from_state,
            "to_state": to_state,
            "transition": transition
        })
    
    def action_start(self, action_id: str, action_type: str, 
                    target: Optional[Dict] = None):
        """Emit an action start event."""
        self.emit("action_started", {
            "action_id": action_id,
            "action_type": action_type,
            "target": target
        })
    
    def action_complete(self, action_id: str, success: bool, 
                       result: Optional[Any] = None, duration_ms: Optional[float] = None):
        """Emit an action completion event."""
        self.emit("action_completed", {
            "action_id": action_id,
            "success": success,
            "result": result,
            "duration_ms": duration_ms
        })
    
    def process_start(self, process_id: str, process_name: str, 
                     action_count: int = 0):
        """Emit a process start event."""
        self.emit("process_started", {
            "process_id": process_id,
            "process_name": process_name,
            "action_count": action_count
        })
    
    def process_complete(self, process_id: str, success: bool, 
                        duration_ms: Optional[float] = None):
        """Emit a process completion event."""
        self.emit("process_completed", {
            "process_id": process_id,
            "success": success,
            "duration_ms": duration_ms
        })
    
    def match_found(self, image_id: str, location: Dict[str, int], 
                   confidence: float):
        """Emit an image match found event."""
        self.emit("match_found", {
            "image_id": image_id,
            "location": location,
            "confidence": confidence
        })
    
    def execution_status(self, status: str, details: Optional[Dict] = None):
        """Emit execution status update."""
        status_data = {"status": status}
        if details:
            status_data.update(details)
        
        self.emit("execution_status", status_data)


# Global emitter instance
_emitter: Optional[EventEmitter] = None


def get_emitter(debug: bool = False) -> EventEmitter:
    """Get or create the global event emitter."""
    global _emitter
    if _emitter is None:
        _emitter = EventEmitter(debug=debug)
    return _emitter


def emit_event(event_type: str, data: Dict[str, Any]):
    """Convenience function to emit events."""
    get_emitter().emit(event_type, data)


def log(level: str, message: str, details: Optional[Dict] = None):
    """Convenience function to emit logs."""
    get_emitter().log(level, message, details)