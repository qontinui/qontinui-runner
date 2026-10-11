#!/usr/bin/env python3
"""
IPC Latency Benchmark Script

Measures the round-trip latency of IPC communication between Rust and Python
by calling various endpoints through the runner's HTTP API.

Usage:
    python benchmark-ipc.py --runner-url URL [--iterations N] [--warmup N]

Which runner:
    There is no default target. The old default was the primary runner on
    port 9876, the runner hosting the operator's live sessions, and a
    benchmark there loads those sessions while measuring a busy runner. So
    the script refuses to reach port 9876 implicitly, the way
    scripts/perf-harness.mjs refuses PRIMARY_RUNNER_PORT. Benchmark a temp
    runner: spawn one through the dev supervisor
    (POST http://127.0.0.1:9875/runners/spawn-test, ports 9877-9899) and pass
    its URL. To benchmark the primary on purpose, name it explicitly:
    --runner-url http://127.0.0.1:9876.

Requirements:
    - The runner at --runner-url must be running.
    - httpx must be installed: pip install httpx (only needed once a target
      has been accepted; the refusal itself needs nothing beyond Python).

Results:
    Prints statistics including min, max, mean, median, p95, p99 latencies.

Exit status:
    0 on a completed run, 1 when the runner cannot be reached or httpx is
    missing, 2 when the target is refused or is not an http(s) URL.
"""

from __future__ import annotations

import argparse
import statistics
import sys
import time
from dataclasses import dataclass
from typing import TYPE_CHECKING
from urllib.parse import urlsplit

if TYPE_CHECKING:
    import httpx

#: The primary runner's port. The script never targets it implicitly.
PRIMARY_RUNNER_PORT = 9876

#: Exit status for a refused or unusable target.
EXIT_TARGET_REFUSED = 2


class TargetRefused(Exception):
    """The benchmark target was refused before any request was sent."""


def resolve_target(runner_url: str | None) -> str:
    """Return the runner base URL to benchmark, or raise ``TargetRefused``.

    ``runner_url`` is the operator's ``--runner-url``, or ``None`` when it was
    not passed. Omitting it would reach the old default, the primary runner,
    so that is refused. An explicit URL is accepted, port 9876 included: the
    operator named the port, which is the override.
    """
    if runner_url is None:
        raise TargetRefused(
            f"refusing to default to port {PRIMARY_RUNNER_PORT}: that is the primary "
            "runner, which hosts live sessions, and a benchmark there both loads "
            "them and measures a busy runner. Benchmark a temp runner instead: "
            "spawn one through the dev supervisor "
            "(POST http://127.0.0.1:9875/runners/spawn-test) and pass its URL, "
            "e.g. --runner-url http://127.0.0.1:9877. To benchmark the primary on "
            f"purpose, pass it explicitly: --runner-url http://127.0.0.1:{PRIMARY_RUNNER_PORT}"
        )
    parts = urlsplit(runner_url)
    if parts.scheme not in ("http", "https") or not parts.hostname:
        raise TargetRefused(
            f"--runner-url {runner_url!r} is not an http(s) URL with a host, "
            "e.g. http://127.0.0.1:9877"
        )
    try:
        parts.port
    except ValueError as e:
        raise TargetRefused(f"--runner-url {runner_url!r} has an invalid port: {e}") from e
    return runner_url


def targets_primary(runner_url: str) -> bool:
    """True when an accepted ``runner_url`` names the primary runner's port."""
    return urlsplit(runner_url).port == PRIMARY_RUNNER_PORT


