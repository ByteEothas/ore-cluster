mod args;
mod config;
mod dynamic_fee;
mod listener;
mod mine;
mod send_and_confirm;
mod send_and_confirm_auto;
mod send_and_confirm_jito;
mod send_and_confirm_rpc;
mod utils;

use crate::config::{
    CONFIRM_DELAY, CONFIRM_RETRIES, GATEWAY_DELAY, GATEWAY_RETRIES, JITO_RETRIES, JTIO_CLIENT_URL,
    MAX_JITO_TIP, MAX_RPC_PRIORITY_FEE, STEP_MULTIPLE,
};
use args::*;
use clap::{command, Parser, Subcommand};
use drillx::Solution;
use serde::{Deserialize, Serialize};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_program::pubkey::Pubkey;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    signature::{read_keypair_file, Keypair},
};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::net::SocketAddr;
use std::time::{Duration, SystemTime};
use std::{cell::RefCell, sync::Arc, sync::RwLock};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use tracing::{error, info, warn};
use tracing_subscriber::util::SubscriberInitExt;
use url::Url;

const SERVER_HOST: &str = "0.0.0.0";
const SERVER_PORT: u16 = 3000;
const BUFFER_TIME: u64 = 1;
struct ClientInfo {
    cores: u64,
    sender: tokio::net::tcp::OwnedWriteHalf,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum ActionType {
    Wait,
    Start,
    Send,
}

struct SharedState {
    clients: Arc<RwLock<HashMap<SocketAddr, ClientInfo>>>,
    total_cores: Arc<RwLock<u64>>,
    action_type: Arc<RwLock<ActionType>>,
    mining_clients: Arc<RwLock<HashSet<SocketAddr>>>,
    best_difficulty: Arc<RwLock<u32>>,
    solution: Arc<RwLock<Option<Solution>>>,
}

#[derive(Debug, Deserialize)]
struct LandStatus {
    pub count: u64,
    pub last_15_times: [u64; 15],
    pub fee_multiple: f32,
    pub difficulty: u32,
    pub difficulty_hits: HashMap<u32, u64>,
    pub start_time: SystemTime,
    pub total_hash_times: u64,
    pub total_retry_times: u64,
    pub total_failed_times: u64,
    pub total_error_times: u64,
}

struct UserConfig {
    pub gateway_retries: u64,
    pub gateway_delay: u64,
    pub confirm_retries: usize,
    pub confirm_delay: u64,
    pub max_rpc_priority_fee: u64,
    pub step_multiple: f32,
    pub max_jito_tip: u64,
    pub jito_retries: u64,
}
impl fmt::Display for UserConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "GATEWAY_RETRIES: {}\nGATEWAY_DELAY: {}\nCONFIRM_RETRIES: {}\nCONFIRM_DELAY: {}\nMAX_RPC_PRIORITY_FEE: {}\nSTEP_MULTIPLE: {}\nMAX_JITO_TIP: {}\nJITO_RETRIES: {}",
            self.gateway_retries,
            self.gateway_delay,
            self.confirm_retries,
            self.confirm_delay,
            self.max_rpc_priority_fee,
            self.step_multiple,
            self.max_jito_tip,
            self.jito_retries,
        )
    }
}

struct Miner {
    pub priority_fee: Option<u64>,
    pub dynamic_fee_url: Option<String>,
    pub enable_dynamic_fee: bool,
    pub enable_jito: bool,
    pub boost_1: Option<String>,
    pub boost_data_1: RefCell<Vec<Pubkey>>,
    pub rpc_client: Arc<RpcClient>,
    pub send_client: Arc<RpcClient>,
    pub jito_client: Arc<RpcClient>,
    pub websocket: Option<String>,
    pub land_status: RefCell<LandStatus>,
    pub last_balance: RefCell<u64>,
    pub earned_balance: RefCell<u64>,
    pub last_hash_at: RefCell<i64>,
    pub keypair: Keypair,
    pub fee_payer: Keypair,
    pub last_reset_at: Arc<RwLock<i64>>,
    pub user_config: UserConfig,
    pub tip: Arc<RwLock<u64>>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    #[command(about = "Start mining")]
    Mine(MineArgs),
}

