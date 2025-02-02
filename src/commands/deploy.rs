use std::{
    collections::{BTreeMap, HashSet},
    fs,
    io::Write,
    path::Path,
    sync::Arc,
    time,
};

use futures_util::TryStreamExt;
use notify::Watcher;

use crate::{
    build, commands, context, docker, network,
    prelude::*,
    presentation,
    services::{self, ToContainerConfig},
};

const WATCH_POLL_INTERVAL: time::Duration = time::Duration::from_secs(1);
const WATCH_COOLDOWN: time::Duration = time::Duration::from_secs(3);

pub async fn deploy(
    context: &context::Context,
    docker: &bollard::Docker,
    services: &services::Services,
) -> Result<()> {
    if dotenvy::from_path(context.app_config().env_file(context.override_context())).is_ok() {
        presentation::print_env_file_loaded();
    } else {
        presentation::print_env_file_failed_to_load();
    }

    if context.should_generate_env_file() {
        presentation::print_env_file_generating();
        generate_env(services, context)?;
    }

    if context.should_create_network() {
        presentation::print_network_creating();
        network::create_dploy_network(docker).await?;
    }

    presentation::print_dependencies_starting();
    deploy_dependencies(services, context, docker).await?;

    if let Some(service) = services.app() {
        deploy_app_service(service, context, docker).await?;
    }

    presentation::print_post_up_running();
    services.post_up(docker).await?;

    if context.should_print_connection_info() {
        let connection_info = services.connection_info();
        presentation::print_connection_info(&connection_info);
    }

    Ok(())
}

pub async fn deploy_watch(
    context: Arc<context::Context>,
    docker: Arc<bollard::Docker>,
    services: &services::Services,
    watch_paths: &[&Path],
) -> Result<()> {
    if watch_paths.is_empty() {
        bail!("Called with --watch flag but no paths were provided. Please provide at least one path to watch in the dploy.toml");
    }

    deploy(&context, &docker, services).await?;
    let mut handle = tokio::spawn(commands::logs::logs(
        Arc::clone(&context),
        Arc::clone(&docker),
        services::ServiceKind::App,
        None,
    ));

    let (tx, rx) = std::sync::mpsc::channel();
    let (tx_abort, rx_abort) = std::sync::mpsc::channel();

    let mut debouncer = notify_debouncer_full::new_debouncer(WATCH_POLL_INTERVAL, None, tx)?;

    let watcher = debouncer.watcher();

    for path in watch_paths {
        watcher
            .watch(path, notify::RecursiveMode::Recursive)
            .context("Could not start watcher. Please make sure the folder exists")?;
    }

    ctrlc::set_handler(move || {
        presentation::print_ctrlc_received();
        tx_abort.send(()).unwrap();
    })?;

    let mut last_deploy = time::Instant::now();

    // don't care about blocking here
    loop {
        if rx_abort.try_recv().is_ok() {
            break;
        }

        if let Ok(Ok(events)) = rx.try_recv() {
            if time::Instant::now() - last_deploy < WATCH_COOLDOWN
                || events.is_empty()
                || !events.iter().any(|event| event.kind.is_modify())
            {
                continue;
            }

            presentation::print_watch_files_changed();

            handle.abort();

            if let Some(service) = services.app() {
                deploy_app_service(service, &context, &docker).await?;
            }

            handle = tokio::spawn(commands::logs::logs(
                Arc::clone(&context),
                Arc::clone(&docker),
                services::ServiceKind::App,
                None,
            ));

            last_deploy = time::Instant::now();
        }
    }

    handle.abort();

    presentation::print_ctrlc_started();

    Ok(())
}

async fn deploy_app_service(
    app_service: &services::app::AppService,
    context: &context::Context,
    docker: &bollard::Docker,
) -> Result<()> {
    let container_config = app_service.to_container_config(context)?;
    let container_name = container_config.container_name();
    let dockerfile = context.app_config().dockerfile(context.override_context());

    presentation::print_image_building(container_name, dockerfile);
    build::build_app_service_image(context, app_service, docker).await?;
    presentation::print_image_built(container_name);

    let existing_container = match docker.inspect_container(container_name, None).await {
        Ok(container) => Some(container),
        Err(bollard::errors::Error::DockerResponseServerError {
            status_code: 404, ..
        }) => None,
        Err(e) => return Err(e.into()),
    };

    if existing_container.is_some() {
        presentation::print_app_container_removing(container_name);
        docker.stop_container(container_name, None).await?;
        docker.remove_container(container_name, None).await?;
    }

    presentation::print_app_container_creating(container_name);
    docker
        .create_container(
            Some(bollard::container::CreateContainerOptions {
                name: container_name,
                ..Default::default()
            }),
            container_config.config().clone(),
        )
        .await?;

    presentation::print_app_container_starting(container_name);
    docker
        .start_container(
            container_name,
            None::<bollard::container::StartContainerOptions<String>>,
        )
        .await?;

    presentation::print_app_container_success(container_name);

    Ok(())
}

