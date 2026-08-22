use std::path::PathBuf;
use clap::{Parser, Subcommand};
use tokio::fs;
use serde_json::Value;
use anyhow::{bail, Context, Result};
use crate::identity::Identity;
#[derive(Parser, Debug)]
#[command(name = "zgalaxy-cli", author, version, about = "ZGALAXY Sovereign ZeroTier-Compatible Command-Line Interface")]
pub struct Cli {
    #[arg(short, long, default_value = "http://127.0.0.1:9993")]
    pub endpoint: String,

    #[arg(short, long)]
    pub secret: Option<String>,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Generic REST call against the local daemon (ops/diagnostics tool):
    /// `zgalaxy-cli rest GET /status` | `rest POST /controller/network '{}'`
    Rest {
        #[arg(help = "HTTP method: GET | POST | DELETE")]
        method: String,
        #[arg(help = "API path, e.g. /status or /network/<nwid>")]
        path: String,
        #[arg(help = "JSON body for POST (optional)")]
        body: Option<String>,
    },

    /// Show node status, address, and connectivity
    Status,

    /// Show node info (alias for status)
    Info,

    /// Join a private network by 16-character Network ID
    Join {
        #[arg(help = "16-hex digit Network ID (e.g. 069ae38092000001)")]
        nwid: String,
    },

    /// Leave a private network
    Leave {
        #[arg(help = "16-hex digit Network ID")]
        nwid: String,
    },

    /// List all currently joined networks
    #[command(name = "listnetworks", alias = "list-networks")]
    ListNetworks,

    /// List active connected peers and roots
    #[command(name = "listpeers", alias = "list-peers")]
    ListPeers,

    /// Identity & Moon management tool (zerotier-idtool compatible)
    #[command(name = "idtool", alias = "id-tool")]
    IdTool {
        #[command(subcommand)]
        sub: IdToolCommands,
    },
}

#[derive(Subcommand, Debug)]
pub enum IdToolCommands {
    /// Generate public and secret identity keypair
    Generate {
        #[arg(help = "Output secret identity file path", default_value = "identity.secret")]
        secret_file: String,
        #[arg(help = "Output public identity file path", default_value = "identity.public")]
        public_file: String,
    },

    /// Initialize moon JSON configuration template from public identity
    #[command(name = "initmoon", alias = "init-moon")]
    InitMoon {
        #[arg(help = "Input public identity file path", default_value = "identity.public")]
        public_file: String,
    },

    /// Compile moon JSON configuration into signed binary .moon file
    #[command(name = "genmoon", alias = "gen-moon")]
    GenMoon {
        #[arg(help = "Input moon JSON file path", default_value = "moon.json")]
        moon_json: String,
    },

    /// Build a signed planet (world.bin) from a moon definition — native
    /// replacement for the C++ mkmoonworld tool. Invocations of a binary
    /// named `mkmoonworld*` are routed here automatically (argv0 dispatch).
    #[command(name = "mkmoonworld", alias = "mk-moonworld")]
    MkMoonWorld {
        #[arg(help = "Input moon JSON file path", default_value = "moon.json")]
        moon_json: String,
    },
}

impl Cli {
    /// Retrieve the authentication secret token from file or argument.
    pub async fn resolve_token(&self) -> String {
        if let Some(ref s) = self.secret {
            return s.trim().to_string();
        }

        let paths = [
            PathBuf::from("/var/lib/zerotier-one/authtoken.secret"),
            PathBuf::from("./zerotier-var/authtoken.secret"),
            PathBuf::from("./authtoken.secret"),
        ];

        for p in &paths {
            if p.exists() {
                if let Ok(c) = fs::read_to_string(p).await {
                    return c.trim().to_string();
                }
            }
        }

        String::new()
    }

