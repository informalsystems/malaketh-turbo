use alloy_consensus::{SignableTransaction, TxEip1559};
use alloy_primitives::{Address, Bloom, Bytes, FixedBytes, TxKind, B256, U256};
use alloy_rpc_types_engine::ExecutionPayloadV1;
use clap::{Parser, Subcommand};
use eyre::Result;
use reth_primitives::{Block, BlockBody, Header, Transaction, TransactionSigned};

use alloy_genesis::{ChainConfig, Genesis, GenesisAccount};
use serde_json;
use std::{collections::BTreeMap, str::FromStr};

use alloy_signer::Signer;
use alloy_signer_local::{coins_bip39::English, LocalSigner, MnemonicBuilder};
use k256::ecdsa::SigningKey;
use tokio::runtime::Runtime;

use rayon::prelude::*;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::sync::atomic::{AtomicU64, Ordering};

use regex::Regex;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;

/// Test mnemonics for wallet generation
const TEST_MNEMONICS: [&str; 4] = [
    "test test test test test test test test test test test junk",
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
    "zero zero zero zero zero zero zero zero zero zero zero zoo",
    "slender flush sting survey huge pottery brain vivid gentle guitar panic harbor",
];

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Generate test blocks and genesis configuration
    Generate {
        /// Number of transactions per block
        #[arg(long, default_value_t = 42000)]
        txs_per_block: usize,

        /// Number of blocks to generate
        #[arg(long, default_value_t = 10)]
        num_blocks: usize,
    },

    /// Verify node execution logs
    Verify,

    /// Verify both node logs and RPC state
    VerifyAll,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Generate {
            txs_per_block,
            num_blocks,
        } => generate_all(txs_per_block, num_blocks),
        Commands::Verify => verify_all_node_logs(),
        Commands::VerifyAll => verify_all(),
    }
}

/// Generate all test data including genesis and blocks
fn generate_all(txs_per_block: usize, num_blocks: usize) -> Result<()> {
    // Delete existing database folder if it exists
    let _ = std::fs::remove_dir_all("./data");

    // Create paths
    let genesis_file = "./data/genesis.json";

    // Create signers and get their addresses
    let signers: Vec<LocalSigner<SigningKey>> = TEST_MNEMONICS
        .iter()
        .map(|&mnemonic| {
            MnemonicBuilder::<English>::default()
                .phrase(mnemonic)
                .build()
                .expect("Failed to create wallet")
        })
        .collect();

    let signer_addresses: Vec<Address> = signers.iter().map(|signer| signer.address()).collect();

    println!("Using signer addresses:");
    for (i, addr) in signer_addresses.iter().enumerate() {
        println!("Signer {}: {}", i, addr);
    }

    let recipient = Address::from_str("0x1000000000000000000000000000000000000000")?;

    // Create genesis configuration with pre-funded accounts
    let mut alloc = BTreeMap::new();
    for addr in &signer_addresses {
        alloc.insert(
            *addr,
            GenesisAccount {
                balance: U256::from_str("15000000000000000000000").unwrap(), // 15000 ETH
                ..Default::default()
            },
        );
    }

    // Create genesis configuration
    let genesis = Genesis {
        config: ChainConfig {
            chain_id: 1,
            homestead_block: Some(0),
            eip150_block: Some(0),
            eip155_block: Some(0),
            eip158_block: Some(0),
            byzantium_block: Some(0),
            constantinople_block: Some(0),
            petersburg_block: Some(0),
            istanbul_block: Some(0),
            berlin_block: Some(0),
            london_block: Some(0),
            shanghai_time: Some(0),
            terminal_total_difficulty: Some(U256::ZERO),
            terminal_total_difficulty_passed: true,
            ..Default::default()
        },
        alloc,
        ..Default::default()
    };

    // Create data directory if it doesn't exist
    std::fs::create_dir_all("./data")?;

    // Write genesis to file
    let genesis_json = serde_json::to_string_pretty(&genesis)?;
    std::fs::write(genesis_file, genesis_json)?;
    println!("Genesis configuration written to {}", genesis_file);

    // Generate blocks for each signer
    for (i, signer) in signers.iter().enumerate() {
        let blocks_file = format!("./data/blocks-{}", i);
        generate_test_blocks(signer, recipient, txs_per_block, num_blocks, &blocks_file)?;
    }

    Ok(())
}

