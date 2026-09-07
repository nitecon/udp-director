/// Example client implementation for UDP Director
/// This demonstrates the three-phase flow: Query, Connect, Reset
///
/// Note: This is a simplified example for demonstration purposes.
/// Production clients should include proper error handling and retry logic.
use serde_json::json;
use std::io::{Read, Write};
use std::net::{TcpStream, UdpSocket};

const DIRECTOR_IP: &str = "127.0.0.1";
const QUERY_PORT: u16 = 9000;
const DATA_PORT: u16 = 7777;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("UDP Director Client Example");
    println!("============================\n");

    // Phase 1: Query for a backend
    println!("Phase 1: Querying for a game server...");
    let token_a = query_for_server("gameserver", "game-servers", Some("de_dust2"))?;
    println!("✓ Received token: {}\n", token_a);

    // Phase 2: Establish session
    println!("Phase 2: Establishing UDP session...");
    let socket = establish_session(&token_a)?;
    println!("✓ Session established\n");

    // Simulate gameplay
    println!("Simulating gameplay...");
    send_game_data(&socket, b"PLAYER_MOVE x:100 y:200")?;
    send_game_data(&socket, b"PLAYER_SHOOT target:enemy1")?;
    println!("✓ Game data sent\n");

    // Phase 3: Session reset (e.g., player wants to switch servers)
    println!("Phase 3: Switching to a different server...");
    let token_b = query_for_server("gameserver", "game-servers", Some("cs_office"))?;
    println!("✓ Received new token: {}", token_b);

    reset_session(&socket, &token_b)?;
    println!("✓ Session reset to new server\n");

    // Continue gameplay on new server
    println!("Continuing gameplay on new server...");
    send_game_data(&socket, b"PLAYER_MOVE x:50 y:75")?;
    println!("✓ Game data sent to new server\n");

    println!("Example completed successfully!");
    Ok(())
}

/// Phase 1: Query the Director for a matching backend
fn query_for_server(
    resource_type: &str,
    namespace: &str,
    map_name: Option<&str>,
) -> Result<String, Box<dyn std::error::Error>> {
    // Connect to query server
    let mut stream = TcpStream::connect((DIRECTOR_IP, QUERY_PORT))?;

    // Build query JSON
    let mut query = json!({
        "resourceType": resource_type,
        "namespace": namespace,
        "statusQuery": {
            "jsonPath": "status.state",
            "expectedValue": "Allocated"
        }
    });

    // Add label selector if map specified
    if let Some(map) = map_name {
        query["labelSelector"] = json!({
            "game.example.com/map": map
        });
    }

    // Send query
    let query_json = serde_json::to_string(&query)?;
    stream.write_all(query_json.as_bytes())?;
    stream.flush()?;

    // Read response
    let mut response = String::new();
    stream.read_to_string(&mut response)?;

    // Parse response
    let response_json: serde_json::Value = serde_json::from_str(&response)?;

    if let Some(token) = response_json.get("token") {
        Ok(token.as_str().unwrap().to_string())
    } else if let Some(error) = response_json.get("error") {
        Err(format!("Query error: {}", error).into())
    } else {
        Err("Invalid response from server".into())
    }
}

/// Phase 2: Establish a UDP session with the token
fn establish_session(token: &str) -> Result<UdpSocket, Box<dyn std::error::Error>> {
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.connect((DIRECTOR_IP, DATA_PORT))?;

    // Send token as first packet
    socket.send(token.as_bytes())?;

    Ok(socket)
}

/// Send game data over the established session
fn send_game_data(socket: &UdpSocket, data: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    socket.send(data)?;
    Ok(())
}

/// Phase 3: Reset the session to a new backend
fn reset_session(socket: &UdpSocket, new_token: &str) -> Result<(), Box<dyn std::error::Error>> {
    // Magic bytes: 0xFFFFFFFF + "RESET"
    let magic_bytes = hex::decode("FFFFFFFF5245534554")?;

    // Build control packet: [magic_bytes][new_token]
    let mut control_packet = magic_bytes;
    control_packet.extend_from_slice(new_token.as_bytes());

    // Send control packet
    socket.send(&control_packet)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_control_packet_format() {
        let magic_bytes = hex::decode("FFFFFFFF5245534554").unwrap();
        let token = "test-token-123";

        let mut control_packet = magic_bytes.clone();
        control_packet.extend_from_slice(token.as_bytes());

        // Verify packet starts with magic bytes
        assert!(control_packet.starts_with(&magic_bytes));

        // Verify token is appended
        let token_part = &control_packet[magic_bytes.len()..];
        assert_eq!(token_part, token.as_bytes());
    }
}
