//! Dev tooling entrypoint.

mod sync_compat;

fn main() {
    let mut args = std::env::args();
    let _bin = args.next();
    let task = args.next();

    let result = match task.as_deref() {
        Some("sync-compat") => sync_compat::run(args.collect()),
        Some("migrate") => sync_compat::run(args.collect()),
        _ => {
            eprintln!("usage: cargo xtask <sync-compat|migrate>");
            Ok(())
        }
    };

    if let Err(error) = result {
        eprintln!("xtask: {error:#}");
        std::process::exit(1);
    }
}
