#!/bin/bash
set -e

# Deploy the web build to Cloudflare Pages (direct upload via wrangler).
# Auth: run `npx wrangler login` once, or export CLOUDFLARE_API_TOKEN
# (and CLOUDFLARE_ACCOUNT_ID if the token has access to multiple accounts).
# One-time project setup: npx wrangler pages project create coop-web --production-branch master

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
PROJECT_ROOT="$SCRIPT_DIR/../.."
WWW_DIR="$SCRIPT_DIR/../www"
DIST_DIR="$WWW_DIR/dist"

PROJECT_NAME="coop-web"
PRODUCTION_BRANCH="master"

BRANCH=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --branch)
            BRANCH="$2"
            shift 2
            ;;
        *)
            echo -e "${RED}Unknown argument: $1${NC}"
            echo "Usage: $0 [--branch <name>]"
            exit 1
            ;;
    esac
done

if [[ -z "$BRANCH" ]]; then
    BRANCH="$(git -C "$PROJECT_ROOT" rev-parse --abbrev-ref HEAD)"
fi

echo -e "${GREEN}Building Coop Web for deploy (branch: $BRANCH)...${NC}"

"$SCRIPT_DIR/build-wasm.sh" --release

echo -e "${GREEN}Building frontend...${NC}"
(cd "$WWW_DIR" && npm install --no-fund --no-audit && npm run build)

# Cloudflare Pages rejects files over 25 MiB
MAX_FILE_BYTES=$((25 * 1024 * 1024))
while IFS= read -r -d '' file; do
    SIZE=$(stat -f%z "$file")
    if [[ "$SIZE" -gt "$MAX_FILE_BYTES" ]]; then
        echo -e "${RED}Error: $file is $((SIZE / 1024 / 1024)) MiB, over Cloudflare's 25 MiB per-file limit.${NC}"
        echo "Shrink the wasm (wasm-opt -Oz, font subsetting) before deploying."
        exit 1
    fi
done < <(find "$DIST_DIR" -type f -print0)

echo -e "${GREEN}Deploying to Cloudflare Pages project '$PROJECT_NAME'...${NC}"
npx wrangler pages deploy "$DIST_DIR" \
    --project-name "$PROJECT_NAME" \
    --branch "$BRANCH" \
    --commit-dirty=true

if [[ "$BRANCH" == "$PRODUCTION_BRANCH" ]]; then
    echo -e "${GREEN}✓ Deployed to production.${NC}"
else
    echo -e "${YELLOW}✓ Deployed as preview for branch '$BRANCH'.${NC}"
    echo -e "${YELLOW}Pass --branch $PRODUCTION_BRANCH to deploy to production.${NC}"
fi
