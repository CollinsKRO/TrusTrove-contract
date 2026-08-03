#!/bin/bash
set -e

STELLAR="/mnt/c/Program Files (x86)/Stellar CLI/stellar.exe"

# Load configuration from .env (falling back to .env.example) using safe
# line-by-line parsing. Unlike `source`, this never executes arbitrary shell
# commands, so shell metacharacters in the file cannot be used for code
# execution (see CVE / issue #455).
load_env() {
  local file="$1"
  if [ ! -f "$file" ]; then
    echo "Warning: $file not found; skipping." >&2
    return 1
  fi
  while IFS= read -r line || [ -n "$line" ]; do
    # Trim surrounding whitespace
    line="$(printf '%s' "$line" | sed 's/^[[:space:]]*//; s/[[:space:]]*$//')"
    # Skip blank lines and comments
    [ -z "$line" ] && continue
    case "$line" in
      \#*) continue ;;
    esac
    # Only accept KEY=VALUE pairs
    case "$line" in
      *=*)
        key="${line%%=*}"
        value="${line#*=}"
        # Trim trailing whitespace from the key
        key="$(printf '%s' "$key" | sed 's/[[:space:]]*$//')"
        # Strip matching surrounding quotes from the value
        case "$value" in
          \"*\") value="${value#\"}"; value="${value%\"}" ;;
          \'*\') value="${value#\'}"; value="${value%\'}" ;;
        esac
        # Only export keys that are valid shell identifiers; anything else
        # (e.g. keys with spaces or special characters) is skipped.
        case "$key" in
          *[!A-Za-z0-9_]*|'') continue ;;
        esac
        export "$key=$value"
        ;;
    esac
  done < "$file"
}

if [ -f .env ]; then
  load_env .env
else
  load_env .env.example
fi

echo "=== Building all contracts ==="
"$STELLAR" contract build

echo ""
echo "=== Deploying registry_contract ==="
REGISTRY_ID=$("$STELLAR" contract deploy \
  --wasm target/wasm32v1-none/release/trusttrove_registry.wasm \
  --source $DEPLOYER_ACCOUNT \
  --network testnet)
echo "Registry: $REGISTRY_ID"
sleep 3

"$STELLAR" contract invoke \
  --id $REGISTRY_ID \
  --source $DEPLOYER_ACCOUNT \
  --network testnet \
  -- initialize \
  --admin $("$STELLAR" keys address $DEPLOYER_ACCOUNT)
sleep 3

echo ""
echo "=== Deploying invoice_contract ==="
INVOICE_ID=$("$STELLAR" contract deploy \
  --wasm target/wasm32v1-none/release/trusttrove_invoice.wasm \
  --source $DEPLOYER_ACCOUNT \
  --network testnet)
echo "Invoice: $INVOICE_ID"
sleep 3

"$STELLAR" contract invoke \
  --id $INVOICE_ID \
  --source $DEPLOYER_ACCOUNT \
  --network testnet \
  -- initialize \
  --admin $("$STELLAR" keys address $DEPLOYER_ACCOUNT) \
  --registry_contract $REGISTRY_ID
sleep 3

echo ""
echo "=== Deploying USDC escrow_contract ==="
ESCROW_USDC_ID=$("$STELLAR" contract deploy \
  --wasm target/wasm32v1-none/release/trusttrove_escrow.wasm \
  --source $DEPLOYER_ACCOUNT \
  --network testnet)
echo "USDC Escrow: $ESCROW_USDC_ID"
sleep 3

echo ""
echo "=== Deploying USDC pool_contract ==="
POOL_USDC_ID=$("$STELLAR" contract deploy \
  --wasm target/wasm32v1-none/release/trusttrove_pool.wasm \
  --source $DEPLOYER_ACCOUNT \
  --network testnet)
echo "USDC Pool: $POOL_USDC_ID"
sleep 3

echo ""
echo "=== Initializing USDC escrow ==="
"$STELLAR" contract invoke \
  --id $ESCROW_USDC_ID \
  --source $DEPLOYER_ACCOUNT \
  --network testnet \
  -- initialize \
  --admin $("$STELLAR" keys address $DEPLOYER_ACCOUNT) \
  --pool_contract $POOL_USDC_ID \
  --invoice_contract $INVOICE_ID \
  --usdc_asset $USDC_ISSUER
sleep 3

echo ""
echo "=== Initializing USDC pool ==="
"$STELLAR" contract invoke \
  --id $POOL_USDC_ID \
  --source $DEPLOYER_ACCOUNT \
  --network testnet \
  -- initialize \
  --admin $("$STELLAR" keys address $DEPLOYER_ACCOUNT) \
  --invoice_contract $INVOICE_ID \
  --escrow_contract $ESCROW_USDC_ID \
  --usdc_asset $USDC_ISSUER
sleep 3

echo ""
echo "=== Deploying XLM escrow_contract ==="
ESCROW_XLM_ID=$("$STELLAR" contract deploy \
  --wasm target/wasm32v1-none/release/trusttrove_escrow.wasm \
  --source $DEPLOYER_ACCOUNT \
  --network testnet)
echo "XLM Escrow: $ESCROW_XLM_ID"
sleep 3

echo ""
echo "=== Deploying XLM pool_contract ==="
POOL_XLM_ID=$("$STELLAR" contract deploy \
  --wasm target/wasm32v1-none/release/trusttrove_pool.wasm \
  --source $DEPLOYER_ACCOUNT \
  --network testnet)
echo "XLM Pool: $POOL_XLM_ID"
sleep 3

echo ""
echo "=== Initializing XLM escrow ==="
"$STELLAR" contract invoke \
  --id $ESCROW_XLM_ID \
  --source $DEPLOYER_ACCOUNT \
  --network testnet \
  -- initialize \
  --admin $("$STELLAR" keys address $DEPLOYER_ACCOUNT) \
  --pool_contract $POOL_XLM_ID \
  --invoice_contract $INVOICE_ID \
  --usdc_asset $XLM_ASSET
sleep 3

echo ""
echo "=== Initializing XLM pool ==="
"$STELLAR" contract invoke \
  --id $POOL_XLM_ID \
  --source $DEPLOYER_ACCOUNT \
  --network testnet \
  -- initialize \
  --admin $("$STELLAR" keys address $DEPLOYER_ACCOUNT) \
  --invoice_contract $INVOICE_ID \
  --escrow_contract $ESCROW_XLM_ID \
  --usdc_asset $XLM_ASSET
sleep 3

echo ""
echo "=== Wiring USDC pool_contract into invoice_contract ==="
"$STELLAR" contract invoke \
  --id $INVOICE_ID \
  --source $DEPLOYER_ACCOUNT \
  --network testnet \
  -- set_pool_contract \
  --pool_contract $POOL_USDC_ID
sleep 3

echo ""
echo "==========================================="
echo "Deployment complete. Add to trusttrove-app .env.local:"
echo ""
echo "NEXT_PUBLIC_REGISTRY_CONTRACT_ID=$REGISTRY_ID"
echo "NEXT_PUBLIC_INVOICE_CONTRACT_ID=$INVOICE_ID"
echo "NEXT_PUBLIC_ESCROW_USDC_CONTRACT_ID=$ESCROW_USDC_ID"
echo "NEXT_PUBLIC_ESCROW_XLM_CONTRACT_ID=$ESCROW_XLM_ID"
echo "NEXT_PUBLIC_POOL_USDC_CONTRACT_ID=$POOL_USDC_ID"
echo "NEXT_PUBLIC_POOL_XLM_CONTRACT_ID=$POOL_XLM_ID"
echo "==========================================="
