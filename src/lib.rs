use std::fmt::{self, Display};
use std::io::Error;
use std::num::NonZeroU64;
use std::slice::from_raw_parts;
use std::ptr::null;

use bincode::{Decode, Encode, config};

pub mod ffi;
pub mod store;

type BlockId = NonZeroU64;

#[derive(Debug, Copy, Clone, Encode, Decode)]
#[repr(C)]
pub struct BlockHeader {
    pub id: BlockId,
    pub len: usize,
}

const BINCODE_CONFIG: config::Configuration<config::LittleEndian, config::Fixint, config::NoLimit> =
    config::legacy();

impl BlockHeader {
    /// Encodes the block header into bytes.
    ///
    /// # Panics
    /// Panics if:
    /// - The header data exceeds bincode's size limits (should never happen)
    /// - The header contains invalid data that cannot be serialized    
    #[must_use]
    pub fn as_bytes(&self) -> Box<[u8]> {
        // TODO: use a more efficient encoding method
        bincode::encode_to_vec(self, BINCODE_CONFIG)
            .unwrap()
            .into_boxed_slice()
    }

    /// Decodes a block header from bytes.
    ///
    /// # Errors
    /// Returns an error if:
    /// - The input bytes are not a valid block header
    /// - The input bytes are too short
    /// - The decoded data is invalid
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        // TODO: use a more efficient decoding method
        let (header, _) =
            bincode::borrow_decode_from_slice::<BlockHeader, _>(bytes, BINCODE_CONFIG)
                .map_err(Error::other)?;
        Ok(header)
    }
}

#[derive(Debug, Copy, Clone)]
#[repr(C)]
pub struct Block {
    pub header: BlockHeader,
    pub data: *const u8,
}
impl Block {
    /// Encodes the block into bytes.
    #[must_use]
    pub fn as_bytes(&self) -> Box<[u8]> {
        let data_slice = unsafe { from_raw_parts(self.data, self.header.len) };
        self.header
            .id
            .get()
            .to_ne_bytes()
            .iter()
            .chain(self.header.len.to_ne_bytes().iter())
            .chain(data_slice.iter())
            .copied()
            .collect()
    }
}

impl Display for Block {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Block {{ id: {}, len: {}, data: {:?} }}",
            self.header.id, self.header.len, self.data
        )
    }
}

impl Default for Block {
    fn default() -> Self {
        Block {
            header: BlockHeader {
                id: BlockId::MAX,
                len: 0,
            },
            data: null(),
        }
    }
}
