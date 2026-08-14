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

        match self.command {
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
                                let lat = peer["latency_ms"].as_i64().unwrap_or(-1);
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
                    println!("Generated identity keypair: {} & {}", public_file, secret_file);
                    println!("Node address: {}", id.address);
                }
                IdToolCommands::InitMoon { public_file } => {
                    let content = fs::read_to_string(&public_file).await?;
                    let id = Identity::parse(&content)?;
                    let template = serde_json::json!({
                        "id": id.address.to_string(),
                        "objtype": "world",
                        "roots": [{
                            "id": id.address.to_string(),
                            "stableEndpoints": ["dz.dreamzone.cc:9993"]
                        }],
                        "signingKey": hex::encode(id.verifying_key.to_bytes()),
                        "signingKey_secret": ""
                    });
                    println!("{}", serde_json::to_string_pretty(&template)?);
                }
                IdToolCommands::GenMoon { moon_json } => {
                    let content = fs::read_to_string(&moon_json).await
                        .with_context(|| format!("Failed to read moon config {}", moon_json))?;
                    let value: Value = serde_json::from_str(&content)?;

                    let id_str = value["id"].as_str().unwrap_or("").to_string();
                    let root_ids: Vec<String> = value["roots"]
                        .as_array()
                        .map(|roots| roots.iter().filter_map(|r| r["id"].as_str().map(|s| s.to_string())).collect())
                        .unwrap_or_default();

                    let id = id_str.parse::<crate::identity::Address>()
                        .context("Invalid moon id address")?;
                    let timestamp = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)?
                        .as_millis() as u64;

                    let roots = root_ids
                        .iter()
                        .filter_map(|r| r.parse::<crate::identity::Address>().ok())
                        .map(|addr| crate::world::WorldRoot {
                            identity: addr,
                            stable_endpoints: vec!["dz.dreamzone.cc:9993".to_string()],
                        })
                        .collect();

                    let mut world = crate::world::World::new(
                        crate::world::WORLD_TYPE_MOON,
                        id.to_u64(),
                        timestamp,
                        roots,
                    );

                    // Sign the world if a secret signing key is available
                    if let Some(secret_key_hex) = value["signingKey_secret"].as_str() {
                        if !secret_key_hex.is_empty() {
                            let secret_identity = crate::identity::Identity::parse(secret_key_hex)?;
                            let sig = secret_identity.sign(&world.encode())?;
                            world.signature = sig.to_vec();
                        }
                    }

                    let output = moon_json.replace(".json", ".moon");
                    world.save_to_file(&output).await?;
                    println!("Signed moon written to {}", output);
                }
            },
        }
        Ok(())
    }
}

async fn fetch_json(url: &str, token: &str) -> Result<Value> {
    let parsed = url::Url::parse(url)?;
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

    Ok(serde_json::json!({ "deleted": true }))
}
