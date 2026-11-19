//! Docker's Engine API, spoken over its Unix socket: which container publishes a port (for
//! Ports, where a container's port otherwise shows as Docker's own backend), stopping one, what
//! a prune would free and the prune itself (for Developer Files' commands, `devtools`).
//!
//! The socket is Docker Desktop's (`~/.docker/run/docker.sock`), OrbStack's, Colima's or the
//! system's `/var/run/docker.sock`, the first that answers. A request is HTTP/1.1 with
//! `Connection: close`, read to the end; a chunked body is joined. Every call is bounded by
//! `TIMEOUT`, so a Docker that hangs costs a reading two seconds, not the tab.
//!
//! Under the agent (`--home`) only sockets in that home are tried, never the system's, and a
//! `.docker` file there stands in for the containers, one a line, tab-separated:
//! `id name image publicPort compose-project` (the project may be empty).
use serde_json::Value;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(2);

/// A running container and the host ports it publishes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Container {
    pub id: String,
    /// Without Docker's leading slash: "shop-db-1".
    pub name: String,
    pub image: String,
    /// docker compose's project, "" for a container started on its own.
    pub project: String,
    pub ports: Vec<u16>,
}

/// The sockets to try, in order.
fn sockets(home: &Path, agent: bool) -> Vec<PathBuf> {
    let mut list: Vec<PathBuf> = [".docker/run/docker.sock", ".orbstack/run/docker.sock", ".colima/default/docker.sock", ".rd/docker.sock"].iter().map(|p| home.join(p)).collect();
    if !agent {
        list.push(PathBuf::from("/var/run/docker.sock"));
    }
    list
}

/// The first socket that exists, if any: Docker is installed and may be running.
pub fn socket(home: &Path) -> Option<PathBuf> {
    sockets(home, std::env::var_os("EXACT_AGENT_HOME").is_some()).into_iter().find(|p| p.exists())
}

/// `method path` to the Engine; the reply's status and body.
pub fn request(socket: &Path, method: &str, path: &str) -> Result<(u16, String), String> {
    let mut stream = UnixStream::connect(socket).map_err(|e| format!("Docker isn’t running ({e})."))?;
    stream.set_read_timeout(Some(TIMEOUT)).ok();
    stream.set_write_timeout(Some(TIMEOUT)).ok();
    let length = if method == "POST" { "Content-Length: 0\r\n" } else { "" };
    write!(stream, "{method} {path} HTTP/1.1\r\nHost: docker\r\n{length}Connection: close\r\n\r\n").map_err(|e| e.to_string())?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).map_err(|e| format!("Docker didn’t answer ({e})."))?;
    parse_response(&raw).ok_or_else(|| "Docker’s answer couldn’t be read.".into())
}

/// An HTTP/1.1 response's status and body, joining a chunked one.
pub fn parse_response(raw: &[u8]) -> Option<(u16, String)> {
    let text = String::from_utf8_lossy(raw);
    let (head, body) = text.split_once("\r\n\r\n")?;
    let status = head.split_whitespace().nth(1)?.parse().ok()?;
    let chunked = head.lines().any(|l| l.to_ascii_lowercase().starts_with("transfer-encoding:") && l.to_ascii_lowercase().contains("chunked"));
    if !chunked {
        return Some((status, body.to_owned()));
    }
    let mut out = String::new();
    let mut rest = body;
    loop {
        let (size, after) = rest.split_once("\r\n")?;
        let n = usize::from_str_radix(size.trim().split(';').next()?, 16).ok()?;
        if n == 0 {
            return Some((status, out));
        }
        out.push_str(after.get(..n)?);
        rest = after.get(n..)?.strip_prefix("\r\n").unwrap_or(after.get(n..)?);
    }
}

/// `GET /containers/json`'s running containers.
pub fn parse_containers(json: &str) -> Vec<Container> {
    let Ok(Value::Array(list)) = serde_json::from_str::<Value>(json) else { return Vec::new() };
    list.iter()
        .map(|c| {
            let mut ports: Vec<u16> = c["Ports"].as_array().into_iter().flatten().filter_map(|p| p["PublicPort"].as_u64()).map(|p| p as u16).collect();
            ports.sort_unstable();
            ports.dedup();
            Container {
                id: c["Id"].as_str().unwrap_or_default().to_owned(),
                name: c["Names"][0].as_str().unwrap_or_default().trim_start_matches('/').to_owned(),
                image: c["Image"].as_str().unwrap_or_default().to_owned(),
                project: c["Labels"]["com.docker.compose.project"].as_str().unwrap_or_default().to_owned(),
                ports,
            }
        })
        .collect()
}

/// The running containers: the agent's `.docker`, else the Engine's; none when Docker is off.
pub fn containers(home: &Path) -> Vec<Container> {
    if std::env::var_os("EXACT_AGENT_HOME").is_some() {
        return std::fs::read_to_string(home.join(".docker")).map(|t| parse_fixture(&t)).unwrap_or_default();
    }
    let Some(socket) = socket(home) else { return Vec::new() };
    match request(&socket, "GET", "/containers/json") {
        Ok((200, body)) => parse_containers(&body),
        _ => Vec::new(),
    }
}

fn parse_fixture(text: &str) -> Vec<Container> {
    text.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            let [id, name, image, port, project] = f[..] else { return None };
            Some(Container { id: id.into(), name: name.into(), image: image.into(), project: project.into(), ports: vec![port.parse().ok()?] })
        })
        .collect()
}

