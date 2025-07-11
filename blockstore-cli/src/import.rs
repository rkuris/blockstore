//! LevelDB Import Module
//!
//! This module handles importing blocks from a LevelDB database into the blockstore format.
//! The import process works as follows:
//!
//! 1. **Key Structure**:
//!    - LevelDB keys are prefixed with a single character indicating the type:
//!      - 'h': Block header (followed by 32 bytes of hash) or header hash (followed by 1 byte)
//!      - 'b': Block body
//!      - 'r': Block receipts
//!    - We skip the 'h' row with the header hash, because it is not needed for the import.
//!    - After the prefix, the key contains 8 bytes of big-endian encoded block height
//!
//! 2. **Import Process**:
//!    - Opens both the source LevelDB and target blockstore
//!    - Creates parallel iterators for headers and bodies (and optionally receipts)
//!    - Iterates through streams simultaneously, matching blocks by height
//!    - For each matching set of header/body (and receipts if enabled):
//!      - Combines them into a single RLP-encoded block
//!      - Writes the combined block to the blockstore
//!
//! 3. **Error Handling**:
//!    - Validates that all three streams (header/body/receipts) have matching heights
//!    - Reports progress every 100,000 blocks
//!    - Returns early if any stream ends or if heights don't match
//!
//! 4. **Performance Considerations**:
//!    - Uses LevelDB's native iteration capabilities
//!    - Disables cache filling and enables checksum verification
//!    - Uses async mode for blockstore writes to improve performance
use std::error::Error;
use std::num::NonZeroUsize;
use std::path::Path;

use db_key::Key;
use leveldb::database::Database;
use leveldb::iterator::{Iterable, LevelDBIterator as _};
use leveldb::options::{Options, ReadOptions};

use blockstore::{Block, BlockHeight, Store, SyncMode};

#[derive(Debug, Clone, Copy)]
pub enum LevelDBKey {
    Body(BlockHeight),
    Header(BlockHeight),
    HeaderHash(BlockHeight),
    Receipts(BlockHeight),
    BodyStart,
    HeaderStart,
    ReceiptsStart,
    None,
}

impl LevelDBKey {
    fn height(&self) -> Option<BlockHeight> {
        match self {
            Self::Body(height)
            | Self::Header(height)
            | Self::HeaderHash(height)
            | Self::Receipts(height) => Some(*height),
            _ => None,
        }
    }

    fn is_header(&self) -> bool {
        matches!(self, Self::Header(_))
    }

    fn is_body(&self) -> bool {
        matches!(self, Self::Body(_))
    }

    fn is_receipts(&self) -> bool {
        matches!(self, Self::Receipts(_))
    }
}

// A LevelDBKey is the key for LevelDB. "None" implies the first possible key in leveldb.
// These keys are represented internally in leveldb by geth as literal 'b' followed by
// 8 bytes which is the big-endian encoded height, followed by the block hash.
// We ignore the block hash when reading keys, and never construct keys with a block hash.
// This means callers must only use generated LevelDBKey values as the start of an iterator
// if a block fetch is required.
impl Key for LevelDBKey {
    fn from_u8(key: &[u8]) -> Self {
        // println!("key: {key:?}");
        // format must be "b" followed by the big-endian encoded block ID or we return None
        if let (Some(first), Some(blockid_bytes)) = (key.first(), key.get(1..9)) {
            match first {
                b'b' => Self::Body(BlockHeight::from_be_bytes(
                    blockid_bytes.try_into().unwrap(),
                )),
                b'h' => {
                    if key.len() == 1 + 8 + 32 {
                        Self::Header(BlockHeight::from_be_bytes(
                            blockid_bytes.try_into().unwrap(),
                        ))
                    } else if key.len() == 1 + 8 + 1 {
                        Self::HeaderHash(BlockHeight::from_be_bytes(
                            blockid_bytes.try_into().unwrap(),
                        ))
                    } else {
                        Self::None
                    }
                }
                b'r' => Self::Receipts(BlockHeight::from_be_bytes(
                    blockid_bytes.try_into().unwrap(),
                )),
                _ => Self::None,
            }
        } else {
            Self::None
        }
    }

