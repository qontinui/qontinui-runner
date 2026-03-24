use bollard::container::{
    Config, CreateContainerOptions, LogOutput, LogsOptions, RemoveContainerOptions,
    StartContainerOptions, WaitContainerOptions,
};
use bollard::image::CreateImageOptions;
use bollard::models::{HostConfig, Mount, MountTypeEnum};
use bollard::Docker;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::time::Instant;
use tracing::{debug, error, info, warn};

use super::container_config::ContainerConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerResult {
    pub exit_code: Option<i64>,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
    pub container_id: String,
}

pub struct DockerManager {
    client: Option<Docker>,
}

impl DockerManager {
    /// Create a new DockerManager. Attempts to connect to Docker.
    /// If Docker is unavailable, the manager is created but is_available() returns false.
    pub async fn new() -> Self {
        match Docker::connect_with_local_defaults() {
            Ok(client) => {
                // Verify connection
                match client.ping().await {
                    Ok(_) => {
                        info!("Docker connection established");
                        Self {
                            client: Some(client),
                        }
                    }
                    Err(e) => {
                        warn!("Docker ping failed (Docker may not be running): {}", e);
                        Self { client: None }
                    }
                }
            }
            Err(e) => {
                warn!("Could not connect to Docker: {}", e);
                Self { client: None }
            }
        }
    }

    pub fn is_available(&self) -> bool {
        self.client.is_some()
    }

    /// Pull an image if it's not already present
    pub async fn ensure_image(&self, image: &str) -> Result<(), String> {
        let client = self.client.as_ref().ok_or("Docker not available")?;

        // Check if image exists locally
        match client.inspect_image(image).await {
            Ok(_) => {
                debug!("Image {} already present", image);
                return Ok(());
            }
            Err(_) => {
                info!("Pulling image {}...", image);
            }
        }

        // Pull the image
        let options = CreateImageOptions {
            from_image: image,
            ..Default::default()
        };

        let mut stream = client.create_image(Some(options), None, None);
        while let Some(result) = stream.next().await {
            match result {
                Ok(info) => {
                    if let Some(status) = info.status {
                        debug!("Pull {}: {}", image, status);
                    }
                }
                Err(e) => {
                    return Err(format!("Failed to pull image {}: {}", image, e));
                }
            }
        }

        info!("Image {} pulled successfully", image);
        Ok(())
    }

    /// Execute a command in an isolated container
    pub async fn execute_in_container(
        &self,
        command: &str,
        config: &ContainerConfig,
        working_directory: Option<&str>,
    ) -> Result<ContainerResult, String> {
        let client = self.client.as_ref().ok_or("Docker not available")?;
        let start = Instant::now();

        // Ensure image is available
        self.ensure_image(&config.base_image).await?;

        // Build mounts
        let mut mounts = Vec::new();
        if let Some(work_dir) = working_directory {
            mounts.push(Mount {
                target: Some("/workspace".to_string()),
                source: Some(work_dir.to_string()),
                typ: Some(MountTypeEnum::BIND),
                read_only: Some(config.read_only_mount),
                ..Default::default()
            });
        }

        // Build host config
        let mut host_config = HostConfig {
            mounts: if mounts.is_empty() {
                None
            } else {
                Some(mounts)
            },
            ..Default::default()
        };

        // Resource limits
        if let Some(memory_mb) = config.memory_limit_mb {
            host_config.memory = Some((memory_mb * 1024 * 1024) as i64);
        }
        if let Some(cpu) = config.cpu_limit {
            host_config.nano_cpus = Some((cpu * 1_000_000_000.0) as i64);
        }

        // Network
        if !config.network_enabled {
            host_config.network_mode = Some("none".to_string());
        }

        // Create container
        let container_name = format!(
            "qontinui-exec-{}",
            uuid::Uuid::new_v4()
                .to_string()
                .split('-')
                .next()
                .unwrap_or("tmp")
        );

        let container_config = Config {
            image: Some(config.base_image.clone()),
            cmd: Some(vec![
                "sh".to_string(),
                "-c".to_string(),
                command.to_string(),
            ]),
            working_dir: if working_directory.is_some() {
                Some("/workspace".to_string())
            } else {
                None
            },
            host_config: Some(host_config),
            ..Default::default()
        };

        let create_options = CreateContainerOptions {
            name: &container_name,
            platform: None,
        };

        let container = client
            .create_container(Some(create_options), container_config)
            .await
            .map_err(|e| format!("Failed to create container: {}", e))?;

        let container_id = container.id.clone();
        debug!("Created container: {}", container_id);

        // Start container
        client
            .start_container(&container_id, None::<StartContainerOptions<String>>)
            .await
            .map_err(|e| format!("Failed to start container: {}", e))?;

        // Wait for completion with timeout
        let wait_result = tokio::time::timeout(
            std::time::Duration::from_secs(config.timeout_secs),
            async {
                let mut stream = client.wait_container(
                    &container_id,
                    None::<WaitContainerOptions<String>>,
                );
                while let Some(result) = stream.next().await {
                    match result {
                        Ok(response) => return Ok(response.status_code),
                        Err(e) => return Err(format!("Wait error: {}", e)),
                    }
                }
                Err("Container wait stream ended without result".to_string())
            },
        )
        .await;

        let exit_code = match wait_result {
            Ok(Ok(code)) => Some(code),
            Ok(Err(e)) => {
                warn!("Container wait error: {}", e);
                // Try to stop the container
                let _ = client.stop_container(&container_id, None).await;
                None
            }
            Err(_) => {
                warn!(
                    "Container timed out after {}s, stopping",
                    config.timeout_secs
                );
                let _ = client.stop_container(&container_id, None).await;
                None
            }
        };

        // Collect logs
        let mut stdout = String::new();
        let mut stderr = String::new();

        let log_options = LogsOptions::<String> {
            stdout: true,
            stderr: true,
            ..Default::default()
        };

        let mut log_stream = client.logs(&container_id, Some(log_options));
        while let Some(result) = log_stream.next().await {
            match result {
                Ok(output) => match output {
                    LogOutput::StdOut { message } => {
                        stdout.push_str(&String::from_utf8_lossy(&message));
                    }
                    LogOutput::StdErr { message } => {
                        stderr.push_str(&String::from_utf8_lossy(&message));
                    }
                    _ => {}
                },
                Err(e) => {
                    warn!("Error reading container logs: {}", e);
                    break;
                }
            }
        }

        let duration_ms = start.elapsed().as_millis() as u64;

        // Cleanup container
        let remove_options = RemoveContainerOptions {
            force: true,
            ..Default::default()
        };
        if let Err(e) = client
            .remove_container(&container_id, Some(remove_options))
            .await
        {
            warn!("Failed to remove container {}: {}", container_id, e);
        } else {
            debug!("Removed container {}", container_id);
        }

        Ok(ContainerResult {
            exit_code,
            stdout,
            stderr,
            duration_ms,
            container_id,
        })
    }
}
