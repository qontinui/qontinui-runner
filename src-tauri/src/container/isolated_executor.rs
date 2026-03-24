use tracing::{info, warn};

use super::container_config::ContainerConfig;
use super::docker_client::{ContainerResult, DockerManager};

pub struct IsolatedExecutor {
    docker: std::sync::Arc<DockerManager>,
    config: ContainerConfig,
}

impl IsolatedExecutor {
    pub fn new(docker: std::sync::Arc<DockerManager>, config: ContainerConfig) -> Self {
        Self { docker, config }
    }

    pub fn is_available(&self) -> bool {
        self.config.enabled && self.docker.is_available()
    }

    /// Execute a command in a container
    pub async fn execute(
        &self,
        command: &str,
        working_directory: Option<&str>,
    ) -> Result<ContainerResult, String> {
        if !self.docker.is_available() {
            return Err("Docker is not available".to_string());
        }

        info!(
            "Executing in container: {}",
            &command[..command.len().min(100)]
        );
        self.docker
            .execute_in_container(command, &self.config, working_directory)
            .await
    }

    /// Execute with fallback to host execution.
    /// Returns Ok(Some(result)) if container execution succeeded,
    /// Ok(None) if container not available (caller should fall back to host),
    /// Err if container execution failed.
    pub async fn try_execute(
        &self,
        command: &str,
        working_directory: Option<&str>,
    ) -> Result<Option<ContainerResult>, String> {
        if !self.is_available() {
            return Ok(None); // Signal to use host execution
        }

        match self.execute(command, working_directory).await {
            Ok(result) => Ok(Some(result)),
            Err(e) => {
                warn!(
                    "Container execution failed, caller should fall back to host: {}",
                    e
                );
                Ok(None) // Signal fallback
            }
        }
    }
}
