// Swift async function test fixture
import Foundation

func fetchData() async -> String {
    try? await Task.sleep(nanoseconds: 1_000_000_000)
    return "data"
}

func syncFunction() -> String {
    return "sync"
}
