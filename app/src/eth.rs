//! Internal state of the application. This is a simplified abstract to keep it simple.
//! A regular application would have mempool implemented, a proper database and input methods like RPC.

use std::fs::File;
use std::io::{BufReader, Read};

use bytes::Bytes;
use color_eyre::eyre;
use tracing::{error, info};

use alloy_genesis::Genesis as EthGenesis;
use malachitebft_reth_types::Address;

use reth::{
    api::NodeTypesWithDBAdapter,
    beacon_consensus::EthBeaconConsensus,
    providers::{
        providers::{BlockchainProvider, StaticFileProvider},
        ProviderFactory,
    },
    rpc::eth::EthApi,
};

use reth_node_ethereum::{EthEvmConfig, EthExecutorProvider, EthereumNode};

use reth::rpc::builder::{
    RethRpcModule, RpcModuleBuilder, RpcServerConfig, RpcServerHandle, TransportRpcModuleConfig,
};

use reth::tasks::TokioTaskExecutor;

use alloy_consensus::BlockHeader;
use alloy_primitives::{Address as EthAddress, U256};
use alloy_rpc_types_engine::ExecutionPayloadV1;
use eyre::Result;
use reth_chainspec::{ChainSpec, ChainSpecBuilder};
use reth_db::{mdbx::DatabaseArguments, DatabaseEnv};
use reth_db_common::init::init_genesis;
use reth_evm::execute::{BlockExecutorProvider, ExecutionOutcome, Executor};
use reth_primitives::{Block, RecoveredBlock};
use reth_primitives_traits::transaction::signed::SignedTransaction;
use reth_provider::{AccountReader, BlockWriter, DatabaseProviderFactory, StateProviderFactory};
use reth_revm::database::StateProviderDatabase;
use reth_trie::{updates::TrieUpdates, HashedPostStateSorted};
use serde_json;
use std::path::PathBuf;
use std::time::Instant;
use std::{collections::BTreeMap, sync::Arc};

/// Read blocks from a file one at a time
pub struct BlockProposer {
    reader: BufReader<File>,
}

impl BlockProposer {
    pub fn new(path: &str) -> eyre::Result<Self> {
        let file = File::open(path)?;
        Ok(Self {
            reader: BufReader::new(file),
        })
    }

