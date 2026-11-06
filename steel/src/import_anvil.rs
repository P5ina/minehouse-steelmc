//! `steel import-anvil`: converts a vanilla world save into Steel region files.
#![expect(
    clippy::print_stdout,
    reason = "a command line tool reports its progress on stdout"
)]

use std::path::PathBuf;
use std::sync::Arc;

use steel_core::anvil_import::{self, AnvilImportError, ImportReport, LevelReport, PlayerReport};
use steel_core::bootstrap::init_globals;
use tokio::runtime::Builder;

const USAGE: &str =
    "usage: steel import-anvil <vanilla world dir> [--save-path <dir>] [--domain <name>]

Converts the overworld, the nether and the end of a vanilla save into
<save-path>/<domain>/worlds/<dimension> (region files and level.toml), and the players into
<save-path>/<domain>/players. Defaults: --save-path saves, --domain minecraft.
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
        match import(&options, &pool).await {
            Ok(summary) => summary.print(),
            Err(error) => {
                eprintln!("{error}");
                1
            }
        }
    })
}

/// Reports of a finished import.
struct Summary {
    level: LevelReport,
    players: PlayerReport,
    chunks: ImportReport,
}

/// Imports level data and players first: they are small and validate the vanilla save before
/// the chunks, which take most of the time.
async fn import(
    options: &Options,
    pool: &Arc<rayon::ThreadPool>,
) -> Result<Summary, AnvilImportError> {
    let dimensions =
        anvil_import::vanilla_dimensions(&options.world, &options.save_path, &options.domain);
    let level = anvil_import::import_level_data(&options.world, &dimensions)?;
    print!("{level}");
    let players =
        anvil_import::import_players(&options.world, &options.save_path, &options.domain).await?;
    print!("{players}");

    let mut chunks = ImportReport::default();
    for dimension in &dimensions {
        println!(
            "Importing {} into {}",
            dimension.source.display(),
            dimension.target.display()
        );
        let report = anvil_import::import_dimension(dimension, Arc::clone(pool)).await?;
        print!("{report}");
        chunks.merge(report);
    }
    Ok(Summary {
        level,
        players,
        chunks,
    })
}

impl Summary {
    /// Prints what is left to do by hand and returns the exit code.
    fn print(&self) -> i32 {
        println!(
            "Done: {} chunks, {} players, seed {}. The seed, spawn, time, weather, difficulty and \
             game rules are in each world's level.toml.",
            self.chunks.chunks_imported, self.players.players, self.level.seed
        );
        let game_mode = match self.level.game_type {
            1 => Some("creative"),
            2 => Some("adventure"),
            3 => Some("spectator"),
            _ => None,
        };
        if let Some(game_mode) = game_mode {
            println!(
                "The vanilla default game mode is {game_mode}; set `default_gamemode = \
                 \"{game_mode}\"` in config/worlds.toml."
            );
        }
        if self.level.hardcore {
            println!(
                "The vanilla world is hardcore; Steel has no hardcore mode, so it runs as a \
                 normal world."
            );
        }
        let mut exit_code = 0;
        if !self.chunks.failed_chunks.is_empty() {
            eprintln!(
                "{} chunks failed and were not imported; Steel will generate them again.",
                self.chunks.failed_chunks.len()
            );
            exit_code = 1;
        }
        if !self.players.failed_players.is_empty() {
            eprintln!(
                "{} players failed and were not imported; they start fresh on their next join.",
                self.players.failed_players.len()
            );
            exit_code = 1;
        }
        exit_code
    }
}
