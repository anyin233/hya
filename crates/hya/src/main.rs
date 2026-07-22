use std::os::unix::process::CommandExt as _;
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let launcher = match std::env::current_exe() {
        Ok(executable) => executable.with_file_name("hya-ts"),
        Err(error) => {
            eprintln!("hya: failed to resolve current executable: {error}");
            return ExitCode::FAILURE;
        }
    };
    let error = Command::new(&launcher)
        .arg0("hya")
        .args(std::env::args_os().skip(1))
        .exec();
    eprintln!("hya: failed to launch `{}`: {error}", launcher.display());
    ExitCode::FAILURE
}