    pub fn read_block(&mut self) -> eyre::Result<Option<Vec<u8>>> {
        let mut len_bytes = [0u8; 4];
        match self.reader.read_exact(&mut len_bytes) {
            Ok(_) => {
                let len = u32::from_be_bytes(len_bytes) as usize;
                let mut block_bytes = vec![0u8; len];
                self.reader.read_exact(&mut block_bytes)?;

                Ok(Some(block_bytes))
            }
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn propose_block(&mut self, current_height: u64) -> eyre::Result<Bytes> {
        let Some(block) = self.read_block()? else {
            return Ok(Bytes::new());
        };

        // Deserialize the block
        let payload: ExecutionPayloadV1 = serde_json::from_slice(&block)?;
        let mut block: Block = payload.try_into_block()?;

        // Update the block number to match consensus height
        block.header.number = current_height;

        // Convert back to payload and serialize
        let payload = ExecutionPayloadV1::from_block_slow(&block);
        let bytes = serde_json::to_vec(&payload)?;

        Ok(Bytes::from(bytes))
    }
}
// Tracks and reports timing information for block execution steps
struct BlockExecutionTimer {
    start_time: Instant,
    starts: BTreeMap<&'static str, Instant>,
    timings: BTreeMap<&'static str, f64>,
}

impl BlockExecutionTimer {
    fn new() -> Self {
        Self {
            start_time: Instant::now(),
            starts: BTreeMap::new(),
            timings: BTreeMap::new(),
        }
    }

    fn start(&mut self, name: &'static str) {
        self.starts.insert(name, Instant::now());
    }

    fn end(&mut self, name: &'static str) {
        if let Some(start) = self.starts.remove(name) {
            self.timings.insert(name, start.elapsed().as_secs_f64());
        }
    }

    fn report(&self, block_number: u64, num_txs: usize, gas_used: u64, num_receipts: usize) {
        let total_time = self.start_time.elapsed().as_secs_f64();
        let tracked_time: f64 = self.timings.values().sum();

        info!("Block execution completed:");
        info!("  Gas used: {}", gas_used);
        info!("  Number of receipts: {}", num_receipts);

        info!("\nBlock {} timing breakdown:", block_number);
        for (name, duration) in &self.timings {
            info!("  {} time: {:.2}s", name, duration);
        }
        info!("  Total time: {:.2}s", total_time);
        info!("  Other overhead: {:.2}s", total_time - tracked_time);
        info!(
            "  Transactions per second: {:.2}",
            num_txs as f64 / total_time
        );
    }
}

/// Handles block execution and database interactions
#[derive(Clone)]
pub struct BlockExecutor {
    blockchain: Arc<BlockchainProvider<NodeTypesWithDBAdapter<EthereumNode, Arc<DatabaseEnv>>>>,
    spec: Arc<ChainSpec>,
}

impl BlockExecutor {
    /// Create a new BlockExecutor with the given database path and genesis configuration
    pub fn new(db_path: PathBuf, genesis: EthGenesis) -> Result<Self> {
        info!("Creating BlockExecutor with db_path: {:?}", db_path);

        // Delete existing database folder if it exists
        if db_path.exists() {
            info!("Removing existing db directory");
            std::fs::remove_dir_all(&db_path)?;
        }

        // Create directories
        info!("Creating db directory");
        std::fs::create_dir_all(&db_path)?;

        // Create static files path
        let static_files_path = db_path.join("static_files");
        info!("Creating static files directory at {:?}", static_files_path);
        std::fs::create_dir_all(&static_files_path)?;

        // Verify directories exist and have correct permissions
        if !static_files_path.exists() {
            error!("Static files directory was not created successfully");
            return Err(eyre::eyre!("Failed to create static files directory"));
        }

        // Create chain specification
        let spec = Arc::new(ChainSpecBuilder::mainnet().genesis(genesis).build());

        info!("Creating provider factory");
        let factory = ProviderFactory::<NodeTypesWithDBAdapter<EthereumNode, Arc<DatabaseEnv>>>::new_with_database_path(
            &db_path,
            spec.clone(),
            DatabaseArguments::default(),
            StaticFileProvider::read_write(static_files_path)?,
        )?;

        info!("Initializing genesis state");
        if let Err(e) = init_genesis(&factory) {
            error!("Failed to initialize genesis: {}", e);
            return Err(e.into());
        }

        info!("Creating blockchain provider");
        let blockchain = Arc::new(BlockchainProvider::new(factory)?);

        Ok(Self { blockchain, spec })
    }

    /// Execute and commit the next block
    pub fn next_block(&self, data: &Bytes) -> Result<()> {
        // Deserialize the block data into a Block
        let payload: ExecutionPayloadV1 = serde_json::from_slice(&data)
            .map_err(|e| eyre::eyre!("Failed to deserialize ExecutionPayloadV1: {}", e))?;
        let block: Block = payload
            .try_into_block()
            .map_err(|e| eyre::eyre!("Failed to convert ExecutionPayloadV1 to Block: {}", e))?;

        println!("\nExecuting block {}...", block.number);
        if let Some(tx) = block.body.transactions.first() {
            println!("Transaction signer: {}", tx.recover_signer().unwrap());
        }

        // Store initial balances for validation
        let mut initial_balances = BTreeMap::new();
        for tx in &block.body.transactions {
            let signer = tx.recover_signer().unwrap();
            if !initial_balances.contains_key(&signer) {
                // Convert alloy_primitives::Address to our Address type
                let signer_bytes: [u8; 20] = *signer.as_ref();
                let our_signer = Address::new(signer_bytes);

                if let Some(balance) = self.get_balance(&our_signer)? {
                    initial_balances.insert(signer, balance);
                    println!("Initial balance for {}: {}", signer, balance);
                }
            }
        }

        let mut timer = BlockExecutionTimer::new();

        // Provider setup
        timer.start("Provider setup");
        let executor_provider = EthExecutorProvider::ethereum(self.spec.clone());
        let state_provider = self.blockchain.latest()?;
        let executor = executor_provider.executor(StateProviderDatabase::new(&state_provider));
        timer.end("Provider setup");

        // Block recovery
        timer.start("Block recovery");
        let recovered_block = RecoveredBlock::try_recover(block.clone())?;
        timer.end("Block recovery");

        // Block execution
        timer.start("Execution");
        let result = executor.execute(&recovered_block)?;
        timer.end("Execution");

        // Store these before moving result
        let gas_used = result.gas_used;
        let num_receipts = result.receipts.len();

        // Validate receipts
        if num_receipts != block.body.transactions.len() {
            return Err(eyre::eyre!(
                "Receipt count mismatch: got {} but expected {}",
                num_receipts,
                block.body.transactions.len()
            ));
        }

        // Check all receipts are successful
        for receipt in &result.receipts {
            if !receipt.success {
                return Err(eyre::eyre!(
                    "Transaction failed: cumulative_gas_used={}",
                    receipt.cumulative_gas_used
                ));
            }
        }

        // Write setup
        timer.start("Write setup");
        let provider_rw = self.blockchain.database_provider_rw()?;
        let execution_outcome = ExecutionOutcome::from((result, recovered_block.number()));
        timer.end("Write setup");

        // State commit
        timer.start("State commit");
        provider_rw.append_blocks_with_state(
            vec![recovered_block],
            &execution_outcome,
            HashedPostStateSorted::default(),
            TrieUpdates::default(),
        )?;
        provider_rw.commit()?;
        timer.end("State commit");

        // Validate final state
        for (signer, initial_balance) in initial_balances {
            // Convert alloy_primitives::Address to our Address type
            let signer_bytes: [u8; 20] = *signer.as_ref();
            let our_signer = Address::new(signer_bytes);

            if let Some(final_balance) = self.get_balance(&our_signer)? {
                println!("Final balance for {}: {}", signer, final_balance);
                if final_balance > initial_balance {
                    return Err(eyre::eyre!(
                        "Unexpected balance increase for {}: {} -> {}",
                        signer,
                        initial_balance,
                        final_balance
                    ));
                }
            }
        }

        timer.report(
            block.number,
            block.body.transactions.len(),
            gas_used,
            num_receipts,
        );

        Ok(())
    }

    /// Get account balance
    fn get_balance(&self, address: &Address) -> Result<Option<U256>> {
        let state_provider = self.blockchain.latest()?;
        // Convert our Address type to alloy_primitives::Address
        let eth_address = EthAddress::new(address.into_inner());

        Ok(state_provider
            .basic_account(&eth_address)?
            .map(|account| account.balance))
    }

    /// Start the RPC server
    pub async fn start_server(&self) -> Result<RpcServerHandle> {
        // Configure which RPC namespaces to expose
        let module_config = TransportRpcModuleConfig::default().with_http([RethRpcModule::Eth]);

        // Create the RPC module builder with our components
        let rpc_builder = RpcModuleBuilder::default()
            .with_provider((*self.blockchain).clone())
            .with_noop_pool() // We don't need transaction pool for this example
            .with_noop_network() // We don't need network for this example
            .with_executor(TokioTaskExecutor::default())
            .with_evm_config(EthEvmConfig::new(self.spec.clone()))
            .with_block_executor(EthExecutorProvider::ethereum(self.spec.clone()))
            .with_consensus(EthBeaconConsensus::new(self.spec.clone()));

        // Build the server modules
        let server = rpc_builder.build(module_config, Box::new(EthApi::with_spawner));

        // Configure and start the server
        let server_config = RpcServerConfig::http(Default::default())
            .with_http_address("127.0.0.1:8545".parse().unwrap());

        let handle = server_config.start(&server).await?;

        println!(
            "RPC server started at http://{}",
            handle.http_local_addr().unwrap()
        );

        Ok(handle)
    }
}
