use clap::{arg, Parser};

#[derive(Parser, Debug)]
pub struct MineArgs {
    #[arg(
        long,
        short,
        value_name = "SECONDS",
        help = "The number seconds before the deadline to stop mining and start submitting.",
        default_value = "5"
    )]
    pub buffer_time: u64,
}

#[derive(Parser, Debug)]
pub struct ProofArgs {
    #[arg(value_name = "ADDRESS", help = "The address of the proof to fetch.")]
    pub address: Option<String>,
}