/// Stop a container (Docker's own ten seconds' grace), or kill it at once.
pub fn stop(home: &Path, id: &str, force: bool) -> Result<(), String> {
    if !id.bytes().all(|b| b.is_ascii_alphanumeric()) || id.is_empty() {
        return Err("That isn’t a container.".into());
    }
    if std::env::var_os("EXACT_AGENT_HOME").is_some() {
        return Ok(());
    }
    let socket = socket(home).ok_or("Docker isn’t running.")?;
    let path = if force { format!("/containers/{id}/kill") } else { format!("/containers/{id}/stop?t=10") };
    // A stop waits for the container: give it Docker's grace and a little more.
    match request(&socket, "POST", &path)? {
        // 304: already stopped.
        (204 | 304, _) => Ok(()),
        (404, _) => Err("That container is gone.".into()),
        (status, body) => Err(message(&body).unwrap_or_else(|| format!("Docker refused (status {status})."))),
    }
}

/// The Engine's error message: `{"message": "…"}`.
fn message(body: &str) -> Option<String> {
    serde_json::from_str::<Value>(body).ok()?["message"].as_str().map(str::to_owned)
}

/// What `docker system prune` frees, from `GET /system/df`: stopped containers' writable
/// layers, dangling images no container uses, and build cache not in use.
pub fn parse_reclaimable(json: &str) -> u64 {
    let Ok(v) = serde_json::from_str::<Value>(json) else { return 0 };
    let stopped: u64 = v["Containers"].as_array().into_iter().flatten().filter(|c| c["State"].as_str() != Some("running")).filter_map(|c| c["SizeRw"].as_u64()).sum();
    let dangling: u64 = v["Images"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|i| i["Containers"].as_i64().unwrap_or(0) <= 0)
        .filter(|i| i["RepoTags"].as_array().is_none_or(|t| t.is_empty() || t.iter().all(|t| t.as_str() == Some("<none>:<none>"))))
        .map(|i| i["Size"].as_u64().unwrap_or(0).saturating_sub(i["SharedSize"].as_u64().unwrap_or(0)))
        .sum();
    let cache: u64 = v["BuildCache"].as_array().into_iter().flatten().filter(|b| b["InUse"].as_bool() != Some(true) && b["Shared"].as_bool() != Some(true)).filter_map(|b| b["Size"].as_u64()).sum();
    stopped + dangling + cache
}

pub fn reclaimable(socket: &Path) -> Result<u64, String> {
    match request(socket, "GET", "/system/df")? {
        (200, body) => Ok(parse_reclaimable(&body)),
        (status, body) => Err(message(&body).unwrap_or_else(|| format!("Docker refused (status {status})."))),
    }
}

/// `docker system prune`: stopped containers, unused networks, dangling images and unused build
/// cache. The bytes Docker says it freed.
pub fn prune(socket: &Path) -> Result<u64, String> {
    let mut freed = 0;
    for path in ["/containers/prune", "/networks/prune", "/images/prune", "/build/prune"] {
        let (status, body) = request(socket, "POST", path)?;
        if status != 200 {
            return Err(message(&body).unwrap_or_else(|| format!("Docker refused (status {status}).")));
        }
        let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        freed += v["SpaceReclaimed"].as_u64().unwrap_or(0);
    }
    Ok(freed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_plain_and_a_chunked_reply() {
        let plain = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n[]";
        assert_eq!(parse_response(plain), Some((200, "[]".into())));
        let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n[{}]\r\n3\r\n,{}\r\n0\r\n\r\n";
        assert_eq!(parse_response(chunked), Some((200, "[{}],{}".into())));
        assert_eq!(parse_response(b"HTTP/1.1 204 No Content\r\n\r\n"), Some((204, String::new())));
        assert_eq!(parse_response(b"garbage"), None);
    }

    #[test]
    fn names_each_container_by_its_published_ports() {
        let json = r#"[
          {"Id":"abc123","Names":["/shop-db-1"],"Image":"postgres:16","Labels":{"com.docker.compose.project":"shop"},
           "Ports":[{"IP":"0.0.0.0","PrivatePort":5432,"PublicPort":5433,"Type":"tcp"},{"IP":"::","PrivatePort":5432,"PublicPort":5433,"Type":"tcp"}]},
          {"Id":"def456","Names":["/cache"],"Image":"redis","Labels":{},"Ports":[{"PrivatePort":6379,"Type":"tcp"}]}
        ]"#;
        let c = parse_containers(json);
        assert_eq!(c[0], Container { id: "abc123".into(), name: "shop-db-1".into(), image: "postgres:16".into(), project: "shop".into(), ports: vec![5433] });
        assert!(c[1].ports.is_empty());
        assert!(parse_containers("not json").is_empty());
    }

    #[test]
    fn counts_what_a_prune_frees() {
        let json = r#"{
          "Containers":[{"State":"exited","SizeRw":100},{"State":"running","SizeRw":5000}],
          "Images":[{"RepoTags":["<none>:<none>"],"Containers":0,"Size":1000,"SharedSize":200},
                    {"RepoTags":["node:20"],"Containers":0,"Size":9000,"SharedSize":0},
                    {"RepoTags":[],"Containers":1,"Size":7000,"SharedSize":0}],
          "BuildCache":[{"InUse":false,"Shared":false,"Size":30},{"InUse":true,"Size":400}]
        }"#;
        assert_eq!(parse_reclaimable(json), 100 + 800 + 30);
    }

    #[test]
    fn the_agent_never_reaches_the_systems_socket() {
        assert!(!sockets(Path::new("/h"), true).contains(&PathBuf::from("/var/run/docker.sock")));
        assert!(sockets(Path::new("/h"), false).contains(&PathBuf::from("/var/run/docker.sock")));
    }

    #[test]
    fn refuses_an_id_that_isnt_one() {
        assert!(stop(Path::new("/nowhere"), "../../x", false).is_err());
        assert!(stop(Path::new("/nowhere"), "", false).is_err());
    }
}
