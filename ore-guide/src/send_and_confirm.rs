use chrono::Local;
use solana_client::{
    client_error::{ClientError, ClientErrorKind, Result as ClientResult},
    rpc_config::RpcSendTransactionConfig,
};
use solana_program::instruction::Instruction;
use solana_sdk::{
    commitment_config::CommitmentLevel,
    compute_budget::ComputeBudgetInstruction,
    signature::{Signature, Signer},
    transaction::Transaction,
};
use solana_transaction_status::{TransactionConfirmationStatus, UiTransactionEncoding};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::Miner;
use crate::{config::MAX_MULTIPLE, utils::get_latest_blockhash_with_retries};
use tracing::{error, info, instrument};

const RPC_RETRIES: usize = 0;
const _SIMULATION_RETRIES: usize = 4;

impl Miner {
    #[instrument(name = "sending(rpc)", skip_all)]
    pub async fn send_and_confirm(
        &self,
        ixs: &[Instruction],
        cus: u32,
        skip_confirm: bool,
        per_time: u64,
    ) -> ClientResult<Signature> {
        let signer = &self.keypair;
        let client = self.rpc_client.clone();
        let fee_payer = &self.fee_payer;
        let mut send_client = self.send_client.clone();
        let mut priority_fee = self.priority_fee.unwrap_or(0);

        // Load user'config
        let user_config = &self.user_config;
        let gateway_retries = user_config.gateway_retries;
        let gateway_delay = user_config.gateway_delay;
        let confirm_retries = user_config.confirm_retries;
        let confirm_delay = user_config.confirm_delay;
        let max_rpc_priority_fee = user_config.max_rpc_priority_fee;
        let step_multiple = user_config.step_multiple;

        // Set compute budget
        let mut final_ixs = vec![];
        final_ixs.push(ComputeBudgetInstruction::set_compute_unit_limit(cus));

        // Increase fee by last confirm_lapsed_millis
        let mut land_status = self.land_status.borrow_mut();
        if per_time > 6000 {
            land_status.fee_multiple = if land_status.fee_multiple < 2.0 {
                2.0
            } else {
                land_status.fee_multiple * 2.0
            };
        } else if per_time >= 3000 && per_time <= 6000 {
            land_status.fee_multiple = if land_status.fee_multiple >= 2.0 {
                1.0
            } else {
                land_status.fee_multiple * step_multiple
            }
        } else if per_time < 3000 {
            land_status.fee_multiple = if land_status.fee_multiple >= 2.0 {
                1.0
            } else {
                land_status.fee_multiple / step_multiple
            }
        }
        // Limit max multiple
        land_status.fee_multiple = land_status.fee_multiple.min(MAX_MULTIPLE);
        // Fly record
        land_status.total_hash_times += 1;

        // Use regular RPC
        priority_fee = (priority_fee as f32 * land_status.fee_multiple) as u64;
        priority_fee = priority_fee.min(max_rpc_priority_fee);
        info!(
            "difficulty: {} RPC fee: {}",
            land_status.difficulty, priority_fee
        );

        // Set compute unit price
        final_ixs.push(ComputeBudgetInstruction::set_compute_unit_price(
            priority_fee,
        ));

        // Add in user instructions
        final_ixs.extend_from_slice(ixs);

        // Build tx
        let send_cfg = RpcSendTransactionConfig {
            skip_preflight: true,
            preflight_commitment: Some(CommitmentLevel::Confirmed),
            encoding: Some(UiTransactionEncoding::Base64),
            max_retries: Some(RPC_RETRIES),
            min_context_slot: None,
        };
        let mut tx = Transaction::new_with_payer(&final_ixs, Some(&fee_payer.pubkey()));

        // Submit tx
        let mut attempts = 0;
        // Resign the tx
        let (hash, _slot) = get_latest_blockhash_with_retries(&send_client).await?;
        if signer.pubkey() == fee_payer.pubkey() {
            tx.sign(&[&signer], hash);
        } else {
            tx.sign(&[&signer, &fee_payer], hash);
        }

        let timer = Instant::now();
        loop {
            info!("attempt {}", attempts);
            // Send transaction
            attempts += 1;
            match send_client
                .send_transaction_with_config(&tx, send_cfg)
                .await
            {
                Ok(sig) => {
                    // Skip confirmation
                    if skip_confirm {
                        info!("sent: {}", sig);
                        return Ok(sig);
                    }

                    // Confirm transaction
                    for _ in 0..confirm_retries {
                        tokio::time::sleep(Duration::from_millis(confirm_delay)).await;
                        match client.get_signature_statuses(&[sig]).await {
                            Ok(signature_statuses) => {
                                for status in signature_statuses.value {
                                    if let Some(status) = status {
                                        if let Some(err) = status.err {
                                            // Reset local proof's last_hash_at
                                            self.reset_last_at();

                                            error!("{}", &err.to_string());
                                            // Fly record
                                            land_status.total_error_times += 1;
                                            return Err(ClientError {
                                                request: None,
                                                kind: ClientErrorKind::Custom(err.to_string()),
                                            });
                                        } else if let Some(confirmation) =
                                            status.confirmation_status
                                        {
                                            match confirmation {
                                                TransactionConfirmationStatus::Processed => {}
                                                TransactionConfirmationStatus::Confirmed
                                                | TransactionConfirmationStatus::Finalized => {
                                                    let now = Local::now();
                                                    let elapsed = timer.elapsed().as_secs();
                                                    // land_status.time += elapsed * 1000;

                                                    let formatted_time =
                                                        now.format("%Y-%m-%d %H:%M:%S").to_string();
                                                    info!(
                                                        "Timestamp: {} ({} secs)",
                                                        formatted_time, elapsed
                                                    );
                                                    info!("OK {}", sig);
                                                    // Fly record
                                                    land_status.total_retry_times += attempts - 1;
                                                    let index = (land_status.count % 15) as usize;
                                                    land_status.last_15_times[index] =
                                                        elapsed * 1000;
                                                    land_status.count += 1;

                                                    return Ok(sig);
                                                }
                                            }
                                        }
                                    }
                                }
                            }

                            // Handle confirmation errors
                            Err(err) => {
                                error!("{}", &err.kind().to_string());
                            }
                        }
                    }
                }

                // Handle submit errors
                Err(err) => {
                    // Change send rpc
                    send_client = if Arc::ptr_eq(&send_client, &self.send_client) {
                        self.rpc_client.clone()
                    } else {
                        self.send_client.clone()
                    };
                    error!("{}", &err.kind().to_string());
                    tokio::time::sleep(Duration::from_millis(gateway_delay)).await;
                }
            }

            // Retry
            if attempts > gateway_retries {
                // Reset local proof's last_hash_at
                self.reset_last_at();
                // Fly record
                land_status.total_failed_times += 1;

                error!("Max retries");
                return Err(ClientError {
                    request: None,
                    kind: ClientErrorKind::Custom("Max retries".into()),
                });
            }
        }
    }
}
