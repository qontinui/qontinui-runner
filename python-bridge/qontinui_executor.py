#!/usr/bin/env python3
"""
Qontinui executor that integrates with the actual Qontinui library.
"""

import sys
import json
import time
import traceback
import threading
from typing import Optional, Dict, Any, List
from enum import Enum
from pathlib import Path
import base64
import tempfile
import os

# Add parent directory to path to import qontinui
sys.path.insert(0, str(Path(__file__).parent.parent.parent))

try:
    from qontinui import (
        QontinuiStateManager,
        State,
        StateTraversal,
        StateTransition,
        Find,
        Image,
        Region,
        Location,
        FluentActions,
        ActionChain,
    )
    QONTINUI_AVAILABLE = True
except ImportError:
    QONTINUI_AVAILABLE = False
    print(json.dumps({
        "type": "event",
        "event": "error",
        "timestamp": time.time(),
        "sequence": 0,
        "data": {
            "message": "Qontinui library not available. Please install qontinui package.",
            "details": "Run: pip install -e /home/jspinak/qontinui_parent_directory/qontinui"
        }
    }), flush=True)


class EventType(Enum):
    """Event types for communication with Tauri."""
    READY = "ready"
    CONFIG_LOADED = "config_loaded"
    EXECUTION_STARTED = "execution_started"
    STATE_CHANGED = "state_changed"
    ACTION_STARTED = "action_started"
    ACTION_COMPLETED = "action_completed"
    PROCESS_STARTED = "process_started"
    PROCESS_COMPLETED = "process_completed"
    EXECUTION_COMPLETED = "execution_completed"
    ERROR = "error"
    LOG = "log"
    MATCH_FOUND = "match_found"
    SCREENSHOT_TAKEN = "screenshot_taken"


