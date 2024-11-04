use crate::config::{AN_HOUR, MIN_DIFFICULTY};
use cached::proc_macro::cached;
use ore_api::{
    consts::{CONFIG_ADDRESS, PROOF, TOKEN_DECIMALS},
    state::{Config, Proof},
};
use ore_boost_api::state::{Boost, Stake};
use solana_client::client_error::{ClientError, ClientErrorKind};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_program::{pubkey::Pubkey, sysvar};
use solana_sdk::{clock::Clock, hash::Hash};
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};
use steel::AccountDeserialize;
use tokio::time::sleep;
use tracing::{error, warn};

pub const BLOCKHASH_QUERY_RETRIES: usize = 10;
pub const BLOCKHASH_QUERY_DELAY: u64 = 500;
pub const ACCOUNT_QUERY_DELAY: u64 = 1000;
pub const ACCOUNT_QUERY_RETRIES: usize = 4;

pub async fn get_config(client: &RpcClient) -> Config {
    if let Ok(data) = client.get_account_data(&CONFIG_ADDRESS).await {
        *Config::try_from_bytes(&data).expect("Failed to parse config account")
    } else {
        warn!("Failed to get config account");
        Config {
            base_reward_rate: 0,
            last_reset_at: 0,
            min_difficulty: MIN_DIFFICULTY,
            top_balance: 0,
        }
    }
}

pub async fn get_boost(client: &RpcClient, address: Pubkey) -> Boost {
    let data = client
        .get_account_data(&address)
        .await
        .expect("Failed to get boost account");
    *Boost::try_from_bytes(&data).expect("Failed to parse boost account")
}

pub async fn get_stake(client: &RpcClient, address: Pubkey) -> Stake {
    let data = client
        .get_account_data(&address)
        .await
        .expect("Failed to get stake account");
    *Stake::try_from_bytes(&data).expect("Failed to parse stake account")
}

pub async fn get_proof_with_authority_retries(
    client: &RpcClient,
    authority: Pubkey,
    lash_hash_at: i64,
) -> Proof {
    let proof_address = proof_pubkey(authority);
    let mut attempts = 0;
    let mut local_lash_hash_at = lash_hash_at;
    loop {
        match client.get_account_data(&proof_address).await {
            Ok(data) => {
                let proof = *Proof::try_from_bytes(&data).expect("Failed to parse proof account");
                if proof.last_hash_at > local_lash_hash_at {
                    return proof;
                } else {
                    warn!("Proof is old");
                }
            }
            Err(err) => error!("Failed to get proof: {}", err),
        }
        tokio::time::sleep(Duration::from_millis(ACCOUNT_QUERY_DELAY)).await;
        attempts += 1;
        if attempts >= ACCOUNT_QUERY_RETRIES {
            error!("Max retries reached for get proof query");
            local_lash_hash_at = 0;
        }
    }
}

pub async fn get_clock(client: &RpcClient) -> Clock {
    let data = client
        .get_account_data(&sysvar::clock::ID)
        .await
        .expect("Failed to get clock account");
    bincode::deserialize::<Clock>(&data).expect("Failed to deserialize clock")
}

pub fn amount_u64_to_string(amount: u64) -> String {
    amount_u64_to_f64(amount).to_string()
}

pub fn amount_u64_to_f64(amount: u64) -> f64 {
    (amount as f64) / 10f64.powf(TOKEN_DECIMALS as f64)
}

pub fn approximate_amount_hour(amount: u64, elapse_secs: u64) -> String {
    format!(
        "{:.8}",
        amount_u64_to_f64(amount) / elapse_secs as f64 * AN_HOUR as f64
    )
}

pub async fn get_latest_blockhash_with_retries(
    client: &RpcClient,
) -> Result<(Hash, u64), ClientError> {
    let mut attempts = 0;

    loop {
        if let Ok((hash, slot)) = client
            .get_latest_blockhash_with_commitment(client.commitment())
            .await
        {
            return Ok((hash, slot));
        }

        // Retry
        sleep(Duration::from_millis(BLOCKHASH_QUERY_DELAY)).await;
        attempts += 1;
        if attempts >= BLOCKHASH_QUERY_RETRIES {
            return Err(ClientError {
                request: None,
                kind: ClientErrorKind::Custom(
                    "Max retries reached for latest blockhash query".into(),
                ),
            });
        }
    }
}

#[cached]
pub fn proof_pubkey(authority: Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[PROOF, authority.as_ref()], &ore_api::ID).0
}

pub fn get_current_unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time went backwards")
        .as_secs()
}
