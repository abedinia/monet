# Monet

Monet is a terminal network monitor for Linux. It draws in a holographic cyan, violet and magenta palette on dark glass panels. You get large live readouts, a mirrored traffic waveform (download rises above the axis, upload falls below it), braille area charts that glow at their crests, and keyboard-driven views for links, sockets, flows, latency and events.

Debian packages are in [`releases/`](releases/):

| Platform | Debian package |
| --- | --- |
| Linux Intel/AMD (x86_64) | [`.deb`](releases/monet-0.3.0-linux-x86_64.deb) |
| Linux ARM64 (aarch64) | [`.deb`](releases/monet-0.3.0-linux-aarch64.deb) |

Verify downloads with `cd releases && sha256sum -c SHA256SUMS`, then install with `sudo apt install ./monet-0.3.0-linux-x86_64.deb` (use the `aarch64` file on ARM64).

Monet also has a macOS collector in the source tree. On macOS it reads interface counters and socket tables through the system `netstat` and `ifconfig` commands. Per-connection TCP byte counters are currently available on Linux only. No macOS packages are published. Build the macOS targets on a Mac with `cargo build --release --target aarch64-apple-darwin` or `cargo build --release --target x86_64-apple-darwin`.

Build and run with Rust 1.74 or newer:

```sh
./scripts/build.sh
./monet
```

The build works offline and uses only the Rust standard library. Run as your regular user in a UTF-8 terminal with `stty` available.

| Key | Action |
| --- | --- |
| `1` | Overview: download, upload, socket and TCP round-trip readouts; mirrored traffic waveform; connection history and the busiest flows |
| `2` | Links: a card per interface with live rates, sparklines, totals, packets/s, errors and drops, MAC, MTU, speed and addresses |
| `3` | Sockets: connection history, a TCP state breakdown and a scrollable table with endpoints, state, process, PID, live rate and queues |
| `4` | System: host details, DNS resolvers, collection notices, IPv4 routes and kernel protocol counters |
| `5` | Flows: the busiest TCP transfers with live meters and session totals |
| `6` | Analytics: current, average, 95th percentile, peak and session volume; packets/s, errors and drops, volume per link, and link utilization |
| `7` | Latency: mean TCP round-trip and retransmit history, RTT histogram, TCP states, top remote ports, sockets by process, and slowest peers |
| `8` | Event log: new remote connections and listeners, link changes, errors and drops, and traffic spikes |
| `c` | Switch between braille and block charts |
| `m` | Turn motion (the light sweep, pulse and shimmer) on or off |
| `[` / `]` | Cycle through interface charts, including loopback |
| `↑` / `↓` or `k` / `j` | Scroll the current view or move the selection |
| `p` or `Space` | Pause or resume sampling |
| `?` | Show every key; any key closes it |
| `q` or `Ctrl+C` | Quit |

In **Flows**, press `d`, `u`, or `t` to sort by download, upload, or their sum. Press `g` to cycle through individual connections, remote IPs, and process names. Press `b` to switch between bytes per second and session bytes, and `l` to toggle loopback when other interfaces are available. Use `↑` / `↓` to select a row and `Enter` to inspect its local and remote IPs and ports, process and PID when readable, byte counters, TCP round-trip time, and retransmits.

```sh
./monet --interval 500   # Sample every 500 ms (range 250–10000)
./monet --no-color      # Keep the terminal layout with plain colors
./monet --no-motion     # Start without animation (useful over slow SSH)
./monet --once          # Print a JSON snapshot for scripts
```

`NO_COLOR` also disables ANSI colors. With redirected input or output, Monet prints a plain interface report and exits.

The aggregate chart excludes loopback, unless loopback is the only visible interface. Select a physical interface to measure that link alone since virtual interfaces can count the same traffic. Interface totals come from Linux kernel counters. Process ownership depends on Linux permissions and refreshes every five seconds. Charts keep up to 300 samples in memory, and the event log keeps the latest 500 entries, timed from when Monet started. A spike is at least 1 MiB/s and four times the recent average. Errors and drops are logged at most once a minute per interface. Link utilization needs an interface that reports its speed, which Wi-Fi often does not. Retransmits in view 7 come from the kernel-wide TCP counter.

Each traffic chart scales to the peak in its visible window. The connection history tracks open TCP and UDP sockets together, with its baseline lifted so small changes stay visible. Monet adapts to truecolor, 256-color and 16-color terminals. In smaller terminals the overview switches to compact layouts, and long views scroll with a scrollbar and a position readout in the footer.

Monet reads Linux TCP socket diagnostics in the current network namespace. Per-connection download is received TCP data, and upload is TCP data acknowledged by the peer. Session bytes are the deltas observed while sampling and retain up to 4,096 recently closed connections that transferred data. Transfers that start and end between samples can be missed. The socket lifetime counters in the detail view also include transfers from before Monet started. The interface charts include UDP and network overhead, while per-peer byte accounting uses TCP. UDP endpoints still appear in Connections. If socket diagnostics are blocked by the execution environment, Monet displays the error and keeps interface charts available.

Run the tests locally with `cargo test --offline --locked`, `./scripts/build.sh`, and `python3 tests/smoke.py`.

Monet is available under the MIT license. See [LICENSE](LICENSE).
