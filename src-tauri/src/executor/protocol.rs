use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Write;
use tokio::sync::mpsc;
use tracing::{debug, error, info};

/// Channel capacity for command queue
pub const COMMAND_CHANNEL_CAPACITY: usize = 100;

/// Command sent to Python executor
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutorCommand {
    #[serde(rename = "type")]
    pub cmd_type: String,
    pub id: String,
    pub command: String,
    pub params: Option<Value>,
}

/// Response from Python executor
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutorResponse {
    #[serde(rename = "type")]
    pub resp_type: String,
    pub id: String,
    pub success: bool,
    pub data: Option<Value>,
    pub error: Option<String>,
}

/// Event from Python executor
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutorEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    pub event: String,
    pub timestamp: f64,
    pub sequence: u32,
    pub data: Value,
}

/// Handles command/response protocol with Python executor
pub struct ProtocolHandler {
    command_tx: Option<mpsc::Sender<ExecutorCommand>>,
}

impl ProtocolHandler {
    pub fn new() -> Self {
        Self { command_tx: None }
    }

    /// Creates a command channel and returns sender and receiver
    pub fn create_channel() -> (
        mpsc::Sender<ExecutorCommand>,
        mpsc::Receiver<ExecutorCommand>,
    ) {
        mpsc::channel::<ExecutorCommand>(COMMAND_CHANNEL_CAPACITY)
    }

    /// Sets the command sender
    pub fn set_sender(&mut self, tx: mpsc::Sender<ExecutorCommand>) {
        self.command_tx = Some(tx);
    }

    /// Gets a clone of the command sender
    pub fn get_sender(&self) -> Option<mpsc::Sender<ExecutorCommand>> {
        self.command_tx.clone()
    }

    /// Sends a command to Python executor
    pub async fn send_command(&self, cmd: ExecutorCommand) -> Result<(), String> {
        if let Some(ref tx) = self.command_tx {
            tx.send(cmd)
                .await
                .map_err(|e| format!("Failed to send command: {}", e))?;
            Ok(())
        } else {
            Err("Command channel not initialized".to_string())
        }
    }

    /// Command sender task - reads from channel and writes to stdin
    pub async fn command_sender_task(
        mut stdin: std::process::ChildStdin,
        mut cmd_rx: mpsc::Receiver<ExecutorCommand>,
    ) {
        while let Some(cmd) = cmd_rx.recv().await {
            debug!("Command sender received command: {}", cmd.command);
            match serde_json::to_string(&cmd) {
                Ok(json) => {
                    debug!("Serialized command to JSON, length: {} bytes", json.len());
                    debug!(
                        "JSON payload (first 200 chars): {}",
                        &json.chars().take(200).collect::<String>()
                    );

                    if let Err(e) = writeln!(stdin, "{}", json) {
                        error!("Failed to write command to stdin: {}", e);
                        break;
                    }

                    if let Err(e) = stdin.flush() {
                        error!("Failed to flush stdin: {}", e);
                        break;
                    }

                    debug!(
                        "Successfully wrote command '{}' to Python stdin and flushed",
                        cmd.command
                    );
                }
                Err(e) => {
                    error!("Failed to serialize command: {}", e);
                }
            }
        }

        info!("Command sender task ending");
    }
}

impl Default for ProtocolHandler {
    fn default() -> Self {
        Self::new()
    }
}