#[derive(Parser, Debug)]
#[command(about, version)]
struct Args {
    #[arg(
        long,
        value_name = "NETWORK_URL",
        help = "Network address of your RPC provider",
        global = true
    )]
    rpc: Option<String>,

    #[arg(
        long,
        value_name = "SEND_NETWORK_URL",
        help = "Network address of your Send RPC provider",
        global = true
    )]
    rpc_send: Option<String>,

    #[arg(
        long,
        value_name = "WEBSOCKET_URL",
        help = "Network address of your websocket",
        global = true
    )]
    websocket: Option<String>,

    #[arg(
        long,
        value_name = "KEYPAIR_FILEPATH",
        help = "Filepath to signer keypair.",
        global = true
    )]
    keypair: Option<String>,

    #[arg(
        long,
        value_name = "FEE_PAYER_FILEPATH",
        help = "Filepath to transaction fee payer keypair.",
        global = true
    )]
    fee_payer: Option<String>,

    #[arg(
        long,
        value_name = "MICROLAMPORTS",
        help = "Price to pay for compute units. If dynamic fees are enabled, this value will be used as the cap.",
        default_value = "500000",
        global = true
    )]
    priority_fee: Option<u64>,

    #[arg(
        long,
        value_name = "DYNAMIC_FEE_URL",
        help = "RPC URL to use for dynamic fee estimation.",
        global = true
    )]
    dynamic_fee_url: Option<String>,

    #[arg(long, help = "Enable dynamic priority fees", global = true)]
    enable_dynamic_fee: bool,

    #[arg(long, help = "Enable jito", global = true)]
    enable_jito: bool,

    #[arg(
        long,
        value_name = "MINT_ADDRESS",
        help = "The token to apply as boost #1"
    )]
    pub boost_1: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Serialize, Deserialize, Debug)]
struct ProofData {
    challenge: [u8; 32],
    cutoff_time: u64,
    last_hash_at: i64,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    set_up_logging();

    // Version
    info!(
        "ore-guide version: {}",
        env!("CARGO_PKG_VERSION").to_string()
    );
    // Ore miner
    let args = Args::parse();
    let cluster = args.rpc.expect("No rpc server.");
    let rpc_send = args.rpc_send.unwrap_or(cluster.clone());
    let websocket = args.websocket;
    let default_keypair = args.keypair.expect("No key pair.");
    let fee_payer_filepath = args.fee_payer.unwrap_or(default_keypair.clone());
    let rpc_client = RpcClient::new_with_commitment(cluster, CommitmentConfig::confirmed());
    let send_client = RpcClient::new_with_commitment(rpc_send, CommitmentConfig::confirmed());
    let jito_client = RpcClient::new(JTIO_CLIENT_URL.to_string());
    let keypair = read_keypair_file(default_keypair.clone())
        .expect(format!("No keypair found at {}", default_keypair).as_str());
    let fee_payer = read_keypair_file(fee_payer_filepath.clone())
        .expect(format!("No fee payer keypair found at {}", fee_payer_filepath).as_str());
    let last_reset_at = Arc::new(RwLock::new(0));
    let user_config = UserConfig {
        gateway_retries: get_env("GATEWAY_RETRIES", GATEWAY_RETRIES),
        gateway_delay: get_env("GATEWAY_DELAY", GATEWAY_DELAY),
        confirm_retries: get_env("CONFIRM_RETRIES", CONFIRM_RETRIES),
        confirm_delay: get_env("CONFIRM_DELAY", CONFIRM_DELAY),
        max_rpc_priority_fee: get_env("MAX_RPC_PRIORITY_FEE", MAX_RPC_PRIORITY_FEE),
        step_multiple: get_env("STEP_MULTIPLE", STEP_MULTIPLE),
        max_jito_tip: get_env("MAX_JITO_TIP", MAX_JITO_TIP),
        jito_retries: get_env("JITO_RETRIES", JITO_RETRIES),
    };

    let miner = Arc::new(Miner::new(
        Arc::new(rpc_client),
        Arc::new(send_client),
        Arc::new(jito_client),
        websocket,
        args.priority_fee,
        args.dynamic_fee_url,
        args.enable_dynamic_fee,
        args.enable_jito,
        args.boost_1,
        keypair,
        fee_payer,
        last_reset_at,
        user_config,
    ));
    // Listener
    if let Some(ws) = &miner.websocket {
        info!(
            "Websocket: {}",
            Url::parse(&ws).unwrap().host_str().unwrap().to_string()
        );
        miner.get_config().await;
    } else {
        miner.get_config_pull().await;
    }
    // Get jito tip
    if miner.enable_jito {
        miner.get_jito_tip().await;
    }
    // Get boost data
    miner.fetch_boost_data(&miner.boost_1).await;

