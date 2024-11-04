use crate::Miner;
use solana_client::client_error::Result as ClientResult;
use solana_program::instruction::Instruction;
use solana_sdk::signature::Signature;
use tracing::info;

impl Miner {
    pub async fn send_and_confirm_auto(
        &self,
        ixs: &[Instruction],
        cus: u32,
        skip_confirm: bool,
    ) -> ClientResult<Signature> {
        // Land status
        let land_status = self.land_status.borrow();
        let per_time = get_average_time(&land_status.last_15_times);

        info!("Last 15 times: {:?}", land_status.last_15_times);
        info!(
            "Land: {} avg time: {}ms fee: {}x",
            land_status.count, per_time, land_status.fee_multiple
        );
        drop(land_status);
        // Choosing send method
        let max_jito_tip = self.user_config.max_jito_tip;
        let jito_tip = *self.tip.read().unwrap();
        if self.enable_jito && jito_tip <= max_jito_tip {
            // Using jito to send
            self.send_and_confirm_jito(ixs, cus, skip_confirm, per_time)
                .await
        } else {
            if self.enable_dynamic_fee {
                // Using dynamic fee Rpc to send
                self.send_and_confirm_rpc(ixs, cus, skip_confirm).await
            } else {
                // Using Rpc to send
                self.send_and_confirm(ixs, cus, skip_confirm, per_time)
                    .await
            }
        }
    }
}

fn get_average_time(duration: &[u64]) -> u64 {
    let non_zero_elements: Vec<&u64> = duration.iter().filter(|&&x| x != 0).collect();
    let sum: u64 = non_zero_elements.iter().copied().sum();
    let count = non_zero_elements.len();
    if count == 0 {
        0
    } else {
        sum / count as u64
    }
}
