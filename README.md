# Malachite <> Reth integration

## Goals

The goal in this repo is to build an MVP of an integration of Reth with Malachite consensus engine. Rough architecture:

- Use Malachite as a CL (consensus layer client)
- Use Reth as a EL (execution layer client)
- Uses the [channel-based API of Malachite](<[url](https://github.com/informalsystems/malachite/tree/main/code/examples/channel)>) to do the integration between CL and EL
- We want to test the integration at large scale O(100) nodes and quantify the latency and throughput

## Background

To get familiar with the project and its goals, the following resources can be useful:

- The [channel-based application tutorial in Malachite](<[url](https://github.com/informalsystems/malachite/blob/main/docs/tutorials/channels.md)>)
- The proof of concept -- a very naive integration -- Reth x Malachite integration in [rem-poc](<[url](https://github.com/adizere/rem-poc)>)
- Examples from the [reth repo](<[url](https://github.com/paradigmxyz/reth/tree/main/examples)>)

## Development

### Run a local testnet

#### Compile the app

```
$ cargo build
```

#### Setup the testnet

Generate configuration and genesis for three nodes using the `testnet` command:

```
$ cargo run -- testnet --nodes 3 --home nodes
```

This will create the configuration for three nodes in the `nodes` folder. Feel free to inspect this folder and look at the generated files.

#### Spawn the nodes

```
$ bash scripts/spawn.bash --nodes 3 --home nodes
```

If successful, the logs for each node can then be found at `nodes/X/logs/node.log`.

```
$ tail -f nodes/0/logs/node.log
```

Press `Ctrl-C` to stop all the nodes.

Generate EVM blocks to propose

```
cargo run --bin utils -- generate
```

Run an EVM network with an RPC

```
rm -rf ./nodes; cargo build; cargo run --bin malachitebft-reth-app -- testnet --nodes 3 --home nodes; bash scripts/spawn.bash --nodes 3 --home nodes --rpc-node 0
```

Script to verify functioning of EVM network by parsing logs

```
cargo run --bin utils -- verify
```
