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

    /// Execute a command in a container.
    pub async fn execute(
        &self,
        command: &str,
        working_directory: Option<&str>,
    ) -> Result<ContainerResult, String> {
        self.execute_with_policy(command, working_directory, None, &[]).await
    }

    /// Execute a command in a container with an optional security policy overlay
    /// and additional environment variables (proxy URLs, credential placeholders).
    pub async fn execute_with_policy(
        &self,
        command: &str,
        working_directory: Option<&str>,
        security_policy: Option<&crate::security::SecurityPolicy>,
        extra_env: &[String],
    ) -> Result<ContainerResult, String> {
        if !self.docker.is_available() {
            return Err("Docker is not available".to_string());
        }

        let mut effective_config = if let Some(policy) = security_policy {
            crate::security::policy_to_container_config(policy, &self.config)
        } else {
            self.config.clone()
        };

        // Inject extra environment variables (proxy URLs, credential placeholders)
        if !extra_env.is_empty() {
            effective_config.extra_env.extend(extra_env.iter().cloned());
        }

        info!(
            "Executing in container: {} (profile={}, extra_env={})",
            &command[..command.len().min(100)],
            security_policy.map(|p| p.profile_name.as_str()).unwrap_or("none"),
            extra_env.len(),
        );
        self.docker
            .execute_in_container(command, &effective_config, working_directory)
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
        self.try_execute_with_policy(command, working_directory, None, &[]).await
    }

    /// Execute with fallback to host execution and optional security policy overlay.
    pub async fn try_execute_with_policy(
        &self,
        command: &str,
        working_directory: Option<&str>,
        security_policy: Option<&crate::security::SecurityPolicy>,
        extra_env: &[String],
    ) -> Result<Option<ContainerResult>, String> {
        if !self.is_available() {
            return Ok(None); // Signal to use host execution
        }

        match self.execute_with_policy(command, working_directory, security_policy, extra_env).await {
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
