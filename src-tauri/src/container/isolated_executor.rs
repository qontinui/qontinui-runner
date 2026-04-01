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
        self.execute_with_policy(command, working_directory, None).await
    }

    /// Execute a command in a container with an optional security policy overlay.
    ///
    /// When a `SecurityPolicy` is provided, the base container config is transformed
    /// via `policy_to_container_config` to apply security hardening (cap_drop, seccomp,
    /// non-root user, PID limits, etc.) before execution.
    pub async fn execute_with_policy(
        &self,
        command: &str,
        working_directory: Option<&str>,
        security_policy: Option<&crate::security::SecurityPolicy>,
    ) -> Result<ContainerResult, String> {
        if !self.docker.is_available() {
            return Err("Docker is not available".to_string());
        }

        let effective_config = if let Some(policy) = security_policy {
            crate::security::policy_to_container_config(policy, &self.config)
        } else {
            self.config.clone()
        };

        info!(
            "Executing in container: {} (profile={})",
            &command[..command.len().min(100)],
            security_policy.map(|p| p.profile_name.as_str()).unwrap_or("none"),
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
        self.try_execute_with_policy(command, working_directory, None).await
    }

    /// Execute with fallback to host execution and optional security policy overlay.
    pub async fn try_execute_with_policy(
        &self,
        command: &str,
        working_directory: Option<&str>,
        security_policy: Option<&crate::security::SecurityPolicy>,
    ) -> Result<Option<ContainerResult>, String> {
        if !self.is_available() {
            return Ok(None); // Signal to use host execution
        }

        match self.execute_with_policy(command, working_directory, security_policy).await {
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
