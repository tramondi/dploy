use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use bollard::models;

use crate::{
    cli::{Args, Command},
    config::{self, AppConfig},
    constants,
    services::{self, ServiceKind},
    utils,
};

#[derive(Debug)]
pub struct Context {
    args: Args,

    app_config: AppConfig,

    override_context: config::OverrideContext,
}

impl Context {
    pub fn new(
        args: Args,
        app_config: config::AppConfig,
        override_context: config::OverrideContext,
    ) -> Self {
        Self {
            args,
            app_config,
            override_context,
        }
    }

    pub fn args(&self) -> &Args {
        &self.args
    }

    pub fn override_context(&self) -> &config::OverrideContext {
        &self.override_context
    }

    pub fn namespace(&self) -> &str {
        self.args().namespace()
    }

    pub fn app_config(&self) -> &AppConfig {
        &self.app_config
    }

    pub fn container_name_of(&self, service_kind: ServiceKind) -> String {
        let prefix = if service_kind.is_singleton() {
            "dploy-singleton"
        } else {
            self.app_config.name(&self.override_context)
        };

        let suffix = {
            use ServiceKind::*;

            match service_kind {
                Postgres => "postgres",
                Keydb => "keydb",
                Proxy => "proxy",
                App => self.app_config.name(&self.override_context),
            }
        };

        let namespace = if service_kind == services::ServiceKind::Proxy {
            // proxy is not namespaced because it's exposed to the host's network
            constants::DEFAULT_NAMESPACE
        } else {
            self.namespace()
        };

        format!("{prefix}_{suffix}_{namespace}")
    }

    pub fn volume_path_of(&self, service_kind: ServiceKind, path: impl AsRef<Path>) -> PathBuf {
        let volume_path = self
            .get_dploy_dir()
            .join("volumes")
            .join(self.container_name_of(service_kind))
            .join(
                path.as_ref()
                    .to_string_lossy()
                    .replace('\\', "/")
                    .replace('/', "$__$"),
            );

        volume_path
    }

    pub fn should_expose_to_host(&self) -> bool {
        use Command::*;

        matches!(self.args.command(), Dev { .. } | Run { .. })
    }

    pub fn should_expose_app_service_to_host(&self) -> bool {
        use Command::*;

        matches!(self.args.command(), Run { .. })
    }

    pub fn should_print_connection_info(&self) -> bool {
        use Command::*;

        matches!(self.args.command(), Dev { .. } | Run { .. })
    }

    pub fn should_create_app_service(&self) -> bool {
        use Command::*;

        matches!(self.args.command(), Deploy { .. } | Run { .. })
    }

    pub fn should_create_proxy_service(&self) -> bool {
        use Command::*;

        matches!(self.args.command(), Deploy { command: None, .. })
    }

    pub fn should_generate_env_file(&self) -> bool {
        use Command::*;

        matches!(self.args.command(), Dev { .. } | Run { .. })
    }

    pub fn should_create_network(&self) -> bool {
        use Command::*;

        matches!(self.args.command(), Dev { .. } | Run { .. })
    }

    pub fn manual_mount(&self, outer_path: &str, inner_path: &str) -> models::Mount {
        models::Mount {
            source: Some(
                self.get_dploy_dir()
                    .join(outer_path)
                    .to_string_lossy()
                    .to_string(),
            ),
            target: Some(inner_path.to_owned()),

            bind_options: Some(models::MountBindOptions {
                create_mountpoint: Some(true),
                ..Default::default()
            }),

            typ: Some(models::MountTypeEnum::BIND),

            ..Default::default()
        }
    }

    pub fn manual_volume_path(&self, outer_path: &str) -> PathBuf {
        self.get_dploy_dir().join(outer_path)
    }

    pub fn mount(&self, service_kind: ServiceKind, inner_path: &str) -> models::Mount {
        models::Mount {
            source: Some(
                self.volume_path_of(service_kind, inner_path)
                    .to_string_lossy()
                    .to_string(),
            ),
            target: Some(inner_path.to_owned()),

            bind_options: Some(models::MountBindOptions {
                create_mountpoint: Some(true),
                ..Default::default()
            }),

            typ: Some(models::MountTypeEnum::BIND),

            ..Default::default()
        }
    }

