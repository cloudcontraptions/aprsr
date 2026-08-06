# Deploying aprsr

Four ways to run it, in rough order of how often they are wanted. All of them need the same
two things first: a configuration file with **your own callsign** in `server.id`, and a
directory aprsr may write to.

aprsr refuses to start while `server.id` is `NOCALL`. That is not fussiness — APRS-IS
detects loops by looking for a server's own callsign in a packet's path, so two servers
sharing an identity break loop detection for every station whose packets pass through
either of them, not just for the operator who misconfigured one.

```bash
aprsr passcode YOURCALL     # the passcode that goes with it
```

A passcode is only needed to *transmit*: a receive-only server, and a `readonly` uplink,
both work with `passcode = -1`. Get the callsign right first and add the passcode when you
want the server to contribute traffic.

---

## Container

Works with Docker and with Podman. The `Dockerfile` uses no BuildKit-only features, so
`podman build` reads it unmodified — deliberate, because rootless Podman is often the only
runtime installed on the kind of host an APRS-IS server ends up on.

```bash
cp aprsr.example.toml aprsr.toml    # edit server.id
docker build -t aprsr .
docker run -d --name aprsr \
  -p 14580:14580 -p 10152:10152 -p 127.0.0.1:14501:14501 \
  -v "$PWD/aprsr.toml:/etc/aprsr/aprsr.toml:ro" \
  -v aprsr-data:/var/lib/aprsr \
  aprsr
```

Or with compose, which writes the same thing down where it can be reviewed:

```bash
docker compose up -d
docker compose logs -f
```

Points worth knowing:

- **The status port is published on loopback** in `compose.yaml`. It has no authentication
  beyond the optional `http.admin_token`, so exposing it publicly should be a decision, not
  a default. Put a reverse proxy in front of it if it needs to be reachable.
- **The image runs as uid 10001.** A bind-mounted data directory has to be writable by that
  uid; the named volume in `compose.yaml` sidesteps the question entirely.
- **`HEALTHCHECK` runs `aprsr healthcheck`**, which probes `/healthz` over loopback inside
  the container. No curl in the image, no dependency added for one line.
- **File descriptors.** One client is one descriptor. `limits.file_limit` in `aprsr.toml`
  raises the soft limit as far as the hard limit allows, and container runtimes default the
  hard limit low enough to matter on a busy port — hence the `ulimits` block in
  `compose.yaml`.
- **Stopping.** `docker stop` sends `SIGTERM`, which aprsr handles: it says goodbye to
  connected clients and writes the station position cache to the database before exiting.
  Give it a moment (`--timeout 15`) rather than letting the runtime escalate to `SIGKILL`,
  which loses the positions that were about to be saved.

The image is built and exercised in CI on every change — started, logged into over TCP,
probed over HTTP and stopped — but it is **not published to a registry**. Build it yourself.

---

## systemd

```ini
# /etc/systemd/system/aprsr.service
[Unit]
Description=aprsr APRS-IS server
After=network-online.target
Wants=network-online.target

[Service]
Type=exec
ExecStart=/usr/local/bin/aprsr run --config /etc/aprsr/aprsr.toml
# SIGHUP re-reads the configuration without dropping clients. Settings a running server
# cannot adopt are named in the log rather than silently ignored.
ExecReload=/bin/kill -HUP $MAINPID
Restart=on-failure
RestartSec=5s

User=aprsr
Group=aprsr
StateDirectory=aprsr
WorkingDirectory=/var/lib/aprsr

# One descriptor per client. Set this above the client cap you actually want, because
# `limits.file_limit` cannot raise the soft limit past the hard one.
LimitNOFILE=16384

# aprsr needs to read its configuration and write its state directory. Nothing else.
NoNewPrivileges=true
PrivateTmp=true
PrivateDevices=true
ProtectSystem=strict
ProtectHome=true
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectControlGroups=true
RestrictAddressFamilies=AF_INET AF_INET6
RestrictNamespaces=true
LockPersonality=true
MemoryDenyWriteExecute=true

[Install]
WantedBy=multi-user.target
```

```bash
sudo useradd --system --home-dir /var/lib/aprsr --shell /usr/sbin/nologin aprsr
sudo systemctl enable --now aprsr
sudo systemctl reload aprsr      # after editing aprsr.toml
journalctl -u aprsr -f
```

`--log-format json` is worth setting if the logs go anywhere that parses them.

---

## macOS, with launchd

```xml
<!-- /Library/LaunchDaemons/net.aprsr.server.plist -->
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>              <string>net.aprsr.server</string>
  <key>ProgramArguments</key>
  <array>
    <string>/usr/local/bin/aprsr</string>
    <string>run</string>
    <string>--config</string>
    <string>/usr/local/etc/aprsr/aprsr.toml</string>
  </array>
  <key>WorkingDirectory</key>   <string>/usr/local/var/aprsr</string>
  <key>RunAtLoad</key>          <true/>
  <key>KeepAlive</key>          <true/>
  <key>StandardOutPath</key>    <string>/usr/local/var/log/aprsr.log</string>
  <key>StandardErrorPath</key>  <string>/usr/local/var/log/aprsr.log</string>
  <!-- One descriptor per client. -->
  <key>SoftResourceLimits</key>
  <dict><key>NumberOfFiles</key><integer>16384</integer></dict>
</dict>
</plist>
```

```bash
sudo launchctl load -w /Library/LaunchDaemons/net.aprsr.server.plist
```

launchd sends `SIGTERM` on unload, which aprsr handles the same way it handles Ctrl-C.
There is no `SIGHUP` equivalent in a `launchctl` workflow, so reload over HTTP instead —
see below.

---

## Windows

