#![allow(dead_code)]
type Hash = [u8; 32];
type Address = [u8; 20];
type Bloom = [u8; 256];
type BigInt = [u8; 32];
type BlockNonce = [u8; 8];
type Withdrawal = [u8; 32];

#[derive(Debug)]
struct Block {
    header: Header,
    txs: Vec<Transaction>,
    uncles: Vec<Header>,
    withdrawals: Vec<Withdrawal>,
}

#[derive(Debug)]
struct Header {
    parent_hash: Hash,
    uncle_hash: Hash,
    coinbase: Address,
    root: Hash,
    tx_hash: Hash,
    receipt_hash: Hash,
    bloom: Bloom,
    difficulty: BigInt,
    number: BigInt,
    gas_limit: u64,
    gas_used: u64,
    time: u64,
    extra: Vec<u8>,
    mix_digest: Hash,
    nonce: BlockNonce,
    base_fee: BigInt,
    withdrawals_hash: Hash,
    blob_gas_used: u64,
    excess_blob_gas: u64,
    parent_beacon_root: Hash,
    requests_hash: Hash,
}

#[derive(Debug)]
pub struct Transaction {
    pub parent_hash: Hash,
    pub uncle_hash: Hash,
    pub coinbase: Address,
    pub root: Hash,
    pub tx_hash: Hash,
    pub receipt_hash: Hash,
    pub bloom: Bloom,
    pub difficulty: BigInt,
    pub number: BigInt,
    pub gas_limit: u64,
    pub gas_used: u64,
    pub time: u64,
    pub extra: Vec<u8>,
    pub mix_digest: Hash,
    pub nonce: BlockNonce,
    pub base_fee: Option<BigInt>,
    pub withdrawals_hash: Option<Hash>,
    pub blob_gas_used: Option<u64>,
    pub excess_blob_gas: Option<u64>,
    pub parent_beacon_root: Option<Hash>,
    pub requests_hash: Option<Hash>,
}
