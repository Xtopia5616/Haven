#[cfg(windows)]
fn main() {
    use haven_platform::windows_container::execution::broker_service;

    let mut args = std::env::args_os().skip(1);
    let command = args.next();
    let result = match (command.as_deref(), args.next()) {
        (Some(command), None) if command == "install" => broker_service::install(),
        (Some(command), None) if command == "start" => broker_service::start(),
        (Some(command), None) if command == "stop" => broker_service::stop(),
        (Some(command), None) if command == "uninstall" => broker_service::uninstall(),
        (Some(command), None) if command == "status" => broker_service::print_status(),
        (Some(command), None) if command == "--service" => broker_service::run_service_dispatcher(),
        (Some(command), None) if command == "--hcs-export-worker" => {
            broker_service::run_hcs_export_worker()
        }
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "usage: haven-container-broker <install|start|stop|status|uninstall>",
        )),
    };

    if let Err(error) = result {
        eprintln!("Haven Container Broker: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Haven Container Broker is available only on Windows.");
    std::process::exit(1);
}
