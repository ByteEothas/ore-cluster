use crate::config::{JITO_TIP_URL, WS_RETRY_DELAY};
use crate::utils::{get_boost, get_config, get_current_unix_timestamp, get_stake};
use crate::Miner;
use futures::{SinkExt, StreamExt};
use ore_api::{
    consts::{CONFIG_ADDRESS, EPOCH_DURATION},
    state::Config,
};
use ore_boost_api::state::{boost_pda, stake_pda};
use serde::Deserialize;
use serde_json::json;
use solana_program::{program_pack::Pack, pubkey::Pubkey};
use solana_sdk::signer::Signer;
use spl_token::state::Mint;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use steel::AccountDeserialize;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::protocol::Message;
use tracing::{debug, error, info};

#[derive(Deserialize, Debug)]
struct SolanaResponse<T> {
    result: T,
}

#[derive(Deserialize, Debug)]
struct NotificationResponse {
    params: Params,
}

#[derive(Deserialize, Debug)]
struct Params {
    result: ParamsResult,
}

#[derive(Deserialize, Debug)]
struct ParamsResult {
    value: ResultValue,
}

#[derive(Deserialize, Debug)]
struct ResultValue {
    data: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct Tip {
    pub time: String,
    pub landed_tips_25th_percentile: f64,
    pub landed_tips_50th_percentile: f64,
    pub landed_tips_75th_percentile: f64,
    pub landed_tips_95th_percentile: f64,
    pub landed_tips_99th_percentile: f64,
    pub ema_landed_tips_50th_percentile: f64,
}

impl Miner {
    pub async fn get_config_pull(&self) {
        let client = self.rpc_client.clone();
        let last_reset_at = Arc::clone(&self.last_reset_at);
        tokio::spawn(async move {
            loop {
                let config = get_config(&client).await;
                *last_reset_at.write().unwrap() = config.last_reset_at;
                let mut wait_time = (config.last_reset_at.saturating_add(EPOCH_DURATION) as u64)
                    .saturating_sub(get_current_unix_timestamp());
                wait_time = wait_time.max(WS_RETRY_DELAY);
                tokio::time::sleep(Duration::from_secs(wait_time)).await;
            }
        });
    }

    pub async fn get_config(&self) {
        let last_reset_at = Arc::clone(&self.last_reset_at);
        self.get_account_subscribe(&CONFIG_ADDRESS, move |value| {
            if let Ok(data) = base64::decode(&value.data[0]) {
                match Config::try_from_bytes(&data) {
                    Ok(config) => {
                        if last_reset_at.read().unwrap().lt(&config.last_reset_at) {
                            *last_reset_at.write().unwrap() = config.last_reset_at;
                        }
                    }
                    Err(e) => {
                        *last_reset_at.write().unwrap() = 0;
                        error!("Failed to parse config account: {}", e)
                    }
                }
            }
        })
        .await;
    }

