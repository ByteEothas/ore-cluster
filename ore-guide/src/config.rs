pub const WS_RETRY_DELAY: u64 = 30;
pub const MAX_RPC_PRIORITY_FEE: u64 = 8000;
pub const STEP_MULTIPLE: f32 = 1.05;
pub const MAX_MULTIPLE: f32 = 10.0f32;
pub const MAX_JITO_TIP: u64 = 5000;
pub const JTIO_CLIENT_URL: &str = "https://mainnet.block-engine.jito.wtf/api/v1/transactions";
pub const JITO_TIP_URL: &str = "ws://bundles-api-rest.jito.wtf/api/v1/bundles/tip_stream";
pub const TIP_ACCOUNTS: [&str; 8] = [
    "96gYZGLnJYVFmbjzopPSU6QiEV5fGqZNyN9nmNhvrZU5",
    "HFqU5x63VTqvQss8hp11i4wVV8bD44PvwucfZ2bU7gRe",
    "Cw8CFyM9FkoMi7K7Crf6HNQqf4uEMzpKw6QNghXLvLkY",
    "ADaUMid9yfUytqMBgopwjb2DTLSokTSzL1zt6iGPaS49",
    "DfXygSm4jCyNCybVYYK6DwvWqjKee8pbDmJGcLWNDXjh",
    "ADuUkR4vqLUMWXxW9gh6D6L8pMSawimctcNZ5pGwDcEt",
    "DttWaMuVvTiduZRnguLF7jNxTgiMBZ1hyAumKUiL2KRL",
    "3AVi9Tg9Uo68tJfuvoKvqKNWKkC5wPdSSdeBnizKZ6jT",
];
/// The duration of an hour, in seconds.
pub const AN_HOUR: i64 = 3600;
pub const MIN_DIFFICULTY: u64 = 8;
/// Send transaction
pub const GATEWAY_RETRIES: u64 = 60;
pub const CONFIRM_RETRIES: usize = 8;
pub const CONFIRM_DELAY: u64 = 500;
pub const GATEWAY_DELAY: u64 = 800;
pub const JITO_RETRIES: u64 = 2;