aprsr is built and tested on Windows in CI, and runs from a console with no ceremony:

```powershell
aprsr.exe run --config C:\ProgramData\aprsr\aprsr.toml
```

Two things differ from Unix, and both are handled rather than merely survived:

- **There is no `SIGHUP`**, so configuration reload goes over HTTP. Set `http.admin_token`
  and `POST /admin/reload` — the same code path `SIGHUP` triggers, so the behaviour is
  identical.
- **There is no per-process descriptor limit.** `limits.file_limit` is validated and then
  says it does not apply, rather than pretending to have done something.

To run it as a service, wrap it with a service host such as [NSSM] or WinSW. aprsr listens
for `CTRL_CLOSE_EVENT` and `CTRL_SHUTDOWN_EVENT` as well as Ctrl-C and Ctrl-Break, which is
what a service host actually sends when it stops a console application — so it runs its
shutdown path and says goodbye to connected clients rather than having the socket vanish
underneath them.

**A path in TOML needs single quotes on Windows.** A backslash inside a `"double quoted"`
TOML string is an escape, so `run_dir = "C:\ProgramData\aprsr"` is not the path it looks
like. Write `run_dir = 'C:\ProgramData\aprsr'`.

[NSSM]: https://nssm.cc/

---

## After it is running

```bash
curl -s localhost:14501/status.json | jq '.server, .uplinks, .alarms'
curl -s localhost:14501/metrics                    # Prometheus text format
curl -sN localhost:14501/events/status             # a snapshot a second
```

The dashboard at `http://localhost:14501/` shows the same thing for a human, including a
map of what this server has actually heard.

**Reload without restarting**, once `http.admin_token` is set:

```bash
curl -X POST -H "X-Aprsr-Admin-Token: $TOKEN" localhost:14501/admin/reload
```

The response says what was applied and what needs a restart. An invalid file changes
nothing at all, so a typo cannot leave a server half-configured.

**Prefer the environment to the file for the token**: every `[section].key` has an
`APRSR_SECTION__KEY` override, so `APRSR_HTTP__ADMIN_TOKEN` sets it without it ever being
written down.

## TLS

A TLS port is a separate `[[listen]]`, because it has to be: TLS and plaintext cannot share
one, and every APRS-IS client expects the well-known ports to be plaintext. Keep 14580 and
10152 as they are and add a port beside them.

```toml
[[listen]]
name = "Secure client port"
kind = "igate"
bind = "[::]:24580"
tls = { cert = "/etc/letsencrypt/live/aprs.example.net/fullchain.pem", key = "/etc/letsencrypt/live/aprs.example.net/privkey.pem" }
```

`cert` is the **full chain** — leaf first, then the intermediates that chain it to a root.
A file containing only the leaf works against a client that already has the intermediate
cached and fails everywhere else, which is the single most common TLS deployment mistake.
With Let's Encrypt that means `fullchain.pem`, not `cert.pem`. The key may be PKCS#8, PKCS#1
or SEC1.

Both files are read when the server binds, so `aprsr check-config` will not catch a bad path
but the very next `aprsr run` will, immediately, with the filename in the message.
`check-config` does mark which ports are TLS, which is worth checking against what you meant.

**Certificate renewal needs a restart.** Certificates are loaded once at bind time and held
for the life of the process, so a certbot renewal hook should reload or restart the service:

```ini
# /etc/letsencrypt/renewal-hooks/deploy/aprsr.sh
#!/bin/sh
systemctl restart aprsr
```

`SIGHUP` is not enough — it re-reads the configuration, and the certificate paths in it have
not changed.

**File permissions.** The private key must be readable by the user aprsr runs as. With the
systemd unit above that is `DynamicUser=`, so either grant the certificate directory to the
right group or run as a fixed user. Never make the key world-readable to work around it.

### Uplinks over TLS

```toml
[[uplink]]
name = "Secure core"
kind = "readonly"
address = "t2finland.aprs2.net:24152"
tls = {}
```

An empty `tls = {}` is the switch — its presence turns TLS on. The upstream certificate is
verified against the Mozilla root store compiled into the binary, which is the same on all
three platforms; the operating system's own store is deliberately not used, because that is
three different mechanisms and would make an uplink's trust decisions depend on which host it
ran on.

On a closed network with a private certificate authority:

```toml
tls = { ca_file = "/etc/aprsr/site-ca.pem", server_name = "aprs.site-b.internal" }
```

`ca_file` **replaces** the built-in roots rather than adding to them. `server_name` is the
name the certificate is checked against, needed when connecting by IP address or through a
tunnel; left unset it is the host part of `address`. A bare IPv6 literal always needs it.

There is no option to skip verification, and there will not be one. A `qAS` construct naming
a server aprsr did not actually authenticate is not a local mistake — it is wrong information
injected into the whole network, where nobody can tell which server produced it.

A failed verification is reported in the uplink's `last_error` on the dashboard and in
`status.json`, naming both the address and the name that could not be verified — those two
strings side by side are usually the whole diagnosis, because the common cause is a
certificate issued for one member of a DNS rotation rather than for the rotation.

## Joining the network

A server with no `[[uplink]]` relays between its own clients and exchanges nothing with
anybody else. Start read-only:

```toml
[[uplink]]
name = "Core rotate"
kind = "readonly"
address = "rotate.aprs.net:10152"
```

Watch the uplinks panel on the dashboard until it says connected, and check that the peer
it names is one you meant to connect to. Then, when you have a tier-2 registration for the
callsign and `check-config` stops warning about the passcode, change `kind` to `full`.

`check-config` warns specifically about a `full` uplink whose passcode does not verify,
because that failure is otherwise invisible: the link connects, the dashboard says
connected, and nothing this server hears ever reaches the network.
