use std::{net::SocketAddr, sync::Arc, time::Duration};

use axum::{
    body::Body,
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
    Router,
};
use bb8::Pool;
use bb8_redis::{redis::AsyncCommands, RedisConnectionManager};
use eyre::ContextCompat;
use figment::{
    providers::{Env, Format, Toml},
    Figment,
};
use futures::StreamExt;
use moka::future::Cache;
use serde::{Deserialize, Serialize};
use tokio::{select, sync::oneshot};

#[tokio::main]
async fn main() -> eyre::Result<()> {
    color_eyre::install()?;

    let config: Config = Figment::new()
        .merge(Toml::file("shim.toml"))
        .merge(Env::prefixed("SHIM_"))
        .extract()?;

    let manager = bb8_redis::RedisConnectionManager::new(config.database_url)?;
    let pool = bb8::Pool::builder().build(manager).await?;

    let cache = Cache::<String, CacheEntry>::builder()
        .time_to_idle(Duration::from_secs(60 * 60))
        .weigher(|_, v| match v {
            CacheEntry::Empty => 0,
            CacheEntry::Asset(v) => (v.0.len() + v.1.len()) as u32,
            CacheEntry::Card(v) => std::mem::size_of_val(v) as u32,
        })
        .build();

    let mut invalidations = pool.dedicated_connection().await?.into_pubsub();
    invalidations.subscribe("invalidations").await?;
    let (invalidations_kill_tx, mut invalidations_kill_rx) = oneshot::channel();
    let invalidations_task = tokio::spawn((|| {
        let cache = cache.clone();
        async move {
            let mut stream = invalidations.into_on_message();
            while let Some(item) = select! {
                v = stream.next() => v,
                _ = &mut invalidations_kill_rx => None,
            } {
                cache
                    .invalidate(&String::from_utf8_lossy(item.get_payload_bytes()).to_string())
                    .await;
            }
        }
    })());

    let app = Router::new().fallback(move |r| handle(r, pool.clone(), cache.clone()));

    let (server_kill_tx, server_kill_rx) = oneshot::channel();
    let server = axum::Server::bind(&config.listen_on)
        .serve(app.into_make_service())
        .with_graceful_shutdown(async move {
            let _ = server_kill_rx.await;
        });

    let (server_shutdown_tx, server_shutdown_rx) = oneshot::channel();
    tokio::spawn(async move {
        if let Err(err) = server.await {
            println!("server error: {err:?}");
        }
        let _ = server_shutdown_tx.send(());
    });

    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        let _ = invalidations_kill_tx.send(());
        let _ = server_kill_tx.send(());
    });

    invalidations_task.await?;
    let _ = server_shutdown_rx.await;

    Ok(())
}

async fn handle(
    request: Request<Body>,
    pool: Pool<RedisConnectionManager>,
    cache: Cache<String, CacheEntry>,
) -> Result<impl IntoResponse, impl IntoResponse> {
    handle_inner(request, pool, cache).await.map_err(|err| {
        println!("handler error: {err:?}");
        let dbg = format!("{err:?}");
        let inner = ansi_to_html::convert(&dbg, true, true)
            .unwrap_or(dbg)
            .trim()
            .replace('\n', "<br>");
        Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .header("Content-Type", "text/html")
            .body(format!(
                "<!doctype html><h1>500 Internal Server Exception</h1><code>{inner}</code>"
            ))
            .unwrap()
    })
}

async fn handle_inner(
    request: Request<Body>,
    pool: Pool<RedisConnectionManager>,
    cache: Cache<String, CacheEntry>,
) -> eyre::Result<impl IntoResponse> {
    let path = request.uri().path().trim_matches('/');

    let (entry, cache_status) = match cache.get(path) {
        Some(v) => (v, "hit"),
        None => {
            let mut redis = pool.get().await?;

            let asset = redis.get::<_, Option<Vec<u8>>>(format!("asset:{path}")).await?;
            let entry = match asset {
                Some(v) => {
                    let mut iter = v.splitn(2, |x| *x == b';');
                    let mime = iter.next().wrap_err("asset iterator exhausted before first split")?;
                    let body = iter.next().wrap_err("asset iterator exhausted before body")?;
                    CacheEntry::Asset((String::from_utf8_lossy(mime).to_string(), body.into()))
                }
                None => {
                    let card = redis.get::<_, Option<String>>(format!("card:{path}")).await?;
                    match card {
                        Some(s) => CacheEntry::Card(Arc::new(serde_json::from_str(&s)?)),
                        None => CacheEntry::Empty,
                    }
                }
            };

            cache.insert(path.to_string(), entry.clone()).await;
            (entry, "miss")
        }
    };

    Ok(match entry {
        CacheEntry::Empty => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header("X-Cache-Status", cache_status)
            .body(Body::from("not found"))?,
        CacheEntry::Asset((mime, body)) => Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", mime)
            .header("X-Cache-Status", cache_status)
            .body(Body::from(body))?,
        CacheEntry::Card(_) => todo!(),
    })
}

#[derive(Deserialize)]
struct Config {
    pub database_url: String,
    pub listen_on: SocketAddr,
}

#[derive(Clone)]
enum CacheEntry {
    Empty,
    Asset((String, Vec<u8>)),
    Card(Arc<Card>),
}

#[derive(Serialize, Deserialize, Clone)]
struct Card {
    pub name: String,
    pub redirect: String,
    pub icon: String,
}
