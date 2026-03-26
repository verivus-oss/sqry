// Test fixture: Async functions with attributes
// Tests: async_test -> helper (is_async=true)

#[tokio::test]
async fn async_test() {
    helper().await;
    helper2();
}

async fn helper() {}

fn helper2() {}