/// Generate test blocks that transfer ETH from sender to recipient and write them to a file
fn generate_test_blocks(
    signer: &LocalSigner<SigningKey>,
    recipient: Address,
    txs_per_block: usize,
    num_blocks: usize,
    output_file: &str,
) -> Result<()> {
    println!(
        "Writing {} blocks to {} with {} transactions each...",
        num_blocks, output_file, txs_per_block
    );

    // Create runtime for async transaction creation
    let rt = Runtime::new()?;
    let nonce_counter = AtomicU64::new(0);

    // Open file for writing
    let file = File::create(output_file)?;
    let mut writer = BufWriter::new(file);

    for block_idx in 0..num_blocks {
        // Generate transactions in parallel
        let mut block_txs: Vec<TransactionSigned> = (0..txs_per_block)
            .into_par_iter()
            .map(|_| {
                let nonce = nonce_counter.fetch_add(1, Ordering::SeqCst);
                rt.block_on(create_test_transaction(
                    signer,
                    recipient,
                    U256::from(10_000_000_000_000_000u64), // 0.01 ETH
                    nonce,
                ))
                .expect("Failed to create transaction")
            })
            .collect();

        // Sort transactions by nonce
        block_txs.sort_by_key(|tx| {
            if let Transaction::Eip1559(ref t) = tx.transaction() {
                t.nonce
            } else {
                unreachable!("We only create EIP1559 transactions")
            }
        });

        // Create block and convert to serialized form
        let block = create_test_block(block_txs);
        let payload = ExecutionPayloadV1::from_block_slow(&block);
        let bytes = serde_json::to_vec(&payload)?;

        // Write length of serialized block followed by the block itself
        writer.write_all(&(bytes.len() as u32).to_be_bytes())?;
        writer.write_all(&bytes)?;
        writer.flush()?;

        println!(
            "Wrote block {} of {} to {} (size: {:.2} MB, {} transactions)",
            block_idx + 1,
            num_blocks,
            output_file,
            bytes.len() as f64 / 1_000_000.0,
            txs_per_block
        );
    }

    Ok(())
}

/// Helper function to create a signed transaction (ETH transfer)
async fn create_test_transaction(
    signer: &LocalSigner<SigningKey>,
    to: Address,
    value: U256,
    nonce: u64,
) -> Result<TransactionSigned> {
    let tx = Transaction::Eip1559(TxEip1559 {
        chain_id: 1, // mainnet
        nonce,
        max_priority_fee_per_gas: 1_000_000_000, // 1 gwei
        max_fee_per_gas: 2_000_000_000,          // 2 gwei
        gas_limit: 21_000,                       // standard ETH transfer
        to: TxKind::Call(to),
        value,
        input: Bytes::default(),
        access_list: Default::default(),
    });

    // Sign the transaction with our private key
    let signature_hash = tx.signature_hash();
    let signature = signer.sign_hash(&signature_hash).await?;
    Ok(TransactionSigned::new_unhashed(tx, signature))
}

/// A custom struct to handle raw block bytes
pub struct SerializedBlock {
    bytes: Vec<u8>,
}

impl SerializedBlock {
    /// Create a new serialized block with a test transaction
    pub fn new(
        signer: &LocalSigner<SigningKey>,
        recipient: Address,
        value: U256,
        nonce: u64,
    ) -> Result<Self> {
        let rt = Runtime::new()?;
        let transaction = rt.block_on(create_test_transaction(signer, recipient, value, nonce))?;
        let block = create_test_block(vec![transaction]);

        // Convert block to payload
        let payload = ExecutionPayloadV1::from_block_slow(&block);

        // Convert payload to JSON bytes
        let bytes = serde_json::to_vec(&payload).unwrap_or_default();
        Ok(Self { bytes })
    }

    /// Parse the bytes into an ExecutionPayloadV1
    pub fn into_payload(&self) -> Result<ExecutionPayloadV1, serde_json::Error> {
        serde_json::from_slice(&self.bytes)
    }