    /// Execute a client command via HTTP REST call to the daemon.
    pub async fn execute(self) -> Result<()> {
        let token = self.resolve_token().await;
        // idtool subcommands are purely local file operations — they must
        // work during bootstrap before any authtoken.secret exists.
        let needs_token = !matches!(self.command, Commands::IdTool { .. });
        if needs_token && token.is_empty() {
            eprintln!("zerotier-cli: authtoken.secret not found or not readable (permission denied).");
            eprintln!("            Please run with sudo: 'sudo zgalaxy-cli {}'", match &self.command {
                Commands::Status | Commands::Info => "status",
                Commands::ListNetworks => "listnetworks",
                Commands::ListPeers => "listpeers",
                Commands::Join { .. } => "join <nwid>",
                Commands::Leave { .. } => "leave <nwid>",
                _ => "",
            });
            return Ok(());
        }

        match self.command {
            Commands::Rest { method, path, body } => {
                let m = method.to_ascii_uppercase();
                let url = format!("{}{}", self.endpoint.trim_end_matches('/'), path);
                let out = match m.as_str() {
                    "GET" => fetch_json(&url, &token).await?,
                    "POST" => {
                        // Refuse silently-degraded bodies: a typo'd JSON would
                        // otherwise be sent as `{`}` and act on the wrong data.
                        let payload: Value = match body.as_deref() {
                            None | Some("{}") => serde_json::json!({}),
                            Some(text) => serde_json::from_str(text)
                                .with_context(|| format!("invalid JSON body: {}", text))?,
                        };
                        post_json(&url, &token, payload).await?
                    }
                    "DELETE" => delete_req(&url, &token).await?,
                    other => {
                        eprintln!("rest: unsupported method {}", other);
                        std::process::exit(1);
                    }
                };
                println!("{}", serde_json::to_string_pretty(&out)?);
            }
            Commands::Status | Commands::Info => {
                let url = format!("{}/status", self.endpoint);
                match fetch_json(&url, &token).await {
                    Ok(val) => {
                        let addr = val["address"].as_str().unwrap_or("unknown");
                        let ver = val["version"].as_str().unwrap_or("1.3.0");
                        let online = if val["online"].as_bool().unwrap_or(false) { "ONLINE" } else { "OFFLINE" };
                        println!("200 info {} {} {}", addr, ver, online);
                    }
                    Err(e) => {
                        eprintln!("zerotier-cli: cannot connect to service: {}", e);
                    }
                }
            }
            Commands::Join { nwid } => {
                let clean = nwid.trim().to_lowercase();
                let url = format!("{}/network/{}", self.endpoint, clean);
                match post_json(&url, &token, serde_json::json!({})).await {
                    Ok(_) => println!("200 join OK"),
                    Err(e) => eprintln!("zerotier-cli: join failed: {}", e),
                }
            }
            Commands::Leave { nwid } => {
                let clean = nwid.trim().to_lowercase();
                let url = format!("{}/network/{}", self.endpoint, clean);
                match delete_req(&url, &token).await {
                    Ok(_) => println!("200 leave OK"),
                    Err(e) => eprintln!("zerotier-cli: leave failed: {}", e),
                }
            }
            Commands::ListNetworks => {
                let url = format!("{}/network", self.endpoint);
                match fetch_json(&url, &token).await {
                    Ok(val) => {
                        println!("200 listnetworks <nwid> <name> <mac> <status> <type> <dev> <ips>");
                        if let Some(arr) = val.as_array() {
                            for net in arr {
                                let nwid = net["nwid"].as_str().unwrap_or("-");
                                let name = net["name"].as_str().unwrap_or("-");
                                let mac = net["mac"].as_str().unwrap_or("-");
                                let status = net["status"].as_str().unwrap_or("OK");
                                let type_name = net["type_name"].as_str().unwrap_or("PRIVATE");
                                let dev = net["port_device_name"].as_str().unwrap_or("-");
                                let ips = net["assigned_addresses"].as_array()
                                    .map(|a| a.iter().filter_map(|i| i.as_str()).collect::<Vec<_>>().join(","))
                                    .unwrap_or_else(|| "-".to_string());
                                println!("200 listnetworks {} {} {} {} {} {} {}", nwid, name, mac, status, type_name, dev, ips);
                            }
                        }
                    }
                    Err(e) => eprintln!("zerotier-cli: listnetworks failed: {}", e),
                }
            }
            Commands::ListPeers => {
                let url = format!("{}/peer", self.endpoint);
                match fetch_json(&url, &token).await {
                    Ok(val) => {
                        println!("200 listpeers <ztaddr> <path> <latency> <version> <role>");
                        if let Some(arr) = val.as_array() {
                            for peer in arr {
                                let addr = peer["address"].as_str().unwrap_or("-");
                                let path = peer["paths"].as_array()
                                    .and_then(|p| p.first())
                                    .and_then(|p| p["address"].as_str())
                                    .unwrap_or("-");
                                let lat = peer["latency"].as_i64().unwrap_or(-1);
                                let ver = peer["version"].as_str().unwrap_or("1.3.0");
                                let role = peer["role"].as_str().unwrap_or("LEAF");
                                println!("200 listpeers {} {} {} {} {}", addr, path, lat, ver, role);
                            }
                        }
                    }
                    Err(e) => eprintln!("zerotier-cli: listpeers failed: {}", e),
                }
            }
            Commands::IdTool { sub } => match sub {
                IdToolCommands::Generate { secret_file, public_file } => {
                    let id = Identity::generate();
                    fs::write(&public_file, id.to_public_string()).await?;
                    fs::write(&secret_file, id.to_secret_string()?).await?;
                    restrict_secret_permissions(std::path::Path::new(&secret_file));
                    println!("Generated identity keypair: {} & {}", public_file, secret_file);
                    println!("Node address: {}", id.address);
                }
                IdToolCommands::InitMoon { public_file } => {
                    let content = fs::read_to_string(&public_file).await?;
                    let id = Identity::parse(&content)?;

                    // Match zerotier-idtool behavior: when a secret identity
                    // exists next to the public identity, emit the real signing
                    // secret so `genmoon` (and the ZGALAXY engine) can sign the
                    // world immediately. The secret is the full secret identity
                    // string ("<address>:0:<pubhex>:<privhex>").
                    let public_path = PathBuf::from(&public_file);
                    let secret_path = public_path
                        .parent()
                        .map(|p| p.join("identity.secret"))
                        .unwrap_or_else(|| PathBuf::from("identity.secret"));
                    let secret_str = if secret_path.exists() {
                        fs::read_to_string(&secret_path)
                            .await
                            .ok()
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .unwrap_or_default()
                    } else {
                        String::new()
                    };

                    let template = serde_json::json!({
                        "id": id.address.to_string(),
                        "objtype": "world",
                        "worldType": "moon",
                        "updatesMustBeSigned": 1,
                        "roots": [{
                            "id": id.address.to_string(),
                            // Full public identity (address + public key) so
                            // genmoon/mkmoonworld can embed the real key set of
                            // the root in canonical world binaries.
                            "identity": content.trim(),
                            "stableEndpoints": ["dz.dreamzone.cc/9993"]
                        }],
                        "signingKey": hex::encode(id.verifying_key.to_bytes()),
                        "signingKey_SECRET": secret_str
                    });
                    println!("{}", serde_json::to_string_pretty(&template)?);
                }
                IdToolCommands::GenMoon { moon_json } => {
                    let content = fs::read_to_string(&moon_json).await
                        .with_context(|| format!("Failed to read moon config {}", moon_json))?;
                    let value: Value = serde_json::from_str(&content)?;
                    let def = parse_moon_definition(&value)?;
                    let signer = def.signer.ok_or_else(|| anyhow::anyhow!(
                        "moon definition carries no signingKey_SECRET — run `idtool initmoon identity.public` to regenerate signing keys"
                    ))?;

                    let timestamp = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)?
                        .as_millis() as u64;
                    let world = crate::world::World::new(
                        crate::world::WORLD_TYPE_MOON,
                        def.world_id,
                        timestamp,
                        def.roots,
                    );
                    let bytes = world.encode_canonical(&signer, &def.root_keys)?;

                    // Canonical idtool output naming: the world id formatted as
                    // 16 hex characters, e.g. "000000069ae38092.moon".
                    // ZGALAXY's MoonService looks for exactly this file name.
                    let output = format!("{:016x}.moon", def.world_id);
                    fs::write(&output, &bytes).await?;
                    println!("Signed moon written to {} (canonical world format)", output);
                }
                IdToolCommands::MkMoonWorld { moon_json } => {
                    let content = fs::read_to_string(&moon_json).await
                        .with_context(|| format!("Failed to read moon config {}", moon_json))?;
                    let value: Value = serde_json::from_str(&content)?;
                    let def = parse_moon_definition(&value)?;
                    let signer = def.signer.ok_or_else(|| anyhow::anyhow!(
                        "moon definition carries no signingKey_SECRET — run `idtool initmoon identity.public` to regenerate signing keys"
                    ))?;

                    // Canonical InetAddress entries cannot carry hostnames, so
                    // the official mkmoonworld drops them — mirror that and
                    // refuse a planet with no reachable root (every client
                    // would strand otherwise).
                    let roots: Vec<crate::world::WorldRoot> = def.roots.iter().map(|r| {
                        crate::world::WorldRoot {
                            identity: r.identity,
                            stable_endpoints: r.stable_endpoints.iter()
                                .filter(|ep| crate::world::endpoint_is_ip(ep))
                                .cloned()
                                .collect(),
                        }
                    }).collect();
                    if roots.iter().all(|r| r.stable_endpoints.is_empty()) {
                        bail!("moon definition has no IP stable endpoints — refusing to build a planet with unreachable roots");
                    }

                    let timestamp = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)?
                        .as_millis() as u64;
                    let world = crate::world::World::new(
                        crate::world::WORLD_TYPE_PLANET,
                        crate::world::WORLD_ID_EARTH,
                        timestamp,
                        roots,
                    );
                    let bytes = world.encode_canonical(&signer, &def.root_keys)?;
                    fs::write("world.bin", &bytes).await?;
                    println!("Signed planet written to world.bin (canonical format, world id {})", crate::world::WORLD_ID_EARTH);
                }
            },
        }
        Ok(())
    }
}

