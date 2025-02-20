all: clean
	cargo build
	docker compose up -d
	./scripts/add_peers.sh 
	cargo run -- testnet --nodes 3 --home nodes
	bash scripts/spawn.bash --nodes 3 --home nodes

clean:
	docker compose down
	rm -rf ./nodes
	rm -rf ./rethdata
	rm -rf ./monitoring/data-grafana