@dataclass
class BenchmarkResult:
    """Results from a benchmark run."""

    operation: str
    iterations: int
    total_time_ms: float
    latencies_ms: list[float]

    @property
    def min_ms(self) -> float:
        return min(self.latencies_ms) if self.latencies_ms else 0

    @property
    def max_ms(self) -> float:
        return max(self.latencies_ms) if self.latencies_ms else 0

    @property
    def mean_ms(self) -> float:
        return statistics.mean(self.latencies_ms) if self.latencies_ms else 0

    @property
    def median_ms(self) -> float:
        return statistics.median(self.latencies_ms) if self.latencies_ms else 0

    @property
    def stdev_ms(self) -> float:
        return (
            statistics.stdev(self.latencies_ms)
            if len(self.latencies_ms) > 1
            else 0
        )

    @property
    def p95_ms(self) -> float:
        if not self.latencies_ms:
            return 0
        sorted_latencies = sorted(self.latencies_ms)
        idx = int(len(sorted_latencies) * 0.95)
        return sorted_latencies[min(idx, len(sorted_latencies) - 1)]

    @property
    def p99_ms(self) -> float:
        if not self.latencies_ms:
            return 0
        sorted_latencies = sorted(self.latencies_ms)
        idx = int(len(sorted_latencies) * 0.99)
        return sorted_latencies[min(idx, len(sorted_latencies) - 1)]

    def print_summary(self):
        """Print a summary of the benchmark results."""
        print(f"\n{self.operation}")
        print("=" * 60)
        print(f"  Iterations:    {self.iterations}")
        print(f"  Total time:    {self.total_time_ms:.2f} ms")
        print(f"  Throughput:    {self.iterations / (self.total_time_ms / 1000):.1f} ops/sec")
        print()
        print("  Latency Statistics (ms):")
        print(f"    Min:         {self.min_ms:.3f}")
        print(f"    Max:         {self.max_ms:.3f}")
        print(f"    Mean:        {self.mean_ms:.3f}")
        print(f"    Median:      {self.median_ms:.3f}")
        print(f"    Std Dev:     {self.stdev_ms:.3f}")
        print(f"    P95:         {self.p95_ms:.3f}")
        print(f"    P99:         {self.p99_ms:.3f}")


def benchmark_health_check(client: httpx.Client, iterations: int) -> BenchmarkResult:
    """Benchmark the health check endpoint (Rust-only, no Python)."""
    latencies = []
    start_total = time.perf_counter()

    for _ in range(iterations):
        start = time.perf_counter()
        response = client.get("/health")
        end = time.perf_counter()

        if response.status_code == 200:
            latencies.append((end - start) * 1000)
        else:
            print(f"  Warning: Health check failed with status {response.status_code}")

    end_total = time.perf_counter()

    return BenchmarkResult(
        operation="Health Check (Rust only)",
        iterations=len(latencies),
        total_time_ms=(end_total - start_total) * 1000,
        latencies_ms=latencies,
    )


def benchmark_status(client: httpx.Client, iterations: int) -> BenchmarkResult:
    """Benchmark the status endpoint (Rust + Python IPC)."""
    latencies = []
    start_total = time.perf_counter()

    for _ in range(iterations):
        start = time.perf_counter()
        response = client.get("/status")
        end = time.perf_counter()

        if response.status_code == 200:
            latencies.append((end - start) * 1000)
        else:
            print(f"  Warning: Status failed with status {response.status_code}")

    end_total = time.perf_counter()

    return BenchmarkResult(
        operation="Status (Rust + Python IPC)",
        iterations=len(latencies),
        total_time_ms=(end_total - start_total) * 1000,
        latencies_ms=latencies,
    )


def benchmark_models_list(client: httpx.Client, iterations: int) -> BenchmarkResult:
    """Benchmark the models list endpoint (Rust + Python IPC)."""
    latencies = []
    start_total = time.perf_counter()

    for _ in range(iterations):
        start = time.perf_counter()
        response = client.get("/models")
        end = time.perf_counter()

        if response.status_code == 200:
            latencies.append((end - start) * 1000)
        else:
            print(f"  Warning: Models list failed with status {response.status_code}")

    end_total = time.perf_counter()

    return BenchmarkResult(
        operation="Models List (Rust + Python IPC)",
        iterations=len(latencies),
        total_time_ms=(end_total - start_total) * 1000,
        latencies_ms=latencies,
    )


def benchmark_executor_status(client: httpx.Client, iterations: int) -> BenchmarkResult:
    """Benchmark the executor status endpoint (Rust + Python IPC)."""
    latencies = []
    start_total = time.perf_counter()

    for _ in range(iterations):
        start = time.perf_counter()
        response = client.get("/executor-status")
        end = time.perf_counter()

        if response.status_code == 200:
            latencies.append((end - start) * 1000)
        else:
            print(f"  Warning: Executor status failed with status {response.status_code}")

    end_total = time.perf_counter()

    return BenchmarkResult(
        operation="Executor Status (Rust + Python IPC)",
        iterations=len(latencies),
        total_time_ms=(end_total - start_total) * 1000,
        latencies_ms=latencies,
    )


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Benchmark IPC latency between Rust and Python"
    )
    parser.add_argument(
        "--iterations",
        "-n",
        type=int,
        default=100,
        help="Number of iterations per benchmark (default: 100)",
    )
    parser.add_argument(
        "--runner-url",
        "-u",
        type=str,
        default=None,
        help=(
            "Runner HTTP API URL to benchmark, e.g. a temp runner at "
            "http://127.0.0.1:9877. Required: there is no default, and port "
            f"{PRIMARY_RUNNER_PORT} (the primary runner) is reached only when "
            "named here explicitly."
        ),
    )
    parser.add_argument(
        "--warmup",
        "-w",
        type=int,
        default=10,
        help="Number of warmup iterations (default: 10)",
    )
    return parser


