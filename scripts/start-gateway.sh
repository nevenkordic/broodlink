#!/bin/bash
cd "$(dirname "$0")/.." || exit
export BROODLINK_CONFIG="./config.toml"
# Source .env — each line becomes an exported env var
if [ -f .env ]; then
    while IFS='=' read -r key val; do
        [[ "$key" =~ ^#.*$ ]] && continue
        [[ -z "$key" ]] && continue
        export "$key=$val"
    done < .env
fi
exec ./target/release/a2a-gateway
