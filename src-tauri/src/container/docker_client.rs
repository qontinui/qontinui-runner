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

        // PID limit
        if let Some(pids) = config.pids_limit {
            host_config.pids_limit = Some(pids);
        }

        // Network
        if !config.network_enabled {
            host_config.network_mode = Some("none".to_string());
        }

        // Security hardening: capabilities
        if !config.cap_drop.is_empty() {
            host_config.cap_drop = Some(config.cap_drop.clone());
        }
        if !config.cap_add.is_empty() {
            host_config.cap_add = Some(config.cap_add.clone());
        }

        // Security options (no-new-privileges, seccomp profiles)
        if !config.security_opt.is_empty() {
            host_config.security_opt = Some(config.security_opt.clone());
        }

        // Read-only root filesystem
        if config.read_only_rootfs {
            host_config.readonly_rootfs = Some(true);

            // Add tmpfs mounts for writable temp directories
            let mut tmpfs_map = std::collections::HashMap::new();
            if config.tmpfs_mounts.is_empty() {
                // Default tmpfs mounts when read-only rootfs is enabled
                tmpfs_map.insert("/tmp".to_string(), "rw,noexec,nosuid,size=64m".to_string());
                tmpfs_map.insert(
                    "/var/tmp".to_string(),
                    "rw,noexec,nosuid,size=64m".to_string(),
                );
            } else {
                for mount_spec in &config.tmpfs_mounts {
                    // Parse "path:options" format
                    if let Some((path, opts)) = mount_spec.split_once(':') {
                        tmpfs_map.insert(path.to_string(), opts.to_string());
                    } else {
                        tmpfs_map.insert(mount_spec.to_string(), "rw,noexec,nosuid".to_string());
                    }
                }
            }
            host_config.tmpfs = Some(tmpfs_map);
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

        // Build environment variables
        let env = if config.extra_env.is_empty() {
            None
        } else {
            Some(config.extra_env.clone())
        };

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
            user: config.user.clone(),
            env,
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
                match stream.next().await {
                    Some(Ok(response)) => Ok(response.status_code),
                    Some(Err(e)) => Err(format!("Wait error: {}", e)),
                    None => Err("Container wait stream ended without result".to_string()),
                }
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
