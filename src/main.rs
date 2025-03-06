use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, Mutex};
use tokio::time::{self, Duration};
use tokio_tungstenite::tungstenite::protocol::Message;
use uuid::Uuid;

// Game state types
type ClientId = String;
type Clients = Arc<Mutex<HashMap<ClientId, mpsc::Sender<String>>>>;
type Players = Arc<Mutex<HashMap<ClientId, Player>>>;
type Bullets = Arc<Mutex<Vec<Bullet>>>;

// Constants
const SERVER_ADDR: &str = "127.0.0.1:30000";
const GAME_TICK_MS: u64 = 10;
const WORLD_WIDTH: f32 = 800.0;
const WORLD_HEIGHT: f32 = 600.0;
const INITIAL_PLAYER_X: f32 = 400.0;
const INITIAL_PLAYER_Y: f32 = 300.0;

// Game entities
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Player {
    x: f32,
    y: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Bullet {
    x: f32,
    y: f32,
    dx: f32,
    dy: f32,
    size: f32,
}

// Message types
#[derive(Debug, Serialize, Deserialize)]
enum GameCommand {
    Move { x: f32, y: f32 },
    Shoot { x: f32, y: f32, dx: f32, dy: f32, size: f32 },
}

#[derive(Debug)]
enum GameEvent {
    PlayerInput { id: ClientId, command: GameCommand },
    PlayerDisconnected { id: ClientId },
}

#[derive(Serialize)]
struct InitMessage {
    id: ClientId,
    x: f32,
    y: f32,
}

#[derive(Serialize)]
struct GameState {
    players: HashMap<ClientId, PlayerState>,
    bullets: Vec<Bullet>,
}

#[derive(Serialize)]
struct PlayerState {
    x: f32,
    y: f32,
}

#[derive(Serialize)]
#[serde(tag = "type")]
enum ClientMessage {
    Init(InitMessage),
    State(GameState),
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Starting game server...");

    // Initialize game state
    let players: Players = Arc::new(Mutex::new(HashMap::new()));
    let clients: Clients = Arc::new(Mutex::new(HashMap::new()));
    let bullets: Bullets = Arc::new(Mutex::new(Vec::new()));
    
    // Create game event channel
    let (tx, rx) = mpsc::channel::<GameEvent>(100);

    // Start game loop
    tokio::spawn(game_loop(
        players.clone(),
        clients.clone(),
        bullets.clone(),
        rx,
    ));

    // Start server
    let listener = TcpListener::bind(SERVER_ADDR).await?;
    println!("Server running on ws://{}", SERVER_ADDR);

    // Accept client connections
    while let Ok((stream, addr)) = listener.accept().await {
        println!("New connection from: {}", addr);
        
        let players = players.clone();
        let clients = clients.clone();
        let bullets = bullets.clone();
        let tx = tx.clone();

        tokio::spawn(async move {
            match tokio_tungstenite::accept_async(stream).await {
                Ok(ws_stream) => {
                    handle_connection(ws_stream, players, clients, bullets, tx).await;
                },
                Err(e) => eprintln!("Error during WebSocket handshake: {}", e),
            }
        });
    }

    Ok(())
}

async fn handle_connection(
    ws_stream: tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    players: Players,
    clients: Clients,
    bullets: Bullets,
    tx: mpsc::Sender<GameEvent>,
) {
    // Generate unique client ID
    let client_id = Uuid::new_v4().to_string();
    println!("Player connected: {}", client_id);

    // Create new player
    let new_player = Player {
        x: INITIAL_PLAYER_X,
        y: INITIAL_PLAYER_Y,
    };
    
    // Add player to game state
    players.lock().await.insert(client_id.clone(), new_player.clone());
    
    // Set up client message channel
    let (client_tx, mut client_rx) = mpsc::channel::<String>(10);
    
    // Send initial state to client
    let init_message = ClientMessage::Init(InitMessage {
        id: client_id.clone(),
        x: new_player.x,
        y: new_player.y,
    });
    
    if let Ok(json) = serde_json::to_string(&init_message) {
        let _ = client_tx.send(json).await;
    }
    
    // Add client channel to clients map
    clients.lock().await.insert(client_id.clone(), client_tx);

    // Split WebSocket stream
    let (mut ws_sink, mut ws_stream) = ws_stream.split();

    // Forward messages from client_rx to WebSocket
    let client_id_for_send = client_id.clone();
    let tx_for_send = tx.clone();
    let forward_task = tokio::spawn(async move {
        while let Some(msg) = client_rx.recv().await {
            if ws_sink.send(Message::Text(msg)).await.is_err() {
                // Report disconnection if we can't send
                let _ = tx_for_send.send(GameEvent::PlayerDisconnected {
                    id: client_id_for_send.clone(),
                }).await;
                break;
            }
        }
    });

    // Process incoming WebSocket messages
    while let Some(Ok(msg)) = ws_stream.next().await {
        if let Message::Text(text) = msg {
            process_client_message(&client_id, &text, &tx, &bullets).await;
        }
    }

    // Client disconnected, clean up
    forward_task.abort();
    
    // Notify game loop about disconnection
    let _ = tx.send(GameEvent::PlayerDisconnected {
        id: client_id.clone(),
    }).await;
    
    println!("Player disconnected: {}", client_id);
}

async fn process_client_message(
    client_id: &str,
    message: &str,
    tx: &mpsc::Sender<GameEvent>,
    bullets: &Bullets,
) {
    let parts: Vec<&str> = message.split_whitespace().collect();
    
    if parts.is_empty() {
        return;
    }
    
    match parts[0] {
        "move" => {
            if parts.len() == 3 {
                if let (Ok(x), Ok(y)) = (parts[1].parse::<f32>(), parts[2].parse::<f32>()) {
                    let _ = tx.send(GameEvent::PlayerInput {
                        id: client_id.to_string(),
                        command: GameCommand::Move { x, y },
                    }).await;
                }
            }
        },
        "shoot" => {
            if parts.len() == 6 {
                if let (Ok(x), Ok(y), Ok(dx), Ok(dy), Ok(size)) = (
                    parts[1].parse::<f32>(),
                    parts[2].parse::<f32>(),
                    parts[3].parse::<f32>(),
                    parts[4].parse::<f32>(),
                    parts[5].parse::<f32>(),
                ) {
                    // Create bullet directly
                    let mut bullets = bullets.lock().await;
                    bullets.push(Bullet { x, y, dx, dy, size });
                }
            }
        },
        _ => {
            // Ignore unknown commands
        }
    }
}

async fn game_loop(
    players: Players,
    clients: Clients,
    bullets: Bullets,
    mut rx: mpsc::Receiver<GameEvent>,
) {
    let mut interval = time::interval(Duration::from_millis(GAME_TICK_MS));

    loop {
        tokio::select! {
            // Process game events
            Some(event) = rx.recv() => {
                match event {
                    GameEvent::PlayerInput { id, command } => {
                        handle_player_input(&id, command, &players).await;
                    },
                    GameEvent::PlayerDisconnected { id } => {
                        players.lock().await.remove(&id);
                        clients.lock().await.remove(&id);
                    }
                }
            }
            
            // Update game state and broadcast to clients
            _ = interval.tick() => {
                update_game_state(&bullets).await;
                broadcast_game_state(&players, &clients, &bullets).await;
            }
        }
    }
}

async fn handle_player_input(
    id: &str,
    command: GameCommand,
    players: &Players,
) {
    let mut players = players.lock().await;
    
    if let Some(player) = players.get_mut(id) {
        match command {
            GameCommand::Move { x, y } => {
                player.x = x;
                player.y = y;
            },
            GameCommand::Shoot { .. } => {
                // Shooting is handled directly in process_client_message
            }
        }
    }
}

async fn update_game_state(bullets: &Bullets) {
    let mut bullets = bullets.lock().await;
    
    // Update bullet positions
    bullets.iter_mut().for_each(|bullet| {
        bullet.x += bullet.dx;
        bullet.y += bullet.dy;
    });
    
    // Remove bullets that are out of bounds
    bullets.retain(|bullet| {
        bullet.x >= 0.0 && 
        bullet.x <= WORLD_WIDTH && 
        bullet.y >= 0.0 && 
        bullet.y <= WORLD_HEIGHT
    });
}

async fn broadcast_game_state(
    players: &Players,
    clients: &Clients,
    bullets: &Bullets,
) {
    let players_lock = players.lock().await;
    let clients_lock = clients.lock().await;
    
    if clients_lock.is_empty() {
        return;
    }
    
    // Prepare player states
    let player_states: HashMap<ClientId, PlayerState> = players_lock
        .iter()
        .map(|(id, player)| {
            (
                id.clone(),
                PlayerState {
                    x: player.x,
                    y: player.y,
                },
            )
        })
        .collect();
    
    // Prepare bullet states
    let bullets_lock = bullets.lock().await;
    let bullet_states = bullets_lock.clone();
    
    // Create game state message
    let game_state = ClientMessage::State(GameState {
        players: player_states,
        bullets: bullet_states,
    });
    
    // Serialize game state
    if let Ok(state_json) = serde_json::to_string(&game_state) {
        // Broadcast to all clients
        for tx in clients_lock.values() {
            let _ = tx.send(state_json.clone()).await;
        }
    }
}