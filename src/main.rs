#[allow(dead_code)]
#[allow(unused_variables)]
#[allow(unused_imports)]

use anyhow::Result;
use axum::{
    body::Body,
    extract::{State, Extension, Query},
    http::{Response, StatusCode},
    routing::get,
    routing::post,
    response::IntoResponse,
    Router,
    Json,
};
use tokio::sync::Mutex;
use serde_json::{Value, json};
use serde::{Serialize, Deserialize};
use std::sync::Arc;
use std::collections::HashMap;
use mongodb::{options::ClientOptions, options::AggregateOptions, Client, Database, Collection};
use mongodb::bson::doc;
use tokio_stream::StreamExt;
//use mongodb::bson::oid::ObjectId;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Traffic {
    pub method: String,
    pub scheme: String,
    pub host: String,
    pub path: String,
    pub query: String,
    pub request_headers: HashMap<String, String>,
    pub request_body: Vec<u8>,
    pub request_body_string: Option<String>,
    pub status: u16,
    pub response_headers: HashMap<String, String>,
    pub response_body: Vec<u8>,
    pub response_body_string: Option<String>,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrafficParams {
    pub method: Option<String>,
    pub host: Option<String>,
    pub path: Option<String>,
}

#[derive(Clone)]
struct AppState {
    db: Arc<Mutex<Database>>,
}

// For MongoDB errors
#[derive(Debug, Serialize)]
struct ErrorResponse {
    message: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client_options = ClientOptions::parse("mongodb://127.0.0.1:27017").await?;
    let client = Client::with_options(client_options)?;
    let db = client.database("ohm");
    let shared_state = Arc::new(AppState{
        db: Arc::new(Mutex::new(db))
    });

    let app = Router::new()
        .route("/", get(get_text))
        .route("/json", get(get_json))
        .route("/healthcheck", get(handle_db_healthcheck))
        .route("/db", get(handle_traffic))
        .with_state(shared_state);

    axum::Server::bind(&"0.0.0.0:3000".parse().unwrap())
        .serve(app.into_make_service())
        .await
        .unwrap();

    Ok(())
}

async fn get_text(
    State(state): State<Arc<AppState>>,
) -> &'static str {
    "Hello, World!"
}

async fn get_json(
    State(state): State<Arc<AppState>>,
) -> Json<Value> {
    Json(json!({ "data": 42 }))
}

async fn handle_db_healthcheck(
    State(app_state): State<Arc<AppState>>,
) -> impl IntoResponse {
    match app_state.db.lock().await.list_collection_names(None).await {
        Ok(_) => (StatusCode::OK, "Database is healthy"),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "Database is down"),
    }
}


async fn handle_traffic(
    Query(query): Query<TrafficParams>,
    State(app_state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, impl IntoResponse> {
    let collection : Collection<Traffic> = app_state.db.lock().await.collection("traffic");
    let filter = doc! {
        "host": {"$regex": &query.host, "$options": "i"},

    };
    let data = collection.find(filter, None).await;
    let mut results = vec![];
    match data {
        Ok(mut cursor) => {
            while let Some(document) = cursor.next().await {
                match document {
                    Ok(doc) => results.push(doc),
                    Err(_) => (),
                }
            }
            if results.len() > 0 {
                Ok(Json(results))
            }else{
                let error_response = ErrorResponse { message: "No matching document found.".to_string() };
                Err((StatusCode::NOT_FOUND, Json(error_response)))
            }
        },
        Err(e) => {
            let error_response = ErrorResponse { message: e.to_string() };
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(error_response)))
        }
    }
}
