// Rust async function test fixture

async fn fetch_data() -> String {
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    String::from("data")
}

fn sync_function() -> String {
    String::from("sync")
}