/// A parsed moon.json definition (canonical idtool or ZGALAXY flavor).
struct MoonDefinition {
    world_id: u64,
    roots: Vec<crate::world::WorldRoot>,
    /// Ed25519 public key per root when the root carries a full identity.
    root_keys: Vec<Option<[u8; 32]>>,
    signer: Option<crate::identity::Identity>,
}

/// Parse a moon.json document. Accepts roots written as `{"id": <address>}`
/// (ZGALAXY style) or `{"identity": "<full public identity>"}` (canonical
/// idtool style); accepts both `signingKey_SECRET` and `signingKey_secret`.
fn parse_moon_definition(value: &Value) -> Result<MoonDefinition> {
    let id_str = value["id"].as_str().unwrap_or("").to_string();
    let signer = value["signingKey_SECRET"].as_str()
        .or_else(|| value["signingKey_secret"].as_str())
        .filter(|s| !s.is_empty())
        .map(crate::identity::Identity::parse)
        .transpose()
        .context("Invalid signingKey_SECRET in moon definition")?;

    let mut roots: Vec<crate::world::WorldRoot> = Vec::new();
    let mut root_keys: Vec<Option<[u8; 32]>> = Vec::new();
    if let Some(roots_arr) = value["roots"].as_array() {
        for r in roots_arr {
            let root_identity = r["identity"].as_str()
                .and_then(|s| crate::identity::Identity::parse(s).ok());
            let addr = match root_identity.as_ref() {
                Some(id) => id.address,
                None => {
                    let rid = r["id"].as_str().unwrap_or("").to_string();
                    rid.parse::<crate::identity::Address>()
                        .context("Invalid root address in moon definition")?
                }
            };
            let endpoints: Vec<String> = r["stableEndpoints"].as_array()
                .map(|eps| eps.iter().filter_map(|e| e.as_str().map(String::from)).collect())
                .unwrap_or_default();
            // A bare-address root that matches the signer inherits the
            // signer's key set so canonical binaries carry a real key.
            let key = root_identity
                .map(|id| id.verifying_key.to_bytes())
                .or_else(|| {
                    if signer.as_ref().map(|s| s.address) == Some(addr) {
                        signer.as_ref().map(|s| s.verifying_key.to_bytes())
                    } else {
                        None
                    }
                });
            roots.push(crate::world::WorldRoot { identity: addr, stable_endpoints: endpoints });
            root_keys.push(key);
        }
    }

    let world_id = if !id_str.is_empty() {
        id_str.parse::<crate::identity::Address>()
            .context("Invalid moon id address")?
            .to_u64()
    } else {
        roots.first().map(|r| r.identity.to_u64()).unwrap_or(0)
    };

    Ok(MoonDefinition { world_id, roots, root_keys, signer })
}

