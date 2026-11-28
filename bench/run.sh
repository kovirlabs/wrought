#!/bin/sh
# Run the whole benchmark: build, load equivalent content into Hammerwright,
# WordPress, Drupal and Astro, measure, write bench/results/, tear down.
#
#   bench/run.sh                 # about 15 minutes; needs Docker and network
#   BENCH_KEEP=1 bench/run.sh    # leave the containers up afterwards
#   bench/run.sh build           # only the cold builds
#   bench/run.sh stall 20 1      # 20 PostgreSQL builds with phase timings
#                                # and the disk's sync latency (stall hunt)
#
# Load runs are short (3 s warm-up, 10 s measured) and every container has a
# CPU and memory cap, so other work on the machine keeps running.
set -eu
cd "$(dirname "$0")"
exec python3 run.py "$@"
