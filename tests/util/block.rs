use bitcoin::pow::{Target, TargetExt as _};
use bitcoin::{Block as BitcoinBlock, BlockHeader, compute_merkle_root};
use encoding::{decode_from_slice, encode_to_vec};

const BLOCK_HEADER_LEN: usize = 80;
const MERKLE_ROOT_OFFSET: usize = 36;
const NONCE_OFFSET: usize = 76;

pub struct BlockSolution {
    pub version: u32,
    pub timestamp: u32,
    pub nonce: u32,
    pub coinbase: Vec<u8>,
}

pub fn block_solution(block: &[u8]) -> BlockSolution {
    let block: BitcoinBlock =
        decode_from_slice(block).unwrap_or_else(|e| panic!("failed to decode block: {e}"));
    let (header, transactions) = block.into_parts();
    let coinbase = transactions
        .first()
        .expect("block template must have a coinbase transaction");

    BlockSolution {
        version: header
            .version
            .to_consensus()
            .try_into()
            .expect("block version must fit in u32"),
        timestamp: header.time.to_u32(),
        nonce: header.nonce,
        coinbase: encode_to_vec(coinbase),
    }
}

pub fn block_with_pow(block: &[u8], valid_pow: bool) -> Vec<u8> {
    assert!(
        block.len() >= BLOCK_HEADER_LEN,
        "block must include an 80-byte header"
    );

    let mut block = block.to_vec();
    let mut header: BlockHeader = decode_from_slice(&block[..BLOCK_HEADER_LEN])
        .unwrap_or_else(|e| panic!("failed to decode block header: {e}"));
    let decoded_block: BitcoinBlock =
        decode_from_slice(&block).unwrap_or_else(|e| panic!("failed to decode block: {e}"));
    let (_, transactions) = decoded_block.into_parts();
    // Keep the block self-consistent so submitBlock reaches the intended PoW
    // branch instead of failing earlier on the Merkle root.
    header.merkle_root =
        compute_merkle_root(&transactions).expect("block template must have transactions");
    block[MERKLE_ROOT_OFFSET..MERKLE_ROOT_OFFSET + 32]
        .copy_from_slice(header.merkle_root.as_byte_array());

    let start_nonce = header.nonce;
    let mut nonce = start_nonce;
    loop {
        nonce = nonce.wrapping_add(1);
        header.nonce = nonce;
        block[NONCE_OFFSET..NONCE_OFFSET + 4].copy_from_slice(&nonce.to_le_bytes());

        if Target::from_compact(header.bits).is_met_by(header.block_hash()) == valid_pow {
            return block;
        }

        assert!(
            nonce != start_nonce,
            "failed to find a nonce with valid_pow={valid_pow}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::Network;
    use bitcoin::blockdata::constants::genesis_block;

    #[test]
    fn grinds_valid_and_invalid_pow() {
        let block = regtest_genesis_block();

        let valid = block_with_pow(&block, true);
        let header: BlockHeader = decode_from_slice(&valid[..BLOCK_HEADER_LEN]).unwrap();
        assert!(Target::from_compact(header.bits).is_met_by(header.block_hash()));

        let invalid = block_with_pow(&block, false);
        let header: BlockHeader = decode_from_slice(&invalid[..BLOCK_HEADER_LEN]).unwrap();
        assert!(!Target::from_compact(header.bits).is_met_by(header.block_hash()));
    }

    #[test]
    fn extracts_submit_solution_fields() {
        let block = block_with_pow(&regtest_genesis_block(), true);
        let solution = block_solution(&block);
        let decoded: BitcoinBlock = decode_from_slice(&block).unwrap();
        let (header, transactions) = decoded.into_parts();

        assert_eq!(solution.version, header.version.to_consensus() as u32);
        assert_eq!(solution.timestamp, header.time.to_u32());
        assert_eq!(solution.nonce, header.nonce);
        assert_eq!(solution.coinbase, encode_to_vec(&transactions[0]));
    }

    fn regtest_genesis_block() -> Vec<u8> {
        let checked = genesis_block(Network::Regtest);
        let block = BitcoinBlock::new_unchecked(*checked.header(), checked.transactions().to_vec());
        encode_to_vec(&block)
    }
}
