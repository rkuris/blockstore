use std::error::Error;
use std::num::NonZeroUsize;
use std::path::PathBuf;

use clap::{Parser, Subcommand};

use blockstore::{BlockHeight, Store, SyncMode};

#[cfg(feature = "import")]
mod import;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[arg(short, long)]
    db_path: Option<PathBuf>,

    /// Path to the index file
    #[arg(long)]
    index_path: Option<PathBuf>,

    /// Path to the data file
    #[arg(long)]
    data_path: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    #[cfg(feature = "import")]
    Import {
        /// Path to the `LevelDB` database
        #[arg(short, long)]
        leveldb: PathBuf,

        /// Sync mode (sync or async)
        #[arg(long, default_value = "async")]
        sync: String,

        /// Minimum block height to start from
        #[arg(short, long, default_value = "1")]
        min_height: BlockHeight,

        /// Block height to start importing from
        #[arg(short, long)]
        start_block: Option<BlockHeight>,

        /// Include receipts in the imported blocks
        #[arg(long, default_value = "false")]
        receipts: bool,
    },
    Get {
        /// Block height to get
        #[arg(long)]
        height: BlockHeight,
    },
    Copy {
        /// Path to the target ``BlockStore`` database directory
        #[arg(long)]
        target: PathBuf,

        /// Block height to start copying from
        #[arg(short, long)]
        start_block: Option<BlockHeight>,
    },
}

fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();

    let (index_path, data_path) = path_resolver(cli.db_path, cli.index_path, cli.data_path)?;

    match cli.command {
        Commands::Get { height } => {
            let store = Store::new(
                &index_path,
                &data_path,
                NonZeroUsize::new(1024).unwrap(),
                false,
                SyncMode::Async,
                height,
            )?;

            let block = store.read_block(height)?.expect("block not found");
            println!("{}", pretty_hex::pretty_hex(&block));
        }
        #[cfg(feature = "import")]
        Commands::Import {
            leveldb,
            sync,
            min_height,
            start_block,
            receipts,
        } => {
            import::import(
                &leveldb,
                &index_path,
                &data_path,
                &sync,
                min_height,
                start_block,
                receipts,
            )?;
        }
        Commands::Copy {
            target,
            start_block,
        } => {
            let source_store = Store::new(
                &index_path,
                &data_path,
                NonZeroUsize::new(1024).unwrap(),
                false,
                SyncMode::Async,
                1,
            )?;
            let target_min_height = start_block.unwrap_or_else(|| source_store.min_block_height());
            let target_store = Store::new(
                &target,
                &target,
                NonZeroUsize::new(1024).unwrap(),
                true,
                SyncMode::Async,
                target_min_height,
            )?;
            let mut height = start_block.unwrap_or(target_min_height);
            while let Some(block) = source_store.read_block(height)? {
                target_store.write_block(height, &block)?;
                height = height.checked_add(1).ok_or("Overflow")?;
            }
            println!("Copied blocks up to height {}", height.saturating_sub(1));
        }
    }
    Ok(())
}

#[allow(clippy::needless_pass_by_value)]
fn path_resolver(
    db_path: Option<PathBuf>,
    index_path: Option<PathBuf>,
    data_path: Option<PathBuf>,
) -> Result<(PathBuf, PathBuf), Box<dyn Error>> {
    let index_path = index_path
        .or_else(|| db_path.clone())
        .ok_or::<Box<dyn Error>>("Either --dbpath or --index-path must be specified".into())?;
    let data_path = data_path
        .or_else(|| db_path.clone())
        .ok_or::<Box<dyn Error>>("Either --dbpath or --data-path must be specified".into())?;
    Ok((index_path, data_path))
}
