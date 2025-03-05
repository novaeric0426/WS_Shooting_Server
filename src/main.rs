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

#[derive(Clone, Debug)]
struct Bullet {
    x: f32,
    y: f32,
    dx: f32,
    dy: f32,
    size: f32,
}

#[derive(Debug)]
enum PlayerInput {
    Move { id: String, x: f32, y: f32 },
}

type Players = Arc<Mutex<HashMap<String, Player>>>;
type Clients = Arc<Mutex<HashMap<String, mpsc::Sender<String>>>>;
type Bullets = Arc<Mutex<Vec<Bullet>>>;

#[tokio::main]
async fn main() {
    let players: Players = Arc::new(Mutex::new(HashMap::new()));
    let clients: Clients = Arc::new(Mutex::new(HashMap::new()));
    let bullets: Bullets = Arc::new(Mutex::new(Vec::new())); // 총알 상태 관리

    let (tx, rx) = mpsc::channel::<PlayerInput>(100);

    // 게임 루프 실행
    let players_clone = players.clone();
    let clients_clone = clients.clone();
    let bullets_clone = bullets.clone();
    tokio::spawn(game_loop(players_clone, clients_clone, rx, bullets_clone));

    let listener = TcpListener::bind("127.0.0.1:30000").await.unwrap();
    println!("Server running on ws://127.0.0.1:30000");

    while let Ok((stream, _)) = listener.accept().await {
        let players = players.clone();
        let clients = clients.clone();
        let tx = tx.clone();
        let bullets = bullets.clone();

        tokio::spawn(async move {
            let ws_stream = match tokio_tungstenite::accept_async(stream).await {
                Ok(ws) => ws,
                Err(_) => return,
            };
            handle_connection(ws_stream, players, clients, tx, bullets).await;
        });
    }
}

async fn handle_connection(
    ws_stream: tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    players: Players,
    clients: Clients,
    tx: mpsc::Sender<PlayerInput>,
    bullets: Bullets,
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
            } else if parts.len() == 6 && parts[0] == "shoot" {
                if let (Ok(x), Ok(y), Ok(dx), Ok(dy), Ok(size)) = (parts[1].parse::<f32>(), parts[2].parse::<f32>(), parts[3].parse::<f32>(), parts[4].parse::<f32>(), parts[5].parse::<f32>()) {
                    let mut bullets = bullets.lock().await;
                    let bullet = Bullet { x, y, dx, dy, size, };
                    bullets.push(bullet);
                }

            }
        }
    }

    // 연결 종료 처리
    players.lock().await.remove(&id);
    clients.lock().await.remove(&id);
}

async fn game_loop(players: Players, clients: Clients, mut rx: mpsc::Receiver<PlayerInput>, bullets: Bullets) {
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
                // 총알 위치 업데이트
                let mut bullets = bullets.lock().await;
                bullets.iter_mut().for_each(|bullet| {
                    bullet.x += bullet.dx;
                    bullet.y += bullet.dy;
                });

                // 화면 밖으로 나간 총알 제거
                bullets.retain(|bullet| bullet.x >= 0.0 && bullet.x <= 800.0 && bullet.y >= 0.0 && bullet.y <= 600.0);

                // 플레이어 상태와 총알 정보를 클라이언트에 전송
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

                let bullets_json = bullets.iter().map(|bullet| {
                    json!({
                        "x": bullet.x,
                        "y": bullet.y,
                        "dx": bullet.dx,
                        "dy": bullet.dy,
                        "size": bullet.size,
                    })
                }).collect::<Vec<_>>();

                let state_json = json!({
                    "players": player_states,
                    "bullets": bullets_json,
                }).to_string();

                println!("{}", state_json);

                for tx in clients.values() {
                    let _ = tx.send(state_json.clone()).await;
                }
            }
        }
    }
}