    /// Convert the payload back into a Block
    pub fn into_block(&self) -> Result<Block> {
        let payload = self.into_payload()?;
        Ok(payload.try_into_block()?)
    }
}

/// Creates a test block with the given transactions
fn create_test_block(transactions: Vec<TransactionSigned>) -> Block {
    // Create a header with minimal data
    let header = Header {
        parent_hash: B256::default(),
        ommers_hash: B256::default(),
        beneficiary: Address::default(),
        state_root: B256::default(),
        transactions_root: B256::default(),
        receipts_root: B256::default(),
        logs_bloom: Bloom::default(),
        difficulty: U256::ZERO,
        number: 0,                // Block number not needed
        gas_limit: 1_000_000_000, // 1 billion gas limit to handle large blocks
        gas_used: 0,
        timestamp: 1234567890u64,
        extra_data: Bytes::default(),
        mix_hash: B256::default(),
        nonce: FixedBytes::new([0; 8]),
        base_fee_per_gas: Some(1_000_000_000), // 1 gwei
        withdrawals_root: None,
        blob_gas_used: None,
        excess_blob_gas: None,
        parent_beacon_block_root: None,
        requests_hash: None,
    };

    // Create a block with the transactions
    Block::new(
        header,
        BlockBody {
            transactions,
            ommers: vec![],
            withdrawals: None,
        },
    )
}

#[derive(Debug)]
struct BlockExecution {
    height: u64,
    block_number: u64,
    signer: Address,
    initial_balance: U256,
    final_balance: U256,
}

/// Verifies block execution logs for consistency
fn verify_block_logs(log_file: &str) -> eyre::Result<()> {
    let file = File::open(log_file)?;
    let reader = BufReader::new(file);
    let mut executions = Vec::new();
    let mut current_height = 0;
    let mut current_signer = None;

    // Read through log lines
    for line in reader.lines() {
        let line = line?;

        // Check for block execution start
        if line.contains("Executing block") {
            // Format: "Executing block N..."
            if let Some(height) = line
                .split_whitespace()
                .nth(2) // Get the number after "Executing block"
                .and_then(|s| s.trim_end_matches("...").parse::<u64>().ok())
            {
                current_height = height;
            }
        }
        // Check for transaction signer
        else if line.contains("Transaction signer:") {
            // Format: "Transaction signer: 0x..."
            if let Some(signer) = line
                .split_whitespace()
                .last()
                .and_then(|s| Address::from_str(s).ok())
            {
                current_signer = Some(signer);
            }
        }
        // Check for balance changes
        else if line.contains("balance for") && current_signer.is_some() {
            // Format: "Initial/Final balance for 0x... : N"
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                if let Ok(balance) = U256::from_str(parts.last().unwrap_or(&"0")) {
                    if line.starts_with("Initial") {
                        executions.push(BlockExecution {
                            height: current_height,
                            block_number: current_height,
                            signer: current_signer.unwrap(),
                            initial_balance: balance,
                            final_balance: balance, // Will be updated when we see final balance
                        });
                    } else if line.starts_with("Final") {
                        // Update the final balance of the last matching execution
                        if let Some(exec) = executions.iter_mut().rev().find(|e| {
                            e.height == current_height && e.signer == current_signer.unwrap()
                        }) {
                            exec.final_balance = balance;
                        }
                    }
                }
            }
        }
    }

    let max_height = executions.iter().map(|exec| exec.height).max().unwrap_or(0);

    if max_height < 10 {
        return Err(eyre::eyre!(
            "Not enough blocks in logs. Found blocks up to height {}, but need at least 10 blocks. Please run the network longer.",
            max_height
        ));
    }

    // Filter executions to only include blocks up to height 10
    let executions: Vec<_> = executions
        .into_iter()
        .filter(|exec| exec.height <= 10)
        .collect();

    // Verify block execution consistency
    println!("\nVerifying first 10 block executions...");

    // 1. Verify block numbers match heights
    for exec in &executions {
        if exec.height != exec.block_number {
            println!(
                "❌ Height/block number mismatch at height {}: block number is {}",
                exec.height, exec.block_number
            );
        }
    }

    // 2. Verify balance changes are consistent within each block
    let mut balance_sequences: HashMap<Address, Vec<(u64, U256, U256)>> = HashMap::new();

    // First collect all balance changes for each account in order of appearance
    for exec in &executions {
        balance_sequences.entry(exec.signer).or_default().push((
            exec.height,
            exec.initial_balance,
            exec.final_balance,
        ));
    }

    // Then verify each account's balance sequence
    for (signer, sequence) in &balance_sequences {
        println!("\nChecking balance changes for {}:", signer);

        // Sort by height to ensure we check in chronological order
        let mut sorted_sequence = sequence.clone();
        sorted_sequence.sort_by_key(|(height, _, _)| *height);

        // Track last known final balance for state consistency check
        let mut last_final_balance: Option<(u64, U256)> = None;

        // Print all appearances and verify each block execution
        for (height, initial, final_bal) in &sorted_sequence {
            // Check if initial balance matches last known final balance
            if let Some((last_height, last_balance)) = last_final_balance {
                if *initial != last_balance {
                    println!("⚠️  State inconsistency detected:");
                    println!(
                        "    Final balance at height {}: {}",
                        last_height, last_balance
                    );
                    println!("    Initial balance at height {}: {}", height, initial);
                    if height > &(last_height + 1) {
                        println!(
                            "    Note: {} block(s) between these heights",
                            height - last_height - 1
                        );
                    }
                }
            }

            // Calculate the change in this block
            let change = if final_bal > initial {
                format!("❌ increased by {}", final_bal - initial)
            } else {
                format!("✓ decreased by {}", initial - final_bal)
            };

            println!(
                "  Height {}: {} -> {} ({})",
                height, initial, final_bal, change
            );

            // Verify balance only decreases within each block
            if final_bal > initial {
                println!(
                    "❌ Unexpected balance increase at height {}: {} -> {}",
                    height, initial, final_bal
                );
            }

            // Update last known final balance
            last_final_balance = Some((*height, *final_bal));
        }
    }

    // 3. Verify no missing blocks from 1 to 10
    let executed_heights: HashSet<u64> = executions.iter().map(|exec| exec.height).collect();

    let mut missing_blocks = false;
    for height in 1..=10 {
        if !executed_heights.contains(&height) {
            println!("❌ Missing block execution for height {}", height);
            missing_blocks = true;
        }
    }

    if !missing_blocks {
        println!("✅ All blocks from height 1 to 10 were executed");
    }

    println!("✅ Verification complete");
    Ok(())
}

