fn main() {
    if std::env::args().any(|arg| arg == "--daemon") {
        if let Err(err) = claw_setup_lib::daemon::run_daemon() {
            eprintln!("claw-setup daemon failed: {err:#}");
            std::process::exit(1);
        }
        return;
    }

    claw_setup_lib::run();
}
