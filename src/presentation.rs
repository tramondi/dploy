use console::style;

#[inline]
pub fn print_cli_info() {
    println!(
        "\n\nRunning {} v{}\n\n",
        style(env!("CARGO_PKG_NAME")).cyan(),
        env!("CARGO_PKG_VERSION")
    );
}

#[inline]
pub fn print_config_not_found_error() {
    eprintln!("It seems that the config file does not exist.");
    eprintln!(
        "Please make sure the file exists and is named {}",
        style("config.toml").cyan()
    );
    eprintln!("or specify the path to the config file with the --config flag.\n");
}

macro_rules! generate_println {
    ($($fn_name:ident($message:expr)),+ $(,)? ) => {
        $(
            #[inline]
            pub fn $fn_name() {
                println!("{}", $message);
            }
        )+
    };
}

macro_rules! generate_println_with_label {
    ($($fn_name:ident($message:expr)),+ $(,)? ) => {
        $(
            #[inline]
            pub fn $fn_name(label: &str) {
                println!("[{}] {}", style(label).cyan(), $message);
            }
        )+
    };
}

generate_println! {
    print_dependencies_starting(style("Starting dependencies").cyan()),
    print_dependencies_stopping(style("Stopping dependencies").cyan()),
    print_env_file_generating(style("Generating env file").cyan()),
    print_env_file_loaded(style("Loaded env file").green()),
    print_env_file_failed_to_load(style("Failed to load env file").yellow()),
    print_env_file_generated(style(concat!(
        ".env file was generated. Please make sure to ",
        "fill in your custom environment variables.",
    )).yellow()),
}

generate_println_with_label! {
    print_dependency_stopping(style("Stopping").cyan()),
    print_dependency_stopped(style("Stopped").green()),
    print_dependency_already_stopped(style("Already stopped").green()),
    print_dependency_success(style("Success").green()),
    print_dependency_starting(style("Starting").cyan()),
    print_dependency_creating(style("Creating").cyan()),
    print_dependency_pulling(style("Pulling").cyan()),
    print_image_building(style("Building image").cyan()),
    print_image_built(style("Image built").green()),
    print_app_container_creating(style("Creating container").green()),
    print_app_container_removing(style("Removing container").cyan()),
    print_app_container_starting(style("Starting container").cyan()),
    print_app_container_success(style("Success").green()),
    print_app_container_already_stopped(style("Already stopped").green()),
    print_app_container_stopped(style("Stopped").green()),
}