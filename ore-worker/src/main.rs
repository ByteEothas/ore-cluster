use clap::Parser;
use core::str;
use drillx::{
    equix::{self},
    Hash,
};
use serde::{Deserialize, Serialize};
use std::{io, time::Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{timeout, Duration};
use tracing::{error, info};
use tracing_subscriber::util::SubscriberInitExt;

const SERVER_RETRIES: u64 = 36;
const RETRY_DELAY: u64 = 5;
const CHECK_POINT: u64 = 128 - 1;

#[derive(Serialize, Deserialize, Debug, Clone)]
struct ProofData {
    challenge: [u8; 32],
    cutoff_time: u64,
}

struct BestHash {
    digest: [u8; 16],
    nonce: [u8; 8],
}

// const MIN_DIFFICULTY: u32 = 8;

#[derive(Parser, Debug)]
#[command(about, version)]
struct Args {
    #[arg(
        long,
        value_name = "ORE SERVER URL",
        help = "Network address of Ore server",
        global = true
    )]
    server: Option<String>,

    #[arg(
        long,
        value_name = "CONNECTED WAITING TIME",
        help = "Wait server timeout seconds",
        global = true
    )]
    wait_time: Option<u64>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    set_up_logging();
    info!(
        "ore-worker version: {}",
        env!("CARGO_PKG_VERSION").to_string()
    );

    let args = Args::parse();
    let server_address = args.server.unwrap_or("0.0.0.0:3000".to_string());
    let mut retry_count = 0;
    let retry_delay = Duration::from_secs(RETRY_DELAY);
    let wait_delay = if let Some(wait_time) = args.wait_time {
        Some(Duration::from_secs(wait_time))
    } else {
        None
    };
    loop {
        match TcpStream::connect(&server_address).await {
            Ok(mut stream) => {
                info!("Connected to server");
                // Send number of cores
                let num_cores = num_cpus::get() as u64;
                let cores_message = format!("CORES:{}", num_cores);
                stream.write_all(cores_message.as_bytes()).await?;
                info!("Sent number of cores: {}", num_cores);
                retry_count = 0;
                // Receive from server
                let mut buffer = [0u8; 56];
                loop {
                    match read_with_optional_timeout(&mut stream, &mut buffer, wait_delay).await {
                        Ok(0) => {
                            info!("Server closed the connection");
                            break;
                        }
                        Ok(_) => {
                            let challenge = buffer[..32].try_into().unwrap();
                            let cutoff_time =
                                u64::from_le_bytes(buffer[32..40].try_into().unwrap());
                            let offset = u64::from_le_bytes(buffer[40..48].try_into().unwrap());
                            let total_cores = u64::from_le_bytes(buffer[48..].try_into().unwrap());
                            let proof_data = ProofData {
                                challenge,
                                cutoff_time,
                            };
                            // Run drillx
                            let best_hash = find_hash_par(
                                proof_data.challenge,
                                proof_data.cutoff_time,
                                offset,
                                num_cores,
                                total_cores,
                            )
                            .await;
                            // Send best difficulty to server
                            let digest: [u8; 16] = best_hash.digest;
                            let nonce: [u8; 8] = best_hash.nonce;
                            // Combine digest and nonce into a single buffer
                            let mut combined = [0u8; 24]; // 16 bytes for digest + 8 bytes for nonce
                            combined[..16].copy_from_slice(&digest);
                            combined[16..].copy_from_slice(&nonce);
                            // Send the combined buffer to the server
                            stream.write_all(&combined).await?;
                        }
                        Err(e) => {
                            error!("Failed to read from server: {}", e);
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                error!("Failed to connect to server: {}. Retrying...", e);
                retry_count += 1;
                if retry_count >= SERVER_RETRIES {
                    return Err(Box::new(e) as Box<dyn std::error::Error>);
                }
                tokio::time::sleep(retry_delay).await;
            }
        }
    }
}

async fn read_with_optional_timeout(
    stream: &mut TcpStream,
    buffer: &mut [u8],
    wait_delay: Option<Duration>,
) -> Result<usize, io::Error> {
    if let Some(wait_delay) = wait_delay {
        match timeout(wait_delay, stream.read(buffer)).await {
            Ok(result) => result, // Unwrap the nested Result
            Err(_) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "waiting too long timed out",
            )),
        }
    } else {
        // No timeout
        stream.read(buffer).await
    }
}

async fn find_hash_par(
    challenge: [u8; 32],
    cutoff_time: u64,
    offset: u64,
    cores: u64,
    total_cores: u64,
) -> BestHash {
    // Nonce unit
    let nonce_unit = u64::MAX.saturating_div(total_cores);
    // Dispatch job to each thread
    info!("Mining...(offset: {} cores: {})", offset, cores);
    let nonce_range = {
        let start = offset;
        let end = cores + offset;
        start..end
    };
    let handles: Vec<_> = (nonce_range.start..nonce_range.end)
        .into_iter()
        .map(|i| {
            std::thread::spawn({
                let mut memory = equix::SolverMemory::new();
                move || {
                    // Start hashing
                    let timer = Instant::now();
                    let mut nonce = nonce_unit.saturating_mul(i);
                    let mut best_nonce = nonce;
                    let mut best_difficulty = 0;
                    let mut best_hash = Hash::default();
                    loop {
                        // Create hash
                        let hxs = drillx::hashes_with_memory(
                            &mut memory,
                            &challenge,
                            &nonce.to_le_bytes(),
                        );
                        // Look for best difficulty score in all hashes
                        for hx in hxs {
                            let difficulty = hx.difficulty();
                            if difficulty.gt(&best_difficulty) {
                                best_nonce = nonce;
                                best_difficulty = difficulty;
                                best_hash = hx;
                            }
                        }

                        // Exit if time has elapsed
                        if nonce & CHECK_POINT == 0 {
                            if timer.elapsed().as_secs().ge(&cutoff_time) {
                                // Mine until min difficulty has been met
                                break;
                            }
                        }

                        // Increment nonce
                        nonce += 1;
                    }

                    // Return the best nonce
                    (best_nonce, best_difficulty, best_hash)
                }
            })
        })
        .collect();

    // Join handles and return best nonce
    let mut best_nonce = 0;
    let mut best_difficulty = 0;
    let mut best_hash = Hash::default();
    for h in handles {
        if let Ok((nonce, difficulty, hash)) = h.join() {
            if difficulty > best_difficulty {
                best_difficulty = difficulty;
                best_nonce = nonce;
                best_hash = hash;
            }
        }
    }

    // Update log
    info!(
        "Best hash: {} (difficulty {})",
        bs58::encode(best_hash.h).into_string(),
        best_difficulty
    );

    BestHash {
        digest: best_hash.d,
        nonce: best_nonce.to_le_bytes(),
    }
}

fn set_up_logging() {
    let subscriber = tracing_subscriber::fmt()
        .with_target(false)
        .compact()
        .finish();
    subscriber.init()
}
