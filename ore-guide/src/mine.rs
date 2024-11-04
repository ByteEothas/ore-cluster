use crate::{
    utils::{
        amount_u64_to_string, approximate_amount_hour, get_clock, get_current_unix_timestamp,
        get_proof_with_authority_retries, proof_pubkey,
    },
    Miner, ProofData,
};
use chrono::{Local, TimeZone};
use drillx::Solution;
use ore_api::{
    consts::{BUS_ADDRESSES, BUS_COUNT, EPOCH_DURATION, ONE_MINUTE},
    state::{Bus, Proof},
};
use rand::Rng;
use solana_program::pubkey::Pubkey;
use solana_sdk::signer::Signer;
use std::time::{SystemTime, UNIX_EPOCH};
use steel::AccountDeserialize;
use tracing::{debug, info};

impl Miner {
    pub async fn mine(&self, buffer_time: u64) -> ProofData {
        // Open account, if needed.
        let signer = &self.keypair;

        let mut last_balance = self.last_balance.borrow_mut();
        let mut last_hash_at = self.last_hash_at.borrow_mut();
        let mut earned_balance = self.earned_balance.borrow_mut();
        // Fetch proof
        let proof =
            get_proof_with_authority_retries(&self.rpc_client, signer.pubkey(), *last_hash_at)
                .await;
        let land_status = &self.land_status.borrow();
        let hits = &land_status.difficulty_hits;
        let elapsed_sec = land_status.start_time.elapsed().unwrap().as_secs();
        info!(
            "\nStake: {} ORE\n{}{}{}{}",
            amount_u64_to_string(proof.balance),
            if last_balance.gt(&0) {
                // Count earned balance
                let change = proof.balance.saturating_sub(*last_balance);
                *earned_balance += change;
                format!("Change: {} ORE\n", amount_u64_to_string(change),)
            } else {
                "".to_string()
            },
            if *earned_balance > 0 {
                format!(
                    "Earned: {} ORE (≈ {} ORE/Hour)\n",
                    amount_u64_to_string(*earned_balance),
                    approximate_amount_hour(*earned_balance, elapsed_sec),
                )
            } else {
                "".to_string()
            },
            if hits.len().gt(&0) {
                format!("Hit: {:?}\n", hits)
            } else {
                "".to_string()
            },
            format!(
                "Elapsed: {} secs\nTotal hash: {} Retry: {} Error: {} Failed: {}",
                elapsed_sec,
                land_status.total_hash_times,
                land_status.total_retry_times,
                land_status.total_error_times,
                land_status.total_failed_times,
            ),
        );
        *last_hash_at = proof.last_hash_at;
        *last_balance = proof.balance;

        // Calculate cutoff time
        let cutoff_time = self.get_cutoff(proof, buffer_time).await;
        ProofData {
            challenge: proof.challenge,
            cutoff_time,
            last_hash_at: proof.last_hash_at,
        }
    }

    pub async fn submit(&self, solution: Solution) {
        // Trace solution
        let best_difficulty = solution.to_hash().difficulty();
        let mut land_status = self.land_status.borrow_mut();
        land_status.difficulty = best_difficulty;
        land_status
            .difficulty_hits
            .entry(best_difficulty)
            .and_modify(|count| *count += 1)
            .or_insert(1);
        drop(land_status);
        // Open account, if needed.
        let signer = &self.keypair;
        // Build instruction set
        let mut ixs = vec![ore_api::sdk::auth(proof_pubkey(signer.pubkey()))];
        let mut compute_budget = 500_000;
        // Load config
        let last_reset_at = *self.last_reset_at.read().unwrap();
        debug!(
            "Last reset at: {}",
            self.compare_minutes(
                last_reset_at,
                get_current_unix_timestamp() as i64,
                EPOCH_DURATION,
            )
        );
        let current_timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_secs() as i64;
        if last_reset_at
            .saturating_add(EPOCH_DURATION)
            .saturating_sub(5) // Buffer
            .le(&current_timestamp)
        {
            compute_budget += 100_000;
            ixs.push(ore_api::sdk::reset(signer.pubkey()));
        }

        // Build option (boost) accounts
        let mut optional_accounts: Vec<Pubkey> = vec![];
        let boost_data_1 = self.boost_data_1.borrow();
        optional_accounts = [optional_accounts, boost_data_1.clone()].concat();

        // compute_budget += (optional_accounts.len() / 2) as u32 * 100_000;

        // Build mine ix
        ixs.push(ore_api::sdk::mine(
            signer.pubkey(),
            signer.pubkey(),
            self.find_bus().await,
            solution,
            optional_accounts,
        ));
        // Submit transaction
        self.send_and_confirm_auto(&ixs, compute_budget, false)
            .await
            .ok();
    }

    pub fn reset_last_at(&self) {
        // Reset local proof's last_hash_at
        let mut last_hash_at = self.last_hash_at.borrow_mut();
        *last_hash_at = 0;
    }

    async fn get_cutoff(&self, proof: Proof, buffer_time: u64) -> u64 {
        let clock = get_clock(&self.rpc_client).await;
        proof
            .last_hash_at
            .saturating_add(60)
            .saturating_sub(buffer_time as i64)
            .saturating_sub(clock.unix_timestamp)
            .max(0) as u64
    }

    async fn find_bus(&self) -> Pubkey {
        // Fetch the bus with the largest balance
        if let Ok(accounts) = self.rpc_client.get_multiple_accounts(&BUS_ADDRESSES).await {
            let mut top_bus_balance: u64 = 0;
            let mut top_bus = BUS_ADDRESSES[0];
            for account in accounts {
                if let Some(account) = account {
                    if let Ok(bus) = Bus::try_from_bytes(&account.data) {
                        if bus.rewards.gt(&top_bus_balance) {
                            top_bus_balance = bus.rewards;
                            top_bus = BUS_ADDRESSES[bus.id as usize];
                        }
                    }
                }
            }
            return top_bus;
        }

        // Otherwise return a random bus
        let i = rand::thread_rng().gen_range(0..BUS_COUNT);
        BUS_ADDRESSES[i]
    }

    fn compare_minutes(&self, time1: i64, time2: i64, interval: i64) -> String {
        let minutes = (time2 - time1) / ONE_MINUTE;
        if minutes < interval {
            format!(
                "{} minutes ago",
                if minutes < 1 {
                    "less than a".to_string()
                } else {
                    minutes.to_string()
                }
            )
        } else {
            Local
                .timestamp_opt(time1, 0)
                .unwrap()
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        }
    }
}