async fn fetch_json(url: &str, token: &str) -> Result<Value> {    let parsed = url::Url::parse(url)?;
    let host = parsed.host_str().unwrap_or("127.0.0.1");
    let port = parsed.port().unwrap_or(9993);
    let path = parsed.path();

    let target = format!("{}:{}", host, port);
    let mut stream = tokio::net::TcpStream::connect(target).await?;

    let req = format!(
        "GET {} HTTP/1.1\r\nHost: {}:{}\r\nUser-Agent: zgalaxy-cli/1.3.0\r\nX-ZT1-Auth: {}\r\nConnection: close\r\n\r\n",
        path, host, port, token
    );

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    stream.write_all(req.as_bytes()).await?;

    let mut response_buf = Vec::new();
    stream.read_to_end(&mut response_buf).await?;

    let resp_str = String::from_utf8_lossy(&response_buf);
    if let Some(first_line) = resp_str.lines().next() {
        if first_line.contains("401") {
            bail!("401 Unauthorized (invalid or missing secret token)");
        }
        if first_line.contains("404") {
            bail!("404 Not Found");
        }
    }

    if let Some(body_idx) = resp_str.find("\r\n\r\n") {
        let body = &resp_str[body_idx + 4..];
        let val: Value = serde_json::from_str(body)?;
        return Ok(val);
    }
    bail!("Invalid HTTP response from daemon");
}

