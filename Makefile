all:
	docker compose down
	docker compose up -d
	rm -rf ./nodes
	cargo build
	cargo run -- testnet --nodes 3 --home nodes
	bash scripts/spawn.bash --nodes 3 --home nodes
