#!/usr/bin/env python3
"""
IPC Latency Benchmark Script

Measures the round-trip latency of IPC communication between Rust and Python
by calling various endpoints through the runner's HTTP API.

Usage:
    python benchmark-ipc.py [--iterations N] [--runner-url URL]

Requirements:
    - Runner must be running on the specified URL (default: http://localhost:9876)
    - httpx must be installed: pip install httpx

Results:
    Prints statistics including min, max, mean, median, p95, p99 latencies.
"""

import argparse
import statistics
import sys
import time
from dataclasses import dataclass

try:
    import httpx
except ImportError:
    print("Error: httpx is required. Install with: pip install httpx")
    sys.exit(1)


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


def main():
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
        default="http://localhost:9876",
        help="Runner HTTP API URL (default: http://localhost:9876)",
    )
    parser.add_argument(
        "--warmup",
        "-w",
        type=int,
        default=10,
        help="Number of warmup iterations (default: 10)",
    )

    args = parser.parse_args()

    print(f"IPC Latency Benchmark")
    print(f"=====================")
    print(f"Runner URL: {args.runner_url}")
    print(f"Iterations: {args.iterations}")
    print(f"Warmup: {args.warmup}")

    # Create client with keep-alive
    with httpx.Client(
        base_url=args.runner_url,
        timeout=30.0,
        limits=httpx.Limits(max_keepalive_connections=5, max_connections=10),
    ) as client:
        # Check if runner is available
        try:
            response = client.get("/health")
            if response.status_code != 200:
                print(f"\nError: Runner health check failed: {response.status_code}")
                sys.exit(1)
        except httpx.ConnectError:
            print(f"\nError: Cannot connect to runner at {args.runner_url}")
            print("Make sure the runner is running.")
            sys.exit(1)

        print("\nRunner connected successfully.")

        # Warmup
        print(f"\nWarming up ({args.warmup} iterations)...")
        for _ in range(args.warmup):
            client.get("/health")
            client.get("/status")
        print("Warmup complete.")

        # Run benchmarks
        results = []

        print("\nRunning benchmarks...")

        # 1. Health check (Rust only - baseline)
        print("  - Health check (baseline)...")
        results.append(benchmark_health_check(client, args.iterations))

        # 2. Status (Rust + Python IPC)
        print("  - Status (IPC)...")
        results.append(benchmark_status(client, args.iterations))

        # 3. Models list (Rust + Python IPC)
        print("  - Models list (IPC)...")
        results.append(benchmark_models_list(client, args.iterations))

        # 4. Executor status (Rust + Python IPC)
        print("  - Executor status (IPC)...")
        results.append(benchmark_executor_status(client, args.iterations))

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
            print(f"  Target: IPC latency < 10ms for simple operations")
            print(f"  Status: {'PASS' if target_met else 'NEEDS INVESTIGATION'}")

            for result in results[1:]:
                status = "PASS" if result.median_ms < 10 else "SLOW"
                print(f"    {result.operation}: {result.median_ms:.3f} ms [{status}]")


if __name__ == "__main__":
    main()
