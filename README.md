# ORE CLUSTER

A distributed command line application for ORE mining.

## Mine

To start mine server:

```sh
cargo run --release --package ore-guide -- --rpc https://api.mainnet-beta.solana.com --keypair ~/.config/solana/id.json --priority-fee 10 --dynamic-fee-url https://api.mainnet-beta.solana.com --boost-1 oreoU2P8bN6jkk3jbaiVxYnG1dCXcYxwhwyK9jSybcp mine
```

To start mine client:

```sh
cargo run --release --package ore-worker
```

