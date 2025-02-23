use std::sync::LazyLock;

use axum::{Router, extract::State, response::Html, routing::get};
use redis;
use redis_rate::Limiter;

static KNOCK_LIMIT: LazyLock<redis_rate::Limit> =
    LazyLock::new(|| redis_rate::new_limit!(1, 1, 10));

const KNOCK_LIMIT_KEY: &str = "knock";

#[tokio::main]
async fn main() {
    let redis_client = redis::Client::open("redis://127.0.0.1/").unwrap();
    let limiter = Limiter::new(redis_client);

    #[cfg(feature = "local_accelerate")]
    {
        let limiter_clone = limiter.clone();
        tokio::spawn(async move {
            while let Err(e) = limiter_clone.start_event_sync() {
                eprintln!("Error: {}", e);
            }
        });
    }

    // build our application with a route
    let app = Router::new()
        .route("/", get(knock))
        .route("/reset", get(reset))
        .with_state(limiter);

    // run it
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000")
        .await
        .unwrap();
    println!("listening on {}", listener.local_addr().unwrap());
    axum::serve(listener, app).await.unwrap();
}

async fn knock(State(limiter): State<Limiter>) -> Html<&'static str> {
    let result = limiter.allow(KNOCK_LIMIT_KEY, &KNOCK_LIMIT);
    match result {
        Ok(result) => {
            if !result.limited {
                let response = format!(
                    "<h1>Who's there? Remaining {} requests.</h1>",
                    result.remaining
                );
                println!("Effective knock request");
                Html(Box::leak(response.into_boxed_str()))
            } else {
                let response = format!(
                    "<h1>Too many requests. Try again in {} seconds.</h1>",
                    result.retry_after.unwrap_or_default().as_secs()
                );
                Html(Box::leak(response.into_boxed_str()))
            }
        }
        Err(_) => Html("<h1>Internal server error</h1>"),
    }
}

async fn reset(State(limiter): State<Limiter>) -> Html<&'static str> {
    match limiter.reset(KNOCK_LIMIT_KEY) {
        Ok(_) => {}
        Err(_) => return Html("<h1>Fails to reset remote</h1>"),
    }
    Html("<h1>Cache reset</h1>")
}