class QontinuiExecutor:
    """Executor that uses the Qontinui library for real automation."""
    
    def __init__(self):
        self.config = None
        self.state_manager = None
        self.states = {}
        self.transitions = {}
        self.processes = {}
        self.images = {}
        self.current_state = None
        self.is_running = False
        self._sequence = 0
        self.temp_dir = None
        
        if QONTINUI_AVAILABLE:
            self.state_manager = QontinuiStateManager()
            self.actions = FluentActions()
        
        self._emit_event(EventType.READY, {
            "message": "Qontinui executor initialized",
            "library_available": QONTINUI_AVAILABLE
        })
    
    def _emit_event(self, event_type: EventType, data: Dict[str, Any]):
        """Emit event to Tauri through stdout."""
        event = {
            "type": "event",
            "event": event_type.value,
            "timestamp": time.time(),
            "sequence": self._sequence,
            "data": data
        }
        self._sequence += 1
        print(json.dumps(event), flush=True)
    
    def _emit_log(self, level: str, message: str):
        """Emit log message."""
        self._emit_event(EventType.LOG, {
            "level": level,
            "message": message
        })
    
    def _process_special_keys(self, text: str) -> str:
        """Process special key placeholders in text.
        
        Converts placeholders like {ENTER}, {TAB}, etc. to actual key values.
        """
        # Simple mappings for common special keys
        replacements = {
            "{ENTER}": "\n",
            "{TAB}": "\t",
            "{SPACE}": " ",
            "{BACKSPACE}": "\b",
            # For complex keys, we'll need to handle them separately
            # For now, just remove the placeholders
            "{DELETE}": "",  # TODO: Handle DELETE key
            "{ESCAPE}": "",  # TODO: Handle ESCAPE key
            "{UP}": "",      # TODO: Handle arrow keys
            "{DOWN}": "",
            "{LEFT}": "",
            "{RIGHT}": "",
            "{HOME}": "",
            "{END}": "",
            "{PAGE_UP}": "",
            "{PAGE_DOWN}": "",
            "{INSERT}": "",
            # Function keys
            "{F1}": "", "{F2}": "", "{F3}": "", "{F4}": "",
            "{F5}": "", "{F6}": "", "{F7}": "", "{F8}": "",
            "{F9}": "", "{F10}": "", "{F11}": "", "{F12}": "",
            # Key combos - these need special handling
            "{CTRL+A}": "",  # TODO: Handle key combinations
            "{CTRL+C}": "",
            "{CTRL+V}": "",
            "{CTRL+X}": "",
            "{CTRL+Z}": "",
            "{CTRL+S}": "",
            "{ALT+TAB}": "",
            "{ALT+F4}": "",
        }
        
        result = text
        for placeholder, replacement in replacements.items():
            result = result.replace(placeholder, replacement)
        
        # Log if we had to skip any complex keys
        if any(key in text for key in ["{DELETE}", "{ESCAPE}", "{UP}", "{DOWN}", 
                                        "{CTRL+", "{ALT+", "{F1", "{F2", "{F3"]):
            self._emit_log("warning", "Some special keys are not yet fully supported and were skipped")
        
        return result
    
    def load_configuration(self, config_path: str) -> bool:
        """Load configuration from file and set up Qontinui states."""
        try:
            self._emit_log("info", f"Loading configuration from: {config_path}")
            
            with open(config_path, 'r') as f:
                self.config = json.load(f)
            
            if not QONTINUI_AVAILABLE:
                self._emit_event(EventType.ERROR, {
                    "message": "Cannot load configuration without Qontinui library"
                })
                return False
            
            # Create temp directory for images
            self.temp_dir = tempfile.mkdtemp(prefix="qontinui_")
            
            # Process images - save to temp files
            for img_data in self.config.get("images", []):
                img_id = img_data.get("id")
                img_base64 = img_data.get("data", "")
                img_name = img_data.get("name", f"{img_id}.png")
                
                # Decode base64 and save to temp file
                img_path = os.path.join(self.temp_dir, img_name)
                try:
                    img_bytes = base64.b64decode(img_base64)
                    with open(img_path, 'wb') as f:
                        f.write(img_bytes)
                    
                    # Create Qontinui Image object
                    self.images[img_id] = Image(img_path)
                    self._emit_log("debug", f"Loaded image: {img_id} -> {img_path}")
                except Exception as e:
                    self._emit_log("error", f"Failed to load image {img_id}: {e}")
            
            # Process states
            for state_data in self.config.get("states", []):
                state_id = state_data.get("id")
                state = State(
                    name=state_data.get("name", state_id),
                    description=state_data.get("description", "")
                )
                
                # Add identifying images to state
                for img_info in state_data.get("identifyingImages", []):
                    # Handle both string IDs and object format
                    if isinstance(img_info, str):
                        img_id = img_info
                    else:
                        img_id = img_info.get("image") or img_info.get("imageId")
                    
                    if img_id and img_id in self.images:
                        state.add_image(self.images[img_id])
                
                self.states[state_id] = state
                self.state_manager.add_state(state)
                
                # Set initial state
                if state_data.get("isInitial"):
                    self.current_state = state_id
                    self.state_manager.set_current_state(state)
            
            # Process processes (action sequences)
            for process_data in self.config.get("processes", []):
                process_id = process_data.get("id")
                actions = process_data.get("actions", [])
                self.processes[process_id] = actions
            
            # Process transitions
            for trans_data in self.config.get("transitions", []):
                trans_id = trans_data.get("id")
                trans_type = trans_data.get("type")
                
                if trans_type == "FromTransition":
                    from_state_id = trans_data.get("fromState")
                    to_state_id = trans_data.get("toState")
                    
                    if from_state_id in self.states and to_state_id in self.states:
                        # Store transition data directly for simpler processing
                        self.transitions[trans_id] = {
                            "type": "FromTransition",
                            "from_state": from_state_id,
                            "to_state": to_state_id,
                            "processes": trans_data.get("processes", []),
                            "name": trans_data.get("name", trans_id)
                        }
                elif trans_type == "ToTransition":
                    to_state_id = trans_data.get("toState")
                    if to_state_id in self.states:
                        self.transitions[trans_id] = {
                            "type": "ToTransition",
                            "to_state": to_state_id,
                            "processes": trans_data.get("processes", []),
                            "name": trans_data.get("name", trans_id)
                        }
            
            config_info = {
                "path": config_path,
                "version": self.config.get("version", "unknown"),
                "name": self.config.get("metadata", {}).get("name", "Unnamed"),
                "states": len(self.states),
                "processes": len(self.processes),
                "transitions": len(self.transitions),
                "images": len(self.images)
            }
            self._emit_event(EventType.CONFIG_LOADED, config_info)
            return True
                
        except Exception as e:
            self._emit_event(EventType.ERROR, {
                "message": "Exception loading configuration",
                "details": str(e),
                "traceback": traceback.format_exc()
            })
            return False
    
    def _execute_action(self, action_data: Dict[str, Any]) -> bool:
        """Execute a single action using Qontinui."""
        action_type = action_data.get("type")
        config = action_data.get("config", {})
        
        # Handle missing actions library
        if not QONTINUI_AVAILABLE or not hasattr(self, 'actions'):
            self._emit_log("warning", f"Simulating action: {action_type}")
            time.sleep(0.5)  # Simulate action delay
            return True
        
        try:
            self._emit_event(EventType.ACTION_STARTED, {
                "action_id": action_data.get("id"),
                "action_type": action_type
            })
            
            if action_type == "CLICK":
                target = config.get("target", {})
                if target.get("type") == "image":
                    image_id = target.get("imageId")
                    if image_id in self.images:
                        # Find image on screen
                        matches = Find(self.images[image_id]).find_all()
                        if matches:
                            # Click on first match
                            location = matches[0].location
                            self.actions.click(location)
                            self._emit_log("info", f"Clicked at {location}")
                        else:
                            self._emit_log("warning", f"Image {image_id} not found on screen")
                            return False
                elif target.get("type") == "coordinates":
                    x = target.get("x", 0)
                    y = target.get("y", 0)
                    location = Location(x, y)
                    self.actions.click(location)
                    self._emit_log("info", f"Clicked at ({x}, {y})")
            
            elif action_type == "TYPE":
                text = config.get("text", "")
                clear_before = config.get("clear_before", False)
                press_enter = config.get("press_enter", False)
                pause_before_begin = config.get("pause_before_begin", 0) / 1000.0  # Convert ms to seconds
                pause_after_end = config.get("pause_after_end", 0) / 1000.0  # Convert ms to seconds
                
                # Apply pause before beginning (matches Qontinui's pause_before_begin)
                if pause_before_begin > 0:
                    self._emit_log("debug", f"Pausing {pause_before_begin}s before typing")
                    time.sleep(pause_before_begin)
                
                # Clear existing text if requested
                if clear_before:
                    # Standard approach: Select all text (Ctrl+A) then type over it
                    # This works across most applications and operating systems
                    self._emit_log("info", "Clearing text field (Ctrl+A)")
                    if hasattr(self.actions, 'key_combo'):
                        self.actions.key_combo(['ctrl', 'a'])
                    elif hasattr(self.actions, 'hotkey'):
                        self.actions.hotkey('ctrl', 'a')
                    else:
                        # Fallback: Try to select all text manually
                        self._emit_log("warning", "Key combo not available, attempting manual select-all")
                        # This is a simplified fallback - actual implementation would depend on the library
                        pass
                    time.sleep(0.1)  # Small delay to ensure selection completes
                
                # Process special key placeholders
                processed_text = self._process_special_keys(text)
                if hasattr(self.actions, 'type_text'):
                    self.actions.type_text(processed_text)
                elif hasattr(self.actions, 'type'):
                    self.actions.type(processed_text)
                self._emit_log("info", f"Typed: {text}")
                
                # Note: The {ENTER} placeholder in the text is already handled by _process_special_keys
                # The press_enter flag is kept for backward compatibility but may be redundant
                # if the frontend adds {ENTER} to the text when press_enter is checked
                if press_enter and not text.endswith("{ENTER}"):
                    # Only press Enter if it wasn't already in the text as a placeholder
                    self._emit_log("info", "Pressing Enter key (from press_enter flag)")
                    if hasattr(self.actions, 'press'):
                        self.actions.press('enter')
                    elif hasattr(self.actions, 'key_press'):
                        self.actions.key_press('enter')
                    elif hasattr(self.actions, 'type_text'):
                        self.actions.type_text('\n')
                    elif hasattr(self.actions, 'type'):
                        self.actions.type('\n')
                
                # Apply pause after end (matches Qontinui's pause_after_end)
                if pause_after_end > 0:
                    self._emit_log("debug", f"Pausing {pause_after_end}s after typing")
                    time.sleep(pause_after_end)
            
            elif action_type == "WAIT":
                duration = config.get("duration", 1000) / 1000.0  # Convert ms to seconds
                time.sleep(duration)
                self._emit_log("info", f"Waited {duration} seconds")
            
            elif action_type == "FIND":
                image_id = config.get("image") or config.get("imageId")
                if image_id and image_id in self.images:
                    matches = Find(self.images[image_id]).find_all()
                    if matches:
                        self._emit_log("info", f"Found {len(matches)} matches for image {image_id}")
                        self._emit_event(EventType.MATCH_FOUND, {
                            "image_id": image_id,
                            "matches": len(matches)
                        })
                    else:
                        self._emit_log("warning", f"Image {image_id} not found on screen")
                        return False
                else:
                    self._emit_log("warning", f"Image not specified or not loaded for FIND action")
                    return False
            
            elif action_type == "SCROLL":
                direction = config.get("direction", "down")
                amount = config.get("amount", 3)
                # Simple scroll simulation
                self._emit_log("info", f"Scrolling {direction} by {amount} units")
                time.sleep(0.5)  # Simulate scroll time
            
            elif action_type == "GO_TO_STATE":
                state_id = config.get("state")
                if state_id and state_id in self.states:
                    self.current_state = state_id
                    if QONTINUI_AVAILABLE:
                        self.state_manager.set_current_state(self.states[state_id])
                    self._emit_event(EventType.STATE_CHANGED, {
                        "from_state": self.current_state,
                        "to_state": state_id,
                        "transition": "GO_TO_STATE action"
                    })
                    self._emit_log("info", f"Changed to state: {state_id}")
                else:
                    self._emit_log("warning", f"State {state_id} not found")
                    return False
            
            elif action_type == "RUN_PROCESS":
                process_id = config.get("process")
                if process_id:
                    self._emit_log("info", f"Running sub-process: {process_id}")
                    return self._execute_process(process_id)
                else:
                    self._emit_log("warning", "No process specified for RUN_PROCESS action")
                    return False
            
            elif action_type == "VANISH":
                image_id = config.get("image") or config.get("imageId")
                timeout = config.get("timeout", 5000) / 1000.0  # Convert to seconds
                check_interval = config.get("check_interval", 500) / 1000.0
                
                if image_id and image_id in self.images:
                    start_time = time.time()
                    while time.time() - start_time < timeout:
                        matches = Find(self.images[image_id]).find_all()
                        if not matches:
                            self._emit_log("info", f"Image {image_id} has vanished")
                            return True
                        time.sleep(check_interval)
                    
                    self._emit_log("warning", f"Image {image_id} did not vanish within timeout")
                    return False
                else:
                    self._emit_log("warning", f"Image not specified for VANISH action")
            
            elif action_type == "KEY":
                key = config.get("key", "")
                self.actions.key_press(key)
                self._emit_log("info", f"Pressed key: {key}")
            
            elif action_type == "DRAG":
                from_target = config.get("from", {})
                to_target = config.get("to", {})
                
                # Get from location
                from_loc = None
                if from_target.get("type") == "image":
                    image_id = from_target.get("imageId")
                    if image_id in self.images:
                        matches = Find(self.images[image_id]).find_all()
                        if matches:
                            from_loc = matches[0].location
                elif from_target.get("type") == "coordinates":
                    from_loc = Location(from_target.get("x", 0), from_target.get("y", 0))
                
                # Get to location
                to_loc = None
                if to_target.get("type") == "image":
                    image_id = to_target.get("imageId")
                    if image_id in self.images:
                        matches = Find(self.images[image_id]).find_all()
                        if matches:
                            to_loc = matches[0].location
                elif to_target.get("type") == "coordinates":
                    to_loc = Location(to_target.get("x", 0), to_target.get("y", 0))
                
                if from_loc and to_loc:
                    self.actions.drag(from_loc, to_loc)
                    self._emit_log("info", f"Dragged from {from_loc} to {to_loc}")
                else:
                    self._emit_log("warning", "Could not find drag locations")
                    return False
            
            self._emit_event(EventType.ACTION_COMPLETED, {
                "action_id": action_data.get("id"),
                "success": True
            })
            return True
            
        except Exception as e:
            self._emit_event(EventType.ACTION_COMPLETED, {
                "action_id": action_data.get("id"),
                "success": False,
                "error": str(e)
            })
            self._emit_log("error", f"Action failed: {e}")
            return False
    
    def _execute_process(self, process_id: str) -> bool:
        """Execute a process (sequence of actions)."""
        if process_id not in self.processes:
            self._emit_log("error", f"Process {process_id} not found")
            return False
        
        self._emit_event(EventType.PROCESS_STARTED, {
            "process_id": process_id,
            "process_name": process_id
        })
        
        actions = self.processes[process_id]
        success = True
        
        for action in actions:
            if not self.is_running:
                break
            
            if not self._execute_action(action):
                success = False
                break
            
            # Small delay between actions
            time.sleep(0.5)
        
        self._emit_event(EventType.PROCESS_COMPLETED, {
            "process_id": process_id,
            "success": success
        })
        
        return success
    
    def _run_state_machine(self):
        """Run the state machine execution."""
        try:
            # Execute transitions from current state
            executed_count = 0
            max_iterations = 100  # Prevent infinite loops
            
            while self.is_running and executed_count < max_iterations:
                transition_found = False
                
                # Find transitions from current state
                for trans_id, transition in self.transitions.items():
                    if transition.get("type") == "FromTransition" and transition.get("from_state") == self.current_state:
                        self._emit_log("info", f"Executing transition: {transition.get('name')}")
                        
                        # Execute processes in transition
                        for process_id in transition.get("processes", []):
                            if not self._execute_process(process_id):
                                self._emit_log("error", f"Process {process_id} failed")
                                self.is_running = False
                                break
                        
                        if self.is_running:
                            # Change state
                            old_state = self.current_state
                            self.current_state = transition.get("to_state")
                            
                            # Update state manager if available
                            if self.current_state in self.states and QONTINUI_AVAILABLE:
                                self.state_manager.set_current_state(self.states[self.current_state])
                            
                            self._emit_event(EventType.STATE_CHANGED, {
                                "from_state": old_state,
                                "to_state": self.current_state,
                                "transition": transition.get('name')
                            })
                            
                            # Execute ToTransitions for the new state
                            for to_trans_id, to_transition in self.transitions.items():
                                if to_transition.get("type") == "ToTransition" and to_transition.get("to_state") == self.current_state:
                                    self._emit_log("info", f"Executing ToTransition: {to_transition.get('name')}")
                                    for process_id in to_transition.get("processes", []):
                                        if not self._execute_process(process_id):
                                            self._emit_log("error", f"Process {process_id} in ToTransition failed")
                            
                            # Check if final state
                            for state_data in self.config.get("states", []):
                                if state_data.get("id") == self.current_state and state_data.get("isFinal"):
                                    self._emit_log("info", "Reached final state")
                                    self.is_running = False
                                    break
                            
                            transition_found = True
                            executed_count += 1
                            break
                
                if not transition_found:
                    self._emit_log("info", "No more transitions available from current state")
                    break
            
            self._emit_event(EventType.EXECUTION_COMPLETED, {
                "success": True,
                "final_state": self.current_state,
                "transitions_executed": executed_count
            })
            
        except Exception as e:
            self._emit_event(EventType.ERROR, {
                "message": "State machine execution failed",
                "details": str(e),
                "traceback": traceback.format_exc()
            })
        finally:
            self.is_running = False
    
    def _run_process(self, process_id: str):
        """Run a specific process directly."""
        try:
            self._emit_log("info", f"Starting process execution: {process_id}")
            
            success = self._execute_process(process_id)
            
            self._emit_event(EventType.EXECUTION_COMPLETED, {
                "success": success,
                "process_id": process_id,
                "mode": "process"
            })
            
        except Exception as e:
            self._emit_event(EventType.ERROR, {
                "message": "Process execution failed",
                "details": str(e),
                "traceback": traceback.format_exc()
            })
        finally:
            self.is_running = False
    
    def start_execution(self, mode: str = "state_machine", process_id: str = None) -> bool:
        """Start automation execution."""
        if not self.config:
            self._emit_event(EventType.ERROR, {
                "message": "No configuration loaded"
            })
            return False
        
        if not QONTINUI_AVAILABLE:
            self._emit_event(EventType.ERROR, {
                "message": "Cannot execute without Qontinui library"
            })
            return False
        
        if self.is_running:
            self._emit_event(EventType.ERROR, {
                "message": "Execution already in progress"
            })
            return False
        
        try:
            self.is_running = True
            
            self._emit_event(EventType.EXECUTION_STARTED, {
                "mode": mode,
                "initial_state": self.current_state
            })
            
            if mode == "state_machine":
                # Run state machine in separate thread
                execution_thread = threading.Thread(target=self._run_state_machine)
                execution_thread.daemon = True
                execution_thread.start()
            elif mode == "process":
                # Run a specific process directly
                if not process_id:
                    self._emit_log("error", "Process ID required for process mode")
                    return False
                execution_thread = threading.Thread(target=self._run_process, args=(process_id,))
                execution_thread.daemon = True
                execution_thread.start()
            else:
                self._emit_log("error", f"Unsupported execution mode: {mode}")
                return False
            
            return True
            
        except Exception as e:
            self._emit_event(EventType.ERROR, {
                "message": "Failed to start execution",
                "details": str(e),
                "traceback": traceback.format_exc()
            })
            self.is_running = False
            return False
    
    def stop_execution(self):
        """Stop the current execution."""
        if self.is_running:
            self._emit_log("info", "Stopping execution...")
            self.is_running = False
            self._emit_event(EventType.EXECUTION_COMPLETED, {
                "success": False,
                "reason": "User stopped"
            })
    
    def handle_command(self, command: Dict[str, Any]) -> Dict[str, Any]:
        """Handle command from Tauri."""
        cmd_type = command.get("command")
        params = command.get("params", {})
        
        if cmd_type == "load":
            config_path = params.get("config_path")
            success = self.load_configuration(config_path)
            return {"success": success}
        
        elif cmd_type == "start":
            mode = params.get("mode", "state_machine")
            process_id = params.get("process_id")
            success = self.start_execution(mode, process_id)
            return {"success": success}
        
        elif cmd_type == "stop":
            self.stop_execution()
            return {"success": True}
        
        elif cmd_type == "status":
            return {
                "is_running": self.is_running,
                "current_state": self.current_state,
                "config_loaded": self.config is not None,
                "library_available": QONTINUI_AVAILABLE
            }
        
        else:
            return {
                "success": False,
                "error": f"Unknown command: {cmd_type}"
            }
    
    def __del__(self):
        """Clean up temp directory on exit."""
        if self.temp_dir and os.path.exists(self.temp_dir):
            import shutil
            try:
                shutil.rmtree(self.temp_dir)
            except:
                pass


def main():
    """Main entry point for the Qontinui executor."""
    executor = QontinuiExecutor()
    
    # Read commands from stdin
    for line in sys.stdin:
        try:
            command = json.loads(line.strip())
            
            if command.get("type") == "command":
                response = executor.handle_command(command)
                response["id"] = command.get("id")
                response["type"] = "response"
                print(json.dumps(response), flush=True)
            
        except json.JSONDecodeError as e:
            executor._emit_event(EventType.ERROR, {
                "message": "Invalid JSON command",
                "details": str(e)
            })
        except Exception as e:
            executor._emit_event(EventType.ERROR, {
                "message": "Command execution failed",
                "details": str(e),
                "traceback": traceback.format_exc()
            })


if __name__ == "__main__":
    main()