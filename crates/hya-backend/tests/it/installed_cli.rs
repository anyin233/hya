//! A user-installed native bundle contributes `hya` commands routed to its
//! driver and to its BundleApi, through the same path as built-in bundles,
//! and keeps them under `--bare` (ADR-0040).

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::common::{Scratch, TestResult, assert_success, hya_bin};

const LIB: &str = "libhya_cli_driver_example.so";

/// The example cdylibs Cargo built beside `hya` (`deps/`, then build units
/// newest first). Each Cargo invocation may build its own unit, and only one
/// that `hya`'s invocation built carries `hya`'s ABI digest.
fn example_libraries() -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let bin = PathBuf::from(hya_bin());
    let debug = bin.parent().ok_or("hya binary has no parent")?;
    let mut units = std::fs::read_dir(debug.join("build/hya-cli-driver-example"))
        .into_iter()
        .flatten()
        .filter_map(|unit| {
            let lib = unit.ok()?.path().join("out").join(LIB);
            Some((lib.metadata().ok()?.modified().ok()?, lib))
        })
        .collect::<Vec<_>>();
    units.sort_by(|left, right| right.cmp(left));
    let deps = debug.join("deps").join(LIB);
    let libraries = deps
        .is_file()
        .then_some(deps)
        .into_iter()
        .chain(units.into_iter().map(|(_, lib)| lib))
        .collect::<Vec<_>>();
    if libraries.is_empty() {
        return Err("hya-cli-driver-example cdylib not built".into());
    }
    Ok(libraries)
}

/// Package the example bundle with `library`, stripped, as `runtime.library`.
fn pack_example(scratch: &Scratch, library: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let src = scratch.0.join("src");
    std::fs::create_dir_all(src.join("native"))?;
    let manifest = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../bundles/examples/cli-driver-example/bundle.yaml"),
    )?
    .replace("crate_dir: native", &format!("library: native/{LIB}"));
    std::fs::write(src.join("bundle.yaml"), manifest)?;
    let lib = src.join("native").join(LIB);
    let strip = Command::new("strip")
        .arg("--strip-debug")
        .arg("-o")
        .arg(&lib)
        .arg(library)
        .output()?;
    assert_success("strip example library", &strip);
    let pkg = scratch.0.join("example.hyabundle");
    let pack = scratch
        .hya()
        .arg("bundle")
        .arg("pack")
        .arg(&src)
        .arg(&pkg)
        .output()?;
    assert_success("bundle pack", &pack);
    Ok(pkg)
}

fn with_bare(scratch: &Scratch, bare: bool) -> Command {
    let mut command = scratch.hya();
    if bare {
        command.arg("--bare");
    }
    command.args(["--model", "hya/offline"]);
    command
}

#[test]
fn installed_native_bundle_commands_route_to_its_driver_and_api() -> TestResult {
    for library in example_libraries()? {
        let scratch = Scratch::new("installed-cli")?;
        let pkg = pack_example(&scratch, &library)?;
        let install = scratch
            .hya()
            .args(["bundle", "install", "-y"])
            .arg(&pkg)
            .output()?;
        assert_success("bundle install", &install);

        for bare in [false, true] {
            let echo = with_bare(&scratch, bare)
                .args(["example-echo", "--text", "hi", "--times", "2", "--loud"])
                .output()?;
            if String::from_utf8_lossy(&echo.stderr).contains("different hya-runtime-abi") {
                break;
            }
            assert_success("example-echo", &echo);
            assert_eq!(String::from_utf8(echo.stdout)?, "HI\nHI\n", "bare={bare}");

            let status = with_bare(&scratch, bare)
                .args(["example-status", "--name", "probe"])
                .output()?;
            assert_success("example-status", &status);
            assert_eq!(
                String::from_utf8(status.stdout)?,
                "probe: ok\n",
                "bare={bare}"
            );
            if bare {
                return Ok(());
            }
        }
    }
    Err("no hya-cli-driver-example cdylib matches this hya build; build both in one Cargo invocation (`cargo test -p hya-backend -p hya-cli-driver-example`, or `--workspace`)".into())
}