fn generate_env(services: &services::Services, context: &context::Context) -> Result<()> {
    let existing_env = get_existing_env(context.app_config().env_file(context.override_context()));
    let is_generated_first_time = existing_env.is_none();
    let existing_env = existing_env.unwrap_or_default();

    let services_env_vars = services.env_vars(context);
    let mut own_env_vars_names = HashSet::new();

    for env_name in context.app_config().env(context.override_context()) {
        own_env_vars_names.insert(env_name.clone());
    }

    for env_name in existing_env.keys() {
        own_env_vars_names.insert(env_name.clone());
    }

    for (env_name, _) in &services_env_vars {
        own_env_vars_names.remove(env_name);
    }

    let own_env_vars = {
        let mut own_env_vars = vec![];

        for env_name in own_env_vars_names {
            let env_value = &existing_env
                .get(&env_name)
                .map(|value| value.to_owned())
                .unwrap_or_else(|| "".to_owned());

            own_env_vars.push((env_name.clone(), env_value.clone()));
        }

        own_env_vars
    };

    generate_env_file(&services_env_vars, &own_env_vars, context)?;

    if is_generated_first_time {
        presentation::print_env_file_generated();
    }

    Ok(())
}

fn get_existing_env(env_file_name: &str) -> Option<BTreeMap<String, String>> {
    let mut existing_env = BTreeMap::new();
    let env_file_path = Path::new(env_file_name);

    if !env_file_path.exists() {
        return None;
    }

    let Ok(iter) = dotenvy::from_path_iter(env_file_path) else {
        return None;
    };

    for item in iter {
        let Ok((key, value)) = item else {
            continue;
        };

        existing_env.insert(key, value);
    }

    Some(existing_env)
}

fn generate_env_file(
    services_env_vars: &[(String, String)],
    own_env_vars: &[(String, String)],
    context: &context::Context,
) -> Result<()> {
    let mut file = fs::File::create(context.app_config().env_file(context.override_context()))?;

    for (key, value) in services_env_vars {
        writeln!(file, "{}={}", key, value)?;
    }

    writeln!(file, "\n# Your own variables come after this line")?;
    writeln!(file, "# Feel free to modify them as you want")?;

    for (key, value) in own_env_vars {
        writeln!(file, "{}={}", key, value)?;
    }

    Ok(())
}

async fn deploy_dependencies(
    services: &services::Services,
    context: &context::Context,
    docker: &bollard::Docker,
) -> Result<()> {
    let container_configs = services.to_container_configs(context)?;

    for config in container_configs {
        let container_name = config.container_name();
        let image_name = config.image_name();
        let config = config.config();

        presentation::print_dependency_pulling(container_name);
        docker
            .create_image(
                Some(bollard::image::CreateImageOptions {
                    from_image: image_name,
                    // TODO: allow users to set tag
                    tag: "latest",
                    ..Default::default()
                }),
                None,
                None,
            )
            .try_collect::<Vec<_>>()
            .await?;

        // TODO: check here if container exists and version is the same
        let existing_container = docker::inspect_container(docker, container_name).await?;

        presentation::print_dependency_creating(container_name);

        if existing_container.is_some() {
            if docker::check_container_running(docker, container_name).await? {
                docker.stop_container(container_name, None).await?;
            }

            docker.remove_container(container_name, None).await?;
        }

        docker
            .create_container(
                Some(bollard::container::CreateContainerOptions {
                    name: container_name,
                    ..Default::default()
                }),
                config.clone(),
            )
            .await?;

        presentation::print_dependency_starting(container_name);
        docker
            .start_container(
                container_name,
                None::<bollard::container::StartContainerOptions<String>>,
            )
            .await?;

        presentation::print_dependency_success(container_name);
    }

    Ok(())
}
