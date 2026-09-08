//! Query a map with optional friends, then send gameplay directly through the director.
use serde_json::json;
use std::io::{Read, Write};
use std::net::{TcpStream, UdpSocket};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut query = TcpStream::connect("127.0.0.1:9000")?;
    let request = json!({
        "type": "query", "map": "tutorial", "characterId": "me",
        "friendIds": ["friend-1"]
    });
    writeln!(query, "{request}")?;
    let mut response = String::new();
    query.read_to_string(&mut response)?;
    let response: serde_json::Value = serde_json::from_str(&response)?;
    if let Some(error) = response.get("error") {
        return Err(error.to_string().into());
    }
    println!("Route ready: {response}");
    let gameplay = UdpSocket::bind("0.0.0.0:0")?;
    gameplay.send_to(b"gameplay", "127.0.0.1:7777")?;
    Ok(())
}