    pub fn host_port_binding_of(
        &self,
        service_kind: ServiceKind,
        inner_port: u16,
    ) -> HostPortBinding {
        HostPortBinding::new(
            &self.container_name_of(service_kind),
            inner_port,
            self.args.command(),
        )
    }

    pub fn ssh_credentials(&self) -> Option<SshCredentials> {
        use Command::*;

        let command = self.args().command();

        let Deploy {
            host,
            port,
            username,
            keyfile,
            ..
        } = command
        else {
            return None;
        };

        Some(SshCredentials::new(
            host.clone(),
            *port,
            username.clone(),
            keyfile.clone().map(PathBuf::from),
        ))
    }

    fn get_dploy_dir(&self) -> PathBuf {
        PathBuf::from("/var/lib/dploy")
    }
}

// TODO: some really complex logic going on here
#[derive(Clone, Debug)]
pub struct HostPortBinding {
    /// The port for communication of the app service with this service
    inner_port: u16,
    inner_host: String,

    /// The port for communication of the host with this service
    host_port: Option<u16>,
    host_host: String,

    /// The port inside the container
    internal_port: u16,
}

impl HostPortBinding {
    pub fn new(container_name: &str, internal_port: u16, command: &Command) -> Self {
        use Command::*;

        let host_port = match command {
            Dev { .. } | Run { .. } => Some(utils::network::free_port()),
            _ => None,
        };

        let host_host = "127.0.0.1";

        let inner_host = match command {
            Dev { .. } => host_host,
            _ => container_name,
        };

        let inner_port = match (command, host_port) {
            (Dev { .. }, Some(port)) => port,
            _ => internal_port,
        };

        Self {
            inner_port,
            inner_host: inner_host.to_owned(),
            host_port,
            host_host: host_host.to_owned(),
            internal_port,
        }
    }

    pub fn manual(host_port: u16, host_host: &str, inner_port: u16, inner_host: &str) -> Self {
        Self {
            inner_port,
            inner_host: inner_host.to_owned(),
            host_port: Some(host_port),
            host_host: host_host.to_owned(),
            internal_port: inner_port,
        }
    }

    pub fn to_port_bindings(
        bindings: &[&HostPortBinding],
    ) -> HashMap<String, Option<Vec<models::PortBinding>>> {
        let mut map = HashMap::new();

        for binding in bindings {
            let host_port = binding.host_port();
            let host_host = binding.host_host();
            let internal_port = binding.internal_port();

            if let Some(host_port) = host_port {
                map.insert(
                    // TODO: allow using not only tcp
                    format!("{internal_port}/tcp"),
                    Some(vec![models::PortBinding {
                        host_ip: Some(host_host.to_owned()),
                        host_port: Some(host_port.to_string()),
                    }]),
                );
            }
        }

        map
    }

    pub fn to_port_binding(&self) -> HashMap<String, Option<Vec<models::PortBinding>>> {
        Self::to_port_bindings(&[self])
    }

    pub fn inner_port(&self) -> u16 {
        self.inner_port
    }

    pub fn inner_host(&self) -> &str {
        &self.inner_host
    }

    pub fn host_port(&self) -> Option<u16> {
        self.host_port
    }

    pub fn host_host(&self) -> &str {
        &self.host_host
    }

    pub fn internal_port(&self) -> u16 {
        self.internal_port
    }
}

#[derive(Debug, Clone)]
pub struct SshCredentials {
    host: String,
    port: u16,
    username: String,
    keyfile: Option<PathBuf>,
}

impl SshCredentials {
    pub fn new(host: String, port: u16, username: String, keyfile: Option<PathBuf>) -> Self {
        Self {
            host,
            port,
            username,
            keyfile,
        }
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn username(&self) -> &str {
        &self.username
    }

    pub fn keyfile(&self) -> Option<&Path> {
        self.keyfile.as_deref()
    }
}