    fn as_slice<T, F: Fn(&[u8]) -> T>(&self, f: F) -> T {
        match self {
            Self::Body(height) => {
                let mut bytes: [u8; 9] = [b'b', 0, 0, 0, 0, 0, 0, 0, 0];
                bytes
                    .get_mut(1..9)
                    .expect("array is always 9 bytes")
                    .copy_from_slice(&height.to_be_bytes());
                f(&bytes)
            }
            Self::Header(height) | Self::HeaderHash(height) => {
                let mut bytes: [u8; 9] = [b'h', 0, 0, 0, 0, 0, 0, 0, 0];
                bytes
                    .get_mut(1..9)
                    .expect("array is always 9 bytes")
                    .copy_from_slice(&height.to_be_bytes());
                f(&bytes)
            }
            Self::Receipts(height) => {
                let mut bytes: [u8; 9] = [b'r', 0, 0, 0, 0, 0, 0, 0, 0];
                bytes
                    .get_mut(1..9)
                    .expect("array is always 9 bytes")
                    .copy_from_slice(&height.to_be_bytes());
                f(&bytes)
            }
            Self::BodyStart => f(b"b"),
            Self::HeaderStart => f(b"h"),
            Self::ReceiptsStart => f(b"r"),
            Self::None => unreachable!(),
        }
    }
}

pub fn import(
    leveldb: &Path,
    index_path: &Path,
    data_path: &Path,
    sync: &str,
    min_height: BlockHeight,
    start_block: Option<BlockHeight>,
    include_receipts: bool,
) -> Result<(), Box<dyn Error>> {
    // Open LevelDB
    let mut opts = Options::new();
    opts.create_if_missing = false;
    let db: Database<LevelDBKey> = Database::open(leveldb, opts)?;

    // Create blockstore
    let sync = match sync {
        "sync" => SyncMode::Sync,
        "async" => SyncMode::Async,
        _ => return Err("Invalid sync mode. Must be 'sync' or 'async'".into()),
    };

    let store = Store::new(
        index_path,
        data_path,
        NonZeroUsize::new(1024).unwrap(),
        true,
        sync,
        min_height,
    )?;

    // Read all blocks from LevelDB
    let read_opts = ReadOptions {
        fill_cache: false,
        verify_checksums: true,
        snapshot: None,
    };
    let read_opts_clone = ReadOptions { ..read_opts };
    let body_start = start_block.map_or(LevelDBKey::BodyStart, LevelDBKey::Body);
    let header_start = start_block.map_or(LevelDBKey::HeaderStart, LevelDBKey::Header);
    let header_iter = db.iter(read_opts_clone).from(&header_start);
    let body_iter = db.iter(read_opts).from(&body_start);
    let mut count = 0u64;

    if include_receipts {
        let read_opts_clone2 = ReadOptions {
            fill_cache: false,
            verify_checksums: true,
            snapshot: None,
        };
        let receipts_start = start_block.map_or(LevelDBKey::ReceiptsStart, LevelDBKey::Receipts);
        let receipts_iter = db.iter(read_opts_clone2).from(&receipts_start);

        for (
            ((body_key, body_value), (header_key, header_value)),
            (receipts_key, receipts_value),
        ) in body_iter
            .zip(header_iter.filter(|(k, _)| k.is_header()))
            .zip(receipts_iter)
        {
            if !body_key.is_body() || !header_key.is_header() || !receipts_key.is_receipts() {
                break;
            }
            if body_key.height() != header_key.height()
                || body_key.height() != receipts_key.height()
            {
                println!(
                    "body_key: {body_key:?}, header_key: {header_key:?}, receipts_key: {receipts_key:?}"
                );
                return Err("Body, header, and receipts keys have different heights".into());
            }
            let combined = rlp::encode_list::<&[u8], &[u8]>(&[
                header_value.as_ref(),
                body_value.as_ref(),
                receipts_value.as_ref(),
            ]);
            let block: Block = combined.as_ref().into();
            store.write_block(body_key.height().unwrap(), &block, 0)?;
            count = count.checked_add(1).ok_or("Overflow")?;
            if count % 100_000 == 0 {
                println!("Imported {count} blocks");
            }
        }
    } else {
        for ((body_key, body_value), (header_key, header_value)) in
            body_iter.zip(header_iter.filter(|(k, _)| k.is_header()))
        {
            if !body_key.is_body() || !header_key.is_header() {
                break;
            }
            if body_key.height() != header_key.height() {
                println!("body_key: {body_key:?}, header_key: {header_key:?}");
                return Err("Body and header keys have different heights".into());
            }
            let combined =
                rlp::encode_list::<&[u8], &[u8]>(&[header_value.as_ref(), body_value.as_ref()]);
            let block: Block = combined.as_ref().into();
            store.write_block(body_key.height().unwrap(), &block, 0)?;
            count = count.checked_add(1).ok_or("Overflow")?;
            if count % 100_000 == 0 {
                println!("Imported {count} blocks");
            }
        }
    }

    println!("Imported {count} blocks");
    Ok(())
}
