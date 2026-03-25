// Test fixture: Async functions with .await operator
// Tests: fetch_data, process_data with async/await

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

struct ReadyFuture<T>(Option<T>);

impl<T> Future for ReadyFuture<T> {
    type Output = T;

    fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Self::Output> {
        Poll::Ready(self.0.take().unwrap())
    }
}

async fn fetch_data(id: u32) -> String {
    ReadyFuture(Some(format!("Data-{}", id))).await
}

async fn validate_data(data: &str) -> bool {
    ReadyFuture(Some(!data.is_empty())).await
}

async fn process_data(id: u32) -> Result<String, &'static str> {
    let data = fetch_data(id).await;

    if validate_data(&data).await {
        Ok(data.to_uppercase())
    } else {
        Err("Invalid data")
    }
}

fn main() {
    // Simple executor for demonstration
    let future = process_data(42);
    // In a real application, you would use an async runtime like tokio
    println!("Async functions defined");
}
