#![allow(dead_code)]
#![allow(unused_variables)]
#![allow(unused_imports)]

use anyhow::Result;
use axum::{
    body::Body,
    extract::{Extension, Query, State},
    http::{HeaderValue, Method, Response, StatusCode},
    response::IntoResponse,
    routing::get,
    routing::post,
    Json, Router,
};
use mongodb::bson::doc;
use mongodb::options::FindOptions;
use mongodb::{options::ClientOptions, Client, Collection, Database};
use petgraph::dot::{Config, Dot};
use petgraph::graph::{EdgeIndex, Graph, NodeIndex};
use petgraph::graphmap::GraphMap;
use petgraph::Directed;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_stream::StreamExt;
use tower::ServiceBuilder;
use tower_http::cors::{Any, CorsLayer};
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
    pub page: Option<u64>,
    pub size: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrafficResults {
    pub method: Option<String>,
    pub host: Option<String>,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphResponse {
    pub nodes: Vec<ResponseNode>,
    pub links: Vec<ResponseLink>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseNode {
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseLink {
    pub source: String,
    pub target: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNode {
    pub weight: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphEdge {}

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
    let shared_state = Arc::new(AppState {
        db: Arc::new(Mutex::new(db)),
    });

    let cors = CorsLayer::new()
        .allow_methods([Method::GET, Method::POST])
        .allow_origin("http://localhost:3001".parse::<HeaderValue>().unwrap());

    let app = Router::new()
        .route("/healthcheck", get(handle_db_healthcheck))
        .route("/traffic/graph", get(handle_traffic_graph))
        .route("/traffic/records", get(handle_traffic_records))
        .layer(ServiceBuilder::new().layer(cors))
        .with_state(shared_state);

    axum::Server::bind(&"0.0.0.0:3000".parse().unwrap())
        .serve(app.into_make_service())
        .await
        .unwrap();

    Ok(())
}

async fn handle_db_healthcheck(State(app_state): State<Arc<AppState>>) -> impl IntoResponse {
    match app_state.db.lock().await.list_collection_names(None).await {
        Ok(_) => (StatusCode::OK, "Database is healthy"),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "Database is down"),
    }
}

async fn handle_traffic_graph(
    Query(query): Query<TrafficParams>,
    State(app_state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, impl IntoResponse> {
    let collection: Collection<TrafficResults> = app_state.db.lock().await.collection("traffic");
    let filter = doc! {
        "host": {"$regex": &query.host, "$options": "i"},

    };
    let options = FindOptions::builder()
        .projection(Some(doc! { "method": 1, "host": 1, "path": 1, "_id": 0 }))
        .limit(Some(100))
        .build();
    let data = collection.find(filter, Some(options)).await;
    let mut results = vec![];
    match data {
        Ok(mut cursor) => {
            while let Some(document) = cursor.next().await {
                if let Ok(doc) = document {
                    results.push(doc)
                }
            }
            if !results.is_empty() {
                let (graph, nodes, edges) = traffic_graph_builder(results.clone()).await;
                let response = traffic_graph_response(graph, nodes, edges).await;
                Ok(Json(response))
            } else {
                let error_response = ErrorResponse {
                    message: "No matching document found.".to_string(),
                };
                Err((StatusCode::NOT_FOUND, Json(error_response)))
            }
        }
        Err(e) => {
            let error_response = ErrorResponse {
                message: e.to_string(),
            };
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(error_response)))
        }
    }
}

async fn handle_traffic_records(
    Query(query): Query<TrafficParams>,
    State(app_state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, impl IntoResponse> {
    let mut page_number: u64 = 0;
    if let Some(ref number) = &query.page {
        page_number = *number;
    }
    let mut page_size: u64 = 10;
    if let Some(ref sz) = &query.size {
        page_size = *sz
    }
    let filter = doc! {
        "host": {"$regex": &query.host, "$options": "i"},

    };
    let collection: Collection<TrafficResults> = app_state.db.lock().await.collection("traffic");
    let find_options = FindOptions::builder()
        .sort(doc! { "host": 1 })
        .projection(Some(doc! { "method": 1, "host": 1, "path": 1, "_id": 0 }))
        .skip(Some(page_number * page_size))
        .limit(Some(page_size as i64))
        .build();
    let data = collection.find(filter, Some(find_options)).await;
    match data {
        Ok(mut cursor) => {
            let mut results = vec![];
            while let Some(result) = cursor.next().await {
                match result {
                    Ok(document) => {
                        results.push(document);
                    }
                    Err(e) => {}
                }
            }
            Ok(Json(results))
        }
        Err(e) => {
            let error_response = ErrorResponse {
                message: e.to_string(),
            };
            Err((StatusCode::INTERNAL_SERVER_ERROR, Json(error_response)))
        }
    }
}

async fn traffic_graph_response(
    graph: Graph<GraphNode, GraphEdge, Directed>,
    nodes: HashMap<String, NodeIndex>,
    edges: HashMap<(String, String), EdgeIndex>,
) -> String {
    let mut response = GraphResponse {
        nodes: vec![],
        links: vec![],
    };

    for (id, node_index) in nodes {
        let node = graph.node_weight(node_index).unwrap();
        response.nodes.push(ResponseNode { id });
    }

    for ((source, target), edge_index) in edges {
        let edge = graph.edge_weight(edge_index).unwrap();
        response.links.push(ResponseLink {
            source: source.clone(),
            target: target.clone(),
        });
    }

    serde_json::to_string(&response).unwrap()
}

async fn traffic_graph_builder(
    results: Vec<TrafficResults>,
) -> (
    Graph<GraphNode, GraphEdge, Directed>,
    HashMap<String, NodeIndex>,
    HashMap<(String, String), EdgeIndex>,
) {
    let mut graph = Graph::<GraphNode, GraphEdge, Directed>::new();
    let mut nodes: HashMap<String, NodeIndex> = HashMap::new();
    let mut edges: HashMap<(String, String), EdgeIndex> = HashMap::new();

    for doc in results {
        if let Some(ref host) = doc.host.clone() {
            let host_elements: Vec<String> = host.split('.').map(|s| s.to_string()).collect();
            let len = host_elements.len();
            if len < 2 {
                // Todo -- error.
            }
            for i in (0..len - 1).rev() {
                let node_key = &host_elements[i..len].join(".");
                if nodes.contains_key(node_key) {
                    let node = nodes.get(node_key);
                } else {
                    let weight = GraphNode {
                        weight: node_key.clone(),
                    };
                    let node = graph.add_node(weight);
                    nodes.insert(node_key.clone(), node);
                }

                if i < len - 2 {
                    let parent = &host_elements[i + 1..len].join(".");
                    let edge_key = (parent.clone(), node_key.clone());
                    if edges.contains_key(&edge_key) {
                        let edge = edges.get(&edge_key);
                    } else {
                        let edge = graph.add_edge(nodes[parent], nodes[node_key], GraphEdge {});
                        edges.insert((parent.clone(), node_key.clone()), edge);
                    }
                }
            }
        }

        if let Some(ref path) = doc.path.clone() {
            let path_elements: Vec<String> = path.split('/').map(|s| s.to_string()).collect();
            let len = path_elements.len();
            let host = doc.host.clone().unwrap_or(String::new());
            for i in 0..len {
                let path_key = &format!("{}{}", host, &path_elements[..i+1].join("/"));
                if nodes.contains_key(path_key) {
                    let node = nodes.get(path_key);
                } else {
                    let weight = GraphNode {
                        weight: path_key.clone(),
                    };
                    let node = graph.add_node(weight);
                    nodes.insert(path_key.clone(), node);
                }
                if i == 0 {
                    if nodes.contains_key(&host) {
                        let edge_key = (host.clone(), path_key.clone());
                        if let std::collections::hash_map::Entry::Vacant(e) =
                            edges.entry(edge_key.clone())
                        {
                            let edge = graph.add_edge(nodes[&host], nodes[path_key], GraphEdge {});
                            e.insert(edge);
                        } else {
                            let edge = edges.get(&edge_key);
                        }
                    }
                } else {
                    let parent_key = &format!("{}{}", host, &path_elements[..i].join("/"));
                    let edge_key = (parent_key.clone(), path_key.clone());
                    if let std::collections::hash_map::Entry::Vacant(e) =
                        edges.entry(edge_key.clone())
                    {
                        if nodes.contains_key(&parent_key.to_string()) {
                            let edge = graph.add_edge(
                                nodes[&parent_key.clone()],
                                nodes[path_key],
                                GraphEdge {},
                            );
                            e.insert(edge);
                        } else {
                            let edge = edges.get(&edge_key);
                        }
                    }
                }
            }
        }

        if let Some(ref method) = doc.method.clone() {
            let host = doc.host.clone().unwrap_or(String::new());
            let path = doc.path.clone().unwrap_or(String::new());
            let method_key = format!("{} {}{}", method.clone(), host.clone(), path.clone());
            let parent_key = format!("{}{}", host.clone(), path.clone());
            let edge_key = (parent_key.clone(), method_key.clone());
            if nodes.contains_key(&method_key) {
                let node = nodes.get(&method_key);
            } else {
                let weight = GraphNode {
                    weight: method_key.clone(),
                };
                let node = graph.add_node(weight);
                nodes.insert(method_key.clone(), node);
            }
            if let std::collections::hash_map::Entry::Vacant(e) = edges.entry(edge_key.clone()) {
                let edge = graph.add_edge(nodes[&parent_key], nodes[&method_key], GraphEdge {});
                e.insert(edge);
            } else {
                let edge = edges.get(&edge_key);
            }
        }
    }

    (graph, nodes, edges)
}