async fn post_json(url: &str, token: &str, payload: Value) -> Result<Value> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let parsed = url::Url::parse(url)?;
    let host = parsed.host_str().unwrap_or("127.0.0.1");
    let port = parsed.port().unwrap_or(9993);
    let path = parsed.path();
    let body_json = serde_json::to_string(&payload)?;

    let target = format!("{}:{}", host, port);
    let mut stream = tokio::net::TcpStream::connect(target).await?;

    let req = format!(
        "POST {} HTTP/1.1\r\nHost: {}:{}\r\nUser-Agent: zgalaxy-cli/1.3.0\r\nX-ZT1-Auth: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        path, host, port, token, body_json.len(), body_json
    );

    stream.write_all(req.as_bytes()).await?;

    let mut response_buf = Vec::new();
    stream.read_to_end(&mut response_buf).await?;

    let resp_str = String::from_utf8_lossy(&response_buf);
    if let Some(body_idx) = resp_str.find("\r\n\r\n") {
        let body = &resp_str[body_idx + 4..];
        let val: Value = serde_json::from_str(body).unwrap_or(serde_json::json!({ "success": true }));
        return Ok(val);
    }
    Ok(serde_json::json!({ "success": true }))
}

async fn delete_req(url: &str, token: &str) -> Result<Value> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let parsed = url::Url::parse(url)?;
    let host = parsed.host_str().unwrap_or("127.0.0.1");
    let port = parsed.port().unwrap_or(9993);
    let path = parsed.path();

    let target = format!("{}:{}", host, port);
    let mut stream = tokio::net::TcpStream::connect(target).await?;

    let req = format!(
        "DELETE {} HTTP/1.1\r\nHost: {}:{}\r\nUser-Agent: zgalaxy-cli/1.3.0\r\nX-ZT1-Auth: {}\r\nConnection: close\r\n\r\n",
        path, host, port, token
    );

    stream.write_all(req.as_bytes()).await?;

    let mut response_buf = Vec::new();
    stream.read_to_end(&mut response_buf).await?;

    let resp_str = String::from_utf8_lossy(&response_buf);
    let status_line = resp_str.lines().next().unwrap_or_default();
    let status_code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    if !(200..300).contains(&status_code) {
        let body = resp_str.find("\r\n\r\n").map(|i| &resp_str[i + 4..]).unwrap_or_default();
        bail!("DELETE {} failed: HTTP {} {}", url, status_code, body.trim());
    }

    // Return the server's actual JSON body — callers (and diagnostics) must
    // see the real response (e.g. {"result": true} for network leave), not a
    // client-side fabrication.
    let body = resp_str
        .find("\r\n\r\n")
        .map(|i| &resp_str[i + 4..])
        .unwrap_or_default()
        .trim();
    if body.is_empty() {
        return Ok(serde_json::json!({ "result": true }));
    }
    serde_json::from_str(body)
        .with_context(|| format!("DELETE {}: non-JSON response: {}", url, body))
}

/// Restrict a secret identity file to owner-only permissions (0600).
#[cfg(unix)]
fn restrict_secret_permissions(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path) {
        let mut perms = meta.permissions();
        perms.set_mode(0o600);
        let _ = std::fs::set_permissions(path, perms);
    }
}

#[cfg(not(unix))]
fn restrict_secret_permissions(_path: &std::path::Path) {}
