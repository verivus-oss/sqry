// Mixed C# features

using System.Threading.Tasks;

namespace Demo.Mixed {
    public class ComplexService {
        public string Status { get; set; }

        public ComplexService() {
            Status = "Initialized";
        }

        public async Task<int> ProcessAsync(string input) {
            Status = "Processing";
            var result = await CalculateAsync(input.Length);
            Status = "Complete";
            return result;
        }

        private async Task<int> CalculateAsync(int value) {
            await Task.Delay(10);
            return Helper.Transform(value);
        }

        public static class Helper {
            public static int Transform(int x) {
                return x * 2;
            }
        }
    }

    public class Consumer {
        public async Task UseServiceAsync() {
            var service = new ComplexService();
            service.Status = "Starting";

            var result = await service.ProcessAsync("test");
            var status = service.Status;

            var transformed = ComplexService.Helper.Transform(result);
        }
    }
}
