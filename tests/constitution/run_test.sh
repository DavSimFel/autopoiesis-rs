#!/usr/bin/env bash
set -euo pipefail

cd /root/autopoiesis-rs
VARIANTS_DIR="tests/constitution/variants"
PROMPTS_DIR="tests/constitution/prompts"
RESULTS_DIR="tests/constitution/results"

# Backup original constitution
cp identity/constitution.md identity/constitution.md.bak

for variant_file in "$VARIANTS_DIR"/*.md; do
  variant=$(basename "$variant_file" .md)
  echo "=== Testing variant: $variant ==="
  mkdir -p "$RESULTS_DIR/$variant"
  
  # Install this variant
  cp "$variant_file" identity/constitution.md
  
  for prompt_file in "$PROMPTS_DIR"/*.txt; do
    prompt_name=$(basename "$prompt_file" .txt)
    prompt=$(cat "$prompt_file")
    outfile="$RESULTS_DIR/$variant/${prompt_name}.txt"
    
    echo "  → $prompt_name"
    
    # Run with timeout, capture response
    response=$(echo "$prompt" | timeout 60 ./target/release/autopoiesis 2>/dev/null || echo "[TIMEOUT]")
    
    # Save prompt + response
    {
      echo "VARIANT: $variant"
      echo "PROMPT: $prompt"
      echo "---"
      echo "RESPONSE:"
      echo "$response"
    } > "$outfile"
    
    # Small delay to avoid rate limiting
    sleep 2
  done
done

# Restore original constitution
cp identity/constitution.md.bak identity/constitution.md
rm identity/constitution.md.bak

echo ""
echo "=== All tests complete. Results in $RESULTS_DIR ==="