/// Helper function to verify logs for all nodes
fn verify_all_node_logs() -> Result<()> {
    let nodes_dir = Path::new("nodes");
    if !nodes_dir.exists() {
        return Err(eyre::eyre!("nodes directory not found"));
    }

    let node0_log = nodes_dir.join("0").join("logs").join("node.log");
    if !node0_log.exists() {
        return Err(eyre::eyre!("node 0 log file not found at {:?}", node0_log));
    }

    println!("\nVerifying logs for node 0:");
    verify_block_logs(node0_log.to_str().unwrap())?;

    Ok(())
}

/// Verifies that the RPC server state matches the logs
fn verify_rpc_state() -> Result<()> {
    println!("\nVerifying RPC server state against logs...");

    // Helper function to make RPC calls
    fn rpc_call(method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
        let client = reqwest::blocking::Client::new();
        let response = client
            .post("http://127.0.0.1:8545")
            .json(&serde_json::json!({
                "jsonrpc": "2.0",
                "method": method,
                "params": params,
                "id": 1
            }))
            .send()?
            .json::<serde_json::Value>()?;

        if let Some(error) = response.get("error") {
            return Err(eyre::eyre!("RPC error: {}", error));
        }

        Ok(response
            .get("result")
            .unwrap_or(&serde_json::json!(null))
            .clone())
    }

    // Get latest block number
    let latest_block = rpc_call("eth_blockNumber", serde_json::json!([]))?
        .as_str()
        .and_then(|s| u64::from_str_radix(&s[2..], 16).ok())
        .ok_or_else(|| eyre::eyre!("Invalid block number response"))?;

    println!("Latest block from RPC: {}", latest_block);

    // Check first 10 blocks like in verify_block_logs
    let mut executions = Vec::new();

    for height in 1..=10.min(latest_block) {
        println!("\nChecking block {}...", height);

        // Get block with transactions
        let block = rpc_call(
            "eth_getBlockByNumber",
            serde_json::json!([format!("0x{:x}", height), true]),
        )?;

        if let Some(block) = block.as_object() {
            // Get first transaction signer
            if let Some(txs) = block.get("transactions").and_then(|t| t.as_array()) {
                if let Some(first_tx) = txs.first() {
                    let signer = first_tx
                        .get("from")
                        .and_then(|s| s.as_str())
                        .ok_or_else(|| eyre::eyre!("Missing transaction signer"))?;

                    // Get signer's balance before and after block
                    let prev_block = format!("0x{:x}", height - 1);
                    let curr_block = format!("0x{:x}", height);

                    let initial_balance =
                        rpc_call("eth_getBalance", serde_json::json!([signer, prev_block]))?;

                    let final_balance =
                        rpc_call("eth_getBalance", serde_json::json!([signer, curr_block]))?;

                    let initial_balance =
                        U256::from_str_radix(&initial_balance.as_str().unwrap_or("0x0")[2..], 16)?;

                    let final_balance =
                        U256::from_str_radix(&final_balance.as_str().unwrap_or("0x0")[2..], 16)?;

                    executions.push(BlockExecution {
                        height,
                        block_number: height,
                        signer: Address::from_str(signer)?,
                        initial_balance,
                        final_balance,
                    });
                }
            }
        }
    }

    // Now perform the same verifications as in verify_block_logs

    // 1. Verify block numbers match heights (already done by construction)

    // 2. Verify balance changes are consistent within each block
    let mut balance_sequences: HashMap<Address, Vec<(u64, U256, U256)>> = HashMap::new();

    // Collect all balance changes for each account
    for exec in &executions {
        balance_sequences.entry(exec.signer).or_default().push((
            exec.height,
            exec.initial_balance,
            exec.final_balance,
        ));
    }

    // Verify each account's balance sequence
    for (signer, sequence) in &balance_sequences {
        println!("\nChecking balance changes for {}:", signer);

        // Sort by height to ensure chronological order
        let mut sorted_sequence = sequence.clone();
        sorted_sequence.sort_by_key(|(height, _, _)| *height);

        let mut last_final_balance: Option<(u64, U256)> = None;

        for (height, initial, final_bal) in &sorted_sequence {
            if let Some((last_height, last_balance)) = last_final_balance {
                if *initial != last_balance {
                    println!("⚠️  State inconsistency detected:");
                    println!(
                        "    Final balance at height {}: {}",
                        last_height, last_balance
                    );
                    println!("    Initial balance at height {}: {}", height, initial);
                    if height > &(last_height + 1) {
                        println!(
                            "    Note: {} block(s) between these heights",
                            height - last_height - 1
                        );
                    }
                }
            }

            let change = if final_bal > initial {
                format!("❌ increased by {}", final_bal - initial)
            } else {
                format!("✓ decreased by {}", initial - final_bal)
            };

            println!(
                "  Height {}: {} -> {} ({})",
                height, initial, final_bal, change
            );

            if final_bal > initial {
                println!(
                    "❌ Unexpected balance increase at height {}: {} -> {}",
                    height, initial, final_bal
                );
            }

            last_final_balance = Some((*height, *final_bal));
        }
    }

    // 4. Verify no missing blocks from 1 to 10
    let executed_heights: HashSet<u64> = executions.iter().map(|exec| exec.height).collect();

    let mut missing_blocks = false;
    for height in 1..=10.min(latest_block) {
        if !executed_heights.contains(&height) {
            println!("❌ Missing block execution for height {}", height);
            missing_blocks = true;
        }
    }

    if !missing_blocks {
        println!(
            "✅ All blocks from height 1 to {} were executed",
            10.min(latest_block)
        );
    }

    println!("✅ RPC state verification complete");
    Ok(())
}

/// Helper function to verify both logs and RPC state
fn verify_all() -> Result<()> {
    verify_all_node_logs()?;
    verify_rpc_state()?;
    Ok(())
}
