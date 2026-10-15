#![forbid(unsafe_code)]

mod cli;

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--service") {
        #[cfg(target_os = "linux")]
        let result = cli::service::service();
        #[cfg(not(target_os = "linux"))]
        let result: Result<(), String> = Err("the service runs on Linux; elsewhere use --emit".into());
        if let Err(error) = result {
            eprintln!("padbridge --service: {error}");
            std::process::exit(1);
        }
        return;
    }
    if let Err(error) = cli::emit::run() {
        eprintln!("padbridge: {error}");
        std::process::exit(1);
    }
}
