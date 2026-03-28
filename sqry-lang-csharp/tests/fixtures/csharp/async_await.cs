// Async/await patterns

using System.Threading.Tasks;

namespace Demo.Async {
    public class AsyncService {
        public async Task<string> FetchDataAsync() {
            await Task.Delay(100);
            return "data";
        }

        public async Task<int> CalculateAsync(int x) {
            var data = await FetchDataAsync();
            var result = await ProcessAsync(data);
            return result;
        }

        public async Task<int> ProcessAsync(string input) {
            await Task.Delay(50);
            return input.Length;
        }

        // Non-async method calling async methods
        public string FetchSync() {
            var task = FetchDataAsync();
            return task.Result;
        }
    }
}