    async fn get_account_subscribe<F>(&self, pubkey: &Pubkey, mut process_config: F)
    where
        F: FnMut(&ResultValue) + Send + 'static,
    {
        let pubkey = pubkey.to_string();
        let ws_url = self.websocket.clone().expect("Not found websocket url");
        tokio::spawn(async move {
            loop {
                match connect_async(&ws_url).await {
                    Ok((ws_stream, _)) => {
                        let (mut write, mut read) = ws_stream.split();

                        // Subscribe to the account
                        let subscribe_msg = json!({
                            "jsonrpc": "2.0",
                            "id": 1,
                            "method": "accountSubscribe",
                            "params": [
                                pubkey,
                                {
                                    "encoding": "jsonParsed",
                                    "commitment": "finalized"
                                }
                            ]
                        });

                        if let Err(e) = write.send(Message::Text(subscribe_msg.to_string())).await {
                            error!("Failed to send subscribe message: {}", e);
                            tokio::time::sleep(Duration::from_secs(WS_RETRY_DELAY)).await;
                            continue; // Skip to the next iteration of the loop
                        }

                        // Read the initial subscription response
                        if let Some(msg) = read.next().await {
                            match msg {
                                Ok(first_resp) => {
                                    match serde_json::from_str::<SolanaResponse<u64>>(
                                        first_resp.to_text().unwrap_or(""),
                                    ) {
                                        Ok(first_resp_json) => {
                                            let subscription_id = first_resp_json.result;
                                            info!("Subscription ID: {}", subscription_id);
                                        }
                                        Err(_) => {
                                            error!(
                                                "Failed to parse {} subscription: {}",
                                                pubkey,
                                                first_resp.to_string()
                                            )
                                        }
                                    }
                                }
                                Err(e) => error!("Failed to subscribe {}: {}", pubkey, e),
                            }
                        }

                        // Process further responses
                        while let Some(message) = read.next().await {
                            match message {
                                Ok(Message::Text(text)) => {
                                    if let Ok(NotificationResponse { params }) =
                                        serde_json::from_str::<NotificationResponse>(&text)
                                    {
                                        process_config(&params.result.value);
                                    }
                                }
                                Ok(_) => {}
                                Err(e) => {
                                    error!("Error reading message: {}", e);
                                    break; // Exit the loop to reconnect
                                }
                            }
                        }
                    }
                    Err(e) => error!("Failed to connect to WebSocket: {}", e),
                }
                // Wait before retrying connection
                tokio::time::sleep(Duration::from_secs(WS_RETRY_DELAY)).await;
            }
        });
    }

    pub async fn get_jito_tip(&self) {
        let (ws_stream, _) = connect_async(JITO_TIP_URL).await.unwrap();
        let (_, mut read) = ws_stream.split();
        let tip = Arc::clone(&self.tip);

        tokio::spawn(async move {
            while let Some(message) = read.next().await {
                if let Ok(Message::Text(text)) = message {
                    if let Ok(tips) = serde_json::from_str::<Vec<Tip>>(&text) {
                        for item in tips {
                            let mut tip = tip.write().unwrap();
                            *tip =
                                (item.ema_landed_tips_50th_percentile * (10_f64).powf(9.0)) as u64;
                            debug!("jito ema50th tip: {}", tip);
                        }
                    }
                }
            }
        });
    }

    pub async fn fetch_boost_data(&self, mint_address: &Option<String>) -> Option<BoostData> {
        let Some(mint_address) = mint_address else {
            return None;
        };
        let client = self.rpc_client.clone();
        let signer = &self.keypair;
        let mint_address = Pubkey::from_str(&mint_address).unwrap();
        let boost_address = boost_pda(mint_address).0;
        let stake_address = stake_pda(signer.pubkey(), boost_address).0;
        let mint = client
            .get_account_data(&mint_address)
            .await
            .map(|data| Mint::unpack(&data).unwrap())
            .unwrap();
        let boost_data = BoostData {
            boost_address,
            stake_address,
            mint,
        };
        // show log
        self.log_boost_data(&boost_data, 1).await;

        self.boost_data_1.replace(BoostData::to_vec(&boost_data));
        Some(boost_data)
    }

    async fn log_boost_data(&self, boost_data: &BoostData, id: u64) {
        let client = self.rpc_client.clone();
        let boost = get_boost(&client, boost_data.boost_address).await;
        let stake = get_stake(&client, boost_data.stake_address).await;
        let multiplier =
            (boost.multiplier as f64) * (stake.balance as f64) / (boost.total_stake as f64);
        info!(
            "Boost {}({}x): {:12}x ({})",
            id,
            boost.multiplier,
            multiplier,
            format!(
                "{} of {} ORE",
                stake.balance as f64 / 10f64.powf(boost_data.mint.decimals as f64),
                boost.total_stake as f64 / 10f64.powf(boost_data.mint.decimals as f64),
            )
        );
    }
}

#[derive(Clone)]
pub struct BoostData {
    boost_address: Pubkey,
    stake_address: Pubkey,
    mint: Mint,
}

impl BoostData {
    fn to_vec(boost_data: &BoostData) -> Vec<Pubkey> {
        vec![boost_data.boost_address, boost_data.stake_address]
    }
}