    // Ore server
    let notify = Arc::new(Notify::new());
    let state = Arc::new(SharedState {
        clients: Arc::new(RwLock::new(HashMap::new())),
        total_cores: Arc::new(RwLock::new(0)),
        action_type: Arc::new(RwLock::new(ActionType::Wait)),
        mining_clients: Arc::new(RwLock::new(HashSet::new())),
        best_difficulty: Arc::new(RwLock::new(0)),
        solution: Arc::new(RwLock::new(None)),
    });

    let listener = TcpListener::bind(format!("{}:{}", SERVER_HOST, SERVER_PORT)).await?;
    info!("\n{}", &miner.user_config);
    info!(
        "Rpc: {}{}",
        Url::parse(&miner.rpc_client.url())
            .unwrap()
            .host_str()
            .unwrap()
            .to_string(),
        if !&miner.rpc_client.url().eq(&miner.send_client.url()) {
            format!(
                " Send Rpc:{}",
                Url::parse(&miner.send_client.url())
                    .unwrap()
                    .host_str()
                    .unwrap()
                    .to_string()
            )
        } else {
            "".to_string()
        }
    );
    info!("Server listening on {}:{}", SERVER_HOST, SERVER_PORT);

    let mut last_hash_at: i64 = 0;
    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((socket, addr)) => {
                        info!("New client connected: {}", addr);
                        let state_clone = Arc::clone(&state);
                        let notify_clone = notify.clone();
                        tokio::spawn(handle_client(socket, addr, state_clone,notify_clone));
                    }
                    Err(e) => error!("Failed to accept connection: {}", e),
                }
            }
            _ = notify.notified() => {
                let action_type = *state.action_type.read().unwrap();
                match action_type {
                    ActionType::Start => {
                        // If has some error in last mining
                        let proof_data = miner.mine(BUFFER_TIME).await;
                        if last_hash_at != proof_data.last_hash_at {
                            process_mining_data(&proof_data, &state).await;
                            last_hash_at = proof_data.last_hash_at;
                        } else {
                            // Encounter some error, then proof.challenge may not be update, so just resend the last difficulty
                            warn!("Resend last solution");
                            *state.action_type.write().unwrap() = ActionType::Send;
                            notify.notify_one();
                        }
                    },
                    ActionType::Send => {
                        submit_mining_data(&miner, &state).await;
                        *state.action_type.write().unwrap() = ActionType::Start;
                        notify.notify_one();},
                    ActionType::Wait => {
                        // Reset local proof's last_hash_at
                        *miner.last_hash_at.borrow_mut() = 0;
                        info!("Waiting client...");
                    }
                }
            }
        }
    }
}

async fn process_mining_data(proof_data: &ProofData, state: &Arc<SharedState>) {
    //let proof_data = miner.mine(BUFFER_TIME).await;
    let mut clients = state.clients.write().unwrap();
    let mut offset: u64 = 0; // Start index thread of a machine
    let total_cores = state.total_cores.read().unwrap();
    info!("Mining cores: {:?}", total_cores);

    for (addr, client_info) in clients.iter_mut() {
        let mut combined = [0u8; 56]; // 32 bytes for challenge + 8 bytes for u64 + 8 bytes for calculated number(u64) + 8 bytes for total cores
        combined[..32].copy_from_slice(&proof_data.challenge);
        combined[32..40].copy_from_slice(&proof_data.cutoff_time.to_le_bytes());
        combined[40..48].copy_from_slice(&offset.to_le_bytes());
        combined[48..].copy_from_slice(&total_cores.to_le_bytes());
        if let Err(e) = client_info.sender.write_all(&combined).await {
            error!("Failed to send data to client {}: {}", addr, e);
        }
        // Add nonce offset by total cores
        offset += client_info.cores;
        // Increase mining clients
        state.mining_clients.write().unwrap().insert(*addr);
    }
}

async fn submit_mining_data(miner: &Arc<Miner>, state: &Arc<SharedState>) {
    let solution = state.solution.read().unwrap().unwrap();
    miner.submit(solution).await;
    // Reset best difficulty for next epoch
    *state.best_difficulty.write().unwrap() = 0;
}

