// Swift code calling C functions through bridging header

import Foundation

class BridgingController {
    func setup() {
        // Calls C function from bridging header
        initialize_c_library()
    }

    func processValue(_ value: Int) -> Int {
        // Calls C function that returns a value
        return Int(process_data(Int32(value)))
    }

    func getLibraryVersion() -> String {
        // Calls C function returning a string
        guard let cString = get_version() else {
            return "unknown"
        }
        return String(cString: cString)
    }

    func teardown() {
        cleanup_resources()
    }

    // Also has regular Swift method calls
    func run() {
        setup()
        let result = processValue(42)
        _ = getLibraryVersion()
        teardown()
    }
}
