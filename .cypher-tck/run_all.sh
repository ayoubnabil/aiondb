#!/bin/bash
# Complete TCK test run, file by file to avoid cascade crashes
cd "$(dirname "$0")"
base_dir="../testing/cypher-tck/features"
total_pass=0; total_total=0; crashed=0

declare -A cat_pass cat_total

for feature_file in $(find "$base_dir" -name "*.feature" | sort); do
  fname=$(basename "$feature_file" .feature)
  cat=$(dirname "$feature_file" | sed "s|$base_dir/||")
  line=$(RUST_MIN_STACK=16777216 timeout 30 cargo run --bin cypher-tck -- --file "$fname" 2>&1 | grep "Pass rate")
  if [ -n "$line" ]; then
    p=$(echo "$line" | grep -oP '\(\K\d+(?= /)')
    t=$(echo "$line" | grep -oP '/ \K\d+(?=\))')
    total_pass=$((total_pass + p))
    total_total=$((total_total + t))
    cat_pass[$cat]=$(( ${cat_pass[$cat]:-0} + p ))
    cat_total[$cat]=$(( ${cat_total[$cat]:-0} + t ))
  else
    crashed=$((crashed + 1))
  fi
done

echo ""
echo "╔═══════════════════════════════════════════════════════╗"
echo "║        CYPHER TCK RESULTS                            ║"
echo "╠═══════════════════════════════════════════════════════╣"
for cat in $(echo "${!cat_total[@]}" | tr ' ' '\n' | sort); do
  p=${cat_pass[$cat]}; t=${cat_total[$cat]}
  pct=$(echo "scale=1; $p * 100 / $t" | bc 2>/dev/null)
  printf "║ %-40s %4d/%-4d %5s%% ║\n" "$cat" "$p" "$t" "$pct"
done
echo "╠═══════════════════════════════════════════════════════╣"
pct=$(echo "scale=1; $total_pass * 100 / $total_total" | bc 2>/dev/null)
printf "║ %-40s %4d/%-4d %5s%% ║\n" "TOTAL" "$total_pass" "$total_total" "$pct"
echo "║ Crashed files: $crashed                                    ║"
echo "║ Baseline: 479/3881 (12.3%)                          ║"
echo "╚═══════════════════════════════════════════════════════╝"
