# MalachiteBFT <> Reth

## Run a local testnet

### Compile the app

```
$ cargo build
```

### Setup the testnet

Generate configuration and genesis for three nodes using the `testnet` command:

```
$ cargo run -- testnet --nodes 3 --home nodes
```

This will create the configuration for three nodes in the `nodes` folder. Feel free to inspect this folder and look at the generated files.

### Spawn the nodes

```
$ bash scripts/spawn.bash --nodes 3 --home nodes
```

If successful, the logs for each node can then be found at `nodes/X/logs/node.log`.

```
$ tail -f nodes/0/logs/node.log
```

Press `Ctrl-C` to stop all the nodes.
