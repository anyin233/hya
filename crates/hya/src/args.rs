#[derive(Debug, Default)]
pub(crate) struct Args {
    pub(crate) server: Option<String>,
    pub(crate) http: bool,
    pub(crate) compat: bool,
    pub(crate) backend_bin: Option<String>,
    pub(crate) import_source: Option<String>,
    pub(crate) version: bool,
    pub(crate) help: bool,
}

impl Args {
    pub(crate) fn parse() -> Result<Self, std::io::Error> {
        let mut args = std::env::args().skip(1);
        let mut parsed = Self::default();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--server" => {
                    parsed.server = Some(args.next().ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            "--server requires a URL",
                        )
                    })?);
                }
                "--http" => parsed.http = true,
                "--compat" => parsed.compat = true,
                "--backend-bin" => {
                    parsed.backend_bin = Some(args.next().ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            "--backend-bin requires a path",
                        )
                    })?);
                }
                "--import" => {
                    parsed.import_source = Some(args.next().ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            "--import requires a source name",
                        )
                    })?);
                }
                "--version" | "-v" => parsed.version = true,
                "--help" | "-h" => parsed.help = true,
                other => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        format!("unknown argument: {other}"),
                    ));
                }
            }
        }

        Ok(parsed)
    }
}

pub(crate) fn print_usage() {
    println!("usage: hya [OPTIONS]");
    println!(
        "  (default)          run the `hya` backend in-process and talk to it natively (no HTTP)"
    );
    println!(
        "  --http             spawn `hya-backend serve` and connect over HTTP/SSE (with --compat: `compat serve`)"
    );
    println!("  --server <url>     attach to a running compat-compatible server (hya or compat)");
    println!(
        "  --backend-bin <path>  hya-backend binary to spawn for --http (else $HYA_BACKEND_BIN, sibling build, or PATH)"
    );
    println!("  --import <source>  import model config from a source (currently: compat)");
    println!("  --compat         use the compat backend (native bun bridge) instead of hya");
    println!("  --version, -v      print version");
    println!("  --help, -h         print this help");
}