async fn handle_client(
    socket: TcpStream,
    addr: SocketAddr,
    state: Arc<SharedState>,
    notify: Arc<Notify>,
) {
    let (mut reader, writer) = socket.into_split();
    let mut buffer = [0u8; 24];

    // Read the initial CORES message
    let mut cores_buffer = [0u8; 10];
    if let Ok(n) = reader.read(&mut cores_buffer).await {
        if let Ok(cores_msg) = std::str::from_utf8(&cores_buffer[..n]) {
            if cores_msg.starts_with("CORES:") {
                let cores: u64 = cores_msg[6..].parse().unwrap_or(1);
                info!("Client {} has {} cores", addr, cores);
                state.clients.write().unwrap().insert(
                    addr,
                    ClientInfo {
                        cores,
                        sender: writer,
                    },
                );
                // Update total cores
                let mut total_cores = state.total_cores.write().unwrap();
                *total_cores += cores;
                info!("Total registered cores: {}", *total_cores);
                // First mining
                let mut action_type = state.action_type.write().unwrap();
                if *action_type == ActionType::Wait {
                    *action_type = ActionType::Start;
                    notify.notify_one();
                }
            }
        }
    }
    let mut client_disconnected = false;

    while !client_disconnected {
        tokio::select! {
            result = reader.read_exact(&mut buffer) => {
                match result {
                    Ok(_) => {
                        let digest = &buffer[..16].try_into().unwrap();
                        let nonce = &buffer[16..].try_into().unwrap();
                        let solution = Solution::new(*digest, *nonce);
                        let hash = solution.to_hash();
                        let difficulty = hash.difficulty();

                        if difficulty.gt(&*state.best_difficulty.read().unwrap()) {
                            *state.best_difficulty.write().unwrap() = difficulty;
                            *state.solution.write().unwrap() = Some(solution);
                        }
                        info!("Received {} (difficulty: {})", addr, difficulty);
                    }
                    Err(e) => {
                        error!("Client {} disconnected: {}", addr, e);
                        client_disconnected = true;
                    }
                }
                // Update current mining number
                let mut mining_clients = state.mining_clients.write().unwrap();
                mining_clients.remove(&addr);

                if mining_clients.is_empty() && *state.best_difficulty.read().unwrap() > 0 {
                    *state.action_type.write().unwrap() = ActionType::Send;
                    notify.notify_one();
                }
            }
        }
    }
    // Client disconnected, remove from clients and update total cores
    let mut clients = state.clients.write().unwrap();
    if let Some(client_info) = clients.remove(&addr) {
        let mut total_cores = state.total_cores.write().unwrap();
        *total_cores -= client_info.cores;
        info!("Total cores: {}", *total_cores);
    }
    // Turn into waiting if no clients
    if clients.is_empty() {
        while *state.action_type.read().unwrap() == ActionType::Send {
            warn!("Cannot change mode to Wait from Send");
            std::thread::sleep(Duration::from_secs(5));
        }
        *state.action_type.write().unwrap() = ActionType::Wait;
        notify.notify_one();
    }
}

fn get_env<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn set_up_logging() {
    let subscriber = tracing_subscriber::fmt()
        .with_target(false)
        .compact()
        .finish();
    subscriber.init()
}

impl Miner {
    pub fn new(
        rpc_client: Arc<RpcClient>,
        send_client: Arc<RpcClient>,
        jito_client: Arc<RpcClient>,
        websocket: Option<String>,
        priority_fee: Option<u64>,
        dynamic_fee_url: Option<String>,
        enable_dynamic_fee: bool,
        enable_jito: bool,
        boost_1: Option<String>,
        keypair: Keypair,
        fee_payer: Keypair,
        last_reset_at: Arc<RwLock<i64>>,
        user_config: UserConfig,
    ) -> Self {
        Self {
            rpc_client,
            send_client,
            jito_client,
            websocket,
            priority_fee,
            dynamic_fee_url,
            enable_dynamic_fee,
            enable_jito,
            boost_1,
            boost_data_1: RefCell::new(vec![]),
            land_status: RefCell::new(LandStatus {
                count: 0,
                last_15_times: [0; 15],
                fee_multiple: 1.0,
                difficulty: 0,
                difficulty_hits: HashMap::new(),
                start_time: SystemTime::now(),
                total_hash_times: 0,
                total_retry_times: 0,
                total_failed_times: 0,
                total_error_times: 0,
            }),
            last_balance: RefCell::new(0),
            earned_balance: RefCell::new(0),
            last_hash_at: RefCell::new(0),
            keypair,
            fee_payer,
            last_reset_at,
            user_config,
            tip: Arc::new(RwLock::new(0)),
        }
    }
}