def run_benchmarks(runner_url: str, iterations: int, warmup: int) -> int:
    """Run every benchmark against ``runner_url`` and print the report."""
    try:
        import httpx
    except ImportError:
        print("Error: httpx is required. Install with: pip install httpx")
        return 1

    print("IPC Latency Benchmark")
    print("=====================")
    print(f"Runner URL: {runner_url}")
    print(f"Iterations: {iterations}")
    print(f"Warmup: {warmup}")

    # Create client with keep-alive
    with httpx.Client(
        base_url=runner_url,
        timeout=30.0,
        limits=httpx.Limits(max_keepalive_connections=5, max_connections=10),
    ) as client:
        # Check if runner is available
        try:
            response = client.get("/health")
            if response.status_code != 200:
                print(f"\nError: Runner health check failed: {response.status_code}")
                return 1
        except httpx.ConnectError:
            print(f"\nError: Cannot connect to runner at {runner_url}")
            print("Make sure the runner is running.")
            return 1

        print("\nRunner connected successfully.")

        # Warmup
        print(f"\nWarming up ({warmup} iterations)...")
        for _ in range(warmup):
            client.get("/health")
            client.get("/status")
        print("Warmup complete.")

        # Run benchmarks
        results = []

        print("\nRunning benchmarks...")

        # 1. Health check (Rust only - baseline)
        print("  - Health check (baseline)...")
        results.append(benchmark_health_check(client, iterations))

        # 2. Status (Rust + Python IPC)
        print("  - Status (IPC)...")
        results.append(benchmark_status(client, iterations))

        # 3. Models list (Rust + Python IPC)
        print("  - Models list (IPC)...")
        results.append(benchmark_models_list(client, iterations))

        # 4. Executor status (Rust + Python IPC)
        print("  - Executor status (IPC)...")
        results.append(benchmark_executor_status(client, iterations))

        # Print results
        print("\n" + "=" * 60)
        print("BENCHMARK RESULTS")
        print("=" * 60)

        for result in results:
            result.print_summary()

        # Calculate IPC overhead
        if len(results) >= 2:
            rust_baseline = results[0].mean_ms
            ipc_mean = statistics.mean([r.mean_ms for r in results[1:]])
            ipc_overhead = ipc_mean - rust_baseline

            print("\n" + "=" * 60)
            print("IPC OVERHEAD ANALYSIS")
            print("=" * 60)
            print(f"  Rust baseline (health):  {rust_baseline:.3f} ms")
            print(f"  IPC operations mean:     {ipc_mean:.3f} ms")
            print(f"  IPC overhead:            {ipc_overhead:.3f} ms")
            print(f"  IPC overhead percentage: {(ipc_overhead / ipc_mean) * 100:.1f}%")

            # Check if we meet the < 1ms target for simple operations
            print("\n" + "=" * 60)
            print("TARGET ASSESSMENT")
            print("=" * 60)
            target_met = all(r.median_ms < 10 for r in results[1:])
            print("  Target: IPC latency < 10ms for simple operations")
            print(f"  Status: {'PASS' if target_met else 'NEEDS INVESTIGATION'}")

            for result in results[1:]:
                status = "PASS" if result.median_ms < 10 else "SLOW"
                print(f"    {result.operation}: {result.median_ms:.3f} ms [{status}]")

    return 0


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        runner_url = resolve_target(args.runner_url)
    except TargetRefused as e:
        print(f"benchmark-ipc: {e}", file=sys.stderr)
        return EXIT_TARGET_REFUSED
    if targets_primary(runner_url):
        print(
            f"benchmark-ipc: benchmarking the PRIMARY runner on port {PRIMARY_RUNNER_PORT} "
            "because --runner-url named it explicitly.",
            file=sys.stderr,
        )
    return run_benchmarks(runner_url, args.iterations, args.warmup)


if __name__ == "__main__":
    sys.exit(main())
