use tokio_tungstenite::tungstenite::protocol::Message;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, Mutex};
use futures::{StreamExt, SinkExt};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;
use serde_json::json;
use tokio::time::{self, Duration};

#[derive(Clone, Debug)]
struct Player {
    x: f32,
    y: f32,
}

#[derive(Debug)]
enum PlayerInput {
    Move { id: String, x: f32, y: f32 },
}

type Players = Arc<Mutex<HashMap<String, Player>>>;
type Clients = Arc<Mutex<HashMap<String, mpsc::Sender<String>>>>;

#[tokio::main]
async fn main() {
    let players: Players = Arc::new(Mutex::new(HashMap::new()));
    let clients: Clients = Arc::new(Mutex::new(HashMap::new()));
    let (tx, rx) = mpsc::channel::<PlayerInput>(100);

    // 게임 루프 실행
    let players_clone = players.clone();
    let clients_clone = clients.clone();
    tokio::spawn(game_loop(players_clone, clients_clone, rx));

    let listener = TcpListener::bind("127.0.0.1:30000").await.unwrap();
    println!("Server running on ws://127.0.0.1:30000");

    while let Ok((stream, _)) = listener.accept().await {
        let players = players.clone();
        let clients = clients.clone();
        let tx = tx.clone();

        tokio::spawn(async move {
            let ws_stream = match tokio_tungstenite::accept_async(stream).await {
                Ok(ws) => ws,
                Err(_) => return,
            };
            handle_connection(ws_stream, players, clients, tx).await;
        });
    }
}

async fn handle_connection(
    ws_stream: tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    players: Players,
    clients: Clients,
    tx: mpsc::Sender<PlayerInput>,
) {
    let id = Uuid::new_v4().to_string();
    println!("Player connected: {}", id);

    let new_player = Player { x: 400.0, y: 300.0 };
    let player_x = new_player.x;
    let player_y = new_player.y;
    players.lock().await.insert(id.clone(), new_player);
    
    // 클라이언트 송신 채널 생성
    let (client_tx, mut client_rx) = mpsc::channel::<String>(10);
    client_tx.send(json!({"init": { "id": id, "x": player_x, "y": player_y }}).to_string()).await.unwrap();
    clients.lock().await.insert(id.clone(), client_tx);

    let (mut ws_sink, mut ws_stream) = ws_stream.split();

    // 클라이언트 송신 핸들러
    tokio::spawn(async move {
        while let Some(msg) = client_rx.recv().await {
            let _ = ws_sink.send(Message::Text(msg)).await;
        }
    });

    // 메시지 수신 처리
    while let Some(Ok(msg)) = ws_stream.next().await {
        if let Message::Text(text) = msg {
            let parts: Vec<&str> = text.split_whitespace().collect();
            if parts.len() == 3 && parts[0] == "move" {
                if let (Ok(x), Ok(y)) = (parts[1].parse::<f32>(), parts[2].parse::<f32>()) {
                    let _ = tx.send(PlayerInput::Move { id: id.clone(), x, y }).await;
                }
            }
        }
    }

    // 연결 종료 처리
    players.lock().await.remove(&id);
    clients.lock().await.remove(&id);
}

async fn game_loop(players: Players, clients: Clients, mut rx: mpsc::Receiver<PlayerInput>) {
    let mut interval = time::interval(Duration::from_millis(10));

    loop {
        tokio::select! {
            // 입력 메시지 처리
            Some(input) = rx.recv() => {
                match input {
                    PlayerInput::Move { id, x, y } => {
                        let mut players = players.lock().await;
                        if let Some(player) = players.get_mut(&id) {
                            player.x = x;
                            player.y = y;
                        }
                    }
                }
            }
            // 주기적으로 모든 플레이어 상태 전송
            _ = interval.tick() => {
                let players = players.lock().await;
                let clients = clients.lock().await;
                
                let player_states: HashMap<String, HashMap<String, f32>> = players.iter()
                    .map(|(id, player)| {
                        (
                            id.clone(),
                            HashMap::from([("x".to_string(), player.x), ("y".to_string(), player.y)]),
                        )
                    })
                    .collect();
            
                let state_json = json!({ "players": player_states }).to_string();
                println!("{}", state_json);
            
                for tx in clients.values() {
                    let _ = tx.send(state_json.clone()).await;
                }
            }
        }
    }
}