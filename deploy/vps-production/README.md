# Satonero VPS production stack

Ready-to-copy files that run the full production backend on a VPS via Docker
Compose: **Satonero daemon + Nostr relay + Caddy (automatic TLS/wss)**.

Files:

| File | Purpose |
|---|---|
| `docker-compose.yml` | Daemon (`satonero`) + `nostr-rs-relay` + `caddy` reverse proxy |
| `Caddyfile` | TLS/wss for the relay; **set your relay domain here** |
| `relay_config.toml` | nostr-rs-relay config, allowlisted to Satonero/Mostro event kinds |
| `settings.example.toml` | Daemon config example: nsec, relays, LND paths, Cashu, Balkan fiats |

## Quick start

```bash
cd deploy/vps-production
mkdir -p config/lnd config/relay/data config/caddy

cp settings.example.toml config/settings.toml && nano config/settings.toml
cp relay_config.toml      config/relay/config.toml
cp Caddyfile              config/caddy/Caddyfile && nano config/caddy/Caddyfile

# LND credentials (so they appear at /config/lnd/ in the container):
cp /path/to/lnd/tls.cert     config/lnd/tls.cert
cp /path/to/lnd/data/chain/bitcoin/mainnet/admin.macaroon config/lnd/admin.macaroon

docker compose up -d
docker compose logs -f satonero
```

## Notes

- **Domain/DNS first:** the domain in `Caddyfile` must already resolve to this
  VPS (an A/AAAA record) before `docker compose up`, so Caddy can issue a
  Let's Encrypt certificate.
- **LND first:** in Lightning mode LND must be reachable before the daemon
  starts, or `mostrod` exits. Point `lnd_grpc_host` at your LND (same host via
  `host.docker.internal`, or a remote host).
- **Image:** until a signed `vX.Y.Z` release publishes `:latest` to GHCR, the
  compose file builds the image from the repository (`docker/Dockerfile`). The
  `image:` line is commented for when a release tag exists.
- **Cashu mode** (no LND): set `[cashu].enabled = true` and `mint_urls` in
  `settings.toml`, and you can skip the `config/lnd/` step.

See the full multi-user guide: [`docs/Satonero-Deployment-Guide.md`](../docs/Satonero-Deployment-Guide.md).
