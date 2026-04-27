# Self-hosted rathole on Oracle Cloud Free Tier

`rathole-oracle-deploy.sh` is a one-shot setup script for running the relay
that Plan 1B's tunnel client connects to. Oracle's Always-Free tier gives you
a 4-OCPU ARM VM with 24GB RAM and **10TB egress/month at $0**, which is
enough to self-host a relay for hundreds of concurrent UI Bridge devices.

## One-time setup

1. **Create an OCI Always-Free ARM VM**
   - Shape: `VM.Standard.A1.Flex` (ARM64)
   - Image: Ubuntu 22.04 LTS
   - Network: public IPv4, open the control port (default 2333) and one
     port per exposed service (5200+)
2. **Generate configs in the runner**
   - Open the Connection Wizard → choose Remote → enter your VM's public
     hostname + a shared token → click "Generate config"
   - Copy the **server.toml** tab content
3. **On the VM, run:**
   ```bash
   # Save server.toml first (paste from the wizard)
   sudo bash rathole-oracle-deploy.sh server.toml
   ```
4. **Open the same ports in OCI's VCN Security List** (the script only
   handles the host firewall). Script prints the exact ports to open.

## What the script does

- Downloads rathole binary for your arch (x86_64 or aarch64)
- Installs to `/usr/local/bin/rathole`
- Creates a dedicated unprivileged `rathole` system user
- Drops `server.toml` in `/etc/rathole/` with 0600 perms
- Writes a hardened systemd unit (`ProtectSystem=strict`, `NoNewPrivileges`)
- Opens the host firewall (ufw / firewalld / iptables depending on distro)
- Enables + starts the service

## Operate

```bash
sudo systemctl status rathole
sudo journalctl -u rathole -f         # tail logs
sudo systemctl restart rathole        # after editing server.toml
```

## Alternatives if you don't want OCI

| Host         | Cost        | Notes                                     |
| ------------ | ----------- | ----------------------------------------- |
| Hetzner CX22 | €4/mo       | 20TB egress included, flat pricing        |
| Fly.io       | ~free small | Global edge, pay-per-use                  |
| AWS t4g.nano | $3/mo VM    | **$0.09/GB egress** — watch out for bills |

The script is distro-agnostic — just pass server.toml to `sudo bash` on any
systemd Linux host and it'll work.
