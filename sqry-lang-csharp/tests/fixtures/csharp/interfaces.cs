// Interface declarations and implementations

namespace Demo.Interfaces {
    public interface IRepository {
        void Save(string data);
        string Load();
    }

    public interface ILogger {
        void Log(string message);
    }

    public class Repository : IRepository, ILogger {
        public void Save(string data) {
            Log($"Saving: {data}");
        }

        public string Load() {
            Log("Loading data");
            return "data";
        }

        public void Log(string message) {
            // Log implementation
        }
    }

    public class Service {
        private IRepository _repo;

        public Service(IRepository repo) {
            _repo = repo;
        }

        public void Process() {
            _repo.Save("test");
            var data = _repo.Load();
        }
    }
}
