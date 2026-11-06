//! `steel import-anvil`: converts a vanilla world save into Steel region files.
#![expect(
    clippy::print_stdout,
    reason = "a command line tool reports its progress on stdout"
)]

use std::path::PathBuf;
use std::sync::Arc;

use steel_core::anvil_import::{self, ImportReport};
use steel_core::bootstrap::init_globals;
use tokio::runtime::Builder;

const USAGE: &str =
    "usage: steel import-anvil <vanilla world dir> [--save-path <dir>] [--domain <name>]

Converts the overworld, the nether and the end of a vanilla save into
<save-path>/<domain>/worlds/<dimension>/region. Defaults: --save-path saves, --domain minecraft.
The world must be saved by the Minecraft version Steel targets.";

struct Options {
    world: PathBuf,
    save_path: PathBuf,
    domain: String,
}

fn parse_options(args: &[String]) -> Result<Options, String> {
    let mut world = None;
    let mut save_path = PathBuf::from("saves");
    let mut domain = "minecraft".to_owned();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--save-path" => {
                save_path = args.next().ok_or("--save-path needs a value")?.into();
            }
            "--domain" => {
                domain.clone_from(args.next().ok_or("--domain needs a value")?);
            }
            "-h" | "--help" => return Err(String::new()),
            other if other.starts_with('-') => return Err(format!("unknown option {other}")),
            other if world.is_none() => world = Some(PathBuf::from(other)),
            other => return Err(format!("unexpected argument {other}")),
        }
    }
    Ok(Options {
        world: world.ok_or("missing vanilla world dir")?,
        save_path,
        domain,
    })
}

/// Runs the import and returns the process exit code.
#[must_use]
pub fn run(args: &[String]) -> i32 {
    let options = match parse_options(args) {
        Ok(options) => options,
        Err(error) => {
            if !error.is_empty() {
                eprintln!("{error}\n");
            }
            eprintln!("{USAGE}");
            return 2;
        }
    };

    init_globals();
    let runtime = match Builder::new_multi_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("failed to start the async runtime: {error}");
            return 1;
        }
    };
    let pool = match rayon::ThreadPoolBuilder::new()
        .thread_name(|index| format!("anvil-import-{index}"))
        .build()
    {
        Ok(pool) => Arc::new(pool),
        Err(error) => {
            eprintln!("failed to start the import thread pool: {error}");
            return 1;
        }
    };

    runtime.block_on(async {
        let seed = match anvil_import::read_world_seed(&options.world) {
            Ok(seed) => seed,
            Err(error) => {
                eprintln!("{error}");
                return 1;
            }
        };

        let mut total = ImportReport::default();
        for dimension in
            anvil_import::vanilla_dimensions(&options.world, &options.save_path, &options.domain)
        {
            println!(
                "Importing {} into {}",
                dimension.source.display(),
                dimension.target.display()
            );
            match anvil_import::import_dimension(&dimension, Arc::clone(&pool)).await {
                Ok(report) => {
                    print!("{report}");
                    total.merge(report);
                }
                Err(error) => {
                    eprintln!("{error}");
                    return 1;
                }
            }
        }

        println!(
            "Done: {} chunks. Set `seed = \"{seed}\"` in config/worlds.toml so Steel generates \
             new chunks of the same world.",
            total.chunks_imported
        );
        if total.failed_chunks.is_empty() {
            0
        } else {
            eprintln!(
                "{} chunks failed and were not imported; Steel will generate them again.",
                total.failed_chunks.len()
            );
            1
        }
    })
}
