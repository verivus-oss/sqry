// Multiple namespaces and qualification

namespace Demo.Core {
    public class CoreService {
        public void ProcessCore() {
            var util = new Demo.Utils.Utility();
            util.HelperMethod();
        }
    }
}

namespace Demo.Utils {
    public class Utility {
        public void HelperMethod() {
            var core = new Demo.Core.CoreService();
        }
    }
}

namespace Demo.App {
    public class Application {
        public void Run() {
            var service = new Demo.Core.CoreService();
            service.ProcessCore();

            var util = new Demo.Utils.Utility();
            util.HelperMethod();
        }
    }
}
