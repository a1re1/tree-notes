use std::process::ExitCode;

fn main() -> ExitCode {
    match treenotes::cli::run(std::env::args_os()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            if !err.message.is_empty() {
                eprintln!("error: {}", err.message);
            }
            ExitCode::from(err.code)
        }
    }
}